//! Tauri command 入口。
//!
//! 纪律（需求 4.4）：这一层**只做参数校验与转发**，业务逻辑写在对应领域模块里。
//! 所以随着 M1–M3 推进，这里会越来越薄，真正的代码在 `storage/` `ssh/` `sftp/`。

pub mod advanced;
pub mod backup;
pub mod config_io;
pub mod connections;
pub mod dev;
pub mod diag;
pub mod history;
pub mod mcp;
pub mod pack;
pub mod settings;
pub mod sftp;
pub mod snippets;
pub mod system;
pub mod terminal;
pub mod update;
