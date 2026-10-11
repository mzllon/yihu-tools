//! jsonkit（Rust 化迁移第二批占位）：SDK 协议壳——init/shutdown 链路验证。
//! 完整功能迁移见后续批次。

use yihu_plugin_sdk::prelude::*;

struct Stub;
impl Handler for Stub {
    fn on_query(&self, _qid: u64, _text: &str, _files: &[String]) {}
}

fn main() {
    run(Stub);
}
