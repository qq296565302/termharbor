//! 密钥来源。
//!
//! MVP 用[`KeychainKeyProvider`]：生成一个随机 32 字节数据密钥，存进操作系统钥匙串。
//! 这样不必每次启动都跑 Argon2id，冷启动才守得住 2s 指标。
//!
//! [`DevFileKeyProvider`] 仅供开发和 CI 用，密钥落在明文文件里，启动时会打 warn。生产环境不要用。

use std::path::{Path, PathBuf};

use super::{hex_decode, hex_encode, random_key, CryptoError, KeySource, KEY_LEN};

/// 环境变量：设为 `devfile` 强制走开发期文件密钥。
pub const ENV_KEY_PROVIDER: &str = "TERMHARBOR_KEY_PROVIDER";

pub trait KeyProvider: Send + Sync {
    fn source(&self) -> KeySource;
    /// 读取已有密钥；不存在则生成并持久化。
    fn load_or_create(&self) -> Result<[u8; KEY_LEN], CryptoError>;
}

/// 按环境变量选择密钥来源。默认系统钥匙串。
pub fn default_provider(data_dir: &Path) -> Box<dyn KeyProvider> {
    let force_dev = std::env::var(ENV_KEY_PROVIDER)
        .map(|v| v.eq_ignore_ascii_case("devfile"))
        .unwrap_or(false);

    if force_dev {
        return Box::new(DevFileKeyProvider::new(data_dir.join("dev_key.hex")));
    }

    #[cfg(feature = "keychain")]
    {
        Box::new(KeychainKeyProvider::new(
            "com.termharbor.app",
            "data-encryption-key",
        ))
    }
    #[cfg(not(feature = "keychain"))]
    {
        Box::new(DevFileKeyProvider::new(data_dir.join("dev_key.hex")))
    }
}

/// 把当前密钥导出成 `dev_key.hex`，返回写出的路径。
///
/// 存在的理由很具体：MCP 宿主有时把子进程起在**非交互登录会话**里（Windows 服务、
/// 后台任务），此时系统凭据管理器会报 `ERROR_NO_SUCH_LOGON_SESSION`，读不到密钥。
/// 让用户在他自己的正常会话里跑一次 `--export-key`，后台进程改用
/// `TERMHARBOR_KEY_PROVIDER=devfile` 就能读同一把密钥。
///
/// **安全代价要说清楚**：导出的那一刻起，密钥以明文存在磁盘上，只靠用户目录 ACL 保护。
/// 这是给受控内网环境用的显式逃生口，不是默认行为；用完应当删掉这个文件并恢复默认。
pub fn export_key_to_devfile(data_dir: &Path) -> Result<PathBuf, CryptoError> {
    let provider = default_provider(data_dir);
    let key = provider.load_or_create()?;
    let path = data_dir.join("dev_key.hex");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| CryptoError::KeyStore(e.to_string()))?;
    }
    write_key_file(&path, &hex_encode(&key)).map_err(|e| CryptoError::KeyStore(e.to_string()))?;
    tracing::warn!(
        path = %path.display(),
        "已导出数据密钥到明文文件，仅用于非交互会话的 MCP；用完请删除"
    );
    Ok(path)
}

// ---------------------------------------------------------------- 系统钥匙串

#[cfg(feature = "keychain")]
pub struct KeychainKeyProvider {
    service: String,
    account: String,
}

#[cfg(feature = "keychain")]
impl KeychainKeyProvider {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, CryptoError> {
        keyring::Entry::new(&self.service, &self.account)
            .map_err(|e| CryptoError::KeyStore(e.to_string()))
    }
}

#[cfg(feature = "keychain")]
impl KeyProvider for KeychainKeyProvider {
    fn source(&self) -> KeySource {
        KeySource::Keychain
    }

    fn load_or_create(&self) -> Result<[u8; KEY_LEN], CryptoError> {
        let entry = self.entry()?;
        match entry.get_password() {
            Ok(encoded) => decode_key(&encoded),
            Err(keyring::Error::NoEntry) => {
                let key = random_key();
                entry
                    .set_password(&hex_encode(&key))
                    .map_err(|e| CryptoError::KeyStore(e.to_string()))?;
                tracing::info!("已生成新的数据密钥并写入系统钥匙串");
                Ok(key)
            }
            Err(e) => Err(CryptoError::KeyStore(e.to_string())),
        }
    }
}

