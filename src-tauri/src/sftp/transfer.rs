//! 上传下载：128KB 分块、150ms 进度节流、chunk 边界检查取消、先写 `.part` 再改名。
//!
//! 为什么手写 copy 循环而不用 `tokio::io::copy`：需要同时做三件事 ——
//! 累加进度、按时间节流上报、在每个块边界检查取消。
//! `copy` 把读写的粒度藏起来了，插不进这三个钩子。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use russh_sftp::client::SftpSession;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use super::list::join_path;
use super::map_error;
use crate::error::{AppError, AppResult, ErrCode};

/// 每块 128KB。小于 64KB 会让请求数翻倍（每次往返都有开销），
/// 大于 256KB 则一台慢服务器上单块耗时过长，取消要等很久才生效。
const CHUNK: usize = 128 * 1024;

/// 进度上报节流。传输 1GB 会发上千个块，不节流会把 IPC 刷爆。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Upload,
    Download,
}

impl TransferDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            TransferDirection::Upload => "upload",
            TransferDirection::Download => "download",
        }
    }
}

/// 推给前端的进度。前端按 `transferId` 更新对应的进度条。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferProgress {
    pub transfer_id: String,
    pub direction: &'static str,
    pub name: String,
    pub transferred: u64,
    pub total: u64,
    /// 本次传输的平均速率（字节/秒）
    pub bps: u64,
    /// running | done | failed | canceled
    pub state: &'static str,
    pub error: Option<String>,
}

/// 进度回调。和 ssh 层一样用回调注入，sftp 模块不依赖 Tauri。
pub type ProgressFn = Box<dyn FnMut(TransferProgress) + Send + 'static>;

/// 在途传输的取消令牌注册表。
#[derive(Default)]
pub struct Transfers {
    inner: Mutex<HashMap<String, CancellationToken>>,
}

