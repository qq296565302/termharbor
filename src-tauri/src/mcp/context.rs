//! MCP 侧的运行环境：路径、配置库、以及**用到才拿的密钥**。
//!
//! 两次懒加载，各有各的理由：
//!
//! 1. **库**：`tools/list` 完全不需要数据库，没必要为了回一个静态清单去碰磁盘。
//! 2. **密钥**：`load_or_create()` 会去系统凭据管理器要密钥，在少数配置下可能弹窗。
//!    如果进程一启动就去拿，宿主那边看到的是"进程起来了但不回话"，而且没人知道它在等什么。
//!    改成第一次真要连服务器时才取，至少"卡住"和"刚才那个请求"能对上号。
//!
//! 拿到密钥后会组装一个完整的 `AppState`，这样可以原样复用 `prepare_probe` /
//! `sftp::session::open` —— **和 GUI 走同一套凭据与主机指纹策略**，不另起一套。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::audit::AuditLog;
use super::config::ConfigStore;
use crate::crypto::{default_provider, FieldCipher};
use crate::error::AppResult;
use crate::lease::LeaseStamp;
use crate::logging::LoggingGuard;
use crate::sftp::{SftpManager, Transfers};
use crate::ssh::session::SessionManager;
use crate::state::{AppPaths, AppState};
use crate::storage::Db;

pub struct Context {
    paths: AppPaths,
    db: OnceLock<Arc<Db>>,
    /// 只有需要连服务器时才建（这一步才会去取密钥）
    state: OnceLock<Arc<AppState>>,
    seq: AtomicU64,
    /// 白名单。按 mtime 热加载，改配置不用重启宿主。
    config: ConfigStore,
    /// 审计日志，只追加。
    audit: AuditLog,
    /// 宿主 initialize 时自报的名字。**只用于记录，绝不参与放行判断**。
    client: Mutex<String>,
    /// 是否执行「在线会话校验」。生产恒为 true（租约文件不存在即视为不可用）。
    ///
    /// 测试里默认关掉：不然每个要连服务器的用例都得先伪造一份租约文件。
    /// 需要专门验证闸门的用例用 [`Context::with_lease`] 打开。
    enforce_lease: bool,
    /// 本次调用读到的租约指纹，供审计用。
    ///
    /// 做成「一个调用一个格子」而不是返回值，是为了不让 9 个工具函数都为审计多传一个参数。
    /// 前提是 MCP 侧**串行处理请求**（`server::handle_line` 一次只 await 一个），
    /// 换并发的话这里要改成按请求上下文传递。每次 `call()` 开头会先清空。
    lease_stamp: Mutex<Option<LeaseStamp>>,
}

