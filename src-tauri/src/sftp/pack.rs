//! 「打包传输」的本地实现：多个文件/目录打成一个 tar.gz 再传，落到对端/本地自动解包。
//!
//! 为什么要有这一层：SFTP 是单文件协议，传一棵目录要发很多个 SSH_FXP 请求，
//! 慢而且容易半途而废。先在源端把整批内容打成一个流、只传一个文件，
//! 到对端再解开，往返次数从 O(文件数) 降到 O(1)。
//!
//! 安全边界（全中文说明「为什么这么写」）：
//! - **命令注入**：远端路径全部经 [`shell_quote`] 包成 POSIX 单引号串后才拼进
//!   shell 命令。单引号内 `$`、反引号、反斜杠、空格、换行全部失去特殊含义，
//!   任何用户字符串都不会被当成代码执行；绝不用裸字符串拼接命令。
//! - **目录穿越（Zip Slip）**：解包时逐条校验归档路径，拒绝 `..`、绝对路径、
//!   Windows 盘符/UNC，并拒绝符号链接/硬链接条目；打包时归档内只放相对名，
//!   不带本机绝对路径，从源头避免把 `/etc/passwd` 之类名字写进包里。
//! - **资源上限**：打包累计 512MB、解包累计 1GB、目录递归 64 层。
//!   符号链接一律跳过（不跟随），既避免把链接指向的大树复制进来，也防环路。
//! - **临时文件清理**：本地临时包在流程出口无条件删除；远端临时包在解包命令里
//!   用 `; rc=$?; rm -f ...; exit $rc` 保证失败也清理，下载流程另发一次
//!   best-effort 删除。清理失败只记日志，不掩盖主流程的真实结果。

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::{Archive, Builder, EntryType, Header};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::{AppError, AppResult, ErrCode};
use crate::sftp::session::SftpConn;
use crate::sftp::transfer::{self, ProgressFn, TransferProgress};

/// 单个上传包（打包前）累计大小上限。
const MAX_PACK_BYTES: u64 = 512 * 1024 * 1024;
/// 解包（下载）后累计大小上限，防解压炸弹。
const MAX_EXTRACT_BYTES: u64 = 1024 * 1024 * 1024;
/// 目录递归深度上限，防符号链接/异常深层目录把栈打爆。
const MAX_DEPTH: usize = 64;
/// 远端 `tar` 可能处理大目录，给足超时；exec 通道本身也带输出上限。
const EXEC_TIMEOUT: Duration = Duration::from_secs(120);
/// 远端清理命令的超时，失败不影响主结果。
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);
/// 成功哨兵。`exec_once_limited` 只读 stdout/stderr、拿不到退出码，
/// 只能靠「成功才 echo 的固定标记」来判定远端命令真的成了。
const OK_MARK: &str = "__TERMHARBOR_PACK_OK__";
/// 临时包文件名前缀。用 UUID v7 而非时间戳后缀 —— Windows 时间戳精度低，
/// 并发任务会撞名互相删文件（项目里踩过这个坑）。
const TMP_PREFIX: &str = ".termharbor-pack-";

// ---------------------------------------------------------------------------
// 命令拼装（纯字符串，可单测）
// ---------------------------------------------------------------------------

/// POSIX shell 单引号转义。
///
/// 整体包进 `'...'`，串内每个单引号替换成 `'\''`（闭引号 + 反斜杠转义的引号 + 开引号）。
/// 单引号里所有字符都是字面量，所以 `$HOME`、反引号命令替换、换行、空格都注入不进来。
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// 把远端绝对路径拆成「目录部分 / 名字部分」，供多次 `-C` 打包使用。
///
/// 名字单独取出来是为了 `tar -C <父目录> <名字>`：归档里只留名字，
/// 不会把 `/home/...` 这串绝对路径原样写进去（GNU tar 会警告并剥掉前导 `/`）。
pub fn split_remote_path(path: &str) -> AppResult<(String, String)> {
    let trimmed = path.trim_end_matches('/');
    if !trimmed.starts_with('/') {
        return Err(AppError::validation("远端路径必须是绝对路径"));
    }
    if trimmed.is_empty() || trimmed == "/" {
        return Err(AppError::validation("远端根目录不能作为打包源"));
    }
    match trimmed.rfind('/') {
        Some(0) => Ok(("/".to_string(), trimmed[1..].to_string())),
        Some(index) => Ok((
            trimmed[..index].to_string(),
            trimmed[index + 1..].to_string(),
        )),
        None => Err(AppError::validation("远端路径缺少文件名")),
    }
}

