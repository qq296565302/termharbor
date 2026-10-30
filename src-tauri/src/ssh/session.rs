//! 交互式 SSH 会话。
//!
//! 每个会话一个 Tokio task，独占自己的 SSH 连接与 channel；会话之间互不影响（约束 A-10）。
//!
//! 数据流：
//! ```text
//! russh Channel.wait()
//!   → Utf8StreamDecoder（修分包切断的多字节字符）
//!   → 16ms / 32KB 聚合
//!   → Tauri Channel<OutputBatch>
//!   → xterm.write()
//! ```
//!
//! 背压：前端每消费一段就 `session_ack` 回一个字节数，未确认量超过高水位时
//! 就**停止调用 `channel.wait()`**，压力会自然回传到 TCP。这是 S2 Spike 验证过的做法。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::Handle;
use russh::{ChannelMsg, Disconnect};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use super::client::{establish, ProbeHandler, ProbeRequest, ProbeResult, Purpose};
use super::decoder::Utf8StreamDecoder;
use super::hostkey::HostKeyDecision;
use crate::error::{AppError, AppResult, ErrCode};
use crate::events::OutputBatch;

/// 会话生命周期事件。
///
/// 走回调而不是直接 `AppHandle::emit`，是为了让 ssh 层不依赖 Tauri。
/// 否则 `cargo test --lib` 会把整个窗口层（tao/wry）链进测试二进制，
/// 而测试程序没有 comctl32 v6 清单，加载期就会因为 `TaskDialogIndirect`
/// 找不到入口点直接挂掉（0xc0000139）。这个坑排查了很久，别再引回来。
#[derive(Debug, Clone)]
pub enum SessionEvent {
    Disconnected { session_id: String, reason: String },
}

pub type EventFn = Box<dyn FnMut(SessionEvent) + Send + 'static>;

/// 输出回调。返回 false 表示下游已关闭，会话应当结束。
///
/// 用回调而不是直接吃 `tauri::ipc::Channel`，是为了让 ssh 层不依赖 Tauri 类型 ——
/// 集成测试里可以塞一个收集器进去，真正验证解码 → 聚合 → 推送这条链路。
pub type EmitFn = Box<dyn FnMut(OutputBatch) -> bool + Send + 'static>;

/// 单批最大字节数
pub const BATCH_MAX_BYTES: usize = 32 * 1024;
/// 最长攒多久就flush一次
pub const BATCH_INTERVAL: Duration = Duration::from_millis(16);
/// 未确认字节超过这个量就暂停读取
const HIGH_WATER: u64 = 2 * 1024 * 1024;
const TERM_NAME: &str = "xterm-256color";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub connection_id: String,
    pub title: String,
    pub host_label: String,
    pub cols: u32,
    pub rows: u32,
    pub created_at: u64,
}

pub struct OpenedSession {
    pub info: SessionInfo,
    /// 首次连接且用户刚确认过的主机密钥，调用方需要落库
    pub newly_trusted: Option<(String, String)>,
}

pub struct OpenRequest {
    pub session_id: String,
    pub connection_id: String,
    pub title: String,
    pub host_label: String,
    pub cols: u32,
    pub rows: u32,
    pub probe: ProbeRequest,
}

enum Input {
    Data(Vec<u8>),
    Resize { cols: u32, rows: u32 },
    Close,
}

struct SessionHandle {
    info: SessionInfo,
    input: mpsc::Sender<Input>,
    /// 与 pump 共享的连接句柄。诊断面板的命令走这条连接的独立 channel，
    /// 不复用 PTY —— 否则命令输出会混进终端画面。
    conn: Arc<Handle<ProbeHandler>>,
    /// 已推给前端但还没被 ack 的字节数
    unacked: Arc<AtomicU64>,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
pub struct SessionManager {
    sessions: Mutex<HashMap<String, SessionHandle>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionHandle>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn insert(&self, handle: SessionHandle) {
        self.lock().insert(handle.info.session_id.clone(), handle);
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let mut all: Vec<SessionInfo> = self.lock().values().map(|h| h.info.clone()).collect();
        all.sort_by_key(|info| info.created_at);
        all
    }

