//! 播放器窗体（方向 C）：主视图大封面 + 正在播 + 大控制键；列表（收藏/
//! 分类/地区/搜索）收进第二页；播放中底部迷你条常显。
//!
//! 线程约定：全部控件只在主线程触碰；网络/播放引擎在 std::thread，结果经
//! glib 事件通道回主循环（面板同款模式）。窗体可隐藏——close 只是 withdraw，
//! 收听不中断（设计稿 §6.2，关窗提示只提示一次）。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use gtk::gdk_pixbuf::PixbufLoader;
use gtk::glib;
use gtk::prelude::*;

use crate::api::{self, Channel, Named};
use crate::engine::Engine;
use crate::store;

/// Picture 设 Pixbuf（set_pixbuf 自 4.12 弃用，此处封一层避免散落 allow）
#[allow(deprecated)]
fn set_picture_pixbuf(pic: &gtk::Picture, pix: &gtk::gdk_pixbuf::Pixbuf) {
    pic.set_pixbuf(Some(pix));
}

/// 分类缓存（地区页「分类 × 地区」交叉下拉用；随分类浏览回填）
fn cat_cache() -> &'static Mutex<Vec<(i64, String)>> {
    static CACHE: OnceLock<Mutex<Vec<(i64, String)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(vec![(0, "全部分类".to_string())]))
}

fn cat_cache_store(cats: &[Named]) {
    let mut c = cat_cache().lock().unwrap();
    *c = vec![(0, "全部分类".to_string())];
    c.extend(cats.iter().map(|n| (n.id, n.title.clone())));
}

fn cat_cache_snapshot() -> Vec<(i64, String)> {
    cat_cache().lock().unwrap().clone()
}

/// 事件通道载荷（线程 → 主循环）
pub enum Event {
    Categories(Result<Vec<Named>, String>),
    Regions(Result<Vec<Named>, String>),
    Channels {
        append: bool,
        page: u32,
        tag: Mode,
        res: Result<Vec<Channel>, String>,
    },
    Search {
        kw: String,
        res: Result<Vec<Channel>, String>,
    },
    /// 行封面（列表页，占位 → 回填）。跨线程只传字节（Pixbuf 非 Send），
    /// 主线程 PixbufLoader 解码。
    RowCover { id: i64, bytes: Option<Vec<u8>> },
    /// 主封面（播放页）
    Cover { id: i64, bytes: Option<Vec<u8>> },
    Played { res: Result<(), String>, ch: Channel },
    /// 正在播节目/元信息刷新（v4 详情）
    Detail(Result<Channel, String>),
    /// gst-play 进程退出（直播流中断）
    EngineDied,
}

/// 浏览态（列表页两级），back 沿历史栈回退
#[derive(Clone, PartialEq)]
pub enum Mode {
    Player,
    Favorites,
    CategoryMenu,
    CategoryList { cat: i64, name: String },
    RegionMenu,
    RegionList { region: i64, name: String, cat: i64 },
    Search,
}

pub struct RadioApp {
    app: gtk::Application,
    win: gtk::Window,
    stack: gtk::Stack,
    // 播放页
    cover_pic: gtk::Picture,
    cover_fb: gtk::Label,
    cover_stack: gtk::Stack,
    prog_label: gtk::Label,
    title_label: gtk::Label,
    meta_label: gtk::Label,
    fav_btn: gtk::Button,
    play_btn: gtk::Button,
    vol_scale: gtk::Scale,
    browse_btns: Vec<gtk::Button>,
    // 列表页
    back_btn: gtk::Button,
    browse_title: gtk::Label,
    search_entry: gtk::Entry,
    region_cat_dd: gtk::DropDown,
    scroller: gtk::ScrolledWindow,
    list: gtk::ListBox,
    // 迷你条
    mini_bar: gtk::Box,
    mini_title: gtk::Label,
    mini_stop: gtk::Button,
    // 状态
    engine: Arc<Engine>,
    /// 事件槽（线程 push、主循环 30ms 轮询 drain——面板 cap_async 同款）
    slot: Arc<Mutex<Vec<Event>>>,
    quit_btn: gtk::Button,
    cur: RefCell<Option<Channel>>,
    playing: Cell<bool>,
    favs: RefCell<Vec<store::Fav>>,
    mode: RefCell<Mode>,
    history: RefCell<Vec<Mode>>,
    page: Cell<u32>,
    menu_items: RefCell<Vec<Named>>,
    row_covers: RefCell<HashMap<i64, gtk::Picture>>,
    vol: Cell<u32>,
    /// 封面加载去重（同一封面避免重复线程）
    covers_inflight: RefCell<HashSet<i64>>,
}

