//! 加密层：字段级对称加密 + 密钥派生 + 密钥来源。
//!
//! 分层：
//! - [`cipher::FieldCipher`]：AES-256-GCM，负责单字段加解密（SEC-02）
//! - [`kdf`]：Argon2id 参数与派生（SEC-01），主密码模式用
//! - [`provider`]：密钥从哪里来（系统钥匙串 / 开发期文件 / 主密码，SEC-06 的反面）

pub mod cipher;
pub mod kdf;
pub mod mirror;
pub mod provider;

use rand::RngCore;

pub use cipher::FieldCipher;
pub use kdf::{derive_key, random_salt, KdfParams};
pub use provider::{default_provider, DevFileKeyProvider, KeyProvider};

/// 数据密钥长度（AES-256）。
pub const KEY_LEN: usize = 32;
/// GCM nonce 长度。
pub const NONCE_LEN: usize = 12;
/// GCM 认证标签长度。
pub const TAG_LEN: usize = 16;
/// 密文布局版本号。改布局时递增，旧密文按版本号分派解密路径。
pub const VERSION: u8 = 1;

#[cfg(feature = "keychain")]
pub use provider::KeychainKeyProvider;

/// 加密层内部错误。全部经 [`crate::error::AppError`] 映射成对外错误码，
/// 且 Display 里不含任何密钥材料。
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("没有可用的加密密钥")]
    KeyUnavailable,
    #[error("系统密钥存储不可用: {0}")]
    KeyStore(String),
    #[error("密钥编码非法")]
    InvalidKeyEncoding,
    #[error("不支持的密文版本 {0}")]
    UnsupportedVersion(u8),
    #[error("密文长度非法")]
    Malformed,
    #[error("解密失败：认证标签不匹配")]
    DecryptFailed,
    #[error("加密失败")]
    EncryptFailed,
    #[error("密钥派生失败: {0}")]
    Kdf(String),
}

/// 数据密钥的来源，会出现在自检信息里，便于排查"为什么解不开旧配置"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Keychain,
    DevFile,
    MasterPassword,
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            KeySource::Keychain => "keychain",
            KeySource::DevFile => "dev_file",
            KeySource::MasterPassword => "master_password",
        }
    }
}

/// 生成随机 32 字节数据密钥。
pub fn random_key() -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    rand::rngs::OsRng.fill_bytes(&mut key);
    key
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn hex_decode(input: &str) -> Option<Vec<u8>> {
    let s = input.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = hex_val(pair[0])?;
        let lo = hex_val(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let bytes = random_key();
        let encoded = hex_encode(&bytes);
        assert_eq!(encoded.len(), KEY_LEN * 2);
        assert_eq!(hex_decode(&encoded).unwrap(), bytes.to_vec());
    }

    #[test]
    fn hex_rejects_garbage() {
        assert!(hex_decode("abc").is_none());
        assert!(hex_decode("zz").is_none());
    }

    #[test]
    fn random_keys_differ() {
        assert_ne!(random_key(), random_key());
    }
}
