//! 测试专用 SSH 服务端（仅 `cfg(test)`）。
//!
//! 为什么要有它：`connection_test` 的价值全在「真的能握上手」，
//! 单测里 mock 掉 russh 等于什么都没验证。这里起一个真实 SSH 服务端，
//! 让集成测试跑完整链路：TCP → 握手 → 指纹校验 → 认证。
//!
//! 主机密钥来自仓库根的 `fixtures/`（`ssh-keygen` 生成的一次性密钥）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::keys::{Algorithm, PrivateKey};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId, Preferred};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

pub struct TestServer {
    pub port: u16,
    /// 服务端记录到的 PTY 尺寸，用来断言 request_pty / window_change 真的发出去了
    pub pty: Arc<Mutex<Option<(u32, u32)>>>,
    /// SFTP 场景的临时根目录，Drop 时清理
    pub sftp_root: Option<PathBuf>,
    /// 命令 → 输出。测试通过它模拟服务器的 exec 结果（诊断面板用）。
    pub exec_outputs: Arc<Mutex<HashMap<String, String>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
        if let Some(root) = self.sftp_root.take() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[derive(Clone, Copy)]
pub enum HostKey {
    Ed25519,
    Rsa,
}

#[derive(Clone)]
struct Handler {
    password: Arc<String>,
    pty: Arc<Mutex<Option<(u32, u32)>>>,
    /// 有值时接受 `sftp` 子系统，并以它为文件系统根
    sftp_root: Option<PathBuf>,
    /// channel → 输入队列。用队列而不是直接持有 writer：
    /// `data()` 回调里不能跨 await 持有 std MutexGuard，会让 handler future 不 Send。
    sftp_inputs: Arc<Mutex<HashMap<ChannelId, mpsc::UnboundedSender<Vec<u8>>>>>,
    exec_outputs: Arc<Mutex<HashMap<String, String>>>,
}

impl server::Server for Handler {
    type Handler = Self;

    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> Self {
        self.clone()
    }

    fn handle_session_error(&mut self, _error: <Self::Handler as server::Handler>::Error) {}
}

impl server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        if password == self.password.as_str() {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: russh::server::VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        // 测试服务端不校验公钥内容，只看流程能不能走通
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).trim().to_string();
        let _ = session.channel_success(channel);