/// 远端打包命令（下载用）：把多个源打进 `/tmp` 下一个临时 tar.gz。
///
/// 多个源可能在不同目录，用多次 `-C <父目录> <名字>` 切换；所有路径与名字
/// 都经 [`shell_quote`]。末尾 `&& echo <哨兵>` 才输出成功标记，tar 失败时
/// stderr（含 `tar: ` / `command not found`）会留在输出里供上层判定。
pub fn build_remote_pack_command(tmp_pkg: &str, parts: &[(String, String)]) -> String {
    let mut cmd = format!("tar -czf {} ", shell_quote(tmp_pkg));
    for (parent, name) in parts {
        cmd.push_str("-C ");
        cmd.push_str(&shell_quote(parent));
        cmd.push(' ');
        cmd.push_str(&shell_quote(name));
        cmd.push(' ');
    }
    cmd.push_str("&& echo ");
    cmd.push_str(&shell_quote(OK_MARK));
    cmd
}

/// 远端解包命令（上传用）。
///
/// 刻意**不**用 `sh -c '...'` 再包一层：内层路径本身已是单引号串，
/// 套第二层单引号会形成嵌套引号，转义极易出错、反而打开注入口子。
/// exec 通道本就由远端登录 shell 执行，直接用 `;` 串起
/// 「解包 → 记 rc → 无论成败都删临时包 → 成功才打标记 → 透传 rc」即可。
/// 这样 tar 失败也能删掉临时包，且退出码由 shell 传播（虽然我们不读）。
pub fn build_remote_unpack_command(tmp_pkg: &str, remote_dir: &str) -> String {
    let q_pkg = shell_quote(tmp_pkg);
    let q_dir = shell_quote(remote_dir);
    let q_ok = shell_quote(OK_MARK);
    format!(
        "tar -xzf {q_pkg} -C {q_dir}; __th_rc=$?; rm -f {q_pkg}; \
         if [ $__th_rc -eq 0 ]; then echo {q_ok}; fi; exit $__th_rc"
    )
}

/// 远端 best-effort 清理命令：删临时包和可能残留的 `.part`。
pub fn build_remote_cleanup_command(tmp_pkg: &str) -> String {
    format!(
        "rm -f {} {}",
        shell_quote(tmp_pkg),
        shell_quote(&format!("{tmp_pkg}.part"))
    )
}

/// 判定远端 exec 的输出。exec 通道拿不到退出码，只能靠输出文本：
/// 有成功标记即成功；出现「找不到命令」明确给「缺少 tar」的中文提示；
/// 出现 `tar: `/`No such file`/`Permission denied` 等失败迹象给中文错误；
/// 什么都没有也判失败 —— 宁可报错也**绝不静默当成功**。
pub fn inspect_exec_output(output: &str, action: &str) -> AppResult<()> {
    if output.contains(OK_MARK) {
        return Ok(());
    }

    let lower = output.to_lowercase();
    if lower.contains("command not found") || lower.contains(": not found") {
        return Err(AppError::with_detail(
            ErrCode::SftpIoError,
            format!("服务器缺少 tar 命令，无法{action}"),
        ));
    }

    let failure = [
        "tar: ",
        "no such file",
        "permission denied",
        "cannot ",
        "not found",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if failure {
        return Err(AppError::with_detail(
            ErrCode::SftpIoError,
            format!("远端{action}失败: {}", brief_output(output)),
        ));
    }

    Err(AppError::with_detail(
        ErrCode::SftpIoError,
        format!("远端{action}未返回成功标记，输出: {}", brief_output(output)),
    ))
}

/// 错误里只带一小段输出，别把远端刷屏内容塞满 IPC/日志。
fn brief_output(output: &str) -> String {
    let trimmed = output.trim();
    let mut brief: String = trimmed.chars().take(400).collect();
    if trimmed.chars().count() > 400 {
        brief.push('…');
    }
    brief
}

// ---------------------------------------------------------------------------
// 归档路径与大小校验（纯函数，可单测）
// ---------------------------------------------------------------------------

/// 校验归档条目路径：必须相对、不含 `..`、不是绝对路径或 Windows 盘符/UNC。
///
/// 用纯字符串判断而不是 `Path` 的平台语义：`/` 和 `\` 都当分隔符，
/// 因为 Windows 上 `..\` 和 `../` 一样能穿越，而 Unix 的 `Path` 不认 `\`。
pub fn validate_archive_path(text: &str) -> AppResult<()> {
    if text.is_empty() {
        return Err(AppError::validation("归档条目路径为空"));
    }
    if text.contains('\0') {
        return Err(AppError::validation("归档条目路径含 NUL 字节"));
    }
    let bytes = text.as_bytes();
    if bytes[0] == b'/' || bytes[0] == b'\\' {
        return Err(AppError::validation("归档条目不允许绝对路径"));
    }
    // `C:\...` / `C:/...` 盘符绝对路径
    if bytes.len() >= 2 && bytes[1] == b':' {
        return Err(AppError::validation("归档条目不允许盘符绝对路径"));
    }
    if text.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(AppError::validation("归档条目路径不允许 '..' 穿越"));
    }
    Ok(())
}

