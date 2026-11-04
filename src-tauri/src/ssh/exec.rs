//! 一次性命令执行（诊断面板用）。
//!
//! 在**同一条 SSH 连接**上开一个独立 channel 执行命令，读完就关。
//! 不复用终端的 PTY：那样命令输出会混进用户的终端画面，还会踩乱提示符。
//!
//! 注意这里不收 PTY、不做增量解码 —— exec 通道的输出是纯字节流，
//! 末尾按 UTF-8 整体解码即可（诊断命令的输出不会像 `cat` 那样被切在半个汉字上）。

use std::time::Duration;

use russh::client::Handle;
use russh::{ChannelMsg, Disconnect};

use super::client::{establish, ProbeHandler, ProbeRequest, ProbeResult, Purpose};
use crate::error::{AppError, AppResult, ErrCode};

/// 诊断命令都是秒级的；超过这个时间说明服务器卡了，直接放弃。
const EXEC_TIMEOUT: Duration = Duration::from_secs(12);

/// 输出上限。防止有人在符号链接上跑出几百 MB 把内存打爆。
const MAX_OUTPUT: usize = 256 * 1024;

/// 在连接上执行一条命令，返回合并后的 stdout + stderr。
pub(crate) async fn exec_once(conn: &Handle<ProbeHandler>, command: &str) -> AppResult<String> {
    let mut channel = conn
        .channel_open_session()
        .await
        .map_err(|err| AppError::with_detail(ErrCode::SshDisconnected, err.to_string()))?;

    if let Err(err) = channel.exec(true, command).await {
        let _ = channel.close().await;
        return Err(AppError::with_detail(
            ErrCode::SshDisconnected,
            err.to_string(),
        ));
    }

    let collected = tokio::time::timeout(EXEC_TIMEOUT, async {
        let mut buffer: Vec<u8> = Vec::new();
        let mut truncated = false;

        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    if buffer.len() + data.len() > MAX_OUTPUT {
                        truncated = true;
                        break;
                    }
                    buffer.extend_from_slice(&data);
                }
                // stderr 也要收：「command not found」「Permission denied」都在这里
                Some(ChannelMsg::ExtendedData { data, .. })
                    if buffer.len() + data.len() <= MAX_OUTPUT =>
                {
                    buffer.extend_from_slice(&data);
                }
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
        }

        (buffer, truncated)
    })
    .await;

    let _ = channel.close().await;

    let (buffer, truncated) = collected.map_err(|_| {
        AppError::with_detail(
            ErrCode::SshTcpTimeout,
            format!("命令执行超时（>{EXEC_TIMEOUT:?}）: {command}"),
        )
    })?;

    if truncated {
        tracing::warn!(command, "诊断命令输出超过上限，已截断");
    }

    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// 建一条临时连接、跑一条命令、断开。
///
/// GUI 的诊断复用终端会话的连接，MCP 没有长期会话，所以每次新开一条、用完即关。
/// 代价是多一次握手（内网十几毫秒），换来的是不必为 MCP 维护会话池和超时回收。
pub(crate) async fn exec_on_fresh_connection(
    request: ProbeRequest,
    command: &str,
) -> AppResult<String> {
    let established = establish(&request, Purpose::Session)
        .await
        .map_err(probe_failure_to_error)?;

    let result = exec_once(&established.handle, command).await;
    let _ = established
        .handle
        .disconnect(Disconnect::ByApplication, "", "en")
        .await;
    result
}

/// 把探活的各种失败分支折成一句能给人看的错误。
fn probe_failure_to_error(result: ProbeResult) -> AppError {
    match result {
        ProbeResult::Failed(err) => err,
        // 走到这里说明 known_hosts 策略拦下了这条连接（指纹未确认 / 已变更）。
        // MCP 没有弹窗渠道，只能要求用户先去 GUI 里确认一次 —— 这比悄悄放行安全。
        ProbeResult::NeedsConfirmation { fingerprint, .. } => AppError::with_detail(
            ErrCode::SshHostKeyUnknown,
            format!("这台服务器还没确认过主机指纹（{fingerprint}），请先在图形界面里连接一次"),
        ),
        ProbeResult::HostKeyChanged { stored, .. } => AppError::with_detail(
            ErrCode::SshHostKeyMismatch,
            format!("主机密钥已变更，记录的是 {stored}，已阻断"),
        ),
        ProbeResult::CertificateUnsupported => {
            AppError::with_detail(ErrCode::SshAuthFailed, "服务器只提供证书认证，暂不支持")
        }
        ProbeResult::Connected(_) => AppError::internal("establish 返回了 Connected，不符合预期"),
    }
}