        let output = self
            .exec_outputs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&command)
            .cloned()
            .unwrap_or_else(|| format!("bash: {command}: command not found\n"));

        if !output.is_empty() {
            let _ = session.data(channel, output.into_bytes());
        }
        let _ = session.eof(channel);
        let _ = session.close(channel);
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        *self.pty.lock().unwrap_or_else(|e| e.into_inner()) = Some((col_width, row_height));
        let _ = session.channel_success(channel);
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        *self.pty.lock().unwrap_or_else(|e| e.into_inner()) = Some((col_width, row_height));
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let _ = session.channel_success(channel);
        let _ = session.data(channel, b"ready\r\n".to_vec());
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(root) = self.sftp_root.clone() else {
            let _ = session.channel_failure(channel);
            return Ok(());
        };

        if name != "sftp" {
            let _ = session.channel_failure(channel);
            return Ok(());
        }

        let _ = session.channel_success(channel);

        // SFTP 服务端要求 AsyncRead + AsyncWrite，而 russh 服务端拿不到 channel 的流，
        // 所以用一对 duplex 桥接：一端给 SFTP 服务端，另一端由这里转发到 SSH 通道。
        let (sftp_side, ssh_side) = tokio::io::duplex(256 * 1024);
        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        self.sftp_inputs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(channel, input_tx);

        tokio::spawn(async move {
            crate::sftp::test_support::serve(sftp_side, root).await;
        });

        let handle = session.handle();
        tokio::spawn(async move {
            let (mut ssh_read, mut ssh_write) = tokio::io::split(ssh_side);
            let mut buffer = vec![0u8; 32 * 1024];

            loop {
                tokio::select! {
                    Some(chunk) = input_rx.recv() => {
                        if ssh_write.write_all(&chunk).await.is_err() {
                            break;
                        }
                    }
                    result = ssh_read.read(&mut buffer) => {
                        match result {
                            Ok(0) | Err(_) => break,
                            Ok(read) => {
                                if handle.data(channel, buffer[..read].to_vec()).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // SFTP 通道：转给协议层，不回显
        let sender = self
            .sftp_inputs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&channel)
            .cloned();

        if let Some(sender) = sender {
            let _ = sender.send(data.to_vec());
            return Ok(());
        }

        // 普通终端通道：原样回显，测试就能验证"写进去的字节确实到了对端又回来了"
        let _ = session.data(channel, data.to_vec());
        Ok(())
    }
}

/// 起一个只接受密码认证的服务端。
///
/// `legacy_only` 为 true 时只提供 SHA-1 那套算法，用来复现「老服务器」场景。
pub fn spawn(password: &str, host_key: HostKey, legacy_only: bool) -> TestServer {
    spawn_inner(password, host_key, legacy_only, None)
}

/// 进程内唯一的临时路径。
///
/// 不要用 `SystemTime::now()` 做后缀：Windows 上它的精度只有毫秒级，
/// 并行跑的测试会拿到同一个值，然后共用一个目录互相删文件
/// （症状是"单跑通过、一起跑失败"，极难查）。
/// 进程间用 pid 区分，进程内用原子序号。
pub fn unique_path(prefix: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "termharbor-{prefix}-{}-{unique}",
        std::process::id()
    ))
}

/// 带 SFTP 子系统的服务端。返回服务端与临时根目录（测试可以往里放文件）。
pub fn spawn_sftp() -> (TestServer, PathBuf) {
    let root = unique_path("sftp");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("无法创建 SFTP 测试根目录");

    let server = spawn_inner("pw", HostKey::Ed25519, false, Some(root.clone()));
    (server, root)
}

fn spawn_inner(
    password: &str,
    host_key: HostKey,
    legacy_only: bool,
    sftp_root: Option<PathBuf>,
) -> TestServer {
    let key = match host_key {
        HostKey::Ed25519 => {
            PrivateKey::from_openssh(include_str!("../../../fixtures/host_ed25519"))
                .expect("测试主机密钥损坏")
        }
        HostKey::Rsa => PrivateKey::from_openssh(include_str!("../../../fixtures/host_rsa"))
            .expect("测试主机密钥损坏"),
    };

    // 全老算法档位：kex / host key / cipher / mac 都退到 SHA-1 那一套，
    // 尽量贴近 CentOS 6 时代服务器的实际能力
    let preferred = if legacy_only {
        Preferred {
            kex: std::borrow::Cow::Owned(
                ["diffie-hellman-group14-sha1", "diffie-hellman-group1-sha1"]
                    .iter()
                    .filter_map(|n| russh::kex::Name::try_from(*n).ok())
                    .collect(),
            ),
            key: std::borrow::Cow::Owned(
                ["ssh-rsa"]
                    .iter()
                    .filter_map(|n| n.parse::<Algorithm>().ok())
                    .collect(),
            ),
            cipher: std::borrow::Cow::Owned(
                ["aes128-cbc", "aes256-cbc"]
                    .iter()
                    .filter_map(|n| russh::cipher::Name::try_from(*n).ok())
                    .collect(),
            ),
            mac: std::borrow::Cow::Owned(
                ["hmac-sha1"]
                    .iter()
                    .filter_map(|n| russh::mac::Name::try_from(*n).ok())
                    .collect(),
            ),
            ..Preferred::DEFAULT
        }
    } else {
        Preferred::DEFAULT
    };

    let config = Arc::new(server::Config {
        keys: vec![key],
        preferred,
        inactivity_timeout: Some(Duration::from_secs(30)),
        auth_rejection_time: Duration::from_millis(0),
        auth_rejection_time_initial: Some(Duration::from_millis(0)),
        ..Default::default()
    });

    let std_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("无法绑定测试端口");
    std_listener.set_nonblocking(true).expect("无法设置非阻塞");
    let port = std_listener.local_addr().expect("无法读取地址").port();
    let listener = TcpListener::from_std(std_listener).expect("无法转换为 tokio listener");

    let pty = Arc::new(Mutex::new(None));
    let exec_outputs: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut handler = Handler {
        password: Arc::new(password.to_string()),
        pty: Arc::clone(&pty),
        sftp_root: sftp_root.clone(),
        sftp_inputs: Arc::new(Mutex::new(HashMap::new())),
        exec_outputs: Arc::clone(&exec_outputs),
    };

    let handle = tokio::spawn(async move {
        let _ = handler.run_on_socket(config, &listener).await;
    });

    TestServer {
        port,
        pty,
        sftp_root,
        exec_outputs,
        handle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 直接验证「唯一」这个属性，而不是靠"多跑几轮没失败"来推断。
    ///
    /// 这条测试守住的是一个真实的回归：早先用时间戳做后缀，
    /// Windows 上并行测试撞名，导致一个用例删掉了另一个用例的目录。
    #[test]
    fn unique_path_never_collides_within_a_process() {
        let paths: HashSet<PathBuf> = (0..2000).map(|_| unique_path("probe")).collect();
        assert_eq!(paths.len(), 2000, "同一进程内 2000 次生成必须互不相同");
    }

    #[test]
    fn unique_path_keeps_prefix_for_debugging() {
        let path = unique_path("sftp");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("termharbor-sftp-"), "实际 {name}");
    }
}
