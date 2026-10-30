//! 统一错误类型。
//!
//! 设计要点：
//! - 前端只拿到 `{ code, message, detail }` 三元组，永远不直显 Rust 原始错误（需求 4.6）；
//! - `code` 是稳定契约，改名等同破坏性变更，前端 `src/lib/errors.ts` 有一一对应的映射表；
//! - `detail` 在构造瞬间就脱敏，避免凭据随着错误一路漂到日志和 UI（约束 A-07）。

use std::error::Error;
use std::fmt;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::crypto::CryptoError;
use crate::redact::redact;

pub type AppResult<T> = Result<T, AppError>;

/// 稳定错误码。新增可以，改名不行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrCode {
    // ---- SSH（M2 起使用，M0 先定义契约）
    SshTcpTimeout,
    SshAuthFailed,
    SshHostKeyMismatch,
    SshHostKeyUnknown,
    SshAlgoNegotiation,
    SshDisconnected,
    SshSessionNotFound,
    // ---- SFTP（M3 起使用）
    SftpPermissionDenied,
    SftpNotFound,
    SftpIoError,
    SftpCancelled,
    // ---- 加密与存储
    CryptoKeyUnavailable,
    CryptoDecryptFailed,
    ConnectionNotFound,
    StorageError,
    // ---- 通用
    ValidationError,
    Internal,
}

impl ErrCode {
    /// 默认中文文案。前端有自己的一份映射，这里只作为后端兜底。
    pub fn default_message(self) -> &'static str {
        match self {
            ErrCode::SshTcpTimeout => "无法连接到服务器，请检查地址与端口",
            ErrCode::SshAuthFailed => "认证失败，请检查用户名和凭据",
            ErrCode::SshHostKeyMismatch => "主机密钥已变更，可能存在安全风险，已阻止连接",
            ErrCode::SshHostKeyUnknown => "首次连接该主机，请确认指纹",
            ErrCode::SshAlgoNegotiation => "与服务器的加密算法协商失败",
            ErrCode::SshDisconnected => "连接已断开",
            ErrCode::SshSessionNotFound => "会话不存在或已关闭",
            ErrCode::SftpPermissionDenied => "权限不足，无法执行该操作",
            ErrCode::SftpNotFound => "文件或目录不存在",
            ErrCode::SftpIoError => "文件传输失败",
            ErrCode::SftpCancelled => "传输已取消",
            ErrCode::CryptoKeyUnavailable => "无法获取本地加密密钥",
            ErrCode::CryptoDecryptFailed => "本地配置解密失败",
            ErrCode::ConnectionNotFound => "连接配置不存在",
            ErrCode::StorageError => "本地数据读写失败",
            ErrCode::ValidationError => "输入内容不合法",
            ErrCode::Internal => "应用内部错误",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ErrCode::SshTcpTimeout => "SSH_TCP_TIMEOUT",
            ErrCode::SshAuthFailed => "SSH_AUTH_FAILED",
            ErrCode::SshHostKeyMismatch => "SSH_HOST_KEY_MISMATCH",
            ErrCode::SshHostKeyUnknown => "SSH_HOST_KEY_UNKNOWN",
            ErrCode::SshAlgoNegotiation => "SSH_ALGO_NEGOTIATION",
            ErrCode::SshDisconnected => "SSH_DISCONNECTED",
            ErrCode::SshSessionNotFound => "SSH_SESSION_NOT_FOUND",
            ErrCode::SftpPermissionDenied => "SFTP_PERMISSION_DENIED",
            ErrCode::SftpNotFound => "SFTP_NOT_FOUND",
            ErrCode::SftpIoError => "SFTP_IO_ERROR",
            ErrCode::SftpCancelled => "SFTP_CANCELLED",
            ErrCode::CryptoKeyUnavailable => "CRYPTO_KEY_UNAVAILABLE",
            ErrCode::CryptoDecryptFailed => "CRYPTO_DECRYPT_FAILED",
            ErrCode::ConnectionNotFound => "CONNECTION_NOT_FOUND",
            ErrCode::StorageError => "STORAGE_ERROR",
            ErrCode::ValidationError => "VALIDATION_ERROR",
            ErrCode::Internal => "INTERNAL",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppError {
    pub code: ErrCode,
    pub message: &'static str,
    /// 技术细节，已脱敏。可以给开发看，不给终端用户当主文案。
    pub detail: Option<String>,
}

impl AppError {
    pub fn new(code: ErrCode) -> Self {
        Self {
            code,
            message: code.default_message(),
            detail: None,
        }
    }

    /// 附加脱敏后的技术细节。
    pub fn with_detail(code: ErrCode, detail: impl AsRef<str>) -> Self {
        Self {
            code,
            message: code.default_message(),
            detail: Some(redact(detail.as_ref())),
        }
    }

    pub fn internal(detail: impl AsRef<str>) -> Self {
        Self::with_detail(ErrCode::Internal, detail)
    }

    pub fn validation(detail: impl AsRef<str>) -> Self {
        Self::with_detail(ErrCode::ValidationError, detail)
    }

    pub fn is_locked(&self) -> bool {
        matches!(self.code, ErrCode::CryptoKeyUnavailable)
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Some(d) => write!(f, "[{}] {} ({})", self.code.as_str(), self.message, d),
            None => write!(f, "[{}] {}", self.code.as_str(), self.message),
        }
    }
}

impl Error for AppError {}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut st = serializer.serialize_struct("AppError", 3)?;
        st.serialize_field("code", &self.code)?;
        st.serialize_field("message", &self.message)?;
        st.serialize_field("detail", &self.detail)?;
        st.end()
    }
}

impl From<CryptoError> for AppError {
    fn from(err: CryptoError) -> Self {
        let code = match err {
            CryptoError::KeyUnavailable | CryptoError::KeyStore(_) => ErrCode::CryptoKeyUnavailable,
            CryptoError::DecryptFailed
            | CryptoError::Malformed
            | CryptoError::UnsupportedVersion(_) => ErrCode::CryptoDecryptFailed,
            CryptoError::InvalidKeyEncoding | CryptoError::Kdf(_) | CryptoError::EncryptFailed => {
                ErrCode::Internal
            }
        };
        // CryptoError 的 Display 里不含密钥材料，可以安全外露
        AppError::with_detail(code, err.to_string())
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        AppError::with_detail(ErrCode::StorageError, err.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::with_detail(ErrCode::StorageError, err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_serializes_to_stable_string() {
        let json = serde_json::to_string(&ErrCode::SshHostKeyMismatch).unwrap();
        assert_eq!(json, "\"SSH_HOST_KEY_MISMATCH\"");
        assert_eq!(
            ErrCode::SshHostKeyMismatch.as_str(),
            "SSH_HOST_KEY_MISMATCH"
        );
    }

    #[test]
    fn error_serializes_with_three_fields() {
        let err = AppError::with_detail(ErrCode::SshAuthFailed, "user=root password=hunter2");
        let v: serde_json::Value = serde_json::to_value(&err).unwrap();
        assert_eq!(v["code"], "SSH_AUTH_FAILED");
        assert_eq!(v["message"], "认证失败，请检查用户名和凭据");
        // detail 必须已脱敏
        let detail = v["detail"].as_str().unwrap();
        assert!(!detail.contains("hunter2"), "detail 泄漏了密码: {detail}");
    }

    #[test]
    fn display_is_readable() {
        let err = AppError::new(ErrCode::SftpNotFound);
        assert_eq!(err.to_string(), "[SFTP_NOT_FOUND] 文件或目录不存在");
    }
}