/// 解包时逐条校验 `tar::Entry` 的路径，字符串层 + `Path::components` 双保险。
pub fn validate_entry_path(path: &Path) -> AppResult<()> {
    validate_archive_path(&path.to_string_lossy())?;
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::validation("归档条目路径含越界组件"));
            }
            _ => {}
        }
    }
    Ok(())
}

/// 累计大小并卡上限。抽成纯函数便于单测，不必真造 512MB 文件。
fn accumulate_size(total: u64, add: u64, limit: u64, what: &str) -> AppResult<u64> {
    let next = total
        .checked_add(add)
        .ok_or_else(|| AppError::validation("打包大小计算溢出"))?;
    if next > limit {
        return Err(AppError::validation(format!("{what}超过上限")));
    }
    Ok(next)
}

/// 从任意平台路径取「最后一段」作为归档内顶层名字。
///
/// Windows 的 `\` 在 Unix 上不是分隔符，`Path::file_name()` 会把
/// `C:\Users\a\file.txt` 整串当成一个名字，写进包里既难看又可能触发穿越校验。
/// 统一把 `/` 和 `\` 都当分隔符，只取最后一段。
pub fn entry_name(path: &Path) -> AppResult<String> {
    let raw = path.to_string_lossy();
    let name = raw
        .rsplit(['/', '\\'])
        .find(|seg| !seg.is_empty())
        .ok_or_else(|| AppError::validation("无法从路径提取文件名"))?;
    validate_archive_path(name)?;
    Ok(name.to_string())
}

// ---------------------------------------------------------------------------
// 本地打包 / 解包（同步、纯 Rust，可单测；由上层放进 spawn_blocking）
// ---------------------------------------------------------------------------

/// 把多个本地路径打进一个 `.tar.gz`。归档内只用相对名，逐条校验路径。
pub fn create_archive(
    sources: &[PathBuf],
    dst: &Path,
    cancel: &CancellationToken,
) -> AppResult<()> {
    let file = std::fs::File::create(dst).map_err(|e| pack_io("创建临时归档失败", e))?;
    let gz = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(gz);

    let mut total: u64 = 0;
    for src in sources {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let name = entry_name(src)?;
        add_path(&mut builder, src, &name, 0, &mut total, cancel)?;
    }

    // into_inner 会补写 tar 的两个全零结束块，再 finish 刷完 gzip trailer。
    // 少了这两步得到的是截断的归档，远端 tar 会报 "unexpected EOF"。
    let gz = builder
        .into_inner()
        .map_err(|e| pack_io("收尾归档失败", e))?;
    gz.finish().map_err(|e| pack_io("刷新 gzip 失败", e))?;
    Ok(())
}

fn add_path<W: std::io::Write>(
    builder: &mut Builder<W>,
    src: &Path,
    name: &str,
    depth: usize,
    total: &mut u64,
    cancel: &CancellationToken,
) -> AppResult<()> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    if depth > MAX_DEPTH {
        return Err(AppError::validation("目录层级超过 64 层，疑似符号链接环路"));
    }
    validate_archive_path(name)?;

    // symlink_metadata 不跟随链接：符号链接一律跳过，不把链接目标复制进包，
    // 也避免 A -> B -> A 这种环把递归拖死。
    let meta = std::fs::symlink_metadata(src).map_err(|e| pack_io("读取源元数据失败", e))?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        tracing::warn!(path = %src.display(), "打包跳过符号链接");
        return Ok(());
    }

    if meta.is_dir() {
        let mut header = Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(EntryType::Directory);
        // 目录条目名以 / 结尾是 tar 约定；append_data 内部会写好 path 与 cksum。
        builder
            .append_data(&mut header, format!("{name}/"), std::io::empty())
            .map_err(|e| pack_io("写入目录条目失败", e))?;

        let entries = std::fs::read_dir(src).map_err(|e| pack_io("读取目录失败", e))?;
        for entry in entries {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            let entry = entry.map_err(|e| pack_io("遍历目录失败", e))?;
            let child = entry.file_name().to_string_lossy().to_string();
            // 单段名字也要校验：文件名理论上不能含 '/'，防的是畸形/恶意名字。
            validate_archive_path(&child)?;
            let rel = format!("{name}/{child}");
            add_path(builder, &entry.path(), &rel, depth + 1, total, cancel)?;
        }
    } else if meta.is_file() {
        *total = accumulate_size(*total, meta.len(), MAX_PACK_BYTES, "打包内容")?;
        let mut header = Header::new_gnu();
        header.set_size(meta.len());
        header.set_mode(0o644);
        let mut file = std::fs::File::open(src).map_err(|e| pack_io("打开源文件失败", e))?;
        builder
            .append_data(&mut header, name, &mut file)
            .map_err(|e| pack_io("写入文件条目失败", e))?;
    } else {
        // fifo / socket / 设备文件：跳过，别让 tar 卡在阻塞读上
        tracing::warn!(path = %src.display(), "打包跳过特殊文件");
    }
    Ok(())
}

