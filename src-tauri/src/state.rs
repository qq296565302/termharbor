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

/// 产品文件夹名。快照、MCP 配置、审计日志都放 `%APPDATA%\TermHarbor\`，
/// 都在卸载器 `RmDir /r "$APPDATA\${BUNDLEID}"` 的删除路径之外。
pub const PRODUCT_FOLDER: &str = "TermHarbor";

/// 一次启动内固定不变的路径。
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub db_file: PathBuf,
    /// 产品根目录（Windows 上就是 `%APPDATA%`）。
    ///
    /// **注意这里不是 `TermHarbor` 那一层** —— 它是 `%APPDATA%` 本身，
    /// 往上拼 [`PRODUCT_FOLDER`] 才落到我们的文件夹。0.1.28 及更早直接把
    /// `mcp.json` / `audit.jsonl` 写在这个根目录上，散在了 Roaming 里；
    /// 现在统一走 `mcp::data_dir`。
    pub product_dir: PathBuf,
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

        Ok(Self::under(data_dir, log_dir))
    }

    /// 不依赖 Tauri 的路径解析。MCP 那个无头二进制用这一条。
    ///
    /// **必须和 `resolve()` 算出同一组路径**，否则 MCP 会去读一个不存在的库，
    /// 症状是"连接列表永远是空的"，而且看不出哪里错了。所以真正拼路径的逻辑
    /// 只有 `under()` 这一份，两个入口都调它。
    pub fn resolve_headless() -> AppResult<Self> {
        let (data_root, log_root) = platform_roots()?;
        Ok(Self::under(data_root, log_root))
    }

    /// 唯一一份拼装逻辑：给定两个根目录，算出四个路径。
    ///
    /// `pub(crate)` 是为了让测试能塞一个临时目录进来（`mcp::Context::for_tests`），
    /// 不必真的去碰用户的数据目录。
    pub(crate) fn under(data_dir: PathBuf, log_dir: PathBuf) -> Self {
        // 注意 `backup::dir` 收的是**产品根目录**（Windows 上是 `%APPDATA%`），
        // 不是 `app_data_dir`。少往上走一层就会落到
        // `%APPDATA%\com.termharbor.app\TermHarbor\backups` —— 那正好在卸载器
        // `RmDir /r "$APPDATA\${BUNDLEID}"` 的删除路径里，安全网等于没有。
        let product_dir = data_dir
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.clone());
        let backup_dir = crate::storage::backup::dir(&product_dir);

        Self {
            db_file: data_dir.join("termharbor.db"),
            data_dir,
            log_dir,
            product_dir,
            backup_dir,
        }
    }
}

/// 各平台的应用数据/日志根目录（含 Bundle identifier 那一层）。
///
/// Tauri 的 `app_data_dir()` / `app_log_dir()` 内部用的是同一套规则，见 Tauri v2 的
/// `path` 实现：Windows 走 `%APPDATA%` 与 `%LOCALAPPDATA%`，macOS 走
/// `Application Support` 与 `~/Library/Logs`，Linux 走 `XDG_DATA_HOME`。
#[cfg(windows)]
fn platform_roots() -> AppResult<(PathBuf, PathBuf)> {
    const ID: &str = "com.termharbor.app";

    let roaming = std::env::var_os("APPDATA")
        .ok_or_else(|| AppError::internal("环境变量 APPDATA 缺失，无法定位数据目录"))?;
    // 日志目录按 Tauri 的规则落在 LOCALAPPDATA 下；取不到就退回 roaming，
    // 大不了日志换个地方，不影响功能。
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&roaming));

    Ok((PathBuf::from(roaming).join(ID), local.join(ID).join("logs")))
}

#[cfg(target_os = "macos")]
fn platform_roots() -> AppResult<(PathBuf, PathBuf)> {
    const ID: &str = "com.termharbor.app";

    let home = std::env::var_os("HOME")
        .ok_or_else(|| AppError::internal("环境变量 HOME 缺失，无法定位数据目录"))?;
    let home = PathBuf::from(home);

    Ok((
        home.join("Library/Application Support").join(ID),
        home.join("Library/Logs").join(ID),
    ))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_roots() -> AppResult<(PathBuf, PathBuf)> {
    const ID: &str = "com.termharbor.app";

    let data_root = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or_else(|| AppError::internal("XDG_DATA_HOME 与 HOME 都缺失，无法定位数据目录"))?;

    Ok((data_root.join(ID), data_root.join(ID).join("logs")))
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

    /// `under()` 是两个入口共用的唯一拼装逻辑，路径关系钉死在这里。
    #[test]
    fn under_composes_the_same_four_paths() {
        let paths = AppPaths::under(PathBuf::from("/data/app"), PathBuf::from("/data/logs"));

        assert_eq!(paths.db_file, PathBuf::from("/data/app/termharbor.db"));
        assert_eq!(paths.data_dir, PathBuf::from("/data/app"));
        assert_eq!(paths.log_dir, PathBuf::from("/data/logs"));
        // 快照目录必须落在 data_dir 之外（卸载器会删 data_dir）
        assert!(
            paths.backup_dir.starts_with("/data") && !paths.backup_dir.starts_with("/data/app"),
            "快照目录不能在被卸载器删除的路径里：{:?}",
            paths.backup_dir
        );
    }

    /// 无头解析出的路径要落在真实存在的目录结构上（Windows CI 上 APPDATA 一定在）。
    #[test]
    fn headless_paths_point_at_the_bundle_identifier() {
        let paths = AppPaths::resolve_headless().expect("有 APPDATA/HOME 就该能算出来");
        assert!(
            paths
                .data_dir
                .to_string_lossy()
                .contains("com.termharbor.app"),
            "数据目录必须带 Bundle identifier：{:?}",
            paths.data_dir
        );
        assert!(paths.db_file.ends_with("termharbor.db"));
    }
}
