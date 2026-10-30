//! 应用全局状态。由 Tauri `manage` 注入，所有 command 通过 `State<AppState>` 访问。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::crypto::{FieldCipher, KeySource};
use crate::error::{AppError, AppResult};
use crate::logging::LoggingGuard;
use crate::sftp::{SftpManager, Transfers};
use crate::ssh::session::SessionManager;
use crate::storage::Db;

/// 一次启动内固定不变的路径。
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub db_file: PathBuf,
    /// 快照目录。**故意不在 data_dir 里面** —— 卸载器会把整个
    /// `%APPDATA%\com.termharbor.app` 删掉（含这里的 data_dir），
    /// 快照必须放在那条删除路径之外才有意义，见 `storage::backup`。
    pub backup_dir: PathBuf,
}

impl AppPaths {
    pub fn resolve(app: &tauri::AppHandle) -> AppResult<Self> {
        use tauri::Manager;

        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|e| AppError::internal(format!("无法定位应用数据目录: {e}")))?;
        let log_dir = app
            .path()
            .app_log_dir()
            .map_err(|e| AppError::internal(format!("无法定位日志目录: {e}")))?;

        let backup_dir = crate::storage::backup::dir(&data_dir);

        Ok(Self {
            db_file: data_dir.join("termharbor.db"),
            data_dir,
            log_dir,
            backup_dir,
        })
    }
}

pub struct AppState {
    pub db: Arc<Db>,
    /// `None` 表示密钥不可用（例如用户拒绝了钥匙串授权），此时应用处于未解锁状态。
    pub cipher: Option<Arc<FieldCipher>>,
    pub paths: AppPaths,
    pub key_source: KeySource,
    /// M0 只用于自检心跳，M2 会被真正的会话表取代。
    pub heartbeat: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// 单调递增的会话序号，用来生成 SessionId。
    pub next_seq: AtomicU64,
    /// 所有活着的终端会话。
    pub sessions: Arc<SessionManager>,
    /// 所有活着的 SFTP 连接（与终端连接相互独立，见 sftp/mod.rs 的说明）。
    pub sftp: Arc<SftpManager>,
    /// 在途传输的取消令牌。
    pub transfers: Arc<Transfers>,
    pub _logging: LoggingGuard,
}

impl AppState {
    pub fn is_locked(&self) -> bool {
        self.cipher.is_none()
    }

    /// 需要加密能力的地方统一走这里，拿到锁就把 `CRYPTO_KEY_UNAVAILABLE` 抛给前端。
    pub fn require_cipher(&self) -> AppResult<&Arc<FieldCipher>> {
        self.cipher
            .as_ref()
            .ok_or_else(|| AppError::new(crate::error::ErrCode::CryptoKeyUnavailable))
    }

    pub fn next_session_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::Relaxed) + 1
    }
}

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_millis_is_plausible() {
        // 2024-01-01 之后
        assert!(now_millis() > 1_704_067_200_000);
    }
}