/// 解包 `.tar.gz` 到 `dst_dir`，逐条校验防止目录穿越与符号链接落地。
pub fn extract_archive(src: &Path, dst_dir: &Path, cancel: &CancellationToken) -> AppResult<()> {
    let file = std::fs::File::open(src).map_err(|e| pack_io("打开临时归档失败", e))?;
    let gz = GzDecoder::new(file);
    let mut archive = Archive::new(gz);
    std::fs::create_dir_all(dst_dir).map_err(|e| pack_io("创建解包目录失败", e))?;

    let mut total: u64 = 0;
    for entry in archive
        .entries()
        .map_err(|e| pack_io("读取归档条目失败", e))?
    {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let mut entry = entry.map_err(|e| pack_io("解析归档条目失败", e))?;
        let entry_path = entry
            .path()
            .map_err(|e| pack_io("读取条目名字失败", e))?
            .into_owned();
        validate_entry_path(&entry_path)?;

        let kind = entry.header().entry_type();
        // 符号链接/硬链接条目一律拒绝：否则一条链接就能把写入重定向到
        // dst_dir 之外，这正是 Zip Slip 最经典的落地方。
        if kind.is_symlink() || kind.is_hard_link() {
            return Err(AppError::validation(
                "归档含符号链接/硬链接条目，已拒绝解包",
            ));
        }
        // pax/gnu 扩展头等 tar crate 通常已内部消化；真漏出来也只跳过，
        // 不当文件落地（但它们不该单独出现）。
        if !(kind.is_dir() || kind.is_file()) {
            continue;
        }

        let size = entry
            .header()
            .size()
            .map_err(|e| pack_io("读取条目大小失败", e))?;
        total = accumulate_size(total, size, MAX_EXTRACT_BYTES, "解包内容")?;

        let target = dst_dir.join(&entry_path);
        if kind.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| pack_io("创建解包子目录失败", e))?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| pack_io("创建解包父目录失败", e))?;
            }
            let mut out =
                std::fs::File::create(&target).map_err(|e| pack_io("创建解包文件失败", e))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| pack_io("写入解包文件失败", e))?;
        }
    }
    Ok(())
}

fn pack_io(context: &str, err: std::io::Error) -> AppError {
    AppError::with_detail(ErrCode::SftpIoError, format!("{context}: {err}"))
}

fn cancelled() -> AppError {
    AppError::new(ErrCode::SftpCancelled)
}

// ---------------------------------------------------------------------------
// 进度上报：把一个 ProgressFn 拆成多个阶段共享的句柄
// ---------------------------------------------------------------------------

/// 打包任务分「打包 / 传输 / 解包」几段，但进度事件只有一条 `ProgressFn`。
///
/// 用 `Arc<Mutex<..>>` 包住底层 emit，派生出多个 `ProgressFn`：
/// 一个交给 `transfer::upload/download` 报字节进度，其余用来发阶段文本。
/// 临界区只是一次同步的闭包调用，绝不跨 await 持锁。
struct Reporter {
    inner: Arc<Mutex<ProgressFn>>,
}

impl Reporter {
    fn new(emit: ProgressFn) -> Self {
        Self {
            inner: Arc::new(Mutex::new(emit)),
        }
    }

    fn emit(&self, progress: TransferProgress) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (guard)(progress);
    }

    /// 给 `transfer::*` 用的句柄。
    ///
    /// `transfer::upload` 传完会自己发一条 `done`，可打包任务后面还有解包步骤；
    /// 这里把它的所有终态降级成 `running`，由整体流程统一发唯一的终态，
    /// 免得进度条先显示 done、解包又失败再跳成 failed。
    fn handle(&self) -> ProgressFn {
        let shared = Arc::clone(&self.inner);
        Box::new(move |progress| {
            let progress = if progress.state != "running" {
                TransferProgress {
                    state: "running",
                    error: None,
                    ..progress
                }
            } else {
                progress
            };
            let mut guard = shared
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (guard)(progress);
        })
    }
}