impl Transfers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, transfer_id: String, token: CancellationToken) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(transfer_id, token);
    }

    /// 请求取消。返回 false 表示这个传输已经结束（前端按钮点晚了）。
    pub fn cancel(&self, transfer_id: &str) -> bool {
        match self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(transfer_id)
        {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    pub fn finish(&self, transfer_id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(transfer_id);
    }

    /// 会话关闭时取消它名下的传输。传输 id 用 `session::序号` 拼，靠前缀匹配。
    pub fn cancel_session(&self, session_id: &str) {
        let prefix = format!("{session_id}::");
        let tokens: Vec<CancellationToken> = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|(id, _)| id.starts_with(&prefix))
            .map(|(_, token)| token.clone())
            .collect();

        for token in tokens {
            token.cancel();
        }
    }

    pub fn active_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

/// 速率与节流状态。
struct Meter {
    transfer_id: String,
    direction: &'static str,
    name: String,
    total: u64,
    transferred: u64,
    started: Instant,
    last_emit: Instant,
    emit: ProgressFn,
}

impl Meter {
    fn new(
        transfer_id: String,
        direction: TransferDirection,
        name: String,
        total: u64,
        emit: ProgressFn,
    ) -> Self {
        let now = Instant::now();
        Self {
            transfer_id,
            direction: direction.as_str(),
            name,
            total,
            transferred: 0,
            started: now,
            last_emit: now,
            emit,
        }
    }

    fn bps(&self) -> u64 {
        let seconds = self.started.elapsed().as_secs_f64();
        if seconds <= 0.0 {
            return 0;
        }
        (self.transferred as f64 / seconds) as u64
    }

    fn snapshot(&self, state: &'static str, error: Option<String>) -> TransferProgress {
        TransferProgress {
            transfer_id: self.transfer_id.clone(),
            direction: self.direction,
            name: self.name.clone(),
            transferred: self.transferred,
            total: self.total,
            bps: self.bps(),
            state,
            error,
        }
    }

    /// 拿到远端/本地大小之后再补 total：算目录项时还不知道，但不该为了它推迟创建 meter。
    fn set_total(&mut self, total: u64) {
        self.total = total;
    }

    /// 累加并（按节流间隔）上报。
    fn advance(&mut self, bytes: u64) {
        self.transferred += bytes;
        if self.last_emit.elapsed() >= PROGRESS_INTERVAL {
            self.last_emit = Instant::now();
            let snapshot = self.snapshot("running", None);
            (self.emit)(snapshot);
        }
    }

    fn finish(&mut self, state: &'static str, error: Option<String>) {
        let snapshot = self.snapshot(state, error);
        (self.emit)(snapshot);
    }
}

/// 上传：本地文件 → 远端路径。
///
/// 远端先写 `<目标>.part`，传完再改名，中途失败不会留下半截文件。
pub async fn upload(
    session: &SftpSession,
    local: &Path,
    remote_path: &str,
    transfer_id: String,
    cancel: CancellationToken,
    emit: ProgressFn,
) -> AppResult<u64> {
    let name = file_name_of(remote_path);
    let mut meter = Meter::new(transfer_id, TransferDirection::Upload, name, 0, emit);

    // 所有结果都从这一个出口收，保证成功和失败都一定发一次终态事件。
    // 否则前端会永远停在"进行中"（少了事件就跟丢了一样）。
    let outcome = upload_inner(session, local, remote_path, &cancel, &mut meter).await;

    match outcome {
        Ok(bytes) => {
            meter.finish("done", None);
            Ok(bytes)
        }
        Err(err) => {
            meter.finish(state_of(&err), Some(err.message.to_string()));
            Err(err)
        }
    }
}

async fn upload_inner(
    session: &SftpSession,
    local: &Path,
    remote_path: &str,
    cancel: &CancellationToken,
    meter: &mut Meter,
) -> AppResult<u64> {
    validate_local_read(local).await?;

    let mut source = tokio::fs::File::open(local).await.map_err(|err| {
        AppError::with_detail(ErrCode::SftpIoError, format!("打开本地文件失败: {err}"))
    })?;

    let total = source.metadata().await.map(|meta| meta.len()).unwrap_or(0);
    meter.set_total(total);

    let part_path = part_path_for(remote_path);
    let mut target = session.create(&part_path).await.map_err(map_error)?;

    let result = pump(&mut source, &mut target, meter, cancel).await;

    // File::close 消费 self，成功失败都要关掉，否则远端句柄泄漏
    let close_result = target.close().await;

    let transferred = match result {
        Ok(bytes) => {
            close_result.map_err(|err| {
                AppError::with_detail(ErrCode::SftpIoError, format!("关闭远端文件失败: {err}"))
            })?;
            bytes
        }
        Err(err) => {
            let _ = session.remove_file(&part_path).await;
            return Err(err);
        }
    };

    finish_rename(session, &part_path, remote_path).await?;
    Ok(transferred)
}

/// 下载：远端路径 → 本地文件。同样先写 `.part`。
pub async fn download(
    session: &SftpSession,
    remote_path: &str,
    local: &Path,
    transfer_id: String,
    cancel: CancellationToken,
    emit: ProgressFn,
) -> AppResult<u64> {
    let name = file_name_of(remote_path);
    let mut meter = Meter::new(transfer_id, TransferDirection::Download, name, 0, emit);

    let outcome = download_inner(session, remote_path, local, &cancel, &mut meter).await;

    match outcome {
        Ok(bytes) => {
            meter.finish("done", None);
            Ok(bytes)
        }
        Err(err) => {
            meter.finish(state_of(&err), Some(err.message.to_string()));
            Err(err)
        }
    }
}

async fn download_inner(
    session: &SftpSession,
    remote_path: &str,
    local: &Path,
    cancel: &CancellationToken,
    meter: &mut Meter,
) -> AppResult<u64> {
    validate_local_write(local)?;

    let mut source = session.open(remote_path).await.map_err(map_error)?;

    let total = source.metadata().await.map(|meta| meta.len()).unwrap_or(0);
    meter.set_total(total);

    let part = part_path_for_local(local);
    let mut target = tokio::fs::File::create(&part).await.map_err(|err| {
        AppError::with_detail(ErrCode::SftpIoError, format!("创建本地文件失败: {err}"))
    })?;

    let result = pump(&mut source, &mut target, meter, cancel).await;

    // 先把缓冲刷到盘上再改名，否则可能出现「文件大小对但内容是空的」
    let flush_result = target.flush().await;
    drop(target);

    let transferred = match result {
        Ok(bytes) => {
            flush_result.map_err(|err| {
                AppError::with_detail(ErrCode::SftpIoError, format!("写入本地文件失败: {err}"))
            })?;
            bytes
        }
        Err(err) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(err);
        }
    };

    // 目标已存在时 rename 在 Windows 上会失败，先删掉
    if tokio::fs::try_exists(local).await.unwrap_or(false) {
        tokio::fs::remove_file(local).await.map_err(|err| {
            AppError::with_detail(ErrCode::SftpIoError, format!("覆盖本地文件失败: {err}"))
        })?;
    }

    tokio::fs::rename(&part, local).await.map_err(|err| {
        AppError::with_detail(ErrCode::SftpIoError, format!("保存本地文件失败: {err}"))
    })?;

    Ok(transferred)
}

