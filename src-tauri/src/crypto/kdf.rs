//! Argon2id 密钥派生（SEC-01）。
//!
//! 只在「用户启用了主密码」这条路径上使用（S-05，P1）。
//! MVP 默认走系统钥匙串里的随机数据密钥 —— 因为 Argon2id 每次启动都要派生，
//! 与「冷启动 < 2s」直接冲突（开发计划 §14-4）。

use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use super::{CryptoError, KEY_LEN};

pub const SALT_LEN: usize = 16;

/// KDF 参数随配置一起落库，以便日后升级强度时按记录逐个迁移。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        // OWASP 2024 对 Argon2id 的推荐下限：19 MiB / t=2 / p=1
        Self {
            m_cost_kib: 19_456,
            t_cost: 2,
            p_cost: 1,
        }
    }
}

impl KdfParams {
    pub fn to_argon2_params(&self) -> Result<Params, CryptoError> {
        Params::new(self.m_cost_kib, self.t_cost, self.p_cost, Some(KEY_LEN))
            .map_err(|e| CryptoError::Kdf(e.to_string()))
    }

    /// 单次派生的大致耗时分片，用于 UI 上提示"解锁需要一点时间"。
    pub fn estimated_ms(&self) -> u32 {
        // 经验值：19 MiB 单次约 40–80ms，按内存与迭代数线性估算
        (self.m_cost_kib / 1000).saturating_mul(self.t_cost).max(1)
    }
}

pub fn random_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    salt
}

/// 由主密码派生 32 字节数据密钥。相同 (password, salt, params) 必须得到相同结果。
pub fn derive_key(
    password: &str,
    salt: &[u8],
    params: &KdfParams,
) -> Result<[u8; KEY_LEN], CryptoError> {
    let argon = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        params.to_argon2_params()?,
    );
    let mut out = [0u8; KEY_LEN];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试里用最弱参数，否则每个用例几十毫秒，整套跑下来太慢。
    fn fast_params() -> KdfParams {
        KdfParams {
            m_cost_kib: 8,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn deterministic_for_same_inputs() {
        let salt = random_salt();
        let a = derive_key("correct horse", &salt, &fast_params()).unwrap();
        let b = derive_key("correct horse", &salt, &fast_params()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn salt_changes_result() {
        let a = derive_key("pw", &random_salt(), &fast_params()).unwrap();
        let b = derive_key("pw", &random_salt(), &fast_params()).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn password_changes_result() {
        let salt = random_salt();
        let a = derive_key("pw1", &salt, &fast_params()).unwrap();
        let b = derive_key("pw2", &salt, &fast_params()).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn default_params_are_owasp_floor() {
        let p = KdfParams::default();
        assert!(p.m_cost_kib >= 19_456);
        assert!(p.t_cost >= 2);
    }
}
