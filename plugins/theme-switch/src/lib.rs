//! 深浅色切换（M7/W4 验收插件）：首个「带能力」的官方 WASM 插件。
//!
//! 语义吸收自 sys-theme 演示插件（Python）：搜索 dark/light/深色/浅色
//! 给出切换行；回车经 settings.write@org.gnome.desktop.interface 声明制
//! 授权写 gsettings，成功后 notify 反馈——全链路（query→activate→能力
//! 请求→宿主执行→on_response→二次能力）即 W4 能力桥的验收路径。
//!
//! 防循环要点：on_response 只认「最近一次 settings.write 的请求 id」，
//! notify 自己的回包不再触发新请求。

use std::sync::atomic::{AtomicI32, Ordering};

use yihu_wasm_runtime::cap;
use yihu_wasm_runtime::{export_plugin, PluginHandler, Row};

const ROWS: &[(&str, &str, &str)] = &[
    ("切换到深色模式", "settings.write 经宿主执行", "prefer-dark"),
    ("切换到浅色模式", "settings.write 经宿主执行", "default"),
];

static PENDING_WRITE: AtomicI32 = AtomicI32::new(-1);

pub struct ThemeSwitch;

impl PluginHandler for ThemeSwitch {
    fn handle(text: &str, _now_secs: i64) -> Vec<Row> {
        // 与 sys-theme 同口径：首词命中才出行（广播每键触发，不能全量回）
        let first = text.trim().split_whitespace().next().unwrap_or("").to_lowercase();
        if first.is_empty() {
            return vec![];
        }
        let hay = "切换到深色模式 切换到浅色模式 dark light 深色 浅色 zhuti theme sese";
        if !hay.split(' ').any(|k| first.contains(k)) {
            return vec![];
        }
        ROWS
            .iter()
            .map(|(title, sub, payload)| Row::new(*title, *sub, *payload))
            .collect()
    }

    fn activate(payload: &str) {
        let value = match payload {
            "prefer-dark" | "default" => payload,
            _ => return,
        };
        let id = cap::settings_write("org.gnome.desktop.interface", "color-scheme", value);
        PENDING_WRITE.store(id, Ordering::SeqCst);
    }

    fn on_response(id: i32, ok: bool, _data: &[u8]) {
        // 只认 settings.write 那次请求；swap 保证 notify 回包不再级联
        let pending = PENDING_WRITE.swap(-1, Ordering::SeqCst);
        if id != pending {
            return;
        }
        if ok {
            cap::notify("深浅色切换", "主题已切换");
        } else {
            cap::notify("深浅色切换", "切换失败（见审计日志）");
        }
    }
}

export_plugin!(ThemeSwitch);