impl Context {
    pub fn new() -> AppResult<Self> {
        let paths = AppPaths::resolve_headless()?;
        let dir = super::data_dir(&paths.product_dir);
        let config = ConfigStore::new(&dir);
        let audit = AuditLog::new(&dir);
        Ok(Self {
            paths,
            db: OnceLock::new(),
            state: OnceLock::new(),
            seq: AtomicU64::new(0),
            config,
            audit,
            client: Mutex::new("unknown".into()),
            enforce_lease: true,
            lease_stamp: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub fn for_tests(data_dir: std::path::PathBuf) -> Self {
        let mut paths = AppPaths::under(data_dir.clone(), data_dir.clone());
        // 测试各自独立：不然所有用例共用系统临时目录里的 mcp.json / audit.jsonl，
        // 并行跑时一个用例写的配置会污染另一个。
        paths.product_dir = data_dir.clone();
        let dir = super::data_dir(&paths.product_dir);
        let config = ConfigStore::new(&dir);
        let audit = AuditLog::new(&dir);
        Self {
            paths,
            db: OnceLock::new(),
            state: OnceLock::new(),
            seq: AtomicU64::new(0),
            config,
            audit,
            client: Mutex::new("unknown".into()),
            // 测试默认不查租约：每个用例都要先伪造文件才连得上，噪音太大。
            enforce_lease: false,
            lease_stamp: Mutex::new(None),
        }
    }

    /// 打开在线会话校验，供专门验证这道闸门的用例使用。
    #[cfg(test)]
    pub fn with_lease(mut self, on: bool) -> Self {
        self.enforce_lease = on;
        self
    }

    /// 生产环境恒为 true，见字段注释。
    pub fn enforces_lease(&self) -> bool {
        self.enforce_lease
    }

    /// 记下这次闸门读到的租约指纹（`gated()` 调用）。
    pub fn set_lease_stamp(&self, stamp: Option<LeaseStamp>) {
        if let Ok(mut slot) = self.lease_stamp.lock() {
            *slot = stamp;
        }
    }

    /// 取走并清空。每次 `call()` 开头先取一次，保证不会把上一次的指纹记到这一次头上。
    pub fn take_lease_stamp(&self) -> Option<LeaseStamp> {
        self.lease_stamp
            .lock()
            .map(|mut slot| slot.take())
            .unwrap_or(None)
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn config(&self) -> &ConfigStore {
        &self.config
    }

    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    pub fn set_client(&self, name: impl Into<String>) {
        if let Ok(mut guard) = self.client.lock() {
            *guard = name.into();
        }
    }

    pub fn client(&self) -> String {
        self.client
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| "unknown".into())
    }

    /// 只读工具用这个：开库（必要时建库、跑迁移），不解密任何东西。
    pub fn db(&self) -> AppResult<&Arc<Db>> {
        if let Some(db) = self.db.get() {
            return Ok(db);
        }
        let db = Arc::new(Db::open(&self.paths.db_file)?);
        Ok(self.db.get_or_init(|| db))
    }

    /// 需要连服务器时用这个：拿密钥、组装 `AppState`。
    ///
    /// 密钥取不到就返回 `CRYPTO_KEY_UNAVAILABLE` —— 和 GUI 的未解锁状态是同一个错误码，
    /// 宿主那边能原样看到"不是网络问题，是本地密钥拿不到"。
    pub fn app_state(&self) -> AppResult<Arc<AppState>> {
        if let Some(state) = self.state.get() {
            return Ok(state.clone());
        }

        let db = self.db()?.clone();
        let provider = default_provider(&self.paths.data_dir);
        let key_source = provider.source();

        // 优先读 GUI 写好的镜像：它用 DPAPI 绑到 Windows 用户主密钥，**不依赖交互登录会话**。
        // 读不到再回退系统凭据管理器（桌面会话里可用），并留日志说明走了哪条路。
        let mirror_path = self.paths.data_dir.join(crate::crypto::mirror::FILE_NAME);
        let key = match crate::crypto::mirror::load(&mirror_path) {
            Ok(key) => {
                tracing::info!("使用后台可读的密钥镜像");
                key
            }
            Err(reason) => {
                tracing::warn!(error = %reason, "没有可用密钥镜像，回退系统凭据管理器");
                let key = provider
                    .load_or_create()
                    .map_err(crate::error::AppError::from)?;
                // 这次是在能读钥匙串的会话里跑通的，顺手把镜像补上，
                // 下次即使宿主起在后台会话里也不用再回退。
                if let Err(err) = crate::crypto::mirror::store(&key, &mirror_path) {
                    tracing::warn!(error = %err, "补写密钥镜像失败，下次仍需回退");
                }
                key
            }
        };

        let state = Arc::new(AppState {
            db,
            cipher: Some(Arc::new(FieldCipher::new(&key))),
            paths: self.paths.clone(),
            key_source,
            heartbeat: Mutex::new(None),
            next_seq: AtomicU64::new(0),
            sessions: Arc::new(SessionManager::new()),
            sftp: Arc::new(SftpManager::new()),
            transfers: Arc::new(Transfers::new()),
            started_at_ms: crate::state::now_millis(),
            // MCP 不落盘日志（真日志要和 GUI 分开规划），但 stdout 更不能碰
            _logging: LoggingGuard::disabled(),
        });

        Ok(self.state.get_or_init(|| state).clone())
    }

    /// 一次性会话 id。MCP 这边每次调用都新开一条连接、用完即关，
    /// 所以带上自己的前缀，方便在日志里和 GUI 的会话区分开。
    pub fn next_session_id(&self) -> String {
        format!("mcp-{}", self.seq.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("termharbor-mcp-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn db_is_created_at_the_composed_path() {
        let dir = temp_dir("db");
        let ctx = Context::for_tests(dir.clone());

        let db = ctx.db().expect("空目录也该能建库");
        assert_eq!(db.path(), dir.join("termharbor.db"));
        assert!(dir.join("termharbor.db").exists(), "库文件应该被建出来");
        assert!(db.schema_version().expect("能读 schema 版本") > 0);
    }

    /// 第二次取必须是同一个实例（OnceLock 的语义，重复开库会多一份连接和锁）。
    #[test]
    fn db_is_cached_between_calls() {
        let ctx = Context::for_tests(temp_dir("cache"));
        let first = ctx.db().expect("第一次").clone();
        let second = ctx.db().expect("第二次").clone();
        assert!(Arc::ptr_eq(&first, &second));
    }

    /// 这是"用户零操作"的核心路径：只要 data_dir 里有镜像，MCP 就能自己拿到密钥，
    /// 不需要宿主处在交互会话里，也不需要任何环境变量。
    #[test]
    fn app_state_unlocks_from_mirror_alone() {
        let dir = temp_dir("mirror");
        let key = [0x7u8; 32];
        let mirror_path = dir.join(crate::crypto::mirror::FILE_NAME);
        crate::crypto::mirror::store(&key, &mirror_path).expect("写镜像");

        let ctx = Context::for_tests(dir.clone());
        let state = ctx.app_state().expect("有镜像就该能组装出解锁状态");
        assert!(!state.is_locked(), "有镜像时不该停留在未解锁状态");
    }

    #[test]
    fn session_ids_are_unique_and_namespaced() {
        let ctx = Context::for_tests(temp_dir("seq"));
        assert_eq!(ctx.next_session_id(), "mcp-1");
        assert_eq!(ctx.next_session_id(), "mcp-2");
    }
}
