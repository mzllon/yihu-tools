//! 中心「广播」页：在线电台搜索/分类收听与本地收藏。
//!
//! 布局参考墨鱼FM：主页为搜索框 + 分类瓷贴，点分类/搜索后推入
//! 二级频道列表页（NavigationView 自带返回），底部迷你播放条跨页常驻。
//! 数据来自蜻蜓FM 公开接口（后台线程 + 主循环轮询，同右键菜单页惯例），
//! 播放走 gst-launch 子进程（radio_player），音量经 pactl 按流单独控制。
//! 关窗驻留托盘时声音继续，退出中心时统一结束子进程。

use adw::prelude::*;
use gtk::{gio, glib, gdk};
use gtk::{
    Align, Box as GtkBox, Button, FlowBox, Image, Label, Scale, ScrolledWindow, SearchEntry,
    SelectionMode,
};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cover_disc;
use crate::mpris;
use crate::radio_api::{self, Category, Channel, Favorite, FAVORITES_ID};
use crate::radio_player::{self, RadioPlayer};
use crate::tray;

/// 搜索结果伪分类 id；不对应任何瓷贴。
const SEARCH_ID: u64 = u64::MAX;
const ICON_DEFAULT: &str = "audio-x-generic-symbolic";
const ICON_STAR: &str = "starred-symbolic";
const ICON_UNSTAR: &str = "non-starred-symbolic";
const ICON_PLAY: &str = "media-playback-start-symbolic";
const ICON_STOP: &str = "media-playback-stop-symbolic";

struct Ui {
    player: Rc<RadioPlayer>,
    /// 正在播放的频道 id；None 即未播放。
    playing: Cell<Option<u64>>,
    /// 最近一次播放，用于停止后再次播放。
    last_channel: Cell<Option<u64>>,
    busy: Cell<bool>,
    has_more: Cell<bool>,
    category: Cell<u64>,
    page: Cell<u32>,
    /// 二级列表页是否已在导航栈中（返回时复位）。
    detail_pushed: Cell<bool>,
    /// 瓷贴对应的分类 id（第 0 个固定为「我的收藏」）。
    category_ids: RefCell<Vec<u64>>,
    /// 分类 id → 标题（含收藏伪分类），用于二级页标题。
    category_titles: RefCell<Vec<(u64, String)>>,
    channels: RefCell<Vec<Channel>>,
    favorites: RefCell<Vec<Favorite>>,
    tiles: RefCell<Vec<(u64, Button)>>,
    play_buttons: RefCell<Vec<(u64, Button)>>,
    star_buttons: RefCell<Vec<(u64, Button)>>,
    cover_images: RefCell<Vec<(u64, Image)>>,
    category_flow: FlowBox,
    nav_view: adw::NavigationView,
    detail_page: adw::NavigationPage,
    detail_title: adw::WindowTitle,
    installing: Cell<bool>,
    dep_missing: RefCell<Vec<&'static str>>,
    dep_card: GtkBox,
    install_btn: Button,
    dep_state: Label,
    toggle_btn: Button,
    np_fav_btn: Button,
    vol_scale: Scale,
    /// 迷你播放条上的圆形封面盘；播放时旋转。
    bar_cover: cover_disc::CoverDisc,
    bar_cover_id: Cell<u64>,
    mpris: Option<mpris::RadioMpris>,
    mpris_state: RefCell<String>,
    np_title: Label,
    state_caption: Label,
    channel_box: GtkBox,
    more_btn: Button,
}