impl RadioApp {
    /// 构建整窗（只在主线程第一次 command-line 调用时执行一次）
    pub fn build(app: &gtk::Application) -> Rc<RadioApp> {
        // 后台播放：窗口隐藏也要活着（g_application_hold）
        std::mem::forget(app.hold());

        let win = gtk::Window::new();
        win.set_application(Some(app));
        win.set_title(Some("电台"));
        win.set_icon_name(Some("applications-multimedia-symbolic"));
        win.set_default_size(420, 620);
        win.add_css_class("rp-root");

        // —— 播放页 ——
        let cover_pic = gtk::Picture::new();
        cover_pic.set_size_request(168, 168);
        cover_pic.set_content_fit(gtk::ContentFit::Cover);
        cover_pic.add_css_class("rp-cover");
        let cover_fb = gtk::Label::new(None);
        cover_fb.set_size_request(168, 168);
        cover_fb.set_valign(gtk::Align::Center);
        cover_fb.add_css_class("rp-cover-fallback");
        let cover_stack = gtk::Stack::new();
        cover_stack.add_named(&cover_fb, Some("fb"));
        cover_stack.add_named(&cover_pic, Some("pic"));

        let prog_label = gtk::Label::new(None);
        prog_label.add_css_class("rp-prog");
        prog_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let title_label = gtk::Label::new(Some("选择一个电台开始收听"));
        title_label.add_css_class("rp-title");
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let meta_label = gtk::Label::new(None);
        meta_label.add_css_class("rp-meta");
        meta_label.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let fav_btn = gtk::Button::from_icon_name("non-starred-symbolic");
        fav_btn.add_css_class("rp-ctl");
        let play_btn = gtk::Button::from_icon_name("media-playback-start");
        play_btn.add_css_class("rp-play");

        let more_btn = gtk::MenuButton::new();
        more_btn.set_icon_name("open-menu-symbolic");
        more_btn.add_css_class("rp-more");
        let quit_btn = gtk::Button::with_label("退出（停播）");
        let pop = gtk::Popover::new();
        pop.set_child(Some(&quit_btn));
        more_btn.set_popover(Some(&pop));

        let vol_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        vol_scale.set_value(80.0);
        vol_scale.add_css_class("rp-vol");
        let vol_icon = gtk::Label::new(Some("🔊"));
        let vol_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        vol_row.set_halign(gtk::Align::Center);
        vol_row.set_margin_top(14);
        vol_row.append(&vol_icon);
        vol_row.append(&vol_scale);

        let ctl_row = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        ctl_row.set_halign(gtk::Align::Center);
        ctl_row.set_margin_top(18);
        ctl_row.append(&fav_btn);
        ctl_row.append(&play_btn);
        ctl_row.append(&more_btn);

        let player_page = gtk::Box::new(gtk::Orientation::Vertical, 10);
        player_page.set_margin_top(36);
        player_page.set_margin_bottom(12);
        player_page.set_valign(gtk::Align::Center);
        player_page.append(&cover_stack);
        player_page.append(&prog_label);
        player_page.append(&title_label);
        player_page.append(&meta_label);
        player_page.append(&ctl_row);
        player_page.append(&vol_row);

        let mut browse_btns = Vec::new();
        let entry_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        entry_row.set_halign(gtk::Align::Center);
        entry_row.set_margin_bottom(10);
        for (label, icon) in [
            ("收藏", "non-starred-symbolic"),
            ("分类", "view-list-symbolic"),
            ("地区", "globe-symbolic"),
            ("搜索", "system-search-symbolic"),
        ] {
            let b = gtk::Button::new();
            b.set_icon_name(icon);
            b.set_tooltip_text(Some(label));
            b.add_css_class("rp-ctl");
            entry_row.append(&b);
            browse_btns.push(b);
        }
        player_page.append(&entry_row);

        // —— 列表页 ——
        let back_btn = gtk::Button::from_icon_name("go-previous-symbolic");
        back_btn.add_css_class("rp-back");
        let browse_title = gtk::Label::new(None);
        browse_title.add_css_class("rp-title");
        browse_title.set_halign(gtk::Align::Start);
        browse_title.set_hexpand(true);
        let search_entry = gtk::Entry::new();
        search_entry.set_placeholder_text(Some("搜台名或频率，如 交通91.8"));
        search_entry.add_css_class("rp-entry");
        search_entry.set_hexpand(true);
        search_entry.set_visible(false);
        let region_cat_dd = gtk::DropDown::from_strings(&["全部分类"]);
        region_cat_dd.add_css_class("rp-cat");
        region_cat_dd.set_visible(false);

        let head_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head_row.set_margin_top(10);
        head_row.set_margin_bottom(6);
        head_row.set_margin_start(10);
        head_row.set_margin_end(10);
        head_row.append(&back_btn);
        head_row.append(&browse_title);
        head_row.append(&search_entry);

        let list = gtk::ListBox::new();
        list.add_css_class("rp-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&list));
        scroller.set_vexpand(true);
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

        let browse_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        browse_page.append(&head_row);
        browse_page.append(&region_cat_dd);
        browse_page.append(&scroller);

        // —— 迷你条 ——
        let mini_title = gtk::Label::new(None);
        mini_title.add_css_class("rp-mini-title");
        mini_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        mini_title.set_hexpand(true);
        mini_title.set_halign(gtk::Align::Start);
        let mini_stop = gtk::Button::from_icon_name("media-playback-stop");
        mini_stop.add_css_class("rp-back");
        let mini_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        mini_bar.add_css_class("rp-mini");
        mini_bar.append(&mini_title);
        mini_bar.append(&mini_stop);
        mini_bar.set_visible(false);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&player_page, Some("player"));
        stack.add_named(&browse_page, Some("browse"));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&stack);
        root.append(&mini_bar);
        win.set_child(Some(&root));

