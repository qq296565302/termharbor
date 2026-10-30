//! 连接探测：握手 → 指纹校验 → 认证，然后立刻断开。
//!
//! 不建 shell、不开 PTY —— 「测试连接」按钮要能在 2 秒内给出答案。
//! 也不保存任何会话状态，真正的会话管理在 M2。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::Disconnect;

use super::hostkey::{decide, describe, HostKeyDecision, HostKeyInfo};
use super::profile;
use crate::error::{AppError, ErrCode};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const AUTH_TIMEOUT: Duration = Duration::from_secs(20);

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
            // 会话不看「空闲多久」，只看 keepalive 有没有回应
            Purpose::Session => None,
        },
        keepalive_interval: match purpose {
            Purpose::Probe => None,
            Purpose::Session => Some(KEEPALIVE_INTERVAL),
        },
        keepalive_max: KEEPALIVE_MAX,
        ..Default::default()
    }
}

#[derive(Debug, Clone)]
pub enum ProbeAuth {
    Password(String),
    PrivateKey {
        path: String,
        passphrase: Option<String>,
    },
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
pub(crate) async fn establish(
    req: &ProbeRequest,
    purpose: Purpose,
) -> Result<Established, ProbeResult> {
    let config = Arc::new(config_for(purpose, req.legacy_algorithms));

    let captured = Arc::new(Mutex::new(None));
    let handler = ProbeHandler {
        known: req.known_fingerprint.clone(),
        trust: req.trust_fingerprint.clone(),
        captured: captured.clone(),
    };

    let started = Instant::now();
    let outcome = tokio::time::timeout(
        CONNECT_TIMEOUT,
        client::connect(config, (req.host.as_str(), req.port), handler),
    )
    .await;

    let mut handle = match outcome {
        Err(_) => return Err(ProbeResult::Failed(AppError::new(ErrCode::SshTcpTimeout))),
        Ok(Err(err)) => {
            if let Some(result) = classify_handshake_failure(&captured) {
                return Err(result);
            }
            return Err(ProbeResult::Failed(map_connect_error(&err)));
        }
        Ok(Ok(handle)) => handle,
    };

    // 会话不靠 inactivity_timeout 兜底了，认证阶段必须自带超时，
    // 否则服务器接受了 TCP 却一直不回认证结果时，这一步会永久挂起。
    let authed = match tokio::time::timeout(AUTH_TIMEOUT, authenticate(&mut handle, req)).await {
        Err(_) => Err(AppError::new(ErrCode::SshTcpTimeout)),
        Ok(result) => result,
    };

    if let Err(err) = authed {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        return Err(ProbeResult::Failed(err));
    }

    let elapsed_ms = started.elapsed().as_millis() as u64;
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
    let result = match &req.auth {
        ProbeAuth::Password(password) => {
            session
                .authenticate_password(req.username.clone(), password.clone())
                .await
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

            session
                .authenticate_publickey(
                    req.username.clone(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
        }
    };

    match result {
        Ok(auth) if auth.success() => Ok(()),
        Ok(_) => Err(AppError::new(ErrCode::SshAuthFailed)),
        Err(err) => Err(AppError::with_detail(
            ErrCode::SshAuthFailed,
            err.to_string(),
        )),
    }
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
}
