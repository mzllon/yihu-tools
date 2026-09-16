//! MPRIS 媒体接口：注册 `org.mpris.MediaPlayer2.yihu`。
//!
//! GNOME 顶栏系统菜单、锁屏与键盘媒体键据此展示并控制一呼的
//! 电台播放（暂停/播放/停止）。命令经通道由页面在主线程消费，
//! 状态变化由页面调用 [`RadioMpris::update`] 推送 PropertiesChanged。

use gtk::glib;
use mpris_server::{
    async_trait, zbus, LocalPlayerInterface, LocalRootInterface, LocalServer, Metadata,
    PlaybackStatus, Property, Time, TrackId,
};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    PlayPause,
    Stop,
    Raise,
    Quit,
}

#[derive(Debug, Default)]
struct Shared {
    playing: bool,
    title: String,
    program: String,
    art: String,
}

pub struct Player {
    shared: Arc<Mutex<Shared>>,
    commands: mpsc::Sender<Command>,
}

fn send(commands: &mpsc::Sender<Command>, command: Command) {
    let _ = commands.send(command);
}

#[async_trait(?Send)]
impl LocalRootInterface for Player {
    async fn raise(&self) -> zbus::fdo::Result<()> {
        send(&self.commands, Command::Raise);
        Ok(())
    }
    async fn can_raise(&self) -> zbus::fdo::Result<bool> {
        Ok(true)
    }
    async fn quit(&self) -> zbus::fdo::Result<()> {
        send(&self.commands, Command::Quit);
        Ok(())
    }
    async fn can_quit(&self) -> zbus::fdo::Result<bool> {
        Ok(true)
    }
    async fn fullscreen(&self) -> zbus::fdo::Result<bool> {
        Ok(false)
    }
    async fn can_set_fullscreen(&self) -> zbus::fdo::Result<bool> {
        Ok(false)
    }
    async fn set_fullscreen(&self, _fullscreen: bool) -> zbus::Result<()> {
        Ok(())
    }
    async fn desktop_entry(&self) -> zbus::fdo::Result<String> {
        Ok(crate::APP_ID.to_owned())
    }
    async fn has_track_list(&self) -> zbus::fdo::Result<bool> {
        Ok(false)
    }
    async fn identity(&self) -> zbus::fdo::Result<String> {
        Ok("一呼".to_owned())
    }
    async fn supported_uri_schemes(&self) -> zbus::fdo::Result<Vec<String>> {
        Ok(vec![])
    }
    async fn supported_mime_types(&self) -> zbus::fdo::Result<Vec<String>> {
        Ok(vec![])
    }
}

impl Player {
    fn metadata_of(&self) -> Metadata {
        let shared = self.shared.lock().unwrap();
        player_metadata(&shared)
    }
}

fn track_id() -> TrackId {
    TrackId::try_from("/dev/yihu/radio/track/1").expect("固定轨道路径必合法")
}

fn player_metadata(shared: &Shared) -> Metadata {
    let mut builder = Metadata::builder()
        .trackid(track_id())
        .title(if shared.title.is_empty() {
            "未在播放".to_owned()
        } else {
            shared.title.clone()
        })
        .artist(vec![if shared.program.is_empty() {
            "蜻蜓FM 直播".to_owned()
        } else {
            shared.program.clone()
        }]);
    if !shared.art.is_empty() {
        builder = builder.art_url(shared.art.clone());
    }
    builder.build()
}