        let ra = Rc::new(RadioApp {
            app: app.clone(),
            win,
            stack,
            cover_pic,
            cover_fb,
            cover_stack,
            prog_label,
            title_label,
            meta_label,
            fav_btn,
            play_btn,
            vol_scale,
            browse_btns,
            back_btn,
            browse_title,
            search_entry,
            region_cat_dd,
            scroller,
            list,
            mini_bar,
            mini_title,
            mini_stop,
            engine: Arc::new(Engine::new()),
            slot: Arc::new(Mutex::new(Vec::new())),
            quit_btn,
            cur: RefCell::new(None),
            playing: Cell::new(false),
            favs: RefCell::new(store::load_favs()),
            mode: RefCell::new(Mode::Player),
            history: RefCell::new(Vec::new()),
            page: Cell::new(1),
            menu_items: RefCell::new(Vec::new()),
            row_covers: RefCell::new(HashMap::new()),
            vol: Cell::new(80),
            covers_inflight: RefCell::new(HashSet::new()),
        });

        // 事件泵：30ms 轮询事件槽（面板 cap_async 同款；主循环绝不阻塞）
        let ra2 = ra.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(30), move || {
            let events: Vec<Event> = ra2.slot.lock().unwrap().drain(..).collect();
            for ev in events {
                ra2.on_event(ev);
            }
            glib::ControlFlow::Continue
        });

        ra.wire();
        ra.apply_theme();
        ra
    }

    // —— 信号接线 ——

    fn wire(self: &Rc<Self>) {
        let ra = self.clone();
        self.play_btn.connect_clicked(move |_| ra.toggle_play());
        let ra = self.clone();
        self.fav_btn.connect_clicked(move |_| {
            if let Some(ch) = ra.cur.borrow().clone() {
                ra.toggle_fav_channel(&ch);
            }
        });
        let ra = self.clone();
        self.mini_stop.connect_clicked(move |_| ra.stop_playing());
        let ra = self.clone();
        self.vol_scale.connect_value_changed(move |s| {
            let v = s.value().round().clamp(0.0, 100.0) as u32;
            ra.vol.set(v);
            ra.engine.set_volume(v);
        });

        let modes = [Mode::Favorites, Mode::CategoryMenu, Mode::RegionMenu, Mode::Search];
        for (b, mode) in self.browse_btns.iter().zip(modes) {
            let ra = self.clone();
            b.connect_clicked(move |_| ra.enter_mode(mode.clone()));
        }
        let ra = self.clone();
        self.back_btn.connect_clicked(move |_| ra.go_back());
        let ra = self.clone();
        self.search_entry
            .connect_activate(move |e| ra.do_search(e.text().trim()));
        let ra = self.clone();
        self.list
            .connect_row_activated(move |_, row| ra.activate_row(row.index()));
        let ra = self.clone();
        self.scroller.connect_edge_overshot(move |_, pos| {
            if pos == gtk::PositionType::Bottom {
                ra.load_more();
            }
        });
        // 地区页分类下拉：已进入某地区列表时切换分类 → 重拉
        let ra = self.clone();
        self.region_cat_dd.connect_selected_notify(move |_| {
            if let Mode::RegionList { region, name, .. } = ra.mode.borrow().clone() {
                let cat = ra.selected_cat_id();
                ra.enter_channels(
                    Mode::RegionList { region, name: name.clone(), cat },
                    &name,
                );
            }
        });

        let ra = self.clone();
        self.quit_btn.connect_clicked(move |_| ra.quit());

        // 关窗 = 隐藏继续播（设计稿 §6.2；首次提示一次）
        self.win.connect_close_request(move |w| {
            w.set_visible(false);
            if !store::hide_hint_shown() {
                store::mark_hide_hint_shown();
                let _ = std::process::Command::new("notify-send")
                    .args([
                        "--app-name=一呼电台",
                        "电台已在后台继续播放，可从面板「电台」或插件中心找回；彻底退出在窗内菜单",
                    ])
                    .status();
            }
            glib::Propagation::Stop
        });

        // 周期任务：播放进程巡检（2s）/ 正在播刷新（120s）/ 主题跟随（3s）
        let ra = self.clone();
        glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
            if ra.playing.get() && !ra.engine.is_playing() {
                ra.on_event(Event::EngineDied);
            }
            glib::ControlFlow::Continue
        });
        let ra = self.clone();
        glib::timeout_add_local(std::time::Duration::from_secs(120), move || {
            if ra.playing.get() {
                if let Some(ch) = ra.cur.borrow().clone() {
                    ra.spawn(move |slot| {
                        slot.lock().unwrap().push(Event::Detail(api::detail(ch.id)));
                    });
                }
            }
            glib::ControlFlow::Continue
        });
        let ra = self.clone();
        glib::timeout_add_local(std::time::Duration::from_secs(3), move || {
            ra.apply_theme();
            glib::ControlFlow::Continue
        });

        // SIGTERM/SIGINT：优雅退出并停播（不留孤儿 gst-play）
        let ra = self.clone();
        glib::unix_signal_add_local(libc::SIGTERM, move || {
            ra.quit();
            glib::ControlFlow::Continue
        });
        let ra = self.clone();
        glib::unix_signal_add_local(libc::SIGINT, move || {
            ra.quit();
            glib::ControlFlow::Continue
        });
    }

    fn spawn<F>(&self, f: F)
    where
        F: FnOnce(&Mutex<Vec<Event>>) + Send + 'static,
    {
        let slot = self.slot.clone();
        std::thread::spawn(move || f(&slot));
    }

    fn apply_theme(&self) {
        static LAST: OnceLock<Mutex<String>> = OnceLock::new();
        let dark = crate::theme::detect_dark();
        let accent = crate::theme::detect_accent();
        let key = format!("{dark:?}/{}", crate::theme::rgb(accent, dark).0);
        let last = LAST.get_or_init(|| Mutex::new(String::new()));
        let mut l = last.lock().unwrap();
        if *l == key {
            return;
        }
        *l = key;
        drop(l);
        let css = gtk::CssProvider::new();
        css.load_from_string(&crate::theme::build_css(dark, accent));
        gtk::style_context_add_provider_for_display(
            &gtk::prelude::WidgetExt::display(&self.win),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    // —— 命令行分发（单实例转发入口） ——

    pub fn dispatch(self: &Rc<Self>, args: &[String]) {
        match args.first().map(|s| s.as_str()) {
            Some("--play") => {
                let cid: i64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                if cid == 0 {
                    return;
                }
                let title = if args.len() > 2 {
                    args[2..].join(" ")
                } else {
                    format!("频道 {cid}")
                };
                let mut ch = Channel::default();
                ch.id = cid;
                ch.title = title;
                self.play_channel(&ch);
            }
            Some("--stop") => self.stop_playing(),
            Some("--quit") => self.quit(),
            // 无参 / --open / 其他：开窗置前（single dispatch 语义）
            _ => {
                self.win.present();
            }
        }
    }

    pub fn quit(&self) {
        self.stop_playing();
        self.app.quit();
    }

    // —— 播放 ——

    fn toggle_play(&self) {
        if self.playing.get() {
            self.stop_playing();
        } else if let Some(ch) = self.cur.borrow().clone() {
            self.play_channel(&ch);
        }
    }

    fn play_channel(&self, ch: &Channel) {
        *self.cur.borrow_mut() = Some(ch.clone());
        self.playing.set(true);
        self.prog_label.set_text("⏳ 连接中…");
        self.sync_playing_ui();
        let ch = ch.clone();
        let cid = ch.id;
        let engine = self.engine.clone();
        self.spawn(move |slot| {
            let res = engine.play(&api::stream_url(ch.id));
            slot.lock().unwrap().push(Event::Played { res, ch });
        });
        // 详情补全：搜索行可能缺 nowplaying/封面/元信息
        self.spawn(move |slot| {
            slot.lock().unwrap().push(Event::Detail(api::detail(cid)));
        });
    }

    fn stop_playing(&self) {
        self.playing.set(false);
        self.engine.stop();
        store::write_status(false, None);
        self.sync_playing_ui();
    }

    /// 播放态 → 控件 + status.json + 迷你条
    fn sync_playing_ui(&self) {
        let cur = self.cur.borrow().clone();
        let playing = self.playing.get();
        self.play_btn.set_icon_name(if playing {
            "media-playback-stop"
        } else {
            "media-playback-start"
        });
        if playing {
            if let Some(ch) = &cur {
                self.mini_title
                    .set_text(&format!("🔊 {}", display_title(ch)));
                self.mini_bar.set_visible(true);
                self.title_label.set_text(&display_title(ch));
                self.meta_label.set_text(&ch.meta_line());
                self.prog_label
                    .set_text(&if ch.np_line().is_empty() { "⏳ 连接中…".into() } else { ch.np_line() });
                store::write_status(true, Some(ch));
            }
        } else {
            self.mini_bar.set_visible(false);
            self.prog_label.set_text("");
            self.title_label.set_text(&match &cur {
                Some(ch) => display_title(ch),
                None => "选择一个电台开始收听".to_string(),
            });
        }
        if let Some(ch) = &cur {
            self.set_main_cover(ch);
        }
        self.sync_fav_btn();
    }

    fn set_main_cover(&self, ch: &Channel) {
        let first = display_title(ch).chars().next().unwrap_or('📻');
        self.cover_fb.set_text(&first.to_string());
        let url = ch.cover_url();
        if url.is_empty() {
            self.cover_stack.set_visible_child_name("fb");
            return;
        }
        if let Some(p) = store::cover_cached(ch.id) {
            match std::fs::read(p).ok().as_deref().map(Self::decode_pixbuf).flatten() {
                Some(pix) => {
                    set_picture_pixbuf(&self.cover_pic, &pix);
                    self.cover_stack.set_visible_child_name("pic");
                }
                None => self.cover_stack.set_visible_child_name("fb"),
            }
            return;
        }
        self.cover_stack.set_visible_child_name("fb");
        self.fetch_cover_async(ch.id, &url, true);
    }

    fn fetch_cover_async(&self, id: i64, url: &str, main: bool) {
        if !self.covers_inflight.borrow_mut().insert(id) {
            return;
        }
        let url = url.to_string();
        self.spawn(move |slot| {
            let bytes = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(8))
                .call()
                .ok()
                .and_then(|r| {
                    let mut buf = Vec::new();
                    r.into_reader()
                        .take(4 * 1024 * 1024)
                        .read_to_end(&mut buf)
                        .ok()?;
                    Some(buf)
                });
            // 落缓存（成功才有）；跨线程只传字节，主线程解码（Pixbuf 非 Send）
            let bytes = bytes.and_then(|b| {
                store::cover_store(id, &b)?;
                Some(b)
            });
            let ev = if main {
                Event::Cover { id, bytes }
            } else {
                Event::RowCover { id, bytes }
            };
            slot.lock().unwrap().push(ev);
        });
    }

    /// 字节 → 圆角 Pixbuf（主线程解码；GTK4 无 CSS overflow，圆角在
    /// 像素层打 alpha 蒙版，半径随边长缩放：168px 主封面 ≈24px，40px 行 ≈10px）
    fn decode_pixbuf(bytes: &[u8]) -> Option<gtk::gdk_pixbuf::Pixbuf> {
        let loader = PixbufLoader::new();
        loader.write(bytes).ok()?;
        loader.close().ok()?;
        let pix = loader.pixbuf()?;
        Some(round_corners(&pix, (pix.width() / 7).clamp(8, 28)))
    }

    fn sync_fav_btn(&self) {
        let is_fav = self
            .cur
            .borrow()
            .as_ref()
            .map(|c| self.favs.borrow().iter().any(|f| f.id == c.id))
            .unwrap_or(false);
        self.fav_btn
            .set_icon_name(if is_fav { "starred-symbolic" } else { "non-starred-symbolic" });
        if is_fav {
            self.fav_btn.add_css_class("rp-star-fav");
        } else {
            self.fav_btn.remove_css_class("rp-star-fav");
        }
    }

    fn toggle_fav_channel(self: &Rc<Self>, ch: &Channel) {
        let mut favs = self.favs.borrow_mut();
        store::toggle_fav(&mut favs, ch);
        if let Err(e) = store::save_favs(&favs) {
            eprintln!("[电台] 收藏保存失败：{e}");
        }
        drop(favs);
        self.sync_fav_btn();
        if *self.mode.borrow() == Mode::Favorites {
            self.render_favorites();
        }
        // 行内星标同步：重渲染当前列表（收藏/非收藏视图都涉及星态）
        // —— 行级按钮各自闭包已持有旧状态，整体重绘最简单
    }

    // —— 浏览 ——

    fn enter_mode(self: &Rc<Self>, mode: Mode) {
        self.history.borrow_mut().push(self.mode.borrow().clone());
        self.show_mode(mode);
    }

    fn go_back(self: &Rc<Self>) {
        let prev = self.history.borrow_mut().pop().unwrap_or(Mode::Player);
        self.show_mode(prev);
    }

    fn show_mode(self: &Rc<Self>, mode: Mode) {
        *self.mode.borrow_mut() = mode.clone();
        self.stack.set_visible_child_name("browse");
        match mode {
            Mode::Player => {
                self.stack.set_visible_child_name("player");
            }
            Mode::Favorites => self.render_favorites(),
            Mode::CategoryMenu => {
                self.browse_title.set_text("分类浏览");
                self.search_entry.set_visible(false);
                self.region_cat_dd.set_visible(false);
                self.render_menu(&[("加载中…".to_string(), String::new())], Vec::new());
                self.spawn(move |slot| {
                    slot.lock().unwrap().push(Event::Categories(api::categories()));
                });
            }
            Mode::CategoryList { .. } => {} // enter_channels 负责
            Mode::RegionMenu => {
                self.browse_title.set_text("地区浏览");
                self.search_entry.set_visible(false);
                self.region_cat_dd.set_visible(true);
                self.fill_region_cat_dd();
                self.render_menu(&[("加载中…".to_string(), String::new())], Vec::new());
                self.spawn(move |slot| {
                    slot.lock().unwrap().push(Event::Regions(api::regions()));
                });
            }
            Mode::RegionList { .. } => {} // enter_channels 负责
            Mode::Search => {
                self.browse_title.set_text("搜索");
                self.search_entry.set_visible(true);
                self.region_cat_dd.set_visible(false);
                self.clear_list();
                self.search_entry.grab_focus();
            }
        }
    }

    /// 地区页的分类交叉下拉（billboards/{分类}/{地区}，0 = 全部）
    fn fill_region_cat_dd(&self) {
        let cache = cat_cache_snapshot();
        let names: Vec<&str> = cache.iter().map(|(_, n)| n.as_str()).collect();
        let model = gtk::StringList::new(&names);
        self.region_cat_dd.set_model(Some(&model));
        if cache.len() <= 1 {
            // 分类缓存还空：后台拉一次回填（事件里会重建下拉）
            self.spawn(move |slot| {
                slot.lock().unwrap().push(Event::Categories(api::categories()));
            });
        }
    }

    fn selected_cat_id(&self) -> i64 {
        let pos = self.region_cat_dd.selected();
        cat_cache_snapshot()
            .get(pos as usize)
            .map(|(id, _)| *id)
            .unwrap_or(0)
    }

    fn do_search(&self, kw: &str) {
        if kw.is_empty() {
            return;
        }
        self.browse_title.set_text(&format!("搜索「{kw}」"));
        self.render_menu(&[("搜索中…".to_string(), String::new())], Vec::new());
        let kw = kw.to_string();
        self.spawn(move |slot| {
            let res = api::search(&kw);
            slot.lock().unwrap().push(Event::Search { kw, res });
        });
    }

    fn load_more(&self) {
        let mode = self.mode.borrow().clone();
        if let Mode::CategoryList { cat, .. } = mode {
            let page = self.page.get() + 1;
            self.page.set(page);
            self.spawn(move |slot| {
                let res = api::category_channels(cat, page);
                slot.lock().unwrap().push(Event::Channels {
                    append: true,
                    page,
                    tag: Mode::CategoryList { cat, name: String::new() },
                    res,
                });
            });
        }
        // RegionList 组合榜单无分页参数（实测单页），不翻页
    }

    fn clear_list(&self) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        self.row_covers.borrow_mut().clear();
        self.menu_items.borrow_mut().clear();
    }

    /// 菜单行（分类/地区）
    fn render_menu(&self, rows: &[(String, String)], items: Vec<Named>) {
        self.clear_list();
        *self.menu_items.borrow_mut() = items;
        for (title, sub) in rows {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(true);
            let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
            let t = gtk::Label::new(Some(title.as_str()));
            t.add_css_class("rp-row-title");
            t.set_halign(gtk::Align::Start);
            b.append(&t);
            if !sub.is_empty() {
                let s = gtk::Label::new(Some(sub.as_str()));
                s.add_css_class("rp-row-sub");
                s.set_halign(gtk::Align::Start);
                b.append(&s);
            }
            row.set_child(Some(&b));
            self.list.append(&row);
        }
    }

    /// 频道列表行：封面占位 + 标题 + 副标题 + [☆][▶]
    fn render_channels(self: &Rc<Self>, chans: &[Channel], append: bool) {
        if !append {
            self.clear_list();
        }
        let fav_ids = store::fav_ids(&self.favs.borrow());
        for ch in chans {
            let id = ch.id;
            let row = gtk::ListBoxRow::new();
            row.set_activatable(true);
            let h = gtk::Box::new(gtk::Orientation::Horizontal, 8);

            let pic = gtk::Picture::new();
            pic.set_size_request(40, 40);
            pic.set_content_fit(gtk::ContentFit::Cover);
            pic.add_css_class("rp-row-cover");
            let fb = gtk::Label::new(Some(
                &display_title(ch).chars().next().unwrap_or('📻').to_string(),
            ));
            fb.set_size_request(40, 40);
            fb.set_valign(gtk::Align::Center);
            fb.add_css_class("rp-row-fb");
            let cover_wrap = gtk::Stack::new();
            cover_wrap.add_named(&fb, Some("fb"));
            cover_wrap.add_named(&pic, Some("pic"));
            cover_wrap.set_visible_child_name("fb");
            h.append(&cover_wrap);
            self.row_covers.borrow_mut().insert(id, pic);

            let v = gtk::Box::new(gtk::Orientation::Vertical, 2);
            v.set_valign(gtk::Align::Center);
            v.set_hexpand(true);
            let t = gtk::Label::new(Some(&display_title(ch)));
            t.add_css_class("rp-row-title");
            t.set_halign(gtk::Align::Start);
            t.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let s = gtk::Label::new(Some(&ch.sub_line()));
            s.add_css_class("rp-row-sub");
            s.set_halign(gtk::Align::Start);
            s.set_ellipsize(gtk::pango::EllipsizeMode::End);
            v.append(&t);
            v.append(&s);
            h.append(&v);

            let fav = fav_ids.contains(&id);
            let star = gtk::Button::from_icon_name(if fav {
                "starred-symbolic"
            } else {
                "non-starred-symbolic"
            });
            star.add_css_class("rp-star");
            if fav {
                star.add_css_class("rp-star-fav-on");
            }
            let ra = self.clone();
            let ch2 = ch.clone();
            star.connect_clicked(move |_| ra.toggle_fav_channel(&ch2));
            h.append(&star);

            let play = gtk::Button::from_icon_name("media-playback-start");
            play.add_css_class("rp-back");
            let ra = self.clone();
            let ch3 = ch.clone();
            play.connect_clicked(move |_| ra.play_channel(&ch3));
            h.append(&play);

            row.set_child(Some(&h));
            self.list.append(&row);

            // 封面：缓存命中即填；否则异步拉
            let url = ch.cover_url();
            if !url.is_empty() {
                let cached = store::cover_cached(id)
                    .and_then(|p| std::fs::read(p).ok())
                    .and_then(|b| Self::decode_pixbuf(&b));
                if let Some(pix) = cached {
                    self.apply_row_cover(id, &pix);
                } else {
                    self.fetch_cover_async(id, &url, false);
                }
            }
        }
    }

    fn apply_row_cover(&self, id: i64, pix: &gtk::gdk_pixbuf::Pixbuf) {
        let pic = self.row_covers.borrow().get(&id).cloned();
        if let Some(pic) = pic {
            set_picture_pixbuf(&pic, pix);
            if let Some(stack) = pic.parent().and_then(|p| p.downcast::<gtk::Stack>().ok()) {
                stack.set_visible_child_name("pic");
            }
        }
    }

    fn render_favorites(self: &Rc<Self>) {
        self.browse_title.set_text("我的收藏");
        self.search_entry.set_visible(false);
        self.region_cat_dd.set_visible(false);
        let favs = self.favs.borrow();
        if favs.is_empty() {
            self.render_menu(
                &[(
                    "还没有收藏".to_string(),
                    "在频道行点 ☆ 或播放页星标收藏".to_string(),
                )],
                Vec::new(),
            );
            return;
        }
        let chans: Vec<Channel> = favs
            .iter()
            .map(|f| {
                let mut c = Channel::default();
                c.id = f.id;
                c.title = f.title.clone();
                c
            })
            .collect();
        drop(favs);
        self.render_channels(&chans, false);
    }

    /// 菜单行激活（按索引映射 menu_items）
    fn activate_row(&self, index: i32) {
        let mode = self.mode.borrow().clone();
        let item = self.menu_items.borrow().get(index as usize).cloned();
        match mode {
            Mode::CategoryMenu => {
                if let Some(cat) = item {
                    let name = cat.title.clone();
                    self.enter_channels(
                        Mode::CategoryList { cat: cat.id, name: name.clone() },
                        &name,
                    );
                }
            }
            Mode::RegionMenu => {
                if let Some(region) = item {
                    let name = region.title.clone();
                    let cat = self.selected_cat_id();
                    self.enter_channels(
                        Mode::RegionList { region: region.id, name: name.clone(), cat },
                        &name,
                    );
                }
            }
            _ => {}
        }
    }

    fn enter_channels(&self, mode: Mode, title: &str) {
        self.history.borrow_mut().push(self.mode.borrow().clone());
        *self.mode.borrow_mut() = mode.clone();
        self.browse_title.set_text(title);
        self.page.set(1);
        self.render_menu(&[("加载中…".to_string(), String::new())], Vec::new());
        self.spawn(move |slot| {
            let res = match mode {
                Mode::CategoryList { cat, .. } => api::category_channels(cat, 1),
                Mode::RegionList { region, cat, .. } => api::billboards(cat, region),
                _ => Ok(Vec::new()),
            };
            slot.lock().unwrap().push(Event::Channels { append: false, page: 1, tag: mode, res });
        });
    }

    // —— 事件处理 ——

    pub fn on_event(self: &Rc<Self>, ev: Event) {
        match ev {
            Event::Categories(res) => {
                let cats = res.unwrap_or_default();
                cat_cache_store(&cats);
                match self.mode.borrow().clone() {
                    Mode::CategoryMenu => {
                        if cats.is_empty() {
                            self.render_menu(&[("分类获取失败".to_string(), String::new())], Vec::new());
                        } else {
                            let rows: Vec<(String, String)> = cats
                                .iter()
                                .map(|c| (c.title.clone(), String::new()))
                                .collect();
                            self.render_menu(&rows, cats);
                        }
                    }
                    Mode::RegionMenu => self.fill_region_cat_dd(), // 借道回填，重建下拉
                    _ => {}
                }
            }
            Event::Regions(res) => {
                if *self.mode.borrow() != Mode::RegionMenu {
                    return;
                }
                let regions = res.unwrap_or_default();
                if regions.is_empty() {
                    self.render_menu(&[("地区获取失败".to_string(), String::new())], Vec::new());
                } else {
                    let rows: Vec<(String, String)> = regions
                        .iter()
                        .map(|r| (r.title.clone(), String::new()))
                        .collect();
                    self.render_menu(&rows, regions);
                }
            }
            Event::Channels { append, page, tag, res } => {
                // 迟到的旧页丢弃（tag 匹配当前视图；翻页只认最新 page）
                if *self.mode.borrow() != tag || page != self.page.get() {
                    return;
                }
                match res {
                    Err(e) if !append => {
                        self.render_menu(&[(format!("获取失败：{e}"), String::new())], Vec::new())
                    }
                    Err(_) => {}
                    Ok(chans) if chans.is_empty() && !append => {
                        self.render_menu(&[("暂无频道".to_string(), String::new())], Vec::new())
                    }
                    Ok(chans) => self.render_channels(&chans, append),
                }
            }
            Event::Search { kw, res } => {
                // 只认当前搜索词（迟到回包丢弃）
                if !self.browse_title.text().contains(&kw) {
                    return;
                }
                match res {
                    Err(e) => self.render_menu(
                        &[(format!("搜索失败：{e}"), String::new())],
                        Vec::new(),
                    ),
                    Ok(chans) if chans.is_empty() => self.render_menu(
                        &[(format!("没有找到「{kw}」的直播台"), String::new())],
                        Vec::new(),
                    ),
                    Ok(chans) => self.render_channels(&chans, false),
                }
            }
            Event::RowCover { id, bytes } => {
                self.covers_inflight.borrow_mut().remove(&id);
                if let Some(pix) = bytes.as_deref().map(Self::decode_pixbuf).flatten() {
                    self.apply_row_cover(id, &pix);
                }
            }
            Event::Cover { id, bytes } => {
                self.covers_inflight.borrow_mut().remove(&id);
                let cur_ok = self.cur.borrow().as_ref().is_some_and(|c| c.id == id);
                if cur_ok {
                    match bytes.as_deref().map(Self::decode_pixbuf).flatten() {
                        Some(pix) => {
                            set_picture_pixbuf(&self.cover_pic, &pix);
                            self.cover_stack.set_visible_child_name("pic");
                        }
                        None => self.cover_stack.set_visible_child_name("fb"),
                    }
                }
            }
            Event::Played { res, ch } => {
                match res {
                    Ok(()) => {
                        // 保留本次播放台的完整信息（详情回包可能尚未到）
                        let mut merged = ch.clone();
                        if let Some(c) = self.cur.borrow().as_ref() {
                            if c.id == ch.id {
                                merged.title = if c.title.is_empty() { ch.title.clone() } else { c.title.clone() };
                                merged.cover = if c.cover.is_empty() { ch.cover.clone() } else { c.cover.clone() };
                                merged.nowplaying = c.nowplaying.clone().or(ch.nowplaying.clone());
                            }
                        }
                        *self.cur.borrow_mut() = Some(merged);
                        self.playing.set(true);
                        self.engine.set_volume(self.vol.get());
                        self.sync_playing_ui();
                    }
                    Err(e) => {
                        self.playing.set(false);
                        store::write_status(false, None);
                        self.sync_playing_ui();
                        self.prog_label.set_text(&format!("⚠ {e}"));
                    }
                }
            }
            Event::Detail(Ok(d)) => {
                let mut cur = self.cur.borrow_mut();
                let Some(c) = cur.as_mut() else { return };
                if c.id != d.id {
                    return;
                }
                // 详情补全（搜索结果缺 np/cover/元信息）
                if c.title.is_empty() || c.title.starts_with("频道 ") {
                    c.title = d.title.clone();
                }
                if c.cover.is_empty() {
                    c.cover = d.cover.clone();
                }
                c.nowplaying = d.nowplaying.clone();
                c.city = d.city.clone();
                c.top_category_title = d.top_category_title.clone();
                c.audience_count = d.audience_count.clone();
                drop(cur);
                if self.playing.get() {
                    self.sync_playing_ui();
                    let url = self.cur.borrow().as_ref().map(|c| c.cover_url()).unwrap_or_default();
                    let id = d.id;
                    if url.is_empty() {
                        self.cover_stack.set_visible_child_name("fb");
                    } else if store::cover_cached(id).is_none() {
                        self.fetch_cover_async(id, &url, true);
                    }
                }
            }
            Event::Detail(Err(_)) => {
                // 详情刷新失败静默：保留旧正在播信息
            }
            Event::EngineDied => {
                self.playing.set(false);
                store::write_status(false, None);
                self.sync_playing_ui();
                self.prog_label.set_text("⚠ 播放已中断");
            }
        }
    }
}

