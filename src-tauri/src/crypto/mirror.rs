//! 后台可读的密钥镜像。
//!
//! ## 为什么需要它
//!
//! 系统凭据管理器（Windows Credential Manager / macOS Keychain / Secret Service）
//! 的密钥是**绑在交互登录会话**上的。MCP 宿主经常把子进程起成服务或计划任务，
//! 那种会话里读不到钥匙串（`ERROR_NO_SUCH_LOGON_SESSION`）：列表工具能用，
//! 但凡要解密密码去连服务器的工具全废。
//!
//! 让每个用户手动导出密钥不叫产品。所以改成：**应用自己写、MCP 自己读**。
//!
//! ## 安全性
//!
//! Windows 上用 DPAPI（`CryptProtectData`）：密文由**当前 Windows 用户的主密钥**
//! 加密，不依赖交互登录，也解不开别人的。拿到文件不等于拿到密钥。
//! 非 Windows 平台退化为 0600 明文文件（和 VS Code / Git 凭据存储同一档保护），
//! 会打 warn。
//!
//! 文件放在 `app_data_dir` 里 —— 卸载器会连它一起删掉，这正合适。

use std::path::Path;

use super::{CryptoError, KEY_LEN};

const MAGIC: &[u8; 4] = b"THK1";

/// 固定文件名。GUI 写、MCP 读，两边都从 `AppPaths::data_dir` 拼。
pub const FILE_NAME: &str = "key.bin";

/// 写一份后台可读的密钥副本。GUI 启动时调用，失败不该阻断启动。
pub fn store(key: &[u8; KEY_LEN], path: &Path) -> Result<(), CryptoError> {
    let blob = protect(key)?;
    let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + blob.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&blob);

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| CryptoError::KeyStore(e.to_string()))?;
    }
    write_private(path, &bytes)?;
    Ok(())
}

/// 读回密钥。文件不存在 / 不是本用户的密文 / 格式不对，都返回 Err 让调用方回退。
pub fn load(path: &Path) -> Result<[u8; KEY_LEN], CryptoError> {
    let bytes = std::fs::read(path).map_err(|e| CryptoError::KeyStore(e.to_string()))?;
    if bytes.len() < MAGIC.len() + 4 || &bytes[..MAGIC.len()] != MAGIC {
        return Err(CryptoError::KeyStore("密钥镜像格式不对".into()));
    }

    let len = u32::from_le_bytes(
        bytes[MAGIC.len()..MAGIC.len() + 4]
            .try_into()
            .map_err(|_| CryptoError::KeyStore("密钥镜像长度字段损坏".into()))?,
    ) as usize;
    let start = MAGIC.len() + 4;
    let blob = bytes
        .get(start..start + len)
        .ok_or_else(|| CryptoError::KeyStore("密钥镜像被截断".into()))?;

    let plain = unprotect(blob)?;
    let key: [u8; KEY_LEN] = plain
        .try_into()
        .map_err(|_| CryptoError::InvalidKeyEncoding)?;
    Ok(key)
}

// ---------------------------------------------------------------- Windows：DPAPI

#[cfg(windows)]
fn protect(data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() {
        return Err(CryptoError::KeyStore(
            "CryptProtectData 失败（当前用户的主密钥不可用）".into(),
        ));
    }

    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(output.pbData as *mut std::ffi::c_void) };
    Ok(bytes)
}

#[cfg(windows)]
fn unprotect(blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let input = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() {
        return Err(CryptoError::KeyStore(
            "DPAPI 解不开这份密钥（换用户了、或者文件来自别的机器）".into(),
        ));
    }

    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(output.pbData as *mut std::ffi::c_void) };
    Ok(bytes)
}

#[cfg(windows)]
fn write_private(path: &Path, data: &[u8]) -> Result<(), CryptoError> {
    // 用户 profile 目录本身已经是 per-user ACL；DPAPI 又加了一层用户主密钥加密
    std::fs::write(path, data).map_err(|e| CryptoError::KeyStore(e.to_string()))
}

// ---------------------------------------------------------------- 非 Windows

#[cfg(not(windows))]
fn protect(data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    tracing::warn!("非 Windows 平台：密钥镜像以明文存放，仅靠文件权限 0600 保护");
    Ok(data.to_vec())
}

#[cfg(not(windows))]
fn unprotect(blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
    Ok(blob.to_vec())
}

#[cfg(not(windows))]
fn write_private(path: &Path, data: &[u8]) -> Result<(), CryptoError> {
    use std::io::Write;
    // 两个扩展 trait 都要导入：OpenOptionsExt 给 .mode()，PermissionsExt 给
    // Permissions::from_mode()。少导后面那个在 Linux/macOS 上是**编译错误**，
    // 而 Windows 走上面的 cfg(windows) 分支，本地怎么跑都发现不了 —— CI 红在
    // Clippy 步骤就是这个原因（编译不过，所以后面的单测根本没跑）。
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| CryptoError::KeyStore(e.to_string()))?;
    file.write_all(data)
        .map_err(|e| CryptoError::KeyStore(e.to_string()))?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("termharbor-mirror-{}-{n}", std::process::id()))
    }

    #[test]
    fn store_then_load_roundtrip() {
        let path = temp_path();
        let key = [0x5Au8; KEY_LEN];
        store(&key, &path).expect("写入镜像");
        assert_eq!(load(&path).expect("读回镜像"), key);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn missing_file_is_an_error_not_a_new_key() {
        let path = temp_path();
        assert!(
            load(&path).is_err(),
            "读不到就该回退，绝不能顺手生成一把新钥匙"
        );
    }

    #[test]
    fn corrupted_file_is_rejected() {
        let path = temp_path();
        std::fs::write(&path, b"garbage").unwrap();
        assert!(load(&path).is_err());
        let _ = std::fs::remove_file(path);
    }
}