#[async_trait(?Send)]
impl LocalPlayerInterface for Player {
    async fn play_pause(&self) -> zbus::fdo::Result<()> {
        send(&self.commands, Command::PlayPause);
        Ok(())
    }
    async fn play(&self) -> zbus::fdo::Result<()> {
        send(&self.commands, Command::PlayPause);
        Ok(())
    }
    async fn pause(&self) -> zbus::fdo::Result<()> {
        send(&self.commands, Command::PlayPause);
        Ok(())
    }
    async fn stop(&self) -> zbus::fdo::Result<()> {
        send(&self.commands, Command::Stop);
        Ok(())
    }
    async fn next(&self) -> zbus::fdo::Result<()> {
        Ok(())
    }
    async fn previous(&self) -> zbus::fdo::Result<()> {
        Ok(())
    }
    async fn seek(&self, _offset: Time) -> zbus::fdo::Result<()> {
        Ok(())
    }
    async fn set_position(&self, _track_id: TrackId, _position: Time) -> zbus::fdo::Result<()> {
        Ok(())
    }
    async fn open_uri(&self, _uri: String) -> zbus::fdo::Result<()> {
        Ok(())
    }
    async fn can_play(&self) -> zbus::fdo::Result<bool> {
        Ok(true)
    }
    async fn can_pause(&self) -> zbus::fdo::Result<bool> {
        Ok(true)
    }
    async fn can_seek(&self) -> zbus::fdo::Result<bool> {
        Ok(false)
    }
    async fn can_control(&self) -> zbus::fdo::Result<bool> {
        Ok(true)
    }
    async fn can_go_next(&self) -> zbus::fdo::Result<bool> {
        Ok(false)
    }
    async fn can_go_previous(&self) -> zbus::fdo::Result<bool> {
        Ok(false)
    }
    async fn playback_status(&self) -> zbus::fdo::Result<PlaybackStatus> {
        Ok(if self.shared.lock().unwrap().playing {
            PlaybackStatus::Playing
        } else {
            PlaybackStatus::Stopped
        })
    }
    async fn loop_status(&self) -> zbus::fdo::Result<mpris_server::LoopStatus> {
        Ok(mpris_server::LoopStatus::None)
    }
    async fn rate(&self) -> zbus::fdo::Result<f64> {
        Ok(1.0)
    }
    async fn minimum_rate(&self) -> zbus::fdo::Result<f64> {
        Ok(1.0)
    }
    async fn maximum_rate(&self) -> zbus::fdo::Result<f64> {
        Ok(1.0)
    }
    async fn shuffle(&self) -> zbus::fdo::Result<bool> {
        Ok(false)
    }
    async fn volume(&self) -> zbus::fdo::Result<f64> {
        Ok(1.0)
    }
    async fn position(&self) -> zbus::fdo::Result<Time> {
        Ok(Time::from_millis(0))
    }
    async fn metadata(&self) -> zbus::fdo::Result<Metadata> {
        Ok(self.metadata_of())
    }
    async fn set_loop_status(&self, _loop_status: mpris_server::LoopStatus) -> zbus::Result<()> {
        Ok(())
    }
    async fn set_rate(&self, _rate: f64) -> zbus::Result<()> {
        Ok(())
    }
    async fn set_shuffle(&self, _shuffle: bool) -> zbus::Result<()> {
        Ok(())
    }
    async fn set_volume(&self, _volume: f64) -> zbus::Result<()> {
        Ok(())
    }
}

/// 页面持有的句柄：状态变化时调用 [`RadioMpris::update`]。
#[derive(Clone)]
pub struct RadioMpris {
    shared: Arc<Mutex<Shared>>,
    server: Arc<LocalServer<Player>>,
}

pub fn start() -> (Option<RadioMpris>, mpsc::Sender<Command>, mpsc::Receiver<Command>) {
    let (commands, receiver) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let player = Player { shared: shared.clone(), commands: commands.clone() };
    let server = match LocalServer::new("yihu", player) {
        Ok(server) => Arc::new(server),
        Err(e) => {
            eprintln!("[广播] MPRIS 注册失败（顶栏媒体控制不可用）：{e}");
            return (None, commands, receiver);
        }
    };
    // 连接会话总线并注册 org.mpris.MediaPlayer2.yihu；持续服务直至进程退出。
    glib::spawn_future_local({
        let server_for_run = server.clone();
        async move {
            if let Err(e) = server_for_run.init_and_run().await {
                eprintln!("[广播] MPRIS 服务异常退出：{e}");
            }
        }
    });
    (Some(RadioMpris { shared, server }), commands, receiver)
}

impl RadioMpris {
    /// 播放状态/曲目变化时推送；内部有变更检测，可频繁调用。
    pub fn update(&self, playing: bool, title: &str, program: &str, art: &str) {
        {
            let mut shared = self.shared.lock().unwrap();
            if shared.playing == playing
                && shared.title == title
                && shared.program == program
                && shared.art == art
            {
                return;
            }
            shared.playing = playing;
            shared.title = title.to_owned();
            shared.program = program.to_owned();
            shared.art = art.to_owned();
        }
        let server = self.server.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = server.properties_changed(Property::PlaybackStatus).await {
                eprintln!("[广播] MPRIS 播放状态推送失败：{e}");
            }
            if let Err(e) = server.properties_changed(Property::Metadata).await {
                eprintln!("[广播] MPRIS 元数据推送失败：{e}");
            }
        });
    }
}
