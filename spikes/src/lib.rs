//! 三个技术 Spike 的共享脚手架。
//!
//! 只在 `spikes/` 里用，不进产品构建。跑法见 `docs/spike-notes.md`。

pub mod algorithms;
pub mod batch;
pub mod rss;
pub mod serve;
pub mod utf8_stream;

use std::net::TcpListener;

/// 找一个空闲端口（绑定 0 拿到系统分配的端口后立刻释放）。
pub fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("无法绑定本地端口");
    listener.local_addr().expect("无法读取本地地址").port()
}

pub fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}
