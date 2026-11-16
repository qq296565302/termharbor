//! 本地端口转发（`ssh -L`）运行时。
//!
//! 做法和 `ssh -L` 完全一样：
//! 本机 `TcpListener` 接一个连接 → 在 SSH 连接上开一条 `direct-tcpip` 通道 → 双向搬运。
//! 目标地址由**远端**解析，所以能把内网服务搬到本机；本地只监听 `127.0.0.1`，
//! 同网段的其他人连不进来（要放开是另一件事，需要单独的开关）。
//!
//! 两条设计取舍：
//! 1. **隧道自己建一条专用 SSH 连接**，不借用终端标签的会话 —— 否则关掉标签页
//!    会把隧道一起弄死，而用户完全不会把这两件事联系起来。
//! 2. 同一台机器的多条规则共用一个 `links` 条目，不开一堆连接。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use russh::client;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::client::{establish, ProbeHandler, ProbeRequest, ProbeResult, Purpose};
use crate::error::{AppError, AppResult, ErrCode};

/// 开一条转发要做的事。
pub struct StartRequest {
    /// 规则 id（不是连接 id）
    pub tunnel_id: String,
    pub connection_id: String,
    pub local_port: u16,
    pub target_host: String,
    pub target_port: u16,
    /// 这条连接（含跳板链）的探测请求，用来建专用连接
    pub probe: ProbeRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    Stopped,
    Listening { port: u16 },
    Failed { message: String },
}

impl TunnelState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TunnelState::Stopped => "stopped",
            TunnelState::Listening { .. } => "listening",
            TunnelState::Failed { .. } => "failed",
        }
    }
}

struct Running {
    port: u16,
    token: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
pub struct TunnelManager {
    /// connection_id → 专用连接
    links: Mutex<HashMap<String, Arc<client::Handle<ProbeHandler>>>>,
    /// 规则 id → 运行中
    running: Mutex<HashMap<String, Running>>,
    /// 最近一次失败原因，给界面显示（进程重启即清空）
    failures: Mutex<HashMap<String, String>>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self, tunnel_id: &str) -> TunnelState {
        if let Some(running) = self.lock_running().get(tunnel_id) {
            return TunnelState::Listening { port: running.port };
        }
        match self.lock_failures().get(tunnel_id) {
            Some(message) => TunnelState::Failed {
                message: message.clone(),
            },
            None => TunnelState::Stopped,
        }
    }

    /// 开一条转发，返回真正绑上的端口。已经开着时是幂等的。
    pub async fn start(&self, req: StartRequest) -> AppResult<u16> {
        if let Some(port) = self.running_port(&req.tunnel_id) {
            return Ok(port);
        }

        let mut handle = self.link(&req).await?;

        // 先探一下目标再开监听：端口写错、内网不通要立刻报，
        // 而不是让用户拿 Navicat 连半天没反应才回来问。
        if let Err(first) = probe_target(&handle, &req).await {
            // 专用连接可能已经死了（网络断过、服务器重启）：丢掉重建一次再判
            self.forget_link(&req.connection_id, &handle);
            handle = self.link(&req).await?;
            probe_target(&handle, &req).await.map_err(|_| first)?;
        }

        let listener = TcpListener::bind(("127.0.0.1", req.local_port))
            .await
            .map_err(|err| bind_error(req.local_port, &err))?;

        let token = CancellationToken::new();
        let loop_token = token.clone();
        let target_host = req.target_host.clone();
        let target_port = req.target_port;

        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = loop_token.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };

                let (socket, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(err) => {
                        tracing::warn!(error = %err, "转发监听接受连接失败，继续等下一个");
                        continue;
                    }
                };

