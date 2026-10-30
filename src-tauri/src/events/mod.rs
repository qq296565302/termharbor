//! 事件与 Channel 载荷。
//!
//! 命名约定 `域:动作`，前端 `src/lib/ipc.ts` 里有一份对应常量。
//! 高频数据（终端输出）走 Channel，低频状态变更走 Event —— 见需求 4.3。

use serde::Serialize;

/// 连接状态变化：断开、重连、认证失败
pub const EVT_SSH_STATE: &str = "ssh:state";
/// 传输进度，Rust 侧 150ms 节流
pub const EVT_SFTP_PROGRESS: &str = "sftp:progress";

/// 终端输出批次。
///
/// 关键约束：`data` 必须是 `String` 而不是 `Vec<u8>` ——
/// Tauri Channel 走 JSON 序列化，字节数组会膨胀成数字数组，体积翻数倍（开发计划 §9-6）。
#[derive(Debug, Clone, Serialize)]
pub struct OutputBatch {
    pub seq: u64,
    pub data: String,
    /// `data` 的 UTF-8 字节数。前端 ack 必须按这个值回传 ——
    /// JS 的 `string.length` 是 UTF-16 码元数，中文会少算，长期累积会把背压锁死。
    pub bytes: u32,
}
