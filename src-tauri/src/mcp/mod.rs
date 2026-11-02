//! MCP（Model Context Protocol）服务端。
//!
//! 形态与取舍见 `docs/开发计划.md` §17：**独立的无头二进制 + stdio + JSON-RPC 2.0**，
//! 复用 `storage` / `crypto` / `ssh` / `sftp`（这几层本来就不依赖 Tauri）。
//!
//! 为什么手写协议而不引 `rmcp`：M8-1 只需要 `initialize` / `tools/list` / `tools/call`
//! 三个方法，手写约三百行且每一步都能单测；官方 SDK 的 API 还在快速变化，一旦版本跳变，
//! 排障成本比省下的代码量大得多。
//!
//! **stdout 是协议通道**：任何日志、调试输出都必须走 stderr，否则宿主收到的就是一堆
//! 无法解析的行，症状是"连上了但工具全都不见了"。

pub mod jsonrpc;
pub mod server;
pub mod tools;

pub use server::Server;
