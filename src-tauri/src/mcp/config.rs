//! MCP 配置。
//!
//! 文件放在**产品根目录**（`%APPDATA%\TermHarbor\mcp.json`），不在卸载器会删的
//! bundle id 目录里 —— 配置不该随「删除应用数据」被悄悄清掉。
//!
//! 三条硬规矩：
//! 1. **连接级 fail-closed**：文件缺失、格式错、字段不认识，一律按「全关」处理，
//!    绝不退化成「没配置就全放开」。用户必须显式勾一次连接。
//! 2. **热加载**：按 mtime 缓存，改完不用重启 MCP 宿主。
//! 3. **原子写**：GUI 写文件时先写临时文件再 rename，避免 MCP 读到半截内容。
//!
//! ## 授权语义：勾选即授权，列表只用于收窄
//!
//! 0.1.27 是「逐项白名单」：勾了连接还要勾工具、加路径、加命令，三步缺一步就干不了活。
//! 心智模型错了 —— 用户给出去的是一个 SSH 连接，那本来就是整台机器。
//! 0.1.28 起反过来：`enabled: true` 就是这台机器的全部能力，三个列表留空表示不限，
//! 填了才变成收窄。旧配置里的非空列表继续按「只允许这些」解释，语义不变。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

pub const FILE_NAME: &str = "mcp.json";

pub const TOOL_LIST_CONNECTIONS: &str = "list_connections";
pub const TOOL_SFTP_LIST: &str = "sftp_list";
pub const TOOL_SFTP_READ: &str = "sftp_read";
pub const TOOL_SFTP_WRITE: &str = "sftp_write";
pub const TOOL_SFTP_UPLOAD: &str = "sftp_upload";
pub const TOOL_SFTP_DOWNLOAD: &str = "sftp_download";
pub const TOOL_SFTP_MKDIR: &str = "sftp_mkdir";
pub const TOOL_SFTP_RENAME: &str = "sftp_rename";
pub const TOOL_LOAD_SNAPSHOT: &str = "load_snapshot";
pub const TOOL_EXEC: &str = "exec";

/// 全部工具名，给设置页和校验共用。
pub const ALL_TOOLS: &[&str] = &[
    TOOL_LIST_CONNECTIONS,
    TOOL_SFTP_LIST,
    TOOL_SFTP_READ,
    TOOL_SFTP_WRITE,
    TOOL_SFTP_UPLOAD,
    TOOL_SFTP_DOWNLOAD,
    TOOL_SFTP_MKDIR,
    TOOL_SFTP_RENAME,
    TOOL_LOAD_SNAPSHOT,
    TOOL_EXEC,
];

/// 默认超时。装个包、跑个部署脚本都在这个量级内，太短会让「能用」变成「总超时」。
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// 硬上限。再长就不该由 agent 同步等着了，应该转后台。
pub const MAX_TIMEOUT_SECS: u64 = 300;
pub const DEFAULT_MAX_OUTPUT_KB: usize = 64;
pub const MAX_OUTPUT_KB_LIMIT: usize = 512;

