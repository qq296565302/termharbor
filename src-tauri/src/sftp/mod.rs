//! SFTP 浏览、上传下载与文件操作（开发计划 M3）。
//!
//! 三条贯穿全模块的约定：
//!
//! 1. **独立建连**：SFTP 不复用终端会话的 TCP 连接。SSH 通道共享窗口，
//!    大文件传输会把终端的输出窗口饿死，表现就是「传文件时终端卡住」。
//!    多一条连接换来的稳定性值得。
//! 2. **流式分块**：禁止整文件读入内存（约束 A-04）。块大小 128KB。
//! 3. **先写 `.part` 再改名**：中途失败不会在目标位置留下半截文件。
//!
//! 与 `ssh` 模块一样，这里**不依赖 Tauri 类型**：进度用回调注入，
//! 否则 `cargo test --lib` 会把窗口层链进测试二进制（M2 踩过的坑，见 `ssh/session.rs`）。

pub mod list;
pub mod ops;
pub mod pack;
pub mod session;
pub mod transfer;

#[cfg(test)]
pub(crate) mod test_support;

pub use list::RemoteEntry;
pub use session::{OpenedSftp, SftpConn, SftpManager};
pub use transfer::{TransferDirection, TransferProgress, Transfers};

use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::StatusCode;

use crate::error::{AppError, ErrCode};

/// 把 SFTP 底层错误翻成稳定错误码。
///
/// 只有这三个需要区分：路径不存在（前端提示「已删除」）、
/// 没权限（提示 sudo）、其他一律当 IO 失败。原始文本进 `detail`，构造时已脱敏。
pub(crate) fn map_error(err: SftpError) -> AppError {
    let code = match &err {
        SftpError::Status(status) => match status.status_code {
            StatusCode::NoSuchFile => ErrCode::SftpNotFound,
            StatusCode::PermissionDenied => ErrCode::SftpPermissionDenied,
            _ => ErrCode::SftpIoError,
        },
        _ => ErrCode::SftpIoError,
    };

    AppError::with_detail(code, err.to_string())
}
