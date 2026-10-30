//! 本地存储：SQLite 连接与 schema 迁移。
//!
//! 为什么用 `std::sync::Mutex<Connection>` 而不是连接池：
//! 本应用的 DB 负载只有配置 CRUD（几十行、低频），真瓶颈在 SSH/SFTP 而不在 SQLite。
//! 单个 WAL 连接足够，且能省掉 `r2d2-sqlite` 与 `rusqlite` 的版本锁步问题。
//! 所有访问都在 `spawn_blocking` 里进行，不会阻塞 Tokio 工作线程。

pub mod connections;
pub mod db;
pub mod known_hosts;
pub mod migrations;
pub mod settings;

pub use db::Db;
