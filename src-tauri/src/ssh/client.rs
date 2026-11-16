//! 连接探测：握手 → 指纹校验 → 认证，然后立刻断开。
//!
//! 不建 shell、不开 PTY —— 「测试连接」按钮要能在 2 秒内给出答案。
//! 也不保存任何会话状态，真正的会话管理在 M2。

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{Disconnect, MethodKind, MethodSet};
use tokio::io::{AsyncRead, AsyncWrite};

use super::hostkey::{decide, describe, HostKeyDecision, HostKeyInfo};
use super::profile;
use crate::error::{AppError, ErrCode};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 认证阶段超时。分两种用途，因为"该等多久"是两件事：
///
/// - 探活（测试连接）：失败要快，但**不能快到误报**。内网接了 AD/LDAP 的服务器，
///   PAM 走一趟域控十几二十秒很常见 —— 原来卡在 20 秒，正好把它判死。
/// - 会话（终端 / MCP 工具）：进一步放宽，宁可让用户看一分钟的"连接中"，
///   也不要连得上却说连不上（0.1.19 之前就是这么误报的）。
const AUTH_TIMEOUT_PROBE: Duration = Duration::from_secs(30);
const AUTH_TIMEOUT_SESSION: Duration = Duration::from_secs(60);

/// keyboard-interactive 的最多往返轮次。
///
/// PAM 正常只问一两轮（「Password:」），但服务器实现有 bug 时可能一直问下去。
/// 给个上限，避免在这里死循环。
const MAX_KBD_ROUNDS: usize = 5;

/// 交互式会话的 keepalive 间隔。
///
/// 30 秒足够穿透家用路由器的 NAT 映射（通常 60~120 秒过期），
/// 又不会让服务器日志被 keepalive 刷屏。
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// 连续这么多次 keepalive 没有回应才判定连接已死（30s × 3 = 90s）。
const KEEPALIVE_MAX: usize = 3;

/// 建立这条连接的用途，决定空闲策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose {
    /// 一次性探活：用完即断。空闲 20 秒就回收，保证握手卡住时能快速失败。
    Probe,
    /// 交互式会话：用户读代码、发呆都是常态，不能因为「没数据」就断。
    Session,
    /// 跳板那一跳：它要一直替后面几跳转发，所以按会话对待，绝不能设空闲超时。
    Jump,
}

/// 按用途生成客户端配置。
///
/// 这里的区别是踩过坑的：早先两种用途共用 `inactivity_timeout: 20s`，
/// 结果用户连上服务器后不动半分钟就被踢掉 —— 交互式终端根本不可能一直有数据。
/// 空闲时长和「连接已死」是两件事，只能用 keepalive 判断后者。
pub(crate) fn config_for(purpose: Purpose, legacy_algorithms: bool) -> client::Config {
    client::Config {
        preferred: profile::for_connection(legacy_algorithms),
        // 交互式终端最怕 Nagle 攒包
        nodelay: true,
        inactivity_timeout: match purpose {
            Purpose::Probe => Some(Duration::from_secs(20)),
            // 会话和跳板都不看「空闲多久」，只看 keepalive 有没有回应
            Purpose::Session | Purpose::Jump => None,
        },
        keepalive_interval: match purpose {
            Purpose::Probe => None,
            Purpose::Session | Purpose::Jump => Some(KEEPALIVE_INTERVAL),
        },
        keepalive_max: KEEPALIVE_MAX,
        ..Default::default()
    }
}

#[derive(Clone)]
pub enum ProbeAuth {
    Password(String),
    PrivateKey {
        path: String,
        passphrase: Option<String>,
    },
}