    pub fn send_input(&self, session_id: &str, data: Vec<u8>) -> AppResult<()> {
        let sender = self.sender(session_id)?;
        sender
            .try_send(Input::Data(data))
            .map_err(|_| AppError::new(ErrCode::SshDisconnected))
    }

    pub fn resize(&self, session_id: &str, cols: u32, rows: u32) -> AppResult<()> {
        let sender = self.sender(session_id)?;
        sender
            .try_send(Input::Resize { cols, rows })
            .map_err(|_| AppError::new(ErrCode::SshDisconnected))
    }

    /// 前端消费完后回调，用于解锁背压。
    pub fn ack(&self, session_id: &str, bytes: u64) {
        let guard = self.lock();
        if let Some(handle) = guard.get(session_id) {
            let current = handle.unacked.load(Ordering::Relaxed);
            handle
                .unacked
                .store(current.saturating_sub(bytes), Ordering::Relaxed);
        }
    }

    /// 关闭会话。重复关闭视为成功（幂等），避免前端关标签时还要处理错误。
    pub fn close(&self, session_id: &str) {
        let handle = self.lock().remove(session_id);
        if let Some(handle) = handle {
            // 任务收到 Close 后会自己断开连接并退出；发不进去说明它已经走了
            let _ = handle.input.try_send(Input::Close);
        }
    }

    /// 关掉所有会话，返回关闭数量。幂等。
    ///
    /// 用途：前端整页重载后 sessionId 全丢，但会话还活着 —— 不清就是泄漏。
    /// 先把 id 取出来再逐个关，避免持有锁时发消息。
    pub fn close_all(&self) -> usize {
        let ids: Vec<String> = self.lock().keys().cloned().collect();
        for id in &ids {
            self.close(id);
        }
        ids.len()
    }

    /// 任务退出时自清理，避免 map 里留下死会话。
    fn remove_finished(&self, session_id: &str) {
        self.lock().remove(session_id);
    }

    /// 取会话的连接句柄，供一次性命令执行使用。
    pub(crate) fn connection(&self, session_id: &str) -> AppResult<Arc<Handle<ProbeHandler>>> {
        self.lock()
            .get(session_id)
            .map(|handle| Arc::clone(&handle.conn))
            .ok_or_else(|| {
                AppError::with_detail(ErrCode::SshSessionNotFound, format!("id={session_id}"))
            })
    }

