//! 远程文件操作：新建目录、重命名、删除。

use russh_sftp::client::SftpSession;

use super::list::{is_windows_drive_root, join_path};
use super::map_error;
use crate::error::{AppError, AppResult, ErrCode};

/// 递归删除的深度上限。
///
/// 正常目录不会这么深；设上限是为了防病态结构（比如服务端把目录项返回成自引用）
/// 把栈打爆，或者一次误操作删掉整棵树。
const MAX_DEPTH: u32 = 64;

pub async fn create_dir(session: &SftpSession, path: &str) -> AppResult<()> {
    session.create_dir(path).await.map_err(map_error)
}

pub async fn rename(session: &SftpSession, from: &str, to: &str) -> AppResult<()> {
    session.rename(from, to).await.map_err(map_error)
}

/// 路径是否存在、是文件还是目录。上传前用它判断要不要提示覆盖。
pub async fn stat(session: &SftpSession, path: &str) -> AppResult<Option<(bool, u64)>> {
    match session.symlink_metadata(path).await {
        Ok(meta) => Ok(Some((meta.is_dir(), meta.len()))),
        Err(russh_sftp::client::error::Error::Status(status))
            if status.status_code == russh_sftp::protocol::StatusCode::NoSuchFile =>
        {
            Ok(None)
        }
        Err(err) => Err(map_error(err)),
    }
}

/// 删除文件或目录。
///
/// `recursive` 必须由调用方显式传 true —— 删除目录树是破坏性操作，
/// 需求 F-07 要求前端二次确认，后端不给「自动判断要不要递归」的便利。
pub async fn remove(session: &SftpSession, path: &str, recursive: bool) -> AppResult<()> {
    // 拒绝删除根：`remove("/", true)` 会从根开始递归删掉整台机器。
    // 前端点不出来（列表里只有子项），但 IPC 是能被直接调用的，
    // 一条命令的代价太大，必须在这一层挡住。
    if is_root_like(path) {
        return Err(AppError::validation("拒绝对根目录执行删除"));
    }

    // 用 lstat 而不是 stat：符号链接要删链接本身，不能跟着进目标目录
    let meta = session.symlink_metadata(path).await.map_err(map_error)?;

    if !meta.is_dir() {
        return session.remove_file(path).await.map_err(map_error);
    }

    if !recursive {
        // 非空目录会失败，把错误原样抛给用户，而不是悄悄递归删掉
        return session.remove_dir(path).await.map_err(map_error);
    }

    remove_tree(session, path, 0).await
}

/// 根目录、当前目录、盘符根 —— 这些都不是「某个文件夹」，删它们等于删整机。
fn is_root_like(path: &str) -> bool {
    let trimmed = path.trim();
    let trimmed = trimmed.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return true;
    }
    if trimmed == "/" {
        return true;
    }
    // Windows 的 OpenSSH 把盘符映射成 /C:
    is_windows_drive_root(trimmed)
}

/// 深度优先删除。先清子项再删自己，否则 rmdir 会因目录非空失败。
fn remove_tree<'a>(
    session: &'a SftpSession,
    path: &'a str,
    depth: u32,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<()>> + Send + 'a>> {
    Box::pin(async move {
        if depth >= MAX_DEPTH {
            return Err(AppError::with_detail(
                ErrCode::SftpIoError,
                format!("目录层级超过 {MAX_DEPTH}，已中止递归删除: {path}"),
            ));
        }

        let dir = session.read_dir(path).await.map_err(map_error)?;
        let children: Vec<(String, bool)> = dir
            .map(|entry| {
                let meta = entry.metadata();
                // 符号链接当文件处理，不跟进去
                (entry.file_name(), meta.is_dir() && !meta.is_symlink())
            })
            .collect();

        for (name, is_dir) in children {
            let child = join_path(path, &name);
            if is_dir {
                remove_tree(session, &child, depth + 1).await?;
            } else {
                session.remove_file(&child).await.map_err(map_error)?;
            }
        }

        session.remove_dir(path).await.map_err(map_error)
    })
}

#[cfg(test)]
mod tests {
    use super::is_root_like;

    #[test]
    fn root_like_paths_are_refused() {
        // 这些都不是「某个文件夹」，递归删掉等于删整机。
        // 前端点不出来（列表里只有子项），但 IPC 能被直接调用。
        for path in ["/", "//", "/C:", "/c:/", ".", "..", "", "   "] {
            assert!(is_root_like(path), "应当拒绝: {path:?}");
        }
    }

    #[test]
    fn normal_paths_are_still_allowed() {
        for path in ["/tmp", "/var/log", "/home/user", "/C:/Users"] {
            assert!(!is_root_like(path), "不该误伤: {path}");
        }
    }
}