/// **手写 Debug，永远不打印凭据。**
///
/// 这个类型带着明文密码/口令，而 `ProbeRequest` 派生 Debug、又经常出现在
/// 「连不上」的排查路径上（`tracing::error!(?request)` 是最自然的写法）。
/// 派生 Debug 就等于埋一颗地雷：哪天有人顺手打一行日志，密码就进日志文件了。
///
/// 现在这条路被从类型上堵死：`{:?}` 只会得到 `Password(***)`。
/// 真实密钥材料的查看只能在 `establish` 内部按需取用。
impl fmt::Debug for ProbeAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeAuth::Password(_) => f.write_str("Password(***)"),
            ProbeAuth::PrivateKey { path, passphrase } => f
                .debug_struct("PrivateKey")
                .field("path", path)
                .field(
                    "passphrase",
                    &if passphrase.is_some() { "***" } else { "none" },
                )
                .finish(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProbeRequest {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: ProbeAuth,
    /// S1 Spike 结论：为 true 时追加 SHA-1 系列算法兜底
    pub legacy_algorithms: bool,
    /// known_hosts 里已记录的指纹
    pub known_fingerprint: Option<String>,
    /// 用户在确认弹窗里点过「信任」的那枚指纹
    pub trust_fingerprint: Option<String>,
    /// 先经由哪台机器转发（跳板机）。`None` = 直连。
    ///
    /// 递归结构：跳板自己还可以有跳板，于是天然支持多跳（A→B→C），
    /// 不需要在类型上写死"最多两跳"。层级上限在解析侧限制。
    pub jump: Option<Box<ProbeRequest>>,
}

#[derive(Debug, Clone)]
pub struct ProbeSuccess {
    pub elapsed_ms: u64,
    pub algo: String,
    pub fingerprint: String,
    /// true 表示这次是首次连接、用户刚确认，调用方需要把指纹落库
    pub trusted_by_user: bool,
}

/// 建立好的连接：句柄 + 协商结果。
pub(crate) struct Established {
    pub handle: client::Handle<ProbeHandler>,
    pub host_key: HostKeyInfo,
    pub elapsed_ms: u64,
}

#[derive(Debug)]
pub enum ProbeResult {
    Connected(ProbeSuccess),
    /// 首次连接，需要前端展示指纹让用户确认
    NeedsConfirmation {
        algo: String,
        fingerprint: String,
    },
    /// 指纹变了，阻断
    HostKeyChanged {
        algo: String,
        fingerprint: String,
        stored: String,
    },
    CertificateUnsupported,
    Failed(AppError),
}

pub(crate) struct ProbeHandler {
    known: Option<String>,
    trust: Option<String>,
    captured: Arc<Mutex<Option<HostKeyInfo>>>,
    /// 跳板链的句柄。
    ///
    /// 放在 handler 里不是随手一放：russh 把 handler 移进连接任务，
    /// **它的生命周期正好等于这段连接**。于是"跳板活到本段连接结束"这件事
    /// 不需要调用方额外接力持有 —— 少一个可以忘记的字段。
    ///
    /// 只持有不读取，`dead_code` 是预期内的。
    #[allow(dead_code)]
    holds: Vec<Arc<client::Handle<ProbeHandler>>>,
}

impl client::Handler for ProbeHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let info = match describe(key) {
            Some((algo, fingerprint)) => {
                let decision = decide(self.known.as_deref(), self.trust.as_deref(), &fingerprint);
                HostKeyInfo {
                    algo,
                    fingerprint,
                    decision,
                }
            }
            None => HostKeyInfo {
                algo: "ssh-cert".to_string(),
                fingerprint: String::new(),
                decision: HostKeyDecision::UnsupportedCertificate,
            },
        };

        let accept = info.decision.allows_connection();
        *self
            .captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(info);
        Ok(accept)
    }
}

/// 建立连接并完成认证，把句柄交出去。
///
/// `probe` 用完即断，`session` 拿它去开 PTY —— 两条路径共用同一套指纹校验与认证逻辑，
/// 避免出现"测试能连、真连却失败"的分裂。
///
/// 返回 boxed future 是必须的：有跳板时会递归调用自己，而 async fn 的递归
/// 编译器算不出大小，只能手动装箱。多花一次堆分配，换一条能复用的握手路径。
pub(crate) fn establish<'a>(
    req: &'a ProbeRequest,
    purpose: Purpose,
) -> Pin<Box<dyn Future<Output = Result<Established, ProbeResult>> + Send + 'a>> {
    Box::pin(establish_inner(req, purpose))
}

