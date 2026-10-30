//! 测试用 SSH 服务端。
//!
//! 目的不是做一个真服务器，而是造一个**算法限制可控、输出速率可控**的对端，
//! 用来验证客户端在极端条件下的行为。
//!
//! 协议：客户端 exec `stream <字节数>`，服务端就把这么多字节尽快推过去。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Server as _, Session};
use russh::{Channel, ChannelId, Preferred};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Clone, Debug)]
pub struct StreamOptions {
    /// 单次 `channel.data()` 的字节数
    pub chunk: usize,
    /// 每块之间主动让出，避免服务端把整个文件塞进自己的发送缓冲
    pub pace: Option<Duration>,
    /// 让输出里混入中文，用来验证解码器（按 8 字节一组交替）
    pub with_cjk: bool,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            chunk: 32 * 1024,
            pace: None,
            with_cjk: true,
        }
    }
}

struct TestServer {
    opts: Arc<StreamOptions>,
    pending: Arc<Mutex<HashMap<ChannelId, oneshot::Sender<u64>>>>,
}

impl TestServer {
    fn new(opts: StreamOptions) -> Self {
        Self {
            opts: Arc::new(opts),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl server::Server for TestServer {
    type Handler = Self;

    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> Self {
        Self {
            opts: self.opts.clone(),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn handle_session_error(&mut self, error: <Self::Handler as server::Handler>::Error) {
        eprintln!("[server] 会话错误: {error:?}");
    }
}

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth, Self::Error> {
        // 测试服务端，任何凭据都放行
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;

        let (tx, rx) = oneshot::channel::<u64>();
        self.pending
            .lock()
            .expect("pending 锁中毒")
            .insert(channel.id(), tx);

        let opts = self.opts.clone();
        tokio::spawn(async move {
            let total = rx.await.unwrap_or(0);
            if total > 0 {
                stream_out(channel, total, opts).await;
            } else {
                let _ = channel.eof().await;
                let _ = channel.exit_status(0).await;
            }
        });

        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let _ = session.channel_success(channel);
        let command = String::from_utf8_lossy(data);
        let total = command
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        if let Some(tx) = self
            .pending
            .lock()
            .expect("pending 锁中毒")
            .remove(&channel)
        {
            let _ = tx.send(total);
        }
        Ok(())
    }
}

async fn stream_out(channel: Channel<Msg>, total: u64, opts: Arc<StreamOptions>) {
    // 预生成一个块：前半是 ASCII，后半是可打印中文，保证分包会切在多字节字符中间
    let mut block = vec![b'.'; opts.chunk];
    if opts.with_cjk {
        let cjk = "终端港传输测试数据".as_bytes();
        let mut i = opts.chunk / 2;
        while i + cjk.len() <= opts.chunk {
            block[i..i + cjk.len()].copy_from_slice(cjk);
            i += cjk.len() + 1;
        }
    }

    let mut sent: u64 = 0;
    while sent < total {
        let n = (total - sent).min(opts.chunk as u64) as usize;
        if channel.data_bytes(block[..n].to_vec()).await.is_err() {
            break;
        }
        sent += n as u64;
        match opts.pace {
            Some(p) => tokio::time::sleep(p).await,
            None => tokio::task::yield_now().await,
        }
    }

    let _ = channel.eof().await;
    let _ = channel.exit_status(0).await;
    let _ = channel.close().await;
}

/// 固定测试主机密钥。
///
/// 用 `ssh-keygen` 预生成后内嵌，避免依赖 `rand_core` 版本（russh 的 ssh-key
/// 与 rand 0.8/0.9 的 trait 版本互不兼容）。这两个密钥是**一次性测试专用**，
/// 出现在任何生产配置里都应当被当作泄漏处理。
fn host_key(algorithm: &Algorithm) -> Result<PrivateKey> {
    let pem = match algorithm {
        Algorithm::Ed25519 => include_str!("../../fixtures/host_ed25519"),
        _ => include_str!("../../fixtures/host_rsa"),
    };
    Ok(PrivateKey::from_openssh(pem)?)
}

/// 起一个服务端，返回已绑定的端口与任务句柄。
///
/// 端口在返回前就已完成 bind，所以调用方拿到端口时可以直接连，不存在竞态。
pub fn spawn_server(
    preferred: Preferred,
    host_key_algorithm: Algorithm,
    opts: StreamOptions,
) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    spawn_server_at(0, preferred, host_key_algorithm, opts)
}

/// 同上，但可以指定端口（0 表示让系统分配）。
pub fn spawn_server_at(
    port: u16,
    preferred: Preferred,
    host_key_algorithm: Algorithm,
    opts: StreamOptions,
) -> Result<(u16, tokio::task::JoinHandle<()>)> {
    let key = host_key(&host_key_algorithm)?;

    let config = Arc::new(server::Config {
        keys: vec![key],
        preferred,
        inactivity_timeout: Some(Duration::from_secs(120)),
        auth_rejection_time: Duration::from_millis(0),
        auth_rejection_time_initial: Some(Duration::from_millis(0)),
        ..Default::default()
    });

    let std_listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    std_listener.set_nonblocking(true)?;
    let port = std_listener.local_addr()?.port();
    let listener = TcpListener::from_std(std_listener)?;

    let mut handler = TestServer::new(opts);
    let join = tokio::spawn(async move {
        let running = handler.run_on_socket(config, &listener);
        if let Err(err) = running.await {
            eprintln!("[server] 退出: {err:?}");
        }
    });

    Ok((port, join))
}