/// 传输主循环。取消检查放在每个块边界，取消后立即返回。
async fn pump<R, W>(
    reader: &mut R,
    writer: &mut W,
    meter: &mut Meter,
    cancel: &CancellationToken,
) -> AppResult<u64>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut buffer = vec![0u8; CHUNK];
    let mut written: u64 = 0;

    loop {
        if cancel.is_cancelled() {
            return Err(AppError::new(ErrCode::SftpCancelled));
        }

        let read = reader.read(&mut buffer).await.map_err(|err| {
            AppError::with_detail(ErrCode::SftpIoError, format!("读取失败: {err}"))
        })?;

        if read == 0 {
            return Ok(written);
        }

        writer.write_all(&buffer[..read]).await.map_err(|err| {
            AppError::with_detail(ErrCode::SftpIoError, format!("写入失败: {err}"))
        })?;

        written += read as u64;
        meter.advance(read as u64);
    }
}

/// `.part` 改名成最终文件。目标已存在时先删，避免「服务端拒绝覆盖」。
async fn finish_rename(session: &SftpSession, part: &str, target: &str) -> AppResult<()> {
    if session.rename(part, target).await.is_ok() {
        return Ok(());
    }

    // 标准 SSH_FXP_RENAME 在部分服务端上不覆盖已有文件
    let _ = session.remove_file(target).await;
    session
        .rename(part, target)
        .await
        .map_err(|err| AppError::with_detail(ErrCode::SftpIoError, format!("重命名失败: {err}")))
}

fn state_of(err: &AppError) -> &'static str {
    if err.code == ErrCode::SftpCancelled {
        "canceled"
    } else {
        "failed"
    }
}

fn file_name_of(path: &str) -> String {
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// 远端临时文件路径：`/tmp/a.txt` → `/tmp/a.txt.part`
fn part_path_for(path: &str) -> String {
    format!("{path}.part")
}

/// 本地临时文件路径，保留扩展名便于工具识别：
/// `C:\a\b.zip` → `C:\a\b.zip.part`
fn part_path_for_local(path: &Path) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(".part");
    PathBuf::from(raw)
}

/// 上传前的基本检查：必须存在且是普通文件。
async fn validate_local_read(path: &Path) -> AppResult<()> {
    let meta = tokio::fs::metadata(path).await.map_err(|err| {
        AppError::with_detail(ErrCode::SftpIoError, format!("本地文件不可用: {err}"))
    })?;

    if meta.is_dir() {
        return Err(AppError::validation("暂不支持上传整个目录，请选择单个文件"));
    }
    Ok(())
}

/// 下载目标路径检查。
///
/// 前端已经用系统保存对话框限定过一遍，这里是第二道闸：不信任前端传来的任意路径。
fn validate_local_write(path: &Path) -> AppResult<()> {
    if !path.is_absolute() {
        return Err(AppError::validation("本地路径必须是绝对路径"));
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(AppError::validation("目标目录不存在"));
        }
    }

    if is_system_dir(path) {
        return Err(AppError::validation("不允许写入系统目录"));
    }

    Ok(())
}

fn is_system_dir(path: &Path) -> bool {
    #[cfg(windows)]
    {
        let forbidden = ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "windir"];
        forbidden.iter().any(|name| {
            std::env::var(name)
                .ok()
                .filter(|root| !root.is_empty())
                .is_some_and(|root| path.starts_with(Path::new(&root)))
        })
    }

    #[cfg(not(windows))]
    {
        const FORBIDDEN: &[&str] = &[
            "/etc", "/usr", "/bin", "/sbin", "/boot", "/dev", "/proc", "/sys",
        ];
        FORBIDDEN.iter().any(|root| path.starts_with(root))
    }
}

/// 本地文件名的拼接辅助：把远端 entry 的 name 拼到本地目录后面。
pub fn local_join(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

/// 远端目录 + 文件名的拼接辅助，供命令层使用。
pub fn remote_join(dir: &str, name: &str) -> String {
    join_path(dir, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_path_keeps_extension() {
        assert_eq!(part_path_for("/var/log/a.txt"), "/var/log/a.txt.part");
        assert_eq!(
            part_path_for_local(Path::new("/tmp/a.zip")),
            PathBuf::from("/tmp/a.zip.part")
        );
    }

    #[test]
    fn file_name_handles_trailing_slash() {
        assert_eq!(file_name_of("/var/log/a.txt"), "a.txt");
        assert_eq!(file_name_of("/var/log/"), "log");
        assert_eq!(file_name_of("a.txt"), "a.txt");
    }

    #[test]
    fn cancel_is_reported_as_canceled_state() {
        let err = AppError::new(ErrCode::SftpCancelled);
        assert_eq!(state_of(&err), "canceled");
        assert_eq!(state_of(&AppError::new(ErrCode::SftpIoError)), "failed");
    }

    #[test]
    fn relative_paths_are_rejected_for_download() {
        let err = validate_local_write(Path::new("relative.txt")).unwrap_err();
        assert_eq!(err.code, ErrCode::ValidationError);
    }
}