async fn establish_inner(req: &ProbeRequest, purpose: Purpose) -> Result<Established, ProbeResult> {
    let config = Arc::new(config_for(purpose, req.legacy_algorithms));

    let captured = Arc::new(Mutex::new(None));
    let started = Instant::now();

    // 第一段：先拿到一条"能通到目标 SSH 端口"的字节流。
    // 直连就是 TCP；有跳板就先建上一跳，让它在通道里替我们转发。
    // 两条路最终都交给同一个 `connect_stream`，所以握手、指纹、认证逻辑只有一份。
    let (transport, holds) = match &req.jump {
        None => (tcp_transport(&req.host, req.port).await?, Vec::new()),
        Some(jump) => tunnel_transport(jump, req).await?,
    };

    let handler = ProbeHandler {
        known: req.known_fingerprint.clone(),
        trust: req.trust_fingerprint.clone(),
        captured: captured.clone(),
        holds,
    };

    let outcome = tokio::time::timeout(
        CONNECT_TIMEOUT,
        client::connect_stream(config, transport, handler),
    )
    .await;

    let mut handle = match outcome {
        Err(_) => {
            tracing::warn!(
                host = %req.host,
                port = req.port,
                stage = "tcp+kex",
                secs = CONNECT_TIMEOUT.as_secs(),
                "握手超时：TCP 连不上，或连上了但密钥交换没有回应"
            );
            return Err(ProbeResult::Failed(AppError::new(ErrCode::SshTcpTimeout)));
        }
        Ok(Err(err)) => {
            if let Some(result) = classify_handshake_failure(&captured) {
                return Err(result);
            }
            let mapped = map_connect_error(&err);
            tracing::warn!(
                host = %req.host,
                port = req.port,
                stage = "tcp+kex",
                code = mapped.code.as_str(),
                error = %err,
                "握手失败"
            );
            return Err(ProbeResult::Failed(mapped));
        }
        Ok(Ok(handle)) => handle,
    };

    let handshake_ms = started.elapsed().as_millis() as u64;

    // 会话不靠 inactivity_timeout 兜底了，认证阶段必须自带超时，
    // 否则服务器接受了 TCP 却一直不回认证结果时，这一步会永久挂起。
    let auth_timeout = match purpose {
        Purpose::Probe => AUTH_TIMEOUT_PROBE,
        // 跳板那一跳认证走会话口径：它连上就要长期用，不能按探活的短超时判死
        Purpose::Session | Purpose::Jump => AUTH_TIMEOUT_SESSION,
    };

    let authed = match tokio::time::timeout(auth_timeout, authenticate(&mut handle, req)).await {
        Err(_) => Err(AppError::with_detail(
            ErrCode::SshTcpTimeout,
            format!("认证阶段 {} 秒没有结果", auth_timeout.as_secs()),
        )),
        Ok(result) => result,
    };

    if let Err(err) = authed {
        tracing::warn!(
            host = %req.host,
            port = req.port,
            stage = "auth",
            handshake_ms,
            timeout_s = auth_timeout.as_secs(),
            code = err.code.as_str(),
            detail = err.detail.as_deref().unwrap_or("-"),
            "认证阶段失败或超时"
        );
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        return Err(ProbeResult::Failed(err));
    }

    let elapsed_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        host = %req.host,
        port = req.port,
        handshake_ms,
        total_ms = elapsed_ms,
        legacy = req.legacy_algorithms,
        "SSH 建连完成"
    );
    let host_key = captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .ok_or_else(|| ProbeResult::Failed(AppError::internal("握手完成但没有拿到主机密钥信息")))?;

    Ok(Established {
        handle,
        host_key,
        elapsed_ms,
    })
}

/// 一段「已经能通到目标 SSH 端口」的字节流：直连的 TCP，或跳板上的隧道通道。
///
/// 统一成一种类型之后，`establish` 只需要一条握手路径 ——
/// 指纹校验、认证、超时策略不会再出现「直连能用、跳板那条路漏了」的分裂。
///
/// 用自定义 trait 而不是 `dyn AsyncRead + AsyncWrite`：Rust 只允许 trait object
/// 里出现一个非 auto trait，两个读写 trait 拼在一起编译不过。
trait TransportIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> TransportIo for T {}

type Transport = Box<dyn TransportIo + 'static>;

/// 直连：自己建 TCP，顺手关掉 Nagle（交互式终端最怕攒包）。
async fn tcp_transport(host: &str, port: u16) -> Result<Transport, ProbeResult> {
    let outcome = tokio::time::timeout(CONNECT_TIMEOUT, async {
        tokio::net::TcpStream::connect((host, port)).await
    })
    .await;

    let socket = match outcome {
        Err(_) => {
            tracing::warn!(host, port, secs = CONNECT_TIMEOUT.as_secs(), "TCP 连接超时");
            return Err(ProbeResult::Failed(AppError::new(ErrCode::SshTcpTimeout)));
        }
        Ok(Err(err)) => {
            tracing::warn!(host, port, error = %err, "TCP 连接失败");
            return Err(ProbeResult::Failed(AppError::with_detail(
                ErrCode::SshTcpTimeout,
                err.to_string(),
            )));
        }
        Ok(Ok(socket)) => socket,
    };

    if let Err(err) = socket.set_nodelay(true) {
        tracing::warn!(host, port, error = %err, "set_nodelay 失败，继续");
    }
    Ok(Box::new(socket))
}

/// 跳板：先建上一跳（它自己可能还有跳板），再让它在 SSH 通道里替我们转发到目标。
///
/// 这里用的是标准 `direct-tcpip` 通道 —— 和 `ssh -J` 做的事完全一样，
/// 只是把"通道"当成一条普通字节流交给下一段握手，于是目标机根本不知道自己在隧道里。
async fn tunnel_transport(
    jump: &ProbeRequest,
    req: &ProbeRequest,
) -> Result<(Transport, Vec<Arc<client::Handle<ProbeHandler>>>), ProbeResult> {
    let hop = establish(jump, Purpose::Jump)
        .await
        .map_err(|result| jump_failure(jump, result))?;

    let hop_handle = Arc::new(hop.handle);

    let channel = hop_handle
        .channel_open_direct_tcpip(
            req.host.clone(),
            u32::from(req.port),
            "127.0.0.1".to_string(),
            0,
        )
        .await
        .map_err(|err| {
            ProbeResult::Failed(AppError::with_detail(
                ErrCode::SshJumpFailed,
                format!(
                    "跳板机 {} 无法转发到 {}:{}（{err}）：可能是跳板到目标之间不通，或跳板禁止了转发",
                    hop_label(jump),
                    req.host,
                    req.port
                ),
            ))
        })?;

    // into_stream 之后，通道自己握着跳板会话的发送端：隧道活多久由最外层连接决定
    Ok((Box::new(channel.into_stream()), vec![hop_handle]))
}

