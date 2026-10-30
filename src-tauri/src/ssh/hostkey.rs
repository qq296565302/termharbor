//! 主机指纹校验（SEC-04）。
//!
//! 判定顺序固定，不可跳过：
//! 1. known_hosts 有记录且一致 → 信任；
//! 2. 有记录但不一致 → **拒绝**，且不提供"忽略并继续"的选项；
//! 3. 无记录且用户确认过这个指纹 → 接受（随后由调用方落库）；
//! 4. 无记录且未确认 → 拒绝，把指纹交给前端弹确认框。

use russh::keys::{HashAlg, PublicKeyOrCertificate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// 与 known_hosts 记录一致
    Trusted,
    /// 首次连接，用户已确认
    AcceptedByUser,
    /// 首次连接，等待用户确认
    Unknown,
    /// 与记录不一致 —— 可能被中间人，也可能是服务器重装
    Changed { stored: String },
    /// 服务器出示的是主机证书，MVP 不支持
    UnsupportedCertificate,
}

impl HostKeyDecision {
    pub fn allows_connection(&self) -> bool {
        matches!(
            self,
            HostKeyDecision::Trusted | HostKeyDecision::AcceptedByUser
        )
    }
}

#[derive(Debug, Clone)]
pub struct HostKeyInfo {
    pub algo: String,
    pub fingerprint: String,
    pub decision: HostKeyDecision,
}

/// 取出算法名与 SHA256 指纹。
///
/// 指纹格式与 `ssh-keygen -lf` 一致（`SHA256:base64`，无填充），
/// 用户可以直接拿去和运维给的指纹比对。
pub fn describe(key: &PublicKeyOrCertificate) -> Option<(String, String)> {
    match key {
        PublicKeyOrCertificate::PublicKey { key, .. } => Some((
            key.algorithm().to_string(),
            key.fingerprint(HashAlg::Sha256).to_string(),
        )),
        // 主机证书需要一套 CA 信任链才能验证，MVP 不做，也不假装能做
        PublicKeyOrCertificate::Certificate(_) => None,
    }
}

/// 根据 known_hosts 记录与用户确认结果做判定。
pub fn decide(known: Option<&str>, trust: Option<&str>, fingerprint: &str) -> HostKeyDecision {
    match known {
        Some(stored) if stored == fingerprint => HostKeyDecision::Trusted,
        Some(stored) => HostKeyDecision::Changed {
            stored: stored.to_string(),
        },
        None => match trust {
            Some(trusted) if trusted == fingerprint => HostKeyDecision::AcceptedByUser,
            _ => HostKeyDecision::Unknown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "SHA256:abc123";

    #[test]
    fn matching_record_is_trusted() {
        assert_eq!(decide(Some(FP), None, FP), HostKeyDecision::Trusted);
        assert!(decide(Some(FP), None, FP).allows_connection());
    }

    #[test]
    fn changed_fingerprint_blocks_even_if_user_tried_to_trust_it() {
        // 关键：即使用户传了 trust，已有记录不一致也必须阻断
        let decision = decide(Some("SHA256:old"), Some(FP), FP);
        assert_eq!(
            decision,
            HostKeyDecision::Changed {
                stored: "SHA256:old".into()
            }
        );
        assert!(!decision.allows_connection());
    }

    #[test]
    fn unknown_key_needs_explicit_confirmation() {
        assert_eq!(decide(None, None, FP), HostKeyDecision::Unknown);
        assert!(!decide(None, None, FP).allows_connection());
    }

    #[test]
    fn user_confirmation_only_matches_the_same_fingerprint() {
        assert_eq!(decide(None, Some(FP), FP), HostKeyDecision::AcceptedByUser);
        // 确认的是 A，实际来的是 B —— 必须重新弹窗，不能放行
        assert_eq!(
            decide(None, Some("SHA256:other"), FP),
            HostKeyDecision::Unknown
        );
    }
}