pub fn build_page() -> gtk::Widget {
    // —— 主页：搜索 + 分类瓷贴 ——
    let search_row = GtkBox::new(gtk::Orientation::Horizontal, 8);
    let search_entry = SearchEntry::new();
    search_entry.set_placeholder_text(Some("请输入电台关键字搜索，回车确认"));
    search_entry.set_hexpand(true);
    let search_btn = Button::with_label("搜索");
    search_row.append(&search_entry);
    search_row.append(&search_btn);

    let cat_flow = FlowBox::new();
    cat_flow.set_selection_mode(SelectionMode::None);
    cat_flow.set_min_children_per_line(3);
    cat_flow.set_max_children_per_line(3);
    cat_flow.set_row_spacing(8);
    cat_flow.set_column_spacing(8);
    cat_flow.set_homogeneous(true);
    cat_flow.set_hexpand(true);

    let main_view = GtkBox::new(gtk::Orientation::Vertical, 14);
    main_view.set_margin_top(12);
    main_view.set_margin_bottom(12);
    main_view.set_margin_start(24);
    main_view.set_margin_end(24);
    main_view.set_valign(Align::Start);
    main_view.append(&search_row);
    main_view.append(&cat_flow);

    // —— 播放依赖（仅缺失时出现，pkexec 一键安装，同「右键菜单」页惯例）——
    let dep_card = GtkBox::new(gtk::Orientation::Vertical, 10);
    dep_card.add_css_class("card");
    dep_card.add_css_class("card-pad");
    dep_card.set_hexpand(true);
    let dep_title = Label::new(Some("播放依赖"));
    dep_title.add_css_class("sec-title");
    dep_title.set_halign(Align::Start);
    dep_card.append(&dep_title);
    let dep_row = GtkBox::new(gtk::Orientation::Horizontal, 8);
    let dep_name = Label::new(Some("GStreamer 播放组件"));
    dep_name.add_css_class("dim-label");
    dep_name.set_hexpand(true);
    dep_name.set_halign(Align::Start);
    dep_name.set_valign(Align::Center);
    let dep_state = Label::new(Some("检查中…"));
    dep_state.add_css_class("caption-sm");
    dep_state.set_valign(Align::Center);
    let install_btn = Button::with_label("一键安装");
    install_btn.add_css_class("suggested-action");
    install_btn.set_valign(Align::Center);
    dep_row.append(&dep_name);
    dep_row.append(&dep_state);
    dep_row.append(&install_btn);
    dep_card.append(&dep_row);
    let dep_hint = Label::new(Some(
        "点击「一键安装」会弹出系统授权对话框，输入一次密码即可自动完成。",
    ));
    dep_hint.add_css_class("dim-label");
    dep_hint.add_css_class("caption-sm");
    dep_hint.set_wrap(true);
    dep_hint.set_halign(Align::Start);
    dep_card.append(&dep_hint);
    main_view.append(&dep_card);
    let main_page_view = adw::ToolbarView::new();
    let main_header = adw::HeaderBar::new();
    main_header.set_title_widget(Some(&adw::WindowTitle::new("广播", "")));
    main_page_view.add_top_bar(&main_header);
    main_page_view.set_content(Some(&main_view));
    let main_page = adw::NavigationPage::new(&main_page_view, "广播");

    // —— 二级页：频道列表 ——
    let channel_box = GtkBox::new(gtk::Orientation::Vertical, 10);
    let more_btn = Button::with_label("加载更多");
    more_btn.set_visible(false);
    channel_box.append(&more_btn);
    let scroll = ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&channel_box));
    scroll.set_vexpand(true);
    let detail_body = GtkBox::new(gtk::Orientation::Vertical, 8);
    detail_body.set_margin_top(12);
    detail_body.set_margin_bottom(12);
    detail_body.set_margin_start(24);
    detail_body.set_margin_end(24);
    detail_body.append(&scroll);
    let detail_title = adw::WindowTitle::new("频道", "");
    let detail_header = adw::HeaderBar::new();
    detail_header.set_title_widget(Some(&detail_title));
    let detail_page_view = adw::ToolbarView::new();
    detail_page_view.add_top_bar(&detail_header);
    detail_page_view.set_content(Some(&detail_body));
    let detail_page = adw::NavigationPage::new(&detail_page_view, "频道");

    let nav_view = adw::NavigationView::new();
    nav_view.replace(&[main_page]);

    // —— 底部迷你播放条（跨页常驻）——
    let player_bar = GtkBox::new(gtk::Orientation::Horizontal, 10);
    player_bar.set_margin_top(8);
    player_bar.set_margin_bottom(8);
    player_bar.set_margin_start(12);
    player_bar.set_margin_end(12);
    let toggle_btn = Button::from_icon_name(ICON_PLAY);
    let bar_cover = cover_disc::CoverDisc::new(ICON_DEFAULT);
    bar_cover.set_size_request(44, 44);
    bar_cover.set_valign(Align::Center);
    let bar_col = GtkBox::new(gtk::Orientation::Vertical, 2);
    bar_col.set_hexpand(true);
    bar_col.set_valign(Align::Center);
    let np_title = Label::new(Some("未在播放"));
    np_title.add_css_class("row-title");
    np_title.set_halign(Align::Start);
    np_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let state_caption = Label::new(Some(""));
    state_caption.add_css_class("dim-label");
    state_caption.add_css_class("caption-sm");
    state_caption.set_halign(Align::Start);
    state_caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
    bar_col.append(&np_title);
    bar_col.append(&state_caption);
    let vol_scale = Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 5.0);
    vol_scale.set_value(100.0);
    vol_scale.set_size_request(130, -1);
    vol_scale.set_valign(Align::Center);
    vol_scale.set_visible(false);
    let np_fav_btn = Button::from_icon_name(ICON_UNSTAR);
    np_fav_btn.set_tooltip_text(Some("收藏当前电台"));
    np_fav_btn.set_visible(false);
    player_bar.append(&toggle_btn);
    player_bar.append(&bar_cover);
    player_bar.append(&bar_col);
    player_bar.append(&vol_scale);
    player_bar.append(&np_fav_btn);

    // —— MPRIS：顶栏媒体卡片与媒体键；托盘「暂停/继续」复用同一命令通道 ——
    let (mpris, mpris_tx, mpris_rx) = mpris::start();
    tray::set_play_hook(Box::new(move || {
        let _ = mpris_tx.send(mpris::Command::PlayPause);
    }));
    let view = adw::ToolbarView::new();
    view.set_content(Some(&nav_view));
    view.add_bottom_bar(&player_bar);

    let ui = Rc::new(Ui {
        player: Rc::new(RadioPlayer::new()),
        playing: Cell::new(None),
        last_channel: Cell::new(None),
        busy: Cell::new(false),
        has_more: Cell::new(false),
        category: Cell::new(FAVORITES_ID),
        page: Cell::new(1),
        detail_pushed: Cell::new(false),
        category_ids: RefCell::new(vec![FAVORITES_ID]),
        category_titles: RefCell::new(vec![(FAVORITES_ID, "我的收藏".to_owned())]),
        channels: RefCell::new(Vec::new()),
        favorites: RefCell::new(radio_api::load_favorites()),
        tiles: RefCell::new(Vec::new()),
        play_buttons: RefCell::new(Vec::new()),
        star_buttons: RefCell::new(Vec::new()),
        cover_images: RefCell::new(Vec::new()),
        category_flow: cat_flow.clone(),
        nav_view: nav_view.clone(),
        detail_page: detail_page.clone(),
        detail_title: detail_title.clone(),
        installing: Cell::new(false),
        dep_missing: RefCell::new(radio_player::probe_missing()),
        dep_card: dep_card.clone(),
        install_btn: install_btn.clone(),
        dep_state: dep_state.clone(),
        toggle_btn: toggle_btn.clone(),
        np_fav_btn: np_fav_btn.clone(),
        vol_scale: vol_scale.clone(),
        bar_cover: bar_cover.clone(),
        bar_cover_id: Cell::new(0),
        mpris,
        mpris_state: RefCell::new(String::new()),
        np_title,
        state_caption,
        channel_box,
        more_btn: more_btn.clone(),
    });

    // 退出中心时结束播放子进程；关窗驻托盘不受影响。
    if let Some(app) = gio::Application::default() {
        let player = ui.player.clone();
        app.connect_shutdown(move |_| player.stop());
    }
    // SIGINT/SIGTERM 同样带走播放子进程,防止孤儿进程继续出声。
    for signal in [2, 15] {
        let player = ui.player.clone();
        glib::unix_signal_add_local(signal, move || {
            player.stop();
            if let Some(app) = gio::Application::default() {
                app.quit();
            }
            glib::ControlFlow::Continue
        });
    }

    // —— 信号 ——
    {
        let ui_c = ui.clone();
        let entry = search_entry.clone();
        search_btn.connect_clicked(move |_| {
            let kw = entry.text().trim().to_owned();
            run_search(&ui_c, &kw);
        });
    }
    {
        let ui_c = ui.clone();
        let entry = search_entry.clone();
        search_entry.connect_activate(move |_| {
            let kw = entry.text().trim().to_owned();
            run_search(&ui_c, &kw);
        });
    }
    {
        let ui_c = ui.clone();
        toggle_btn.connect_clicked(move |_| toggle_play(&ui_c));
    }
    {
        let ui_c = ui.clone();
        np_fav_btn.connect_clicked(move |_| {
            let Some(id) = ui_c.playing.get().or(ui_c.last_channel.get()) else { return };
            let channel = ui_c.channel_of(id).unwrap_or_else(|| Channel {
                content_id: id,
                title: "未知电台".into(),
                description: String::new(),
                now_playing: None,
                cover: String::new(),
                audience_count: String::new(),
            });
            toggle_favorite(&ui_c, id, &channel.title, &channel.cover);
            ui_c.refresh();
        });
    }
    {
        let ui_c = ui.clone();
        vol_scale.connect_value_changed(move |scale| {
            ui_c.player.set_volume(scale.value() as u32);
        });
    }
    {
        let ui_c = ui.clone();
        more_btn.connect_clicked(move |_| {
            let next = ui_c.page.get() + 1;
            ui_c.page.set(next);
            fetch_channels(&ui_c, next, true);
        });
    }
    // MPRIS 命令（顶栏媒体卡片/媒体键）在主线程消费。
    {
        let ui_c = ui.clone();
        glib::timeout_add_local(Duration::from_millis(200), move || {
            while let Ok(command) = mpris_rx.try_recv() {
                match command {
                    mpris::Command::PlayPause => toggle_play(&ui_c),
                    mpris::Command::Stop => {
                        ui_c.player.stop();
                        ui_c.playing.set(None);
                        ui_c.refresh();
                    }
                    mpris::Command::Raise => {
                        if let Some(window) = gtk::Application::default().active_window() {
                            window.present();
                        }
                    }
                    mpris::Command::Quit => {
                        if let Some(app) = gio::Application::default() {
                            app.quit();
                        }
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }
    {
        let ui_c = ui.clone();
        install_btn.connect_clicked(move |_| {
            if ui_c.installing.get() {
                return;
            }
            ui_c.installing.set(true);
            ui_c.refresh();
            // pkexec 安装为阻塞调用，放后台线程；主循环 150ms 轮询结果。
            let slot: Arc<Mutex<Option<Result<bool, String>>>> = Arc::new(Mutex::new(None));
            {
                let slot = slot.clone();
                std::thread::spawn(move || {
                    let result = radio_player::install_missing_via_pkexec()
                        .map_err(|e| e.to_string());
                    *slot.lock().unwrap() = Some(result);
                });
            }
            let ui_p = ui_c.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || {
                let Some(result) = slot.lock().unwrap().take() else {
                    return glib::ControlFlow::Continue;
                };
                ui_p.installing.set(false);
                *ui_p.dep_missing.borrow_mut() = radio_player::probe_missing();
                match result {
                    Ok(true) => ui_p.state_caption.set_text(""),
                    Ok(false) => {
                        ui_p.state_caption.set_text("依赖安装未完成（授权已取消），可重试")
                    }
                    Err(e) => ui_p.state_caption.set_text(&format!("依赖安装失败：{e}")),
                }
                ui_p.refresh();
                glib::ControlFlow::Break
            });
        });
    }
    // 用户点返回离开二级页后允许下次重新推入。
    {
        let ui_c = ui.clone();
        nav_view.connect_popped(move |_, _| ui_c.detail_pushed.set(false));
    }

    rebuild_tiles(&ui, Vec::new());
    ui.refresh();
    load_categories(&ui);
    view.upcast()
}

impl Ui {
    /// 播放指示与所有控件状态的唯一状态源。
    fn refresh(&self) {
        // 播放进程可能已自行退出（断网/缺插件/流失效），先对齐状态并给出原因。
        if self.playing.get().is_some() && !self.player.is_playing() {
            self.playing.set(None);
            let tail = self.player.error_tail();
            self.state_caption.set_text(&if tail.is_empty() {
                "播放进程已退出，可重试".to_owned()
            } else {
                format!("播放已退出：{tail}")
            });
        }

        let playing = self.playing.get();
        let np_id = playing.or(self.last_channel.get());
        if let Some(id) = playing {
            let program = self
                .channels
                .borrow()
                .iter()
                .find(|c| c.content_id == id)
                .and_then(|c| c.now_playing.clone());
            self.np_title.set_text(&self.title_of(id));
            self.state_caption.set_text(&program.unwrap_or_default());
        } else {
            self.np_title
                .set_text(&np_id.map_or_else(|| "未在播放".to_owned(), |id| self.title_of(id)));
        }
        self.toggle_btn.set_icon_name(if playing.is_some() { ICON_STOP } else { ICON_PLAY });
        self.toggle_btn.set_sensitive(np_id.is_some());
        self.bar_cover.set_spinning(playing.is_some());

        // MPRIS / 顶栏托盘状态推送（内部有变更检测）。
        let (np_title, np_program, np_art) = match np_id.and_then(|id| self.channel_of(id)) {
            Some(c) => (c.title, c.now_playing.unwrap_or_default(), c.cover),
            None => (String::new(), String::new(), String::new()),
        };
        let state = format!("{playing:?}|{np_title}|{np_program}|{np_art}");
        if self.mpris_state.borrow().as_str() != state {
            *self.mpris_state.borrow_mut() = state;
            if let Some(mpris) = self.mpris.as_ref() {
                mpris.update(playing.is_some(), &np_title, &np_program, &np_art);
            }
        }
        let volume_ok = np_id.is_some() && self.player.volume_available();
        self.vol_scale.set_visible(volume_ok);
        self.np_fav_btn.set_visible(np_id.is_some());
        if let Some(id) = np_id {
            let starred = self.favorites.borrow().iter().any(|f| f.content_id == id);
            self.np_fav_btn.set_icon_name(if starred { ICON_STAR } else { ICON_UNSTAR });
        }

        let current = self.category.get();
        for (id, btn) in self.tiles.borrow().iter() {
            if current == *id {
                btn.add_css_class("suggested-action");
            } else {
                btn.remove_css_class("suggested-action");
            }
            btn.set_sensitive(!self.busy.get());
        }
        for (id, btn) in self.play_buttons.borrow().iter() {
            let active = playing == Some(*id);
            btn.set_label(if active { "停止" } else { "播放" });
            btn.set_sensitive(!self.busy.get());
            if active {
                btn.add_css_class("suggested-action");
            } else {
                btn.remove_css_class("suggested-action");
            }
        }
        for (id, btn) in self.star_buttons.borrow().iter() {
            let starred = self.favorites.borrow().iter().any(|f| f.content_id == *id);
            btn.set_icon_name(if starred { ICON_STAR } else { ICON_UNSTAR });
        }
        self.more_btn.set_visible(self.has_more.get() && !self.busy.get());
        self.more_btn.set_sensitive(!self.busy.get());

        // 播放依赖卡片：缺失时出现，安装中禁止重复点击。
        let installing = self.installing.get();
        let missing = self.dep_missing.borrow();
        self.dep_card.set_visible(!missing.is_empty() || installing);
        if installing {
            self.dep_state
                .set_text("正在安装…（请在授权对话框输入密码）");
            self.install_btn.set_sensitive(false);
        } else if missing.is_empty() {
            self.dep_state.set_text("已就绪");
            self.install_btn.set_visible(false);
        } else {
            self.dep_state.set_text(&format!("缺失：{}", missing.join("、")));
            self.install_btn.set_visible(true);
            self.install_btn.set_sensitive(true);
        }
        drop(missing);
        self.push_now_playing();
    }

    fn title_of(&self, id: u64) -> String {
        self.channel_of(id)
            .map(|c| c.title)
            .unwrap_or_else(|| "未知电台".into())
    }

    /// 从当前频道列表与收藏中查找频道信息。
    fn channel_of(&self, id: u64) -> Option<Channel> {
        if let Some(c) = self.channels.borrow().iter().find(|c| c.content_id == id) {
            return Some(c.clone());
        }
        self.favorites
            .borrow()
            .iter()
            .find(|f| f.content_id == id)
            .map(|f| Channel {
                content_id: f.content_id,
                title: f.title.clone(),
                description: String::new(),
                now_playing: None,
                cover: f.cover.clone(),
                audience_count: String::new(),
            })
    }

    /// 推送顶栏托盘的「正在播放」合并行(节目 + 状态角标 + 播放/暂停)。
    fn push_now_playing(&self) {
        let np_id = self.playing.get().or(self.last_channel.get());
        let playing = np_id.is_some();
        let channel = np_id.and_then(|id| self.channel_of(id));
        let mut text = channel.as_ref().map(|c| c.title.clone()).unwrap_or_default();
        if let Some(program) = channel.as_ref().and_then(|c| c.now_playing.clone()) {
            if !program.is_empty() {
                text = format!("{text} · {program}");
            }
        }
        tray::set_now_playing(tray::NowPlaying { playing, text });
    }
}

fn toggle_play(ui: &Rc<Ui>) {
    if let Some(id) = ui.playing.get() {
        ui.player.stop();
        ui.playing.set(None);
        ui.last_channel.set(Some(id));
    } else if let Some(id) = ui.last_channel.get() {
        match ui.player.play(&radio_api::stream_url(id)) {
            Ok(()) => {
                ui.playing.set(Some(id));
                load_bar_cover(ui, id);
            }
            // 可能从托盘触发:窗口置前,让错误提示可见。
            Err(e) => {
                ui.state_caption.set_text(&e);
                if let Some(window) = gtk::Application::default().active_window() {
                    window.present();
                }
            }
        }
    }
    ui.refresh();
}

/// 播放条圆盘封面：按频道拉取；切台/重播时刷新，停止时保留最后一帧。
fn load_bar_cover(ui: &Rc<Ui>, id: u64) {
    let url = ui.channel_of(id).map(|c| c.cover).unwrap_or_default();
    ui.bar_cover_id.set(id);
    if url.is_empty() {
        ui.bar_cover.set_texture(None);
        return;
    }
    run_bg(
        ui,
        move || radio_api::fetch_bytes(&url),
        move |ui, result| {
            let Ok(bytes) = result else { return };
            if ui.bar_cover_id.get() != id {
                return;
            }
            if let Ok(texture) = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)) {
                ui.bar_cover.set_texture(Some(&texture));
            }
        },
    );
}

fn toggle_favorite(ui: &Rc<Ui>, id: u64, title: &str, cover: &str) {
    {
        let mut favs = ui.favorites.borrow_mut();
        if favs.iter().any(|f| f.content_id == id) {
            favs.retain(|f| f.content_id != id);
        } else {
            favs.push(Favorite {
                content_id: id,
                title: title.to_owned(),
                cover: cover.to_owned(),
            });
        }
    }
    if let Err(e) = radio_api::save_favorites(&ui.favorites.borrow()) {
        ui.state_caption.set_text(&format!("收藏保存失败：{e}"));
    }
    if ui.category.get() == FAVORITES_ID {
        rebuild_channel_rows(ui);
    }
}

/// 推入/复用二级频道页。
fn open_detail(ui: &Rc<Ui>, title: &str) {
    ui.detail_title.set_title(title);
    ui.detail_page.set_title(title);
    if !ui.detail_pushed.get() {
        ui.nav_view.push(&ui.detail_page);
        ui.detail_pushed.set(true);
    }
}

/// 瓷贴固定为「我的收藏 + 全部分类」；分类拉取成功后重建一次。
fn rebuild_tiles(ui: &Rc<Ui>, cats: Vec<Category>) {
    while let Some(child) = ui.category_flow.first_child() {
        ui.category_flow.remove(&child);
    }
    let mut tiles = Vec::new();
    let mut entries = vec![(FAVORITES_ID, "我的收藏".to_owned())];
    entries.extend(cats.into_iter().map(|c| (c.id, c.title)));
    *ui.category_titles.borrow_mut() = entries.clone();
    for (id, title) in &entries {
        let btn = Button::with_label(title);
        btn.set_height_request(44);
        let ui_c = ui.clone();
        let id = *id;
        btn.connect_clicked(move |_| set_category(&ui_c, id));
        ui.category_flow.append(&btn);
        tiles.push((id, btn));
    }
    *ui.tiles.borrow_mut() = tiles;
    ui.refresh();
}

fn set_category(ui: &Rc<Ui>, id: u64) {
    if ui.busy.get() {
        return;
    }
    ui.category.set(id);
    ui.page.set(1);
    ui.channels.borrow_mut().clear();
    let title = ui
        .category_titles
        .borrow()
        .iter()
        .find(|(cid, _)| *cid == id)
        .map(|(_, title)| title.clone())
        .unwrap_or_else(|| "频道".into());
    open_detail(ui, &title);
    rebuild_channel_rows(ui);
    ui.refresh();
    load_current_view(ui);
}

fn load_categories(ui: &Rc<Ui>) {
    if ui.busy.get() {
        return;
    }
    ui.busy.set(true);
    ui.refresh();
    run_bg(
        ui,
        || {
            let text = radio_api::fetch_text(&radio_api::categories_url())?;
            radio_api::parse_categories(&text)
        },
        |ui, result| {
            ui.busy.set(false);
            match result {
                Ok(cats) => {
                    let mut ids = vec![FAVORITES_ID];
                    ids.extend(cats.iter().map(|c| c.id));
                    *ui.category_ids.borrow_mut() = ids;
                    rebuild_tiles(ui, cats);
                }
                Err(e) => {
                    ui.state_caption
                        .set_text(&format!("分类获取失败（收藏与搜索仍可用）：{e}"));
                }
            }
            ui.refresh();
        },
    );
}

/// 点瓷贴时的入口：收藏伪分类直接渲染，其余拉第一页。
fn load_current_view(ui: &Rc<Ui>) {
    if ui.category.get() == FAVORITES_ID {
        ui.has_more.set(false);
        rebuild_channel_rows(ui);
        ui.refresh();
    } else {
        fetch_channels(ui, 1, false);
    }
}

fn run_search(ui: &Rc<Ui>, keyword: &str) {
    if ui.busy.get() || keyword.is_empty() {
        return;
    }
    ui.category.set(SEARCH_ID);
    ui.page.set(1);
    ui.busy.set(true);
    ui.state_caption.set_text("搜索中…");
    ui.channels.borrow_mut().clear();
    rebuild_channel_rows(ui);
    open_detail(ui, &format!("搜索：{keyword}"));
    ui.refresh();
    let kw = keyword.to_owned();
    run_bg(
        ui,
        move || {
            let text = radio_api::fetch_text(&radio_api::search_url(&kw))?;
            radio_api::parse_search(&text)
        },
        |ui, result| {
            ui.busy.set(false);
            match result {
                Ok(list) => {
                    ui.has_more.set(false);
                    let count = list.len();
                    *ui.channels.borrow_mut() = list;
                    ui.state_caption.set_text(&if count == 0 {
                        "没有匹配的直播电台".to_owned()
                    } else {
                        String::new()
                    });
                    rebuild_channel_rows(ui);
                }
                Err(e) => {
                    ui.state_caption.set_text(&format!("搜索失败：{e}"));
                }
            }
            ui.refresh();
        },
    );
}

fn fetch_channels(ui: &Rc<Ui>, page: u32, append: bool) {
    if ui.busy.get() {
        return;
    }
    let category = ui.category.get();
    if category == FAVORITES_ID || category == SEARCH_ID {
        return;
    }
    ui.busy.set(true);
    ui.state_caption.set_text("加载中…");
    ui.refresh();
    run_bg(
        ui,
        move || {
            let text = radio_api::fetch_text(&radio_api::channels_url(category, page))?;
            radio_api::parse_channels(&text)
        },
        move |ui, result| {
            ui.busy.set(false);
            match result {
                Ok(page_data) => {
                    ui.has_more.set(page_data.has_more);
                    {
                        let mut list = ui.channels.borrow_mut();
                        if append {
                            list.extend(page_data.channels);
                        } else {
                            *list = page_data.channels;
                        }
                    }
                    ui.state_caption.set_text("");
                    rebuild_channel_rows(ui);
                }
                Err(e) => {
                    ui.state_caption.set_text(&format!("频道获取失败：{e}"));
                }
            }
            ui.refresh();
        },
    );
}

fn rebuild_channel_rows(ui: &Rc<Ui>) {
    while let Some(child) = ui.channel_box.first_child() {
        ui.channel_box.remove(&child);
    }
    // more_btn 是 channel_box 的常驻末尾成员，移除后需放回。
    ui.channel_box.append(&ui.more_btn);
    ui.play_buttons.borrow_mut().clear();
    ui.star_buttons.borrow_mut().clear();
    ui.cover_images.borrow_mut().clear();

    if ui.category.get() == FAVORITES_ID {
        let rows: Vec<Channel> = ui
            .favorites
            .borrow()
            .iter()
            .map(|f| Channel {
                content_id: f.content_id,
                title: f.title.clone(),
                description: String::new(),
                now_playing: None,
                cover: f.cover.clone(),
                audience_count: String::new(),
            })
            .collect();
        if rows.is_empty() {
            ui.channel_box.append(&dim_label(
                "暂无收藏：在分类或搜索结果中点击电台行的星标即可加入。",
            ));
            return;
        }
        for channel in &rows {
            append_channel_row(ui, channel);
        }
        return;
    }

    let channels = ui.channels.borrow();
    if channels.is_empty() {
        ui.channel_box.append(&dim_label(if ui.busy.get() {
            "加载中…"
        } else if ui.category.get() == SEARCH_ID {
            "输入关键字后回车搜索直播电台"
        } else {
            "该分类暂无频道"
        }));
        return;
    }
    for channel in channels.iter() {
        append_channel_row(ui, channel);
    }
}

fn append_channel_row(ui: &Rc<Ui>, channel: &Channel) {
    let id = channel.content_id;
    let is_fav = ui
        .favorites
        .borrow()
        .iter()
        .any(|f| f.content_id == id);

    let cover = Image::new();
    cover.set_pixel_size(48);
    cover.set_icon_name(Some(ICON_DEFAULT));
    cover.set_valign(Align::Center);

    let col = GtkBox::new(gtk::Orientation::Vertical, 2);
    col.set_hexpand(true);
    col.set_valign(Align::Center);
    let title = Label::new(Some(&channel.title));
    title.add_css_class("row-title");
    title.set_halign(Align::Start);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    col.append(&title);
    let subtitle = match (&channel.now_playing, channel.description.is_empty()) {
        (Some(program), _) => program.clone(),
        (None, false) => {
            channel.description.lines().next().unwrap_or_default().to_owned()
        }
        (None, true) => String::new(),
    };
    if !subtitle.is_empty() {
        let sub = Label::new(Some(&subtitle));
        sub.add_css_class("dim-label");
        sub.add_css_class("caption-sm");
        sub.set_halign(Align::Start);
        sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
        col.append(&sub);
    }
    if !channel.audience_count.is_empty() {
        let cnt = Label::new(Some(&format!("收听 {}", channel.audience_count)));
        cnt.add_css_class("dim-label");
        cnt.add_css_class("caption-sm");
        cnt.set_halign(Align::Start);
        col.append(&cnt);
    }

    let fav_btn = Button::from_icon_name(if is_fav { ICON_STAR } else { ICON_UNSTAR });
    fav_btn.set_tooltip_text(Some(if is_fav { "取消收藏" } else { "收藏" }));
    fav_btn.set_valign(Align::Center);
    let play_btn =
        Button::with_label(if ui.playing.get() == Some(id) { "停止" } else { "播放" });
    play_btn.set_valign(Align::Center);

    let row = GtkBox::new(gtk::Orientation::Horizontal, 8);
    row.append(&cover);
    row.append(&col);
    row.append(&fav_btn);
    row.append(&play_btn);
    ui.channel_box.append(&row);
    ui.play_buttons.borrow_mut().push((id, play_btn.clone()));
    ui.star_buttons.borrow_mut().push((id, fav_btn.clone()));
    ui.cover_images.borrow_mut().push((id, cover.clone()));

    let ui_c: Weak<Ui> = Rc::downgrade(ui);
    play_btn.connect_clicked(move |_| {
        let Some(ui) = ui_c.upgrade() else { return };
        if ui.playing.get() == Some(id) {
            ui.player.stop();
            ui.playing.set(None);
        } else {
            match ui.player.play(&radio_api::stream_url(id)) {
                Ok(()) => {
                    ui.playing.set(Some(id));
                    ui.last_channel.set(Some(id));
                    ui.vol_scale.set_value(100.0);
                    load_bar_cover(&ui, id);
                }
                Err(e) => ui.state_caption.set_text(&e),
            }
        }
        ui.refresh();
    });
    let ui_c: Weak<Ui> = Rc::downgrade(ui);
    let title_c = channel.title.clone();
    let cover_c = channel.cover.clone();
    fav_btn.connect_clicked(move |_| {
        let Some(ui) = ui_c.upgrade() else { return };
        toggle_favorite(&ui, id, &title_c, &cover_c);
        ui.refresh();
    });

    if !channel.cover.is_empty() {
        fetch_cover(ui, id, channel.cover.clone());
    }
}

fn fetch_cover(ui: &Rc<Ui>, id: u64, url: String) {
    run_bg(
        ui,
        move || radio_api::fetch_bytes(&url),
        move |ui, result| {
            let Ok(bytes) = result else { return };
            // 频道列表行封面
            let row_image = ui
                .cover_images
                .borrow()
                .iter()
                .find(|(cid, _)| *cid == id)
                .map(|(_, img)| img.clone());
            let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes));
            if let (Some(image), Ok(texture)) = (&row_image, &texture) {
                image.set_paintable(Some(texture));
            }
            // 播放条圆盘(仅当前播放频道)
            if ui.bar_cover_id.get() == id {
                if let Ok(texture) = &texture {
                    ui.bar_cover.set_texture(Some(texture));
                }
            }
        },
    );
}

/// 后台线程执行阻塞请求，主循环 150ms 轮询结果；20s 无响应按超时处理。
fn run_bg<T, F, G>(ui: &Rc<Ui>, work: F, on_done: G)
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
    G: Fn(&Rc<Ui>, Result<T, String>) + 'static,
{
    let slot: Arc<Mutex<Option<Result<T, String>>>> = Arc::new(Mutex::new(None));
    {
        let slot = slot.clone();
        std::thread::spawn(move || {
            *slot.lock().unwrap() = Some(work());
        });
    }
    let ui = ui.clone();
    let start = Instant::now();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        if let Some(result) = slot.lock().unwrap().take() {
            on_done(&ui, result);
            return glib::ControlFlow::Break;
        }
        if start.elapsed() > Duration::from_secs(20) {
            ui.busy.set(false);
            ui.state_caption.set_text("网络请求超时，请检查网络后重试");
            ui.refresh();
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

fn dim_label(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.add_css_class("dim-label");
    l.add_css_class("caption-sm");
    l.set_wrap(true);
    l.set_halign(Align::Start);
    l
}