/// 把"跳板那一跳失败"翻译成能照着做的结论。
///
/// 不能把内层结果原样扔出去：用户会看到「首次连接该主机，请确认指纹」，
/// 却不知道要确认的是跳板还是目标 —— 指纹弹窗是对不上的。
fn jump_failure(jump: &ProbeRequest, result: ProbeResult) -> ProbeResult {
    let label = hop_label(jump);
    match result {
        ProbeResult::NeedsConfirmation { fingerprint, .. } => {
            ProbeResult::Failed(AppError::with_detail(
                ErrCode::SshHostKeyUnknown,
                format!(
                    "跳板机 {label} 是首次连接（指纹 {fingerprint}）。请先在「连接」页对它单独测一次并确认指纹，再连目标"
                ),
            ))
        }
        ProbeResult::HostKeyChanged { stored, .. } => {
            ProbeResult::Failed(AppError::with_detail(
                ErrCode::SshHostKeyMismatch,
                format!("跳板机 {label} 的主机指纹变了（原记录 {stored}），已阻断；确认是服务器重装再清除记录"),
            ))
        }
        ProbeResult::CertificateUnsupported => ProbeResult::Failed(AppError::with_detail(
            ErrCode::SshHostKeyUnknown,
            format!("跳板机 {label} 出示的是主机证书，暂不支持"),
        )),
        ProbeResult::Failed(err) => {
            let detail = err.detail.unwrap_or_else(|| err.message.to_string());
            ProbeResult::Failed(AppError::with_detail(
                err.code,
                format!("跳板机 {label}：{detail}"),
            ))
        }
        // establish 只会返回 Ok / 上面这几种，走不到这里；保守地当失败处理
        ProbeResult::Connected(_) => ProbeResult::Failed(AppError::with_detail(
            ErrCode::SshJumpFailed,
            format!("跳板机 {label} 返回了意外的状态"),
        )),
    }
}

fn hop_label(jump: &ProbeRequest) -> String {
    format!("{}:{}", jump.host, jump.port)
}

pub async fn probe(req: ProbeRequest) -> ProbeResult {
    match establish(&req, Purpose::Probe).await {
        Ok(established) => {
            let trusted_by_user = established.host_key.decision == HostKeyDecision::AcceptedByUser;
            let _ = established
                .handle
                .disconnect(Disconnect::ByApplication, "", "en")
                .await;
            ProbeResult::Connected(ProbeSuccess {
                elapsed_ms: established.elapsed_ms,
                algo: established.host_key.algo,
                fingerprint: established.host_key.fingerprint,
                trusted_by_user,
            })
        }
        Err(result) => result,
    }
}

/// 握手失败时，先看是不是指纹校验把连接拦下来的 —— 那样要给前端可操作的结论，
/// 而不是一句笼统的「连接失败」。
fn classify_handshake_failure(captured: &Arc<Mutex<Option<HostKeyInfo>>>) -> Option<ProbeResult> {
    let info = captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()?;

    match info.decision {
        HostKeyDecision::Unknown => Some(ProbeResult::NeedsConfirmation {
            algo: info.algo,
            fingerprint: info.fingerprint,
        }),
        HostKeyDecision::Changed { stored } => Some(ProbeResult::HostKeyChanged {
            algo: info.algo,
            fingerprint: info.fingerprint,
            stored,
        }),
        HostKeyDecision::UnsupportedCertificate => Some(ProbeResult::CertificateUnsupported),
        HostKeyDecision::Trusted | HostKeyDecision::AcceptedByUser => None,
    }
}

