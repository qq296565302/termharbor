//! 字段级加密（SEC-02）。
//!
//! 密文布局（单字段自描述，便于后续轮换密钥时按版本分派）：
//!
//! ```text
//! [ version: 1 ][ nonce: 12 ][ ciphertext: N ][ tag: 16 ]
//! ```
//!
//! - `version` 同时作为 AAD 参与认证，篡改版本字节会导致解密直接失败；
//! - nonce 每次加密重新随机生成，禁止复用；
//! - 同一明文两次加密必须产生不同密文（测试里锁死这条，防止有人偷偷换成 ECB 或固定 nonce）。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;

use super::{CryptoError, KEY_LEN, NONCE_LEN, TAG_LEN, VERSION};

const HEADER_LEN: usize = 1 + NONCE_LEN;
const MIN_BLOB_LEN: usize = HEADER_LEN + TAG_LEN;

/// 单字段密码器。持有数据密钥，可在线程间共享（内部是 `Aes256Gcm`，无内部可变状态）。
#[derive(Clone)]
pub struct FieldCipher {
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for FieldCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 绝不打印密钥
        f.write_str("FieldCipher(<key redacted>)")
    }
}

impl FieldCipher {
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        Self {
            cipher: Aes256Gcm::new_from_slice(key)
                .expect("AES-256 密钥长度固定为 32 字节，不会失败"),
        }
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad: &[VERSION],
                },
            )
            .map_err(|_| CryptoError::EncryptFailed)?;

        let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    pub fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if blob.len() < MIN_BLOB_LEN {
            return Err(CryptoError::Malformed);
        }
        let version = blob[0];
        if version != VERSION {
            return Err(CryptoError::UnsupportedVersion(version));
        }
        let nonce = Nonce::from_slice(&blob[1..HEADER_LEN]);
        self.cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &blob[HEADER_LEN..],
                    aad: &[VERSION],
                },
            )
            .map_err(|_| CryptoError::DecryptFailed)
    }

    pub fn encrypt_str(&self, plaintext: &str) -> Result<Vec<u8>, CryptoError> {
        self.encrypt(plaintext.as_bytes())
    }

    pub fn decrypt_string(&self, blob: &[u8]) -> Result<String, CryptoError> {
        let bytes = self.decrypt(blob)?;
        String::from_utf8(bytes).map_err(|_| CryptoError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::random_key;

    fn cipher() -> FieldCipher {
        FieldCipher::new(&random_key())
    }

    #[test]
    fn roundtrip_ascii_and_cjk() {
        let c = cipher();
        for plain in ["hunter2", "密码含中文与符号 !@#$%^&*()", ""] {
            let blob = c.encrypt_str(plain).unwrap();
            assert_eq!(c.decrypt_string(&blob).unwrap(), plain);
        }
    }

    #[test]
    fn blob_layout_is_versioned() {
        let c = cipher();
        let blob = c.encrypt_str("x").unwrap();
        assert_eq!(blob[0], VERSION);
        assert_eq!(blob.len(), HEADER_LEN + 1 + TAG_LEN);
    }

    #[test]
    fn nonce_is_never_reused() {
        let c = cipher();
        let a = c.encrypt_str("same-plaintext").unwrap();
        let b = c.encrypt_str("same-plaintext").unwrap();
        assert_ne!(
            a[1..HEADER_LEN],
            b[1..HEADER_LEN],
            "两次加密用了同一个 nonce"
        );
        assert_ne!(a, b);
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let c = cipher();
        let mut blob = c.encrypt_str("secret").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(matches!(c.decrypt(&blob), Err(CryptoError::DecryptFailed)));
    }

    #[test]
    fn tampered_version_fails() {
        let c = cipher();
        let mut blob = c.encrypt_str("secret").unwrap();
        blob[0] = 0x02;
        assert!(matches!(
            c.decrypt(&blob),
            Err(CryptoError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn wrong_key_fails() {
        let a = cipher();
        let b = cipher();
        let blob = a.encrypt_str("secret").unwrap();
        assert!(matches!(b.decrypt(&blob), Err(CryptoError::DecryptFailed)));
    }

    #[test]
    fn truncated_blob_is_malformed() {
        let c = cipher();
        let blob = c.encrypt_str("secret").unwrap();
        assert!(matches!(
            c.decrypt(&blob[..10]),
            Err(CryptoError::Malformed)
        ));
        assert!(matches!(c.decrypt(&[]), Err(CryptoError::Malformed)));
    }

    #[test]
    fn debug_never_leaks_key() {
        let c = cipher();
        assert_eq!(format!("{c:?}"), "FieldCipher(<key redacted>)");
    }
}
