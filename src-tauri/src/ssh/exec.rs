//! 一次性命令执行（诊断面板用）。
//!
//! 在**同一条 SSH 连接**上开一个独立 channel 执行命令，读完就关。
//! 不复用终端的 PTY：那样命令输出会混进用户的终端画面，还会踩乱提示符。
//!
//! 注意这里不收 PTY、不做增量解码 —— exec 通道的输出是纯字节流，
//! 末尾按 UTF-8 整体解码即可（诊断命令的输出不会像 `cat` 那样被切在半个汉字上）。

use std::time::Duration;

use russh::client::Handle;
use russh::ChannelMsg;

use super::client::ProbeHandler;
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