// ---------------------------------------------------------------- 开发期文件

pub struct DevFileKeyProvider {
    path: PathBuf,
}

impl DevFileKeyProvider {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl KeyProvider for DevFileKeyProvider {
    fn source(&self) -> KeySource {
        KeySource::DevFile
    }

    fn load_or_create(&self) -> Result<[u8; KEY_LEN], CryptoError> {
        if let Ok(existing) = std::fs::read_to_string(&self.path) {
            return decode_key(&existing);
        }

        let key = random_key();
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| CryptoError::KeyStore(e.to_string()))?;
        }
        write_key_file(&self.path, &hex_encode(&key))
            .map_err(|e| CryptoError::KeyStore(e.to_string()))?;
        tracing::warn!(
            path = %self.path.display(),
            "使用开发期文件密钥，密钥以明文存放，切勿用于生产环境"
        );
        Ok(key)
    }
}

/// 写密钥文件，**创建时就限权**。
///
/// 先 `fs::write` 再 `chmod` 有个真实的短窗口：文件以 0644 存在了几微秒，
/// 同机其他用户此刻是能读到的。改成创建即带 0600（`mode` 只在新建时生效），
/// 之后那次 chmod 仅用于兜住「文件已存在但权限不对」的情况。
fn write_key_file(path: &Path, data: &str) -> std::io::Result<()> {
    use std::io::Write;

    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
    };

    #[cfg(not(unix))]
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;

    let mut file = file;
    file.write_all(data.as_bytes())?;
    file.flush()?;
    drop(file);

    restrict_permissions(path);
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {
    // Windows 下依赖用户目录 ACL，后续可换成 DPAPI 绑定的 Credential Manager
}

fn decode_key(encoded: &str) -> Result<[u8; KEY_LEN], CryptoError> {
    let bytes = hex_decode(encoded).ok_or(CryptoError::InvalidKeyEncoding)?;
    if bytes.len() != KEY_LEN {
        return Err(CryptoError::InvalidKeyEncoding);
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&bytes);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_path() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir()
            .join(format!(
                "termharbor-test-{}-{}-{n}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("dev_key.hex")
    }

    #[test]
    fn dev_file_generates_then_reuses() {
        let path = temp_path();
        let provider = DevFileKeyProvider::new(&path);
        let first = provider.load_or_create().unwrap();
        let second = provider.load_or_create().unwrap();
        assert_eq!(first, second);
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn dev_file_uses_distinct_keys_per_location() {
        let a = DevFileKeyProvider::new(temp_path())
            .load_or_create()
            .unwrap();
        let b = DevFileKeyProvider::new(temp_path())
            .load_or_create()
            .unwrap();
        assert_ne!(a, b);
    }

    /// 真机验证系统钥匙串：默认被忽略，需要手动执行
    /// `cargo test --lib keychain_roundtrip -- --ignored --nocapture`
    #[cfg(feature = "keychain")]
    #[test]
    #[ignore = "需要真实系统钥匙串，手动执行"]
    fn keychain_roundtrip() {
        const SERVICE: &str = "com.termharbor.app.selftest";
        const ACCOUNT: &str = "data-encryption-key";

        let first = KeychainKeyProvider::new(SERVICE, ACCOUNT)
            .load_or_create()
            .expect("写入系统钥匙串失败");
        let second = KeychainKeyProvider::new(SERVICE, ACCOUNT)
            .load_or_create()
            .expect("读取系统钥匙串失败");
        assert_eq!(first, second, "两次读取拿到的密钥不一致");

        if let Ok(entry) = keyring::Entry::new(SERVICE, ACCOUNT) {
            let _ = entry.delete_credential();
        }
        println!("钥匙串读写正常，测试条目已清理");
    }

    #[test]
    fn invalid_encoded_key_is_rejected() {
        assert!(matches!(
            decode_key("zzzz"),
            Err(CryptoError::InvalidKeyEncoding)
        ));
        assert!(matches!(
            decode_key("abcd"),
            Err(CryptoError::InvalidKeyEncoding)
        ));
    }
}