async fn authenticate(
    session: &mut client::Handle<ProbeHandler>,
    req: &ProbeRequest,
) -> Result<(), AppError> {
    match &req.auth {
        ProbeAuth::Password(password) => {
            authenticate_password(session, &req.username, password, &req.host).await
        }
        ProbeAuth::PrivateKey { path, passphrase } => {
            let key = russh::keys::load_secret_key(path, passphrase.as_deref()).map_err(|err| {
                AppError::with_detail(
                    ErrCode::SshAuthFailed,
                    format!("无法读取私钥 {path}: {err}"),
                )
            })?;

            // RSA 私钥需要跟服务器协商用哪个 SHA-2 变体
            let hash = session
                .best_supported_rsa_hash()
                .await
                .map_err(|err| AppError::with_detail(ErrCode::SshAuthFailed, err.to_string()))?
                .flatten();

            let result = session
                .authenticate_publickey(
                    req.username.clone(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .map_err(|err| AppError::with_detail(ErrCode::SshAuthFailed, err.to_string()))?;

            finish(result)
        }
    }
}

/// 密码认证：先走 `password`，被拒且服务器提供 `keyboard-interactive` 时再走一遍。
///
/// 为什么必须退化：接了 PAM/AD 的服务器经常把 `password` 关掉、只留
/// `keyboard-interactive`。此时 `password` 会被明确拒绝（很快，不是超时），
/// 而其它客户端（FinalShell、OpenSSH）都会自动降级 —— 我们不降级的话，
/// 用户看到的就是"这台机器连不上，换个工具又能连"。
async fn authenticate_password(
    session: &mut client::Handle<ProbeHandler>,
    username: &str,
    password: &str,
    host: &str,
) -> Result<(), AppError> {
    // 先用 `none` 探一次：它必然被拒，但拒绝里带着"服务器接受哪些方式"。
    // 这一步不需要凭据，却能把"到底该用密码还是公钥"从猜变成事实 ——
    // 排查别人能连我们连不上时，这一行日志比什么都直接。
    match session.authenticate_none(username.to_string()).await {
        Ok(client::AuthResult::Failure {
            remaining_methods, ..
        }) => {
            tracing::info!(
                host,
                methods = %method_names(&remaining_methods).join(", "),
                "服务器接受的认证方式"
            );
        }
        Ok(client::AuthResult::Success) => {
            // 服务器允许无认证直接登录（极少见，通常是没有设密码的测试机）
            tracing::warn!(host, "服务器允许 none 认证，跳过密码校验");
            return Ok(());
        }
        Err(err) => {
            tracing::debug!(host, error = %err, "none 探测失败，继续按密码认证");
        }
    }

    let result = session
        .authenticate_password(username.to_string(), password.to_string())
        .await
        .map_err(|err| AppError::with_detail(ErrCode::SshAuthFailed, err.to_string()))?;

    match &result {
        client::AuthResult::Success => Ok(()),
        client::AuthResult::Failure {
            remaining_methods, ..
        } if remaining_methods.contains(&MethodKind::KeyboardInteractive) => {
            tracing::info!(
                host,
                "password 被拒，但服务器提供 keyboard-interactive，用同一密码再试一次"
            );
            keyboard_interactive(session, username, password).await
        }
        client::AuthResult::Failure { .. } => finish(result),
    }
}

/// keyboard-interactive：把服务器的每个提示都用同一个密码回答。
///
/// PAM 的典型问法是「Password: 」，答案就是密码；不问别的。所以这里不做
/// "按提示内容猜"的聪明事 —— 猜错了反而会把密码发到不该发的地方。
async fn keyboard_interactive(
    session: &mut client::Handle<ProbeHandler>,
    username: &str,
    password: &str,
) -> Result<(), AppError> {
    let mut response = session
        .authenticate_keyboard_interactive_start(username.to_string(), None::<String>)
        .await
        .map_err(|err| AppError::with_detail(ErrCode::SshAuthFailed, err.to_string()))?;

    for _ in 0..MAX_KBD_ROUNDS {
        match response {
            client::KeyboardInteractiveAuthResponse::Success => return Ok(()),
            client::KeyboardInteractiveAuthResponse::Failure {
                remaining_methods, ..
            } => {
                return Err(methods_error(&remaining_methods));
            }
            client::KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                // 提示数量可能为 0（服务器只想确认一下），那就回空数组
                let answers = vec![password.to_string(); prompts.len()];
                response = session
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(|err| {
                        AppError::with_detail(ErrCode::SshAuthFailed, err.to_string())
                    })?;
            }
        }
    }

    Err(AppError::with_detail(
        ErrCode::SshAuthFailed,
        format!("keyboard-interactive 往返超过 {MAX_KBD_ROUNDS} 轮"),
    ))
}

fn finish(result: client::AuthResult) -> Result<(), AppError> {
    if result.success() {
        return Ok(());
    }

    match result {
        client::AuthResult::Failure {
            remaining_methods, ..
        } => Err(methods_error(&remaining_methods)),
        client::AuthResult::Success => Ok(()),
    }
}

/// 认证失败时把"服务器还接受什么"写进 detail。
///
/// 不带这句的话，用户只能看到"认证失败，请检查用户名和凭据"，
/// 而真实原因可能是"这台机器根本不接受密码，只接受公钥"——完全不同的处理方式。
fn methods_error(remaining: &MethodSet) -> AppError {
    let names = method_names(remaining);

    if names.is_empty() {
        AppError::new(ErrCode::SshAuthFailed)
    } else {
        AppError::with_detail(
            ErrCode::SshAuthFailed,
            format!("认证失败；服务器仍接受的方式：{}", names.join(", ")),
        )
    }
}

/// `MethodSet` → 给人看的方法名列表。`none` 不是"方式"，滤掉。
fn method_names(methods: &MethodSet) -> Vec<&'static str> {
    methods
        .iter()
        .filter_map(|kind| match kind {
            MethodKind::Password => Some("password"),
            MethodKind::PublicKey => Some("publickey"),
            MethodKind::KeyboardInteractive => Some("keyboard-interactive"),
            MethodKind::HostBased => Some("hostbased"),
            MethodKind::GssapiWithMic => Some("gssapi-with-mic"),
            MethodKind::None => None,
        })
        .collect()
}