/// 发一条阶段进度。`name` 借现有字段表达「打包中 / 上传中 / 解包中」。
fn stage(reporter: &Reporter, tid: &str, direction: &'static str, name: &str) {
    reporter.emit(TransferProgress {
        transfer_id: tid.to_string(),
        direction,
        name: name.to_string(),
        transferred: 0,
        total: 0,
        bps: 0,
        state: "running",
        error: None,
    });
}

/// 整个任务唯一的终态事件：成功 done、取消 canceled、其余 failed。
fn terminal(reporter: &Reporter, tid: &str, direction: &'static str, result: &AppResult<u64>) {
    let (state, error) = match result {
        Ok(_) => ("done", None),
        Err(err) => {
            let state = if err.code == ErrCode::SftpCancelled {
                "canceled"
            } else {
                "failed"
            };
            let message = err
                .detail
                .clone()
                .unwrap_or_else(|| err.message.to_string());
            (state, Some(message))
        }
    };
    reporter.emit(TransferProgress {
        transfer_id: tid.to_string(),
        direction,
        name: (if state == "done" {
            "已完成"
        } else {
            "打包传输"
        })
        .to_string(),
        transferred: 0,
        total: 0,
        bps: 0,
        state,
        error,
    });
}

// ---------------------------------------------------------------------------
// 参数校验（command 层前置调用，避免给必然失败的输入发 transferId）
// ---------------------------------------------------------------------------

/// 逐个校验本地源：非空、绝对路径、存在、不是应用数据目录。
///
/// 注意 `transfer::ensure_upload_source` 会**拒绝目录**，而打包上传本来就允许目录，
/// 所以目录只复用它的「受保护目录」判定，文件才整体调用该函数。
pub fn validate_sources(sources: &[PathBuf], protected: &[&Path]) -> AppResult<()> {
    if sources.is_empty() {
        return Err(AppError::validation("打包上传至少需要选择一个文件或目录"));
    }
    for src in sources {
        check_source(src, protected)?;
    }
    Ok(())
}

fn check_source(path: &Path, protected: &[&Path]) -> AppResult<()> {
    if !path.is_absolute() {
        return Err(AppError::validation("本地路径必须是绝对路径"));
    }
    let real =
        std::fs::canonicalize(path).map_err(|_| AppError::validation("源路径不存在或无法访问"))?;

    // 应用自己的数据目录（加密凭据、主机指纹）一律不许外传。
    for dir in protected {
        let Ok(real_dir) = std::fs::canonicalize(dir) else {
            continue;
        };
        if real.starts_with(&real_dir) {
            return Err(AppError::validation(
                "不允许上传应用自己的数据目录（内含凭据）",
            ));
        }
    }

    if real.is_file() {
        transfer::ensure_upload_source(path, protected)?;
    } else if !real.is_dir() {
        return Err(AppError::validation("源路径既不是文件也不是目录"));
    }
    Ok(())
}

