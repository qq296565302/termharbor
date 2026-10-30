//! 远程目录浏览。

use std::time::UNIX_EPOCH;

use russh_sftp::client::SftpSession;
use serde::Serialize;

use super::map_error;
use crate::error::AppResult;

/// 目录里的一项。字段与前端 `RemoteEntry` 一一对应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEntry {
    pub name: String,
    /// 绝对路径，前端点进去时直接用，不在前端拼字符串
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified_ms: Option<i64>,
    /// 原始 Unix mode（含类型位）。前端只取低 12 位显示权限。
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

/// 读一个目录，目录排在文件前面，再按名称排序。
///
/// 不排序的话顺序完全由服务端决定，同一个目录每次刷新顺序都可能变，
/// 用户会以为文件被移动了。
pub async fn read_dir(session: &SftpSession, path: &str) -> AppResult<Vec<RemoteEntry>> {
    let dir = session.read_dir(path).await.map_err(map_error)?;

    let mut entries: Vec<RemoteEntry> = dir
        .map(|entry| {
            let name = entry.file_name();
            let meta = entry.metadata();
            let is_dir = meta.is_dir();
            let is_symlink = meta.is_symlink();

            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|delta| delta.as_millis() as i64);

            RemoteEntry {
                path: join_path(path, &name),
                name,
                is_dir,
                is_symlink,
                size: meta.len(),
                modified_ms,
                mode: meta.permissions,
                uid: meta.uid,
                gid: meta.gid,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(entries)
}

/// 拼路径。只处理分隔符，不做规范化 —— 规范化交给服务端，本地乱改反而容易出错。
pub fn join_path(parent: &str, name: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// 上一级目录。已经是根时返回 None，前端据此禁用「上一级」。
pub fn parent_path(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    match trimmed.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(index) => Some(trimmed[..index].to_string()),
        // 相对路径没有上级可言，交给前端保持不动
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_handles_both_trailing_slash_cases() {
        assert_eq!(join_path("/var/log", "syslog"), "/var/log/syslog");
        assert_eq!(join_path("/var/log/", "syslog"), "/var/log/syslog");
        assert_eq!(join_path("/", "etc"), "/etc");
    }

    #[test]
    fn parent_of_root_is_none() {
        assert_eq!(parent_path("/"), None);
        assert_eq!(parent_path("//"), None);
    }

    #[test]
    fn parent_steps_up_one_level() {
        assert_eq!(parent_path("/var/log"), Some("/var".to_string()));
        assert_eq!(parent_path("/var/log/"), Some("/var".to_string()));
        assert_eq!(parent_path("/var"), Some("/".to_string()));
    }
}