fn map_connect_error(err: &russh::Error) -> AppError {
    match err {
        russh::Error::NoCommonAlgo { .. } => {
            AppError::with_detail(ErrCode::SshAlgoNegotiation, err.to_string())
        }
        russh::Error::UnknownKey => AppError::new(ErrCode::SshHostKeyUnknown),
        _ => AppError::with_detail(ErrCode::SshTcpTimeout, err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 凭据绝不能出现在 Debug 输出里。
    ///
    /// 这条守的是一个具体的翻车方式：`ProbeRequest` 在排查"连不上"时最自然的写法就是
    /// `tracing::error!(?request)`，而它带着明文密码 —— 派生 Debug 就把密码送进了日志文件。
    #[test]
    fn debug_output_never_contains_credentials() {
        let request = ProbeRequest {
            host: "10.0.0.1".to_string(),
            port: 22,
            username: "root".to_string(),
            auth: ProbeAuth::Password("S3cr3t-Passw0rd".to_string()),
            legacy_algorithms: false,
            known_fingerprint: None,
            trust_fingerprint: None,
            jump: None,
        };

        let rendered = format!("{request:?}");
        assert!(
            !rendered.contains("S3cr3t-Passw0rd"),
            "密码泄漏进了 Debug: {rendered}"
        );
        assert!(rendered.contains("***"), "该看到打码: {rendered}");
        // 非敏感字段要保留，否则排查问题时什么都看不到
        assert!(rendered.contains("10.0.0.1"));
        assert!(rendered.contains("root"));

        let key = ProbeAuth::PrivateKey {
            path: "/home/u/.ssh/id_ed25519".to_string(),
            passphrase: Some("key-Passw0rd".to_string()),
        };
        let rendered = format!("{key:?}");
        assert!(!rendered.contains("key-Passw0rd"), "口令泄漏: {rendered}");
        assert!(rendered.contains("/home/u/.ssh/id_ed25519"), "路径要留着");
    }

    #[test]
    fn debug_marks_missing_passphrase_explicitly() {
        // 没有口令时显示 none，而不是 *** —— 否则排查时会误以为设了口令
        let key = ProbeAuth::PrivateKey {
            path: "/k".to_string(),
            passphrase: None,
        };
        assert!(format!("{key:?}").contains("none"));
    }
    use crate::error::ErrCode;
    use crate::ssh::test_support::{spawn, HostKey};

    /// 回归测试：会话曾经复用探活的 `inactivity_timeout: 20s`，
    /// 结果用户连上服务器后不动 20 秒就被踢掉。空闲和「连接已死」必须分开处理。
    #[test]
    fn session_config_does_not_time_out_on_idle() {
        let probe = config_for(Purpose::Probe, false);
        let session = config_for(Purpose::Session, false);

        assert!(
            probe.inactivity_timeout.is_some(),
            "探活要能快速失败，必须带空闲超时"
        );
        assert!(probe.keepalive_interval.is_none());

        assert!(
            session.inactivity_timeout.is_none(),
            "交互会话绝不能设空闲超时：用户读代码、发呆是常态"
        );
        assert!(
            session.keepalive_interval.is_some(),
            "会话要靠 keepalive 探测死连接，否则网络断了会一直挂着"
        );
        assert!(session.keepalive_max >= 1, "至少要允许一次未回应");
        assert!(session.nodelay, "交互终端必须关 Nagle");
    }

    fn base(port: u16) -> ProbeRequest {
        ProbeRequest {
            host: "127.0.0.1".to_string(),
            port,
            username: "tester".to_string(),
            auth: ProbeAuth::Password("pw".to_string()),
            legacy_algorithms: false,
            known_fingerprint: None,
            trust_fingerprint: None,
            jump: None,
        }
    }

    /// 拿一台测试服务端的指纹（第一次连必然要确认，正好用来取指纹）。
    async fn fingerprint_of(port: u16) -> String {
        match probe(base(port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望拿到首次连接确认，实际 {other:?}"),
        }
    }

    /// 跳板这一跳的指纹不能借用目标的"我信任"：那是两段独立的主机密钥。
    #[tokio::test]
    async fn jump_host_needs_its_own_confirmation() {
        let target = spawn("pw", HostKey::Ed25519, false);
        let jump = spawn("pw", HostKey::Ed25519, false);

        let mut req = base(target.port);
        req.jump = Some(Box::new(base(jump.port)));

        match probe(req).await {
            ProbeResult::Failed(err) => {
                assert_eq!(err.code, ErrCode::SshHostKeyUnknown);
                let detail = err.detail.unwrap_or_default();
                assert!(
                    detail.contains("跳板机"),
                    "文案要点明是跳板要确认：{detail}"
                );
            }
            other => panic!("期望因跳板指纹未知而失败，实际 {other:?}"),
        }
    }

    /// 端到端：真的在跳板的 direct-tcpip 通道里完成第二段握手。
    ///
    /// 判定方式是"拿到的是目标的指纹" —— 只有流量真的走到了目标机才可能拿到。
    #[tokio::test]
    async fn traffic_reaches_the_target_through_the_jump_host() {
        let target = spawn("pw", HostKey::Ed25519, false);
        let jump = spawn("pw", HostKey::Ed25519, false);

        let jump_fp = fingerprint_of(jump.port).await;
        let target_fp = fingerprint_of(target.port).await;

        let mut jump_req = base(jump.port);
        jump_req.trust_fingerprint = Some(jump_fp.clone());

        let mut req = base(target.port);
        req.jump = Some(Box::new(jump_req));

        // 跳板已信任、目标还没确认：应当直接问到目标的指纹
        match probe(req).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => {
                assert_eq!(fingerprint, target_fp, "隧道里握手拿到的必须是目标的指纹");
            }
            other => panic!("期望跳过跳板、问到目标指纹，实际 {other:?}"),
        }

        let mut jump_req = base(jump.port);
        jump_req.trust_fingerprint = Some(jump_fp);

        let mut req = base(target.port);
        req.trust_fingerprint = Some(target_fp);
        req.jump = Some(Box::new(jump_req));

        match probe(req).await {
            ProbeResult::Connected(info) => assert!(info.elapsed_ms > 0),
            other => panic!("期望两段都信任后连通，实际 {other:?}"),
        }
    }

    /// 跳板连不上目标（端口没人听）时，要给"转发失败"而不是含糊的"连不上服务器"。
    #[tokio::test]
    async fn jump_reports_forwarding_failure_explicitly() {
        let jump = spawn("pw", HostKey::Ed25519, false);
        let jump_fp = fingerprint_of(jump.port).await;

        // 找一个没人监听的端口
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead);

        let mut jump_req = base(jump.port);
        jump_req.trust_fingerprint = Some(jump_fp);

        let mut req = base(dead_port);
        req.jump = Some(Box::new(jump_req));

        match probe(req).await {
            ProbeResult::Failed(err) => {
                assert_eq!(err.code, ErrCode::SshJumpFailed);
                let detail = err.detail.unwrap_or_default();
                assert!(
                    detail.contains("无法转发"),
                    "文案要说是转发这一步：{detail}"
                );
                assert!(
                    detail.contains(&dead_port.to_string()),
                    "文案要带目标端口：{detail}"
                );
            }
            other => panic!("期望 SshJumpFailed，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_host_key_asks_for_confirmation() {
        let server = spawn("pw", HostKey::Ed25519, false);

        match probe(base(server.port)).await {
            ProbeResult::NeedsConfirmation { algo, fingerprint } => {
                assert_eq!(algo, "ssh-ed25519");
                assert!(
                    fingerprint.starts_with("SHA256:"),
                    "指纹格式应与 OpenSSH 一致"
                );
            }
            other => panic!("期望 NeedsConfirmation，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn trusting_the_fingerprint_completes_the_connection() {
        let server = spawn("pw", HostKey::Ed25519, false);

        let fingerprint = match probe(base(server.port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望 NeedsConfirmation，实际 {other:?}"),
        };

        let mut req = base(server.port);
        req.trust_fingerprint = Some(fingerprint.clone());

        match probe(req).await {
            ProbeResult::Connected(info) => {
                assert!(info.trusted_by_user, "首次确认应当标记为需要落库");
                assert_eq!(info.fingerprint, fingerprint);
            }
            other => panic!("期望 Connected，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn known_fingerprint_skips_the_prompt() {
        let server = spawn("pw", HostKey::Ed25519, false);

        let fingerprint = match probe(base(server.port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望 NeedsConfirmation，实际 {other:?}"),
        };

        let mut req = base(server.port);
        req.known_fingerprint = Some(fingerprint);

        match probe(req).await {
            ProbeResult::Connected(info) => {
                assert!(!info.trusted_by_user, "已记录过的指纹不该再要求确认");
            }
            other => panic!("期望 Connected，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn changed_fingerprint_blocks_even_when_user_tries_to_trust_it() {
        let server = spawn("pw", HostKey::Ed25519, false);

        let mut req = base(server.port);
        req.known_fingerprint = Some("SHA256:definitely-not-the-real-one".to_string());
        // 同时给出"用户已确认"，模拟前端误传
        req.trust_fingerprint = Some("SHA256:definitely-not-the-real-one".to_string());

        match probe(req).await {
            ProbeResult::HostKeyChanged { stored, .. } => {
                assert_eq!(stored, "SHA256:definitely-not-the-real-one");
            }
            other => panic!("指纹变更必须阻断，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn wrong_password_reports_auth_failure() {
        let server = spawn("pw", HostKey::Ed25519, false);

        let fingerprint = match probe(base(server.port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望 NeedsConfirmation，实际 {other:?}"),
        };

        let mut req = base(server.port);
        req.trust_fingerprint = Some(fingerprint);
        req.auth = ProbeAuth::Password("wrong".to_string());

        match probe(req).await {
            ProbeResult::Failed(err) => assert_eq!(err.code, ErrCode::SshAuthFailed),
            other => panic!("期望认证失败，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn private_key_auth_works() {
        let server = spawn("pw", HostKey::Ed25519, false);

        let fingerprint = match probe(base(server.port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望 NeedsConfirmation，实际 {other:?}"),
        };

        let mut req = base(server.port);
        req.trust_fingerprint = Some(fingerprint);
        req.auth = ProbeAuth::PrivateKey {
            path: format!("{}/../fixtures/host_ed25519", env!("CARGO_MANIFEST_DIR")),
            passphrase: None,
        };

        match probe(req).await {
            ProbeResult::Connected(_) => {}
            other => panic!("私钥认证应当成功，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_key_file_reports_readable_error() {
        let server = spawn("pw", HostKey::Ed25519, false);

        let fingerprint = match probe(base(server.port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望 NeedsConfirmation，实际 {other:?}"),
        };

        let mut req = base(server.port);
        req.trust_fingerprint = Some(fingerprint);
        req.auth = ProbeAuth::PrivateKey {
            path: "/definitely/not/here".to_string(),
            passphrase: None,
        };

        match probe(req).await {
            ProbeResult::Failed(err) => {
                assert_eq!(err.code, ErrCode::SshAuthFailed);
                assert!(err.detail.unwrap_or_default().contains("无法读取私钥"));
            }
            other => panic!("期望失败，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn legacy_server_requires_the_compatibility_switch() {
        let server = spawn("pw", HostKey::Rsa, true);

        // 1) 默认档位：算法协商就失败 —— 这就是真实用户会看到的现象
        match probe(base(server.port)).await {
            ProbeResult::Failed(err) => {
                assert_eq!(
                    err.code,
                    ErrCode::SshAlgoNegotiation,
                    "默认档位连全老算法服务器应当报算法协商失败"
                );
            }
            other => panic!("期望 SshAlgoNegotiation，实际 {other:?}"),
        }

        // 2) 打开兼容开关：握手能过，但首次仍需确认指纹
        let mut req = base(server.port);
        req.legacy_algorithms = true;
        let fingerprint = match probe(req).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望 NeedsConfirmation，实际 {other:?}"),
        };

        // 3) 确认后连通
        let mut req = base(server.port);
        req.legacy_algorithms = true;
        req.trust_fingerprint = Some(fingerprint);
        match probe(req).await {
            ProbeResult::Connected(info) => {
                assert!(info.elapsed_ms > 0);
            }
            other => panic!("开启兼容开关后应当能连上，实际 {other:?}"),
        }
    }

    /// 认证失败时要把"服务器还接受什么"说清楚。
    ///
    /// 真实场景：服务器只允许 publickey，用户却在用密码连。原来只报"认证失败，
    /// 请检查用户名和凭据"，等于让人去查一个根本没问题的东西。
    #[test]
    fn methods_error_lists_what_the_server_still_accepts() {
        let kinds: &[MethodKind] = &[MethodKind::PublicKey, MethodKind::KeyboardInteractive];
        let remaining = MethodSet::from(kinds);
        let err = methods_error(&remaining);

        assert_eq!(err.code, ErrCode::SshAuthFailed);
        let detail = err.detail.expect("要带上剩余方式");
        assert!(detail.contains("publickey"), "实际: {detail}");
        assert!(detail.contains("keyboard-interactive"), "实际: {detail}");
        assert!(
            !detail.contains("password"),
            "password 已经被拒了，不该再列：{detail}"
        );
    }

    /// 服务器什么都没说（`none`）时不要拼出「服务器仍接受的方式：」这种空话。
    #[test]
    fn methods_error_stays_quiet_when_nothing_is_left() {
        let err = methods_error(&MethodSet::empty());
        assert_eq!(err.code, ErrCode::SshAuthFailed);
        assert!(err.detail.is_none());
    }
}
