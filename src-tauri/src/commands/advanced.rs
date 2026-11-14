//! 「高级监控」的 Tauri command 入口。
//!
//! 纪律与 `commands/diag.rs` 一致：这一层只做参数校验、取会话连接句柄、转发给
//! `monitor::advanced` 解析。所有业务逻辑都在领域模块里，能用纯字符串单测覆盖，
//! 不依赖 Tauri 窗口，也不在这里碰 PTY。

use std::time::Duration;

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::monitor::advanced::{self, AdvancedSnapshot};
use crate::state::AppState;

/// 一次 exec 采全所有指标。
///
/// 命令是几十段拼接的长文本，输出也比诊断面板大，所以超时与输出上限都给到
/// 20 秒 / 2MB（诊断默认 12 秒 / 256KB 不够用）。exec_once_limited 已经把
/// stdout 与 stderr 合并，工具缺失的报错也只会变成空段，由 parse 降级处理。
#[tauri::command]
pub async fn advanced_snapshot(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<AdvancedSnapshot> {
    let conn = state.sessions.connection(&session_id)?;
    let raw = crate::ssh::exec::exec_once_limited(
        &conn,
        advanced::SAMPLE_COMMAND,
        Duration::from_secs(20),
        2 * 1024 * 1024,
    )
    .await?;
    Ok(advanced::parse(&raw))
}

/// 给远端进程发 TERM，返回远端 stdout/stderr 合并文本。
///
/// pid 是 `u32`，`format!` 只会拼进数字，不可能带入 shell 元字符，杜绝注入。
/// 不做 sudo 提权：提权需要交互密码，exec 通道给不了，硬做只会把密码写进命令行。
#[tauri::command]
pub async fn advanced_kill(
    state: State<'_, AppState>,
    session_id: String,
    pid: u32,
) -> AppResult<String> {
    // pid 0 是「进程组」语义，杀掉会误伤整个会话，直接拒绝。
    if pid == 0 {
        return Err(AppError::validation("pid 必须大于 0"));
    }

    let conn = state.sessions.connection(&session_id)?;
    let command = format!("kill -TERM {pid}");
    crate::ssh::exec::exec_once_limited(&conn, &command, Duration::from_secs(20), 2 * 1024 * 1024)
        .await
}

/// 远端目录补全候选。
///
/// prefix 拆成目录部分 + 文件名前缀，列远端目录后过滤、按 limit 截断。
/// 目录交给 shell 前必须经 `shell_quote` 单引号转义并加 `--`，保证空格、单引号、
/// `$()` 这类字符都只当字面路径处理。
#[tauri::command]
pub async fn path_complete(
    state: State<'_, AppState>,
    session_id: String,
    prefix: String,
    limit: Option<u32>,
) -> AppResult<Vec<String>> {
    // 换行会把 `ls -1` 的一行一条输出撕成两行，还会让 `@@` 分段解析错位，
    // 因此在拼命令之前直接拒绝，不放进远端 shell。
    if prefix.contains('\n') || prefix.contains('\r') || prefix.contains('\0') {
        return Err(AppError::validation("路径里不允许出现换行或空字符"));
    }

    let (dir_display, file_part) = advanced::split_path_prefix(&prefix);
    // 没有目录部分时（例如输入 "etc"）列当前目录；展示用前缀保持为空，
    // 这样返回的候选仍是 "etc/" 这种相对形式，和用户输入一致。
    let ls_dir = if dir_display.is_empty() {
        "./"
    } else {
        dir_display.as_str()
    };
    let command = advanced::build_ls_command(ls_dir);

    let conn = state.sessions.connection(&session_id)?;
    let raw = crate::ssh::exec::exec_once_limited(
        &conn,
        &command,
        Duration::from_secs(20),
        2 * 1024 * 1024,
    )
    .await?;

    // 默认 50 条。夹在 1..=2000 之间：limit=0 不该直接返回空列表，
    // 一次补全也不该拉回几千个候选把前端卡住。
    let cap = limit.unwrap_or(50).clamp(1, 2000) as usize;
    Ok(advanced::complete_candidates(
        &dir_display,
        &file_part,
        &raw,
        cap,
    ))
}