    fn sender(&self, session_id: &str) -> AppResult<mpsc::Sender<Input>> {
        self.lock()
            .get(session_id)
            .map(|handle| handle.input.clone())
            .ok_or_else(|| {
                AppError::with_detail(ErrCode::SshSessionNotFound, format!("id={session_id}"))
            })
    }
}

/// 打开一个交互式会话：握手 → 认证 → PTY → shell。
pub async fn open(
    manager: Arc<SessionManager>,
    req: OpenRequest,
    emit: EmitFn,
    notify: EventFn,
) -> AppResult<OpenedSession> {
    let established = establish(&req.probe, Purpose::Session)
        .await
        .map_err(probe_failure)?;

    let newly_trusted = if established.host_key.decision == HostKeyDecision::AcceptedByUser {
        Some((
            established.host_key.algo.clone(),
            established.host_key.fingerprint.clone(),
        ))
    } else {
        None
    };

    let handle = Arc::new(established.handle);

    let channel = handle
        .channel_open_session()
        .await
        .map_err(|err| AppError::with_detail(ErrCode::SshDisconnected, err.to_string()))?;

    channel
        .request_pty(true, TERM_NAME, req.cols, req.rows, 0, 0, &[])
        .await
        .map_err(|err| AppError::with_detail(ErrCode::SshDisconnected, err.to_string()))?;
    channel
        .request_shell(true)
        .await
        .map_err(|err| AppError::with_detail(ErrCode::SshDisconnected, err.to_string()))?;

    let info = SessionInfo {
        session_id: req.session_id.clone(),
        connection_id: req.connection_id,
        title: req.title,
        host_label: req.host_label,
        cols: req.cols,
        rows: req.rows,
        created_at: crate::state::now_millis(),
    };

    let (input_tx, input_rx) = mpsc::channel(512);
    let unacked = Arc::new(AtomicU64::new(0));

    let task_manager = Arc::clone(&manager);
    let task_unacked = Arc::clone(&unacked);
    let task_session_id = info.session_id.clone();

    // 先 clone 再 move：pump 和 SessionHandle 各持一份，谁先结束都不会关掉连接
    let task_conn = Arc::clone(&handle);
    let task = tokio::spawn(async move {
        pump(
            task_manager,
            notify,
            task_session_id,
            task_conn,
            channel,
            input_rx,
            emit,
            task_unacked,
        )
        .await;
    });

    manager.insert(SessionHandle {
        info: info.clone(),
        input: input_tx,
        conn: handle,
        unacked,
        task,
    });

    tracing::info!(
        session = %info.session_id,
        connection = %info.connection_id,
        "会话已建立"
    );
    Ok(OpenedSession {
        info,
        newly_trusted,
    })
}

/// 会话主循环：读输出、收输入、按批次推送。
#[allow(clippy::too_many_arguments)]
async fn pump(
    manager: Arc<SessionManager>,
    mut notify: EventFn,
    session_id: String,
    handle: Arc<Handle<ProbeHandler>>,
    mut channel: russh::Channel<russh::client::Msg>,
    mut input: mpsc::Receiver<Input>,
    mut emit: EmitFn,
    unacked: Arc<AtomicU64>,
) {
    let mut decoder = Utf8StreamDecoder::new();
    let mut pending = String::with_capacity(BATCH_MAX_BYTES);
    let mut seq: u64 = 0;
    let mut ticker = tokio::time::interval(BATCH_INTERVAL);
    // 被背压卡住时不要补发积压的 tick，否则恢复读取会连发好几批
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // 断开原因。None 表示是我们自己收的（关标签），不需要通知前端。
    let mut stop_reason: Option<&str> = None;

    loop {
        let paused = unacked.load(Ordering::Relaxed) >= HIGH_WATER;

        tokio::select! {
            // 被背压时这个分支直接禁用，等价于"不再从 SSH 读取"
            msg = channel.wait(), if !paused => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        pending.push_str(&decoder.push(&data));
                        if pending.len() >= BATCH_MAX_BYTES
                            && !emit_output(&mut *emit, &unacked, &mut seq, &mut pending)
                        {
                            break;
                        }
                    }
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        pending.push_str(&decoder.push(&data));
                        if pending.len() >= BATCH_MAX_BYTES
                            && !emit_output(&mut *emit, &unacked, &mut seq, &mut pending)
                        {
                            break;
                        }
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) => {
                        stop_reason = Some("远程 shell 已结束");
                        break;
                    }
                    None => {
                        // 通道没了：可能是服务端把整条连接断了，也可能只是这个 channel 关了
                        stop_reason = Some(if handle.is_closed() {
                            "连接已断开"
                        } else {
                            "会话通道已关闭"
                        });
                        break;
                    }
                    _ => {}
                }
            }
            Some(cmd) = input.recv() => {
                match cmd {
                    Input::Data(bytes) => {
                        if channel.data_bytes(bytes).await.is_err() {
                            stop_reason = Some("写入失败，连接已断开");
                            break;
                        }
                    }
                    Input::Resize { cols, rows } => {
                        // 漏了这一步，vim/top 会按旧尺寸画，看起来就是"显示错乱"
                        let _ = channel.window_change(cols, rows, 0, 0).await;
                    }
                    Input::Close => break,
                }
            }
            _ = ticker.tick() => {
                if !emit_output(&mut *emit, &unacked, &mut seq, &mut pending) {
                    break;
                }
            }
        }
    }

    // 收尾：把攒着的最后一点吐出去
    emit_output(&mut *emit, &unacked, &mut seq, &mut pending);

    let _ = channel.eof().await;
    let _ = channel.close().await;
    let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;

    manager.remove_finished(&session_id);

    tracing::info!(
        session = %session_id,
        invalid_utf8 = decoder.invalid_bytes(),
        "会话已结束"
    );

    if let Some(reason) = stop_reason {
        notify(SessionEvent::Disconnected {
            session_id,
            reason: reason.to_string(),
        });
    }
}