fn default_version() -> u32 {
    1
}
fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}
fn default_output() -> usize {
    DEFAULT_MAX_OUTPUT_KB
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    /// 没有显式列出的连接一律不可见。这个字段保留给「批量开启」场景，默认 false。
    #[serde(default)]
    pub default_enabled: bool,
    /// key 可以是连接 id，也可以是连接名称。
    #[serde(default)]
    pub connections: BTreeMap<String, ConnectionPolicy>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            version: 1,
            default_enabled: false,
            connections: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionPolicy {
    /// 唯一的必选开关：勾了就是「这台机器交给 agent 用」。
    #[serde(default)]
    pub enabled: bool,
    /// 留空 = 全部工具；非空 = 只允许列出的这些（收窄用）
    #[serde(default)]
    pub tools: Vec<String>,
    /// 留空 = 任意远端路径；非空 = 只能碰这些目录前缀（收窄用）
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub exec: ExecPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecPolicy {
    /// 留空 = 任意**单条**命令（复合字符仍然拒绝）；非空 = 只允许这些 token 前缀
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_output")]
    pub max_output_kb: usize,
}

impl Default for ExecPolicy {
    fn default() -> Self {
        Self {
            allow: Vec::new(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_kb: DEFAULT_MAX_OUTPUT_KB,
        }
    }
}

impl ExecPolicy {
    /// 限额在读取时 clamp，防止有人手写配置把上限捅穿。
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_secs.clamp(1, MAX_TIMEOUT_SECS))
    }
    pub fn max_output_bytes(&self) -> usize {
        self.max_output_kb.clamp(1, MAX_OUTPUT_KB_LIMIT) * 1024
    }
}

impl McpConfig {
    /// 按 id 优先、名称兜底找策略。
    pub fn policy_for(&self, id: &str, name: &str) -> Option<&ConnectionPolicy> {
        let by_id = self.connections.get(id);
        if by_id.is_some() {
            return by_id;
        }
        if id != name {
            return self.connections.get(name);
        }
        None
    }

    /// 这台机器对 MCP 是否可见。
    pub fn is_visible(&self, id: &str, name: &str) -> bool {
        match self.policy_for(id, name) {
            Some(policy) => policy.enabled,
            None => self.default_enabled,
        }
    }

    /// 工具是否可用。`tools` 留空表示这台机器全部工具都开。
    pub fn allows_tool(&self, policy: &ConnectionPolicy, tool: &str) -> bool {
        policy.enabled
            && (policy.tools.is_empty() || policy.tools.iter().any(|allowed| allowed == tool))
    }

    /// 路径是否可访问。`paths` 留空表示不限；填了才按前缀收窄。
    ///
    /// 只做字符串前缀 + 边界检查，不做路径规范化 —— 规范化交给服务端，
    /// 本地乱改反而容易绕过（`/var/log/../etc` 这种由对端 resolve 才是真相）。
    pub fn allows_path(policy: &ConnectionPolicy, path: &str) -> bool {
        if policy.paths.is_empty() {
            return true;
        }
        policy.paths.iter().any(|prefix| {
            let prefix = prefix.trim_end_matches('/');
            path == prefix || path.starts_with(&format!("{prefix}/"))
        })
    }
}

/// 配置存储。mtime 变化时重读，读写都跨进程安全。
pub struct ConfigStore {
    path: PathBuf,
    cache: Mutex<Option<(SystemTime, u64, McpConfig)>>,
}

impl ConfigStore {
    pub fn new(product_dir: &Path) -> Self {
        Self {
            path: product_dir.join(FILE_NAME),
            cache: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读配置。**任何失败都返回全关配置**，不把错误往上抛。
    pub fn load(&self) -> McpConfig {
        let stamp = std::fs::metadata(&self.path)
            .ok()
            .and_then(|meta| meta.modified().ok().map(|time| (time, meta.len())));

        if let Ok(mut cache) = self.cache.lock() {
            if let (Some((time, len)), Some((cached_time, cached_len, config))) =
                (stamp, cache.as_ref())
            {
                if *cached_time == time && *cached_len == len {
                    return config.clone();
                }
            }

            let config = match stamp {
                Some(_) => match std::fs::read_to_string(&self.path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<McpConfig>(&raw).ok())
                {
                    Some(config) => config,
                    None => {
                        tracing::warn!(
                            path = %self.path.display(),
                            "MCP 配置读取或解析失败，本次按「全部关闭」处理"
                        );
                        McpConfig::default()
                    }
                },
                // 文件不存在是全新安装的正常状态，不算错误
                None => McpConfig::default(),
            };

            if let Some((time, len)) = stamp {
                *cache = Some((time, len, config.clone()));
            }
            return config;
        }

        McpConfig::default()
    }

    /// 全量写回（前端拿到的结构改完整个提交）。先写 tmp 再 rename。
    pub fn save(&self, config: &McpConfig) -> AppResult<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let raw = serde_json::to_string_pretty(config)
            .map_err(|err| AppError::internal(format!("MCP 配置序列化失败: {err}")))?;

        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, raw)?;
        std::fs::rename(&tmp, &self.path)?;

        if let Ok(mut cache) = self.cache.lock() {
            *cache = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("th-mcp-config-{tag}-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_means_everything_off() {
        let cfg = ConfigStore::new(&temp_dir("missing")).load();
        assert!(!cfg.default_enabled);
        assert!(cfg.connections.is_empty());
        assert!(!cfg.is_visible("id-1", "web-01"));
    }

    #[test]
    fn invalid_json_is_fail_closed() {
        let dir = temp_dir("invalid");
        std::fs::write(dir.join(FILE_NAME), "{not json").unwrap();
        let cfg = ConfigStore::new(&dir).load();
        assert!(!cfg.is_visible("id-1", "web-01"));
    }

    #[test]
    fn unknown_field_is_fail_closed() {
        let dir = temp_dir("unknown");
        std::fs::write(dir.join(FILE_NAME), r#"{"version":1,"oops":true}"#).unwrap();
        let cfg = ConfigStore::new(&dir).load();
        assert!(!cfg.default_enabled);
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = temp_dir("roundtrip");
        let store = ConfigStore::new(&dir);
        let mut cfg = McpConfig::default();
        cfg.connections.insert(
            "web-01".into(),
            ConnectionPolicy {
                enabled: true,
                tools: vec![TOOL_SFTP_LIST.into()],
                paths: vec!["/var/log".into()],
                exec: ExecPolicy::default(),
            },
        );
        store.save(&cfg).unwrap();

        let loaded = store.load();
        let policy = loaded.policy_for("id-1", "web-01").unwrap();
        assert!(loaded.allows_tool(policy, TOOL_SFTP_LIST));
        assert!(!loaded.allows_tool(policy, TOOL_EXEC));
    }

    #[test]
    fn path_prefix_must_respect_component_boundary() {
        let policy = ConnectionPolicy {
            paths: vec!["/var/log".into()],
            ..Default::default()
        };
        assert!(McpConfig::allows_path(&policy, "/var/log"));
        assert!(McpConfig::allows_path(&policy, "/var/log/nginx"));
        assert!(!McpConfig::allows_path(&policy, "/var/logger"));
        assert!(!McpConfig::allows_path(&policy, "/etc"));
    }

    /// 0.1.28 的核心语义：勾了连接就等于给了这台机器的全部能力。
    /// 空列表是「不限」，不是「全禁」。
    #[test]
    fn empty_lists_mean_unrestricted_not_denied() {
        let cfg = McpConfig::default();
        let policy = ConnectionPolicy {
            enabled: true,
            ..Default::default()
        };

        for tool in ALL_TOOLS {
            assert!(
                cfg.allows_tool(&policy, tool),
                "{tool} 应当默认可用于已启用的连接"
            );
        }
        assert!(McpConfig::allows_path(&policy, "/etc/shadow"));
        assert!(McpConfig::allows_path(&policy, "/"));
    }

    /// 但连接本身没勾，一切照旧全关 —— 升级不能把用户的服务器静默交出去。
    #[test]
    fn disabled_connection_denies_everything_even_with_empty_lists() {
        let cfg = McpConfig::default();
        let policy = ConnectionPolicy::default();
        for tool in ALL_TOOLS {
            assert!(!cfg.allows_tool(&policy, tool));
        }
    }

    /// 填了列表就回到收窄语义，旧配置（0.1.27 写的）不需要迁移。
    #[test]
    fn non_empty_list_still_narrows() {
        let cfg = McpConfig::default();
        let policy = ConnectionPolicy {
            enabled: true,
            tools: vec![TOOL_SFTP_LIST.into()],
            paths: vec!["/var/log".into()],
            ..Default::default()
        };
        assert!(cfg.allows_tool(&policy, TOOL_SFTP_LIST));
        assert!(!cfg.allows_tool(&policy, TOOL_EXEC));
        assert!(!cfg.allows_tool(&policy, TOOL_SFTP_WRITE));
        assert!(!McpConfig::allows_path(&policy, "/etc"));
    }

    #[test]
    fn limits_are_clamped() {
        let policy = ExecPolicy {
            timeout_secs: 9999,
            max_output_kb: 9999,
            ..Default::default()
        };
        assert_eq!(policy.timeout().as_secs(), MAX_TIMEOUT_SECS);
        assert_eq!(policy.max_output_bytes(), MAX_OUTPUT_KB_LIMIT * 1024);
    }
}