/// 下载的前置校验：至少选一个远端路径、目标目录非空且为绝对路径。
pub fn validate_download_request(remote_paths: &[String], local_dir: &str) -> AppResult<()> {
    if remote_paths.is_empty() {
        return Err(AppError::validation("打包下载至少需要选择一个远端路径"));
    }
    if local_dir.trim().is_empty() {
        return Err(AppError::validation("本机目标目录不能为空"));
    }
    if !Path::new(local_dir).is_absolute() {
        return Err(AppError::validation("本机目标目录必须是绝对路径"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 临时路径（UUID v7，绝不用时间戳后缀）
// ---------------------------------------------------------------------------

fn local_tmp_path() -> PathBuf {
    transfer::local_join(
        &std::env::temp_dir(),
        &format!("{TMP_PREFIX}{}.tar.gz", Uuid::now_v7()),
    )
}

fn upload_tmp_remote(remote_dir: &str) -> String {
    transfer::remote_join(
        remote_dir,
        &format!("{TMP_PREFIX}{}.tar.gz", Uuid::now_v7()),
    )
}

fn download_tmp_remote() -> String {
    format!("/tmp/{TMP_PREFIX}{}.tar.gz", Uuid::now_v7())
}

// ---------------------------------------------------------------------------
// 整体流程（async，依赖 SFTP 会话；单测不覆盖这一段）
// ---------------------------------------------------------------------------

/// 打包上传：本地打包 → 上传临时包 → 远端解包 → 清理。
pub async fn upload(
    conn: &SftpConn,
    sources: Vec<PathBuf>,
    remote_dir: String,
    protected: Vec<PathBuf>,
    transfer_id: String,
    cancel: CancellationToken,
    emit: ProgressFn,
) -> AppResult<u64> {
    let reporter = Reporter::new(emit);
    let result = upload_inner(
        conn,
        &sources,
        &remote_dir,
        &protected,
        &transfer_id,
        &cancel,
        &reporter,
    )
    .await;
    terminal(&reporter, &transfer_id, "upload", &result);
    result
}

async fn upload_inner(
    conn: &SftpConn,
    sources: &[PathBuf],
    remote_dir: &str,
    protected: &[PathBuf],
    tid: &str,
    cancel: &CancellationToken,
    reporter: &Reporter,
) -> AppResult<u64> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    if remote_dir.trim().is_empty() {
        return Err(AppError::validation("远端目标目录不能为空"));
    }
    let refs: Vec<&Path> = protected.iter().map(|p| p.as_path()).collect();
    validate_sources(sources, &refs)?;

    let tmp_local = local_tmp_path();
    let tmp_remote = upload_tmp_remote(remote_dir);

    stage(reporter, tid, "upload", "正在打包…");
    {
        let src = sources.to_vec();
        let dst = tmp_local.clone();
        let token = cancel.clone();
        let outcome = tokio::task::spawn_blocking(move || create_archive(&src, &dst, &token))
            .await
            .map_err(|e| AppError::internal(format!("打包任务异常退出: {e}")))?;
        outcome?;
    }

    // 从这一刻起，无论成功失败都要删本地临时包；用内联 async 块把主流程收进一个出口。
    let result: AppResult<u64> = async {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        stage(reporter, tid, "upload", "正在上传…");
        let bytes = transfer::upload(
            &conn.session,
            &tmp_local,
            &tmp_remote,
            tid.to_string(),
            cancel.clone(),
            reporter.handle(),
            &refs,
        )
        .await?;

        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        stage(reporter, tid, "upload", "正在解包…");
        let cmd = build_remote_unpack_command(&tmp_remote, remote_dir);
        let output = conn.exec(&cmd, EXEC_TIMEOUT).await?;
        inspect_exec_output(&output, "解包")?;
        Ok(bytes)
    }
    .await;

    // 本地临时包无条件清理。
    if let Err(err) = std::fs::remove_file(&tmp_local) {
        tracing::warn!(error = %err, path = %tmp_local.display(), "清理本地临时包失败");
    }
    // 失败时远端可能残留 .part / 临时包（解包命令成功时已自带 rm -f），尽力再删一次。
    if result.is_err() {
        let cmd = build_remote_cleanup_command(&tmp_remote);
        let _ = conn.exec(&cmd, CLEANUP_TIMEOUT).await;
    }
    result
}

/// 打包下载：远端打包 → 下载临时包 → 本地解包 → 清理两端临时包。
pub async fn download(
    conn: &SftpConn,
    remote_paths: Vec<String>,
    local_dir: PathBuf,
    transfer_id: String,
    cancel: CancellationToken,
    emit: ProgressFn,
) -> AppResult<u64> {
    let reporter = Reporter::new(emit);
    let result = download_inner(
        conn,
        &remote_paths,
        &local_dir,
        &transfer_id,
        &cancel,
        &reporter,
    )
    .await;
    terminal(&reporter, &transfer_id, "download", &result);
    result
}

async fn download_inner(
    conn: &SftpConn,
    remote_paths: &[String],
    local_dir: &Path,
    tid: &str,
    cancel: &CancellationToken,
    reporter: &Reporter,
) -> AppResult<u64> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    if remote_paths.is_empty() {
        return Err(AppError::validation("打包下载至少需要选择一个远端路径"));
    }

    // 目录部分 / 名字部分分别转义，交给 build_remote_pack_command 逐段 quote。
    let mut parts: Vec<(String, String)> = Vec::with_capacity(remote_paths.len());
    for path in remote_paths {
        parts.push(split_remote_path(path)?);
    }

    let tmp_local = local_tmp_path();
    let tmp_remote = download_tmp_remote();

    let result: AppResult<u64> = async {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        stage(reporter, tid, "download", "正在让服务器打包…");
        let cmd = build_remote_pack_command(&tmp_remote, &parts);
        let output = conn.exec(&cmd, EXEC_TIMEOUT).await?;
        inspect_exec_output(&output, "打包下载")?;

        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        stage(reporter, tid, "download", "正在下载…");
        let bytes = transfer::download(
            &conn.session,
            &tmp_remote,
            &tmp_local,
            tid.to_string(),
            cancel.clone(),
            reporter.handle(),
        )
        .await?;

        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        stage(reporter, tid, "download", "正在解包…");
        let src = tmp_local.clone();
        let dst = local_dir.to_path_buf();
        let token = cancel.clone();
        let outcome = tokio::task::spawn_blocking(move || extract_archive(&src, &dst, &token))
            .await
            .map_err(|e| AppError::internal(format!("解包任务异常退出: {e}")))?;
        outcome?;
        Ok(bytes)
    }
    .await;

    // 本地临时包无条件清理。
    if let Err(err) = std::fs::remove_file(&tmp_local) {
        tracing::warn!(error = %err, path = %tmp_local.display(), "清理本地临时包失败");
    }
    // 远端临时包跨越了两次 exec，必须单独删；失败只记日志，不掩盖主结果。
    let cmd = build_remote_cleanup_command(&tmp_remote);
    let _ = conn.exec(&cmd, CLEANUP_TIMEOUT).await;

    result
}

// ---------------------------------------------------------------------------
// 单测：纯本地、不联网、不依赖 Tauri 窗口
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 唯一临时目录（项目规定：不用时间戳后缀），Drop 即清理。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = crate::ssh::test_support::unique_path(&format!("pack-{tag}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn no_cancel() -> CancellationToken {
        CancellationToken::new()
    }

    /// 1. 打包→解包 round-trip，目录结构与文件内容都保住。
    #[test]
    fn roundtrip_preserves_tree() {
        let dir = TempDir::new("roundtrip");
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("sub/deep")).unwrap();
        std::fs::write(src.join("a.txt"), b"AAA").unwrap();
        std::fs::write(src.join("sub/b.txt"), b"BBB").unwrap();
        std::fs::write(src.join("sub/deep/c.txt"), b"CCC").unwrap();

        let archive = dir.join("bundle.tar.gz");
        let out = dir.join("out");
        create_archive(std::slice::from_ref(&src), &archive, &no_cancel()).unwrap();
        extract_archive(&archive, &out, &no_cancel()).unwrap();

        assert_eq!(std::fs::read(out.join("src/a.txt")).unwrap(), b"AAA");
        assert_eq!(std::fs::read(out.join("src/sub/b.txt")).unwrap(), b"BBB");
        assert_eq!(
            std::fs::read(out.join("src/sub/deep/c.txt")).unwrap(),
            b"CCC"
        );
        assert!(out.join("src/sub/deep").is_dir(), "目录结构必须保留");
    }

    /// 2. 归档内路径只放相对名：不带本机绝对路径、不以 / 开头、不含 ..
    #[test]
    fn archive_entries_never_carry_absolute_paths() {
        let dir = TempDir::new("relative");
        let src = dir.join("project");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("nested/file.txt"), b"x").unwrap();
        let archive = dir.join("bundle.tar.gz");
        create_archive(std::slice::from_ref(&src), &archive, &no_cancel()).unwrap();

        let file = std::fs::File::open(&archive).unwrap();
        let mut tar = Archive::new(GzDecoder::new(file));
        let mut seen = 0;
        for entry in tar.entries().unwrap() {
            let entry = entry.unwrap();
            let text = entry.path().unwrap().to_string_lossy().to_string();
            assert!(!text.starts_with('/'), "条目带绝对路径: {text}");
            assert!(!text.contains(".."), "条目含 ..: {text}");
            assert!(
                !text.contains("termharbor-pack-relative"),
                "条目混入了临时目录名: {text}"
            );
            assert!(validate_archive_path(&text).is_ok());
            seen += 1;
        }
        assert!(seen >= 2, "应当至少有目录和文件两个条目");
    }

    /// 3. 解包逐条拒绝 .. 穿越 / 绝对路径（Zip Slip 的核心防线）。
    #[test]
    fn validate_entry_path_rejects_traversal() {
        assert!(validate_entry_path(Path::new("../evil.txt")).is_err());
        assert!(validate_entry_path(Path::new("a/../../etc/passwd")).is_err());
        assert!(validate_entry_path(Path::new("/etc/passwd")).is_err());
        assert!(validate_entry_path(Path::new("C:/Windows/win.ini")).is_err());
        assert!(
            validate_archive_path("a\\..\\b").is_err(),
            "Windows 反斜杠穿越也要挡"
        );
        assert!(validate_entry_path(Path::new("a/b/c.txt")).is_ok());
    }

    /// 4. shell_quote 覆盖单引号、空格、$、反引号、换行、反斜杠。
    #[test]
    fn shell_quote_handles_dangerous_characters() {
        assert_eq!(shell_quote("simple"), "'simple'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_quote("`whoami`"), "'`whoami`'");
        assert_eq!(shell_quote("a\nb"), "'a\nb'");
        assert_eq!(shell_quote("a\\b"), "'a\\b'");
        // 串里的单引号被拆成 闭引号 + 转义引号 + 开引号，shell 解析后还原成原文
        assert_eq!(shell_quote("a'b$c`d"), r"'a'\''b$c`d'");
    }

    /// 5. 归档条目名提取：Windows 反斜杠路径只取最后一段。
    #[test]
    fn entry_name_handles_windows_backslashes() {
        assert_eq!(
            entry_name(Path::new(r"C:\Users\name\file.txt")).unwrap(),
            "file.txt"
        );
        assert_eq!(entry_name(Path::new(r"C:\Users\name\dir\")).unwrap(), "dir");
        assert_eq!(entry_name(Path::new("/home/u/data")).unwrap(), "data");
        assert!(
            entry_name(Path::new("/home/u/..")).is_err(),
            ".. 不能当名字"
        );
        assert!(entry_name(Path::new("/")).is_err());
    }

    /// 6. 命令拼装：所有用户路径都经 shell_quote，危险字符不会裸拼进命令。
    #[test]
    fn remote_commands_quote_every_user_path() {
        let evil = "/tmp/a b/it's/$HOME/`id`.tar.gz";
        let q = shell_quote(evil);
        let cmd = build_remote_unpack_command(evil, "/dst dir/it's");
        assert!(cmd.contains(&q), "临时包路径必须被 quote 后出现在命令里");
        assert!(!cmd.contains(evil), "绝不能出现未转义的原始路径");
        assert!(cmd.contains("rm -f"), "失败也要清理临时包");
        assert!(cmd.contains("exit $__th_rc"), "要透传退出码");

        let pack = build_remote_pack_command(evil, &[("/a b".to_string(), "it's".to_string())]);
        assert!(pack.contains(&shell_quote("/a b")));
        assert!(pack.contains(&shell_quote("it's")));
        assert!(!pack.contains("/a b/it's"), "原始目录串不能裸出现");

        let cleanup = build_remote_cleanup_command(evil);
        assert!(cleanup.contains(&q));
        assert!(cleanup.contains(&shell_quote(&format!("{evil}.part"))));
    }

    /// 7. 大小上限：抽出的累计函数能挡住超过 512MB / 1GB 的输入与溢出。
    #[test]
    fn size_limits_are_enforced() {
        assert_eq!(
            accumulate_size(100, 50, MAX_PACK_BYTES, "打包内容").unwrap(),
            150
        );
        assert!(accumulate_size(MAX_PACK_BYTES, 1, MAX_PACK_BYTES, "打包内容").is_err());
        assert!(accumulate_size(MAX_EXTRACT_BYTES, 1, MAX_EXTRACT_BYTES, "解包内容").is_err());
        assert!(
            accumulate_size(u64::MAX, 1, MAX_PACK_BYTES, "打包内容").is_err(),
            "溢出也要挡"
        );
    }

    /// 8. 符号链接在打包时被跳过（不跟随、不进归档）。
    ///
    /// Windows 非开发者模式建不了符号链接，这种环境下降级为不测，不算失败。
    #[cfg(windows)]
    fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[cfg(unix)]
    fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[test]
    fn symlinks_are_skipped_when_creatable() {
        let dir = TempDir::new("symlink");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("real.txt"), b"real").unwrap();
        let outside = dir.join("outside.txt");
        std::fs::write(&outside, b"SECRET").unwrap();
        let link = src.join("link.txt");
        if make_symlink(&outside, &link).is_err() {
            return;
        }

        let archive = dir.join("bundle.tar.gz");
        create_archive(std::slice::from_ref(&src), &archive, &no_cancel()).unwrap();

        let file = std::fs::File::open(&archive).unwrap();
        let mut tar = Archive::new(GzDecoder::new(file));
        for entry in tar.entries().unwrap() {
            let entry = entry.unwrap();
            let text = entry.path().unwrap().to_string_lossy().to_string();
            assert!(!text.ends_with("link.txt"), "符号链接不该进归档: {text}");
        }
    }

    /// 9. 远端 exec 输出判定：成功标记 / 缺 tar / 失败迹象 / 兜底都不能静默成功。
    #[test]
    fn exec_output_never_treats_failure_as_success() {
        assert!(inspect_exec_output("__TERMHARBOR_PACK_OK__\n", "解包").is_ok());
        let missing = inspect_exec_output("sh: tar: command not found\n", "打包下载").unwrap_err();
        assert!(missing
            .detail
            .unwrap_or_default()
            .contains("服务器缺少 tar 命令，无法打包下载"));
        assert!(inspect_exec_output("tar: Unexpected EOF\n", "解包").is_err());
        assert!(
            inspect_exec_output("", "解包").is_err(),
            "空输出也必须判失败"
        );
    }
}