/// 推一批给前端。返回 false 表示通道已关闭，调用方应当退出。
fn emit_output(
    emit: &mut (dyn FnMut(OutputBatch) -> bool + Send),
    unacked: &AtomicU64,
    seq: &mut u64,
    pending: &mut String,
) -> bool {
    if pending.is_empty() {
        return true;
    }
    let data = std::mem::take(pending);
    let len = data.len() as u64;
    *seq += 1;

    if emit(OutputBatch {
        seq: *seq,
        bytes: len as u32,
        data,
    }) {
        unacked.fetch_add(len, Ordering::Relaxed);
        true
    } else {
        false
    }
}

fn probe_failure(result: ProbeResult) -> AppError {
    match result {
        ProbeResult::Failed(err) => err,
        ProbeResult::NeedsConfirmation { fingerprint, .. } => {
            AppError::with_detail(ErrCode::SshHostKeyUnknown, fingerprint)
        }
        ProbeResult::HostKeyChanged {
            fingerprint,
            stored,
            ..
        } => AppError::with_detail(
            ErrCode::SshHostKeyMismatch,
            format!("本次 {fingerprint}，记录 {stored}"),
        ),
        ProbeResult::CertificateUnsupported => {
            AppError::with_detail(ErrCode::Internal, "服务器使用主机证书，当前版本暂不支持")
        }
        ProbeResult::Connected(_) => AppError::internal("意外的连接状态"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::test_support::{spawn, HostKey};
    use crate::ssh::{probe, ProbeAuth};

    fn probe_req(port: u16) -> ProbeRequest {
        ProbeRequest {
            host: "127.0.0.1".to_string(),
            port,
            username: "tester".to_string(),
            auth: ProbeAuth::Password("pw".to_string()),
            legacy_algorithms: false,
            known_fingerprint: None,
            trust_fingerprint: None,
        }
    }

    fn collector() -> (Arc<Mutex<Vec<OutputBatch>>>, EmitFn) {
        let store: Arc<Mutex<Vec<OutputBatch>>> = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&store);
        let emit: EmitFn = Box::new(move |batch| {
            writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(batch);
            true
        });
        (store, emit)
    }

    fn text_of(store: &Arc<Mutex<Vec<OutputBatch>>>) -> String {
        store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|batch| batch.data.clone())
            .collect()
    }

    #[tokio::test]
    async fn shell_roundtrip_carries_input_and_output() {
        let server = spawn("pw", HostKey::Ed25519, false);

        // 等价于用户第一次连接时点了「信任」拿到的那枚指纹
        let fingerprint = match probe(probe_req(server.port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望 NeedsConfirmation，实际 {other:?}"),
        };

        let manager = Arc::new(SessionManager::new());
        let (store, emit) = collector();

        let mut request = probe_req(server.port);
        request.trust_fingerprint = Some(fingerprint);

        let opened = open(
            Arc::clone(&manager),
            OpenRequest {
                session_id: "s1".to_string(),
                connection_id: "c1".to_string(),
                title: "测试会话".to_string(),
                host_label: "127.0.0.1".to_string(),
                cols: 100,
                rows: 30,
                probe: request,
            },
            emit,
            Box::new(|_| {}),
        )
        .await
        .expect("会话应当能建立");

        assert_eq!(opened.info.cols, 100);
        assert!(opened.newly_trusted.is_some(), "首次确认应当要求落库指纹");

        // PTY 尺寸必须真的发到服务端，否则 vim/top 会按 80x24 画
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*server.pty.lock().unwrap(), Some((100, 30)));

        // 写进去的字节要原样回来（中文走完整的多字节路径）
        manager
            .send_input("s1", "echo 你好\r".as_bytes().to_vec())
            .expect("写入应当成功");
        tokio::time::sleep(Duration::from_millis(200)).await;

        let text = text_of(&store);
        assert!(text.contains("ready"), "应当收到 shell 横幅，实际 {text:?}");
        assert!(text.contains("echo 你好"), "应当收到回显，实际 {text:?}");

        // 窗口变化要转成 window_change
        manager.resize("s1", 120, 40).expect("resize 应当成功");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*server.pty.lock().unwrap(), Some((120, 40)));

        manager.close("s1");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(manager.list().is_empty(), "关闭后会话应当被清理");
    }

    fn event_collector() -> (Arc<Mutex<Vec<String>>>, EventFn) {
        let store: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&store);
        let notify: EventFn = Box::new(move |event| {
            let SessionEvent::Disconnected { reason, .. } = event;
            writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(reason);
        });
        (store, notify)
    }

    /// 回归测试：会话曾经复用探活那套 `inactivity_timeout: 20s`，
    /// 用户连上服务器后不动半分钟就被踢掉，报「连接已断开」。
    ///
    /// 20 秒的阈值没法在测试里真等（CI 受不了），所以「配置里不再有空闲超时」
    /// 由 `ssh::client` 的配置测试守住；这里验证行为：空闲之后通道依然活着。
    #[tokio::test]
    async fn session_survives_being_idle() {
        let server = spawn("pw", HostKey::Ed25519, false);

        let fingerprint = match probe(probe_req(server.port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望 NeedsConfirmation，实际 {other:?}"),
        };

        let manager = Arc::new(SessionManager::new());
        let (store, emit) = collector();
        let (disconnects, notify) = event_collector();

        let mut request = probe_req(server.port);
        request.trust_fingerprint = Some(fingerprint);

        open(
            Arc::clone(&manager),
            OpenRequest {
                session_id: "idle".to_string(),
                connection_id: "c1".to_string(),
                title: "空闲测试".to_string(),
                host_label: "127.0.0.1".to_string(),
                cols: 80,
                rows: 24,
                probe: request,
            },
            emit,
            notify,
        )
        .await
        .expect("会话应当能建立");

        // 用户什么都不做
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            disconnects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty(),
            "空闲期间不该断开"
        );

        // 空闲之后仍然能打字
        manager
            .send_input("idle", "还在吗\r".as_bytes().to_vec())
            .expect("空闲后仍应能写入");
        tokio::time::sleep(Duration::from_millis(300)).await;

        let text = text_of(&store);
        assert!(
            text.contains("还在吗"),
            "空闲后写入应当原样回显，实际 {text:?}"
        );

        // 主动关标签不该被当成「连接断开」推给前端
        manager.close("idle");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            disconnects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty(),
            "主动关闭不该推送断开事件"
        );
    }

    #[tokio::test]
    async fn input_to_unknown_session_is_rejected() {
        let manager = SessionManager::new();
        let err = manager.send_input("nope", b"x".to_vec()).unwrap_err();
        assert_eq!(err.code, ErrCode::SshSessionNotFound);
    }

    #[tokio::test]
    async fn close_is_idempotent() {
        let manager = SessionManager::new();
        manager.close("nope");
        manager.close("nope");
        assert!(manager.list().is_empty());
    }
}