/// 给 Pixbuf 打圆角 alpha 蒙版（四角外透明；非 RGBA 先转 RGBA）
fn round_corners(
    src: &gtk::gdk_pixbuf::Pixbuf,
    radius: i32,
) -> gtk::gdk_pixbuf::Pixbuf {
    use gtk::gdk_pixbuf::Colorspace::Rgb;
    let has_alpha = src.has_alpha();
    let pix = if has_alpha {
        src.clone()
    } else {
        match gtk::gdk_pixbuf::Pixbuf::new(Rgb, true, 8, src.width(), src.height()) {
            Some(mut dst) => {
                src.copy_area(0, 0, src.width(), src.height(), &mut dst, 0, 0);
                dst
            }
            None => src.clone(),
        }
    };
    if !pix.has_alpha() {
        return pix; // 转换失败：退化为直角
    }
    let (w, h) = (pix.width(), pix.height());
    let (n_ch, stride): (usize, usize) = (pix.n_channels() as usize, pix.rowstride() as usize);
    let r = radius.min(w / 2).min(h / 2);
    let px = unsafe { pix.pixels() };
    let mut clear = |x: i32, y: i32| {
        if x < 0 || y < 0 || x >= w || y >= h {
            return;
        }
        px[(y as usize * stride) + (x as usize * n_ch) + 3] = 0;
    };
    for cy in 0..r {
        for cx in 0..r {
            // 圆方程：角内点到圆心距离超半径 → 透明
            let (dx, dy) = ((r - cx) * (r - cx), (r - cy) * (r - cy));
            if dx + dy > r * r {
                clear(cx, cy);
                clear(w - 1 - cx, cy);
                clear(cx, h - 1 - cy);
                clear(w - 1 - cx, h - 1 - cy);
            }
        }
    }
    pix
}

fn display_title(ch: &Channel) -> String {
    if ch.title.is_empty() {
        format!("频道 {}", ch.id)
    } else {
        ch.title.clone()
    }
}