                let handle = Arc::clone(&handle);
                let host = target_host.clone();
                let child = loop_token.clone();
                tokio::spawn(async move {
                    let channel = match handle
                        .channel_open_direct_tcpip(
                            host.clone(),
                            u32::from(target_port),
                            peer.ip().to_string(),
                            u32::from(peer.port()),
                        )
                        .await
                    {
                        Ok(channel) => channel,
                        Err(err) => {
                            tracing::warn!(
                                target = %format!("{host}:{target_port}"),
                                error = %err,
                                "转发通道开不出来（目标不可达或连接已断）"
                            );
                            return;
                        }
                    };

                    let mut stream = channel.into_stream();
                    let mut socket = socket;
                    tokio::select! {
                        _ = child.cancelled() => {}
                        _ = tokio::io::copy_bidirectional(&mut stream, &mut socket) => {}
                    }
                });
            }
        });

        self.lock_running().insert(
            req.tunnel_id.clone(),
            Running {
                port: req.local_port,
                token,
                task,
            },
        );
        self.lock_failures().remove(&req.tunnel_id);

        tracing::info!(
            tunnel = %req.tunnel_id,
            local = req.local_port,
            target = %format!("{}:{}", req.target_host, req.target_port),
            "本地转发已启动"
        );
        Ok(req.local_port)
    }

    /// 停一条转发（连带掐断正在搬运的连接）。
    pub fn stop(&self, tunnel_id: &str) {
        let running = self.lock_running().remove(tunnel_id);
        if let Some(running) = running {
            running.token.cancel();
            running.task.abort();
            tracing::info!(tunnel = %tunnel_id, "本地转发已停止");
        }
    }

    /// 停掉某条连接的所有转发（删连接时用）。
    pub fn stop_for_connection(&self, connection_ids: &[String], tunnel_ids: &[String]) {
        for id in tunnel_ids {
            self.stop(id);
        }
        for connection_id in connection_ids {
            self.links
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(connection_id);
        }
    }

    /// 进程退出前收摊。
    pub fn stop_all(&self) {
        let running = std::mem::take(&mut *self.lock_running());
        for (_, item) in running {
            item.token.cancel();
            item.task.abort();
        }
        self.links.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// 记下失败原因（前端刷新时能看到"为什么起不来"）。
    pub fn note_failure(&self, tunnel_id: &str, message: String) {
        self.lock_failures().insert(tunnel_id.to_string(), message);
    }

    fn running_port(&self, tunnel_id: &str) -> Option<u16> {
        self.lock_running().get(tunnel_id).map(|item| item.port)
    }

    async fn link(&self, req: &StartRequest) -> AppResult<Arc<client::Handle<ProbeHandler>>> {
        let existing = self.lock_links().get(&req.connection_id).map(Arc::clone);
        if let Some(handle) = existing {
            return Ok(handle);
        }

        let established = establish(&req.probe, Purpose::Session)
            .await
            .map_err(to_app_error)?;
        let handle = Arc::new(established.handle);
        self.lock_links()
            .insert(req.connection_id.clone(), Arc::clone(&handle));
        Ok(handle)
    }

    fn forget_link(&self, connection_id: &str, handle: &Arc<client::Handle<ProbeHandler>>) {
        let mut links = self.lock_links();
        // 只删自己那一份：并发重建时别把别人刚建好的连接也丢了
        if links
            .get(connection_id)
            .is_some_and(|current| Arc::ptr_eq(current, handle))
        {
            links.remove(connection_id);
        }
    }

    fn lock_running(&self) -> std::sync::MutexGuard<'_, HashMap<String, Running>> {
        self.running.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn lock_failures(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.failures.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn lock_links(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, Arc<client::Handle<ProbeHandler>>>> {
        self.links.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// 开一条通道再立刻关掉：只用它确认"远端到目标这条路通"。
async fn probe_target(handle: &client::Handle<ProbeHandler>, req: &StartRequest) -> AppResult<()> {
    handle
        .channel_open_direct_tcpip(
            req.target_host.clone(),
            u32::from(req.target_port),
            "127.0.0.1".to_string(),
            0,
        )
        .await
        .map(|_| ())
        .map_err(|err| {
            AppError::with_detail(
                ErrCode::TunnelFailed,
                format!(
                    "远端连不上 {}:{}（{err}）。{}",
                    req.target_host,
                    req.target_port,
                    target_hint(&req.target_host, &req.probe.host)
                ),
            )
        })
}

/// 目标连不上时给一句"下一步该改什么"。
///
/// 最常见的一次踩坑：目标填成这台服务器**自己的公网 IP**。云主机的公网地址是
/// NAT 映射，机器自己往回连多半不通（hairpin 不支持），看起来就像"内网不通"。
fn target_hint(target: &str, own_host: &str) -> &'static str {
    if looks_like_own_address(target, own_host) {
        "目标填的是这台服务器自己的公网 IP —— 云主机通常连不回自己的公网地址，换成 127.0.0.1 或内网 IP"
    } else {
        "地址/端口写错，或那台机器的防火墙没放行"
    }
}

/// 目标是不是"这台机器自己的地址"（回环地址不算：那是本机服务，完全正常）。
fn looks_like_own_address(target: &str, own_host: &str) -> bool {
    let target = target.trim();
    let own = own_host.trim();
    if target.is_empty() || own.is_empty() {
        return false;
    }
    let is_loopback = matches!(
        target.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    );
    !is_loopback && target.eq_ignore_ascii_case(own)
}

fn bind_error(port: u16, err: &std::io::Error) -> AppError {
    let hint = if err.kind() == std::io::ErrorKind::AddrInUse {
        "这个端口已经被别的程序占用了，换一个（例如 13306 → 13307）"
    } else if err.kind() == std::io::ErrorKind::PermissionDenied {
        "1024 以下的端口需要管理员/root 权限，换一个 1024 以上的"
    } else {
        "本机端口绑不上"
    };
    AppError::with_detail(
        ErrCode::TunnelFailed,
        format!("本机 {port} 端口：{hint}（{err}）"),
    )
}

fn to_app_error(result: ProbeResult) -> AppError {
    match result {
        ProbeResult::NeedsConfirmation { fingerprint, .. } => AppError::with_detail(
            ErrCode::SshHostKeyUnknown,
            format!("首次连接该主机（指纹 {fingerprint}），请先在连接页点一次「测试连接」确认指纹"),
        ),
        ProbeResult::HostKeyChanged { stored, .. } => AppError::with_detail(
            ErrCode::SshHostKeyMismatch,
            format!("主机指纹变了（原记录 {stored}），确认是服务器重装再清除记录"),
        ),
        ProbeResult::CertificateUnsupported => AppError::with_detail(
            ErrCode::SshHostKeyUnknown,
            "服务器出示的是主机证书，暂不支持",
        ),
        ProbeResult::Failed(err) => err,
        ProbeResult::Connected(_) => AppError::internal("转发建连返回了意外的状态"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::test_support::{spawn, HostKey};

    fn start_req(connection_id: &str, probe: ProbeRequest) -> StartRequest {
        StartRequest {
            tunnel_id: format!("t-{connection_id}"),
            connection_id: connection_id.to_string(),
            local_port: 0, // 由测试自己填
            target_host: "127.0.0.1".to_string(),
            target_port: 0,
            probe,
        }
    }

    fn probe_for(port: u16) -> ProbeRequest {
        ProbeRequest {
            host: "127.0.0.1".to_string(),
            port,
            username: "tester".to_string(),
            auth: crate::ssh::ProbeAuth::Password("pw".to_string()),
            legacy_algorithms: false,
            known_fingerprint: None,
            trust_fingerprint: None,
            jump: None,
        }
    }

    /// 目标填成服务器自己的公网 IP 时，文案要直接点出来（用户真踩过）。
    #[test]
    fn own_public_ip_is_called_out() {
        assert!(looks_like_own_address("47.94.196.217", "47.94.196.217"));
        assert!(looks_like_own_address(" Example.COM ", "example.com"));
        assert!(
            !looks_like_own_address("127.0.0.1", "47.94.196.217"),
            "回环是正常写法"
        );
        assert!(!looks_like_own_address("10.0.3.21", "47.94.196.217"));
        assert!(!looks_like_own_address("", "47.94.196.217"));

        assert!(target_hint("47.94.196.217", "47.94.196.217").contains("自己的公网 IP"));
        assert!(target_hint("10.0.3.21", "47.94.196.217").contains("防火墙"));
    }

    /// 端口被占用时报的必须是"本机端口被占用"，而不是含糊的"建连失败"。
    #[tokio::test]
    async fn occupied_local_port_is_reported_clearly() {
        let server = spawn("pw", HostKey::Ed25519, false);
        let fingerprint = match crate::ssh::probe(probe_for(server.port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望首次连接确认，实际 {other:?}"),
        };

        // 占住一个端口，模拟"13306 已经被别的程序用了"
        let blocker = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let busy_port = blocker.local_addr().unwrap().port();

        let mut probe = probe_for(server.port);
        probe.trust_fingerprint = Some(fingerprint);

        let manager = TunnelManager::new();
        let mut req = start_req("c1", probe);
        req.local_port = busy_port;
        req.target_port = server.port;

        let err = manager.start(req).await.unwrap_err();
        assert_eq!(err.code, ErrCode::TunnelFailed);
        let detail = err.detail.unwrap_or_default();
        assert!(detail.contains("占用"), "文案要说清是被占用：{detail}");
        assert!(
            detail.contains(&busy_port.to_string()),
            "要带端口号：{detail}"
        );
    }

    /// 目标端口没人听：在开监听前就该失败，别让用户以为隧道通了。
    #[tokio::test]
    async fn unreachable_target_fails_before_listening() {
        let server = spawn("pw", HostKey::Ed25519, false);
        let fingerprint = match crate::ssh::probe(probe_for(server.port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望首次连接确认，实际 {other:?}"),
        };

        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead);

        let mut probe = probe_for(server.port);
        probe.trust_fingerprint = Some(fingerprint);

        let manager = TunnelManager::new();
        let mut req = start_req("c1", probe);
        req.local_port = 0;
        req.target_port = dead_port;

        let err = manager.start(req).await.unwrap_err();
        assert_eq!(err.code, ErrCode::TunnelFailed);
        let detail = err.detail.unwrap_or_default();
        assert!(
            detail.contains("远端连不上"),
            "要说清是远端那一步：{detail}"
        );
        assert!(detail.contains(&dead_port.to_string()));
    }

    /// 正常路径：本机端口 → SSH 服务器 → 内网目标（这里用本机 echo 服务代替）。
    ///
    /// 判定方式是"从本地端口写进去的字节又回来了"，说明通道真的在双向搬运。
    /// 目标不能直接用 SSH 服务端本身：它会先吐一行 `SSH-2.0-…` 欢迎语，
    /// 第一次读到的就不是我们写进去的字节（这坑踩过）。
    #[tokio::test]
    async fn forwards_bytes_end_to_end() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let echo = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = echo.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buffer = [0u8; 1024];
                    loop {
                        match socket.read(&mut buffer).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => {
                                if socket.write_all(&buffer[..read]).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });

        let server = spawn("pw", HostKey::Ed25519, false);
        let fingerprint = match crate::ssh::probe(probe_for(server.port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望首次连接确认，实际 {other:?}"),
        };

        let mut probe = probe_for(server.port);
        probe.trust_fingerprint = Some(fingerprint);

        let manager = TunnelManager::new();
        let mut req = start_req("c1", probe);
        req.target_port = echo_port;
        // 挑一个空闲端口再交出去，避免和并行跑的测试撞
        let pick = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        req.local_port = pick.local_addr().unwrap().port();
        drop(pick);

        let port = manager.start(req).await.unwrap();
        assert_eq!(
            manager.state("t-c1"),
            TunnelState::Listening { port },
            "起来之后状态必须是 listening"
        );

        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        client.write_all(b"ping-through-tunnel").await.unwrap();

        let mut buffer = vec![0u8; 32];
        let read =
            tokio::time::timeout(std::time::Duration::from_secs(5), client.read(&mut buffer))
                .await
                .expect("5 秒内应该拿到回显")
                .unwrap();
        assert_eq!(&buffer[..read], b"ping-through-tunnel");

        manager.stop("t-c1");
        assert_eq!(manager.state("t-c1"), TunnelState::Stopped);
    }
}
