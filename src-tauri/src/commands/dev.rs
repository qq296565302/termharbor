//! 只在开发期使用的诊断 command。
//!
//! 目前只有一条：`ime_echo`，把前端通过 xterm `onData` 拿到的输入按十六进制
//! 原样送回 Rust，验证"WebView → IPC → Rust"这一段的字节完整性。
//! S3（中文输入法）就是靠它取证的，见 `docs/spike-notes.md`。

use serde::Serialize;

use crate::error::{AppError, AppResult};

#[derive(Debug, Serialize)]
pub struct ImeEcho {
    /// Rust 实际收到的字节数
    pub bytes: usize,
    /// 按 UTF-8 解码后的文本
    pub text: String,
    /// 收到的字节是否为合法 UTF-8
    pub valid_utf8: bool,
}

/// 回显前端传来的原始输入。
///
/// 为什么用 hex 而不是直接传字符串：如果直接在 JS 侧转成字符串再传，
/// 丢字/替换符这类问题会在传输前就被掩盖掉。hex 能保住原始字节。
#[tauri::command]
pub fn ime_echo(data_hex: String) -> AppResult<ImeEcho> {
    let bytes = crate::crypto::hex_decode(&data_hex)
        .ok_or_else(|| AppError::validation("data_hex 不是合法的十六进制串"))?;

    let bytes_len = bytes.len();
    match String::from_utf8(bytes) {
        Ok(text) => Ok(ImeEcho {
            bytes: bytes_len,
            text,
            valid_utf8: true,
        }),
        Err(err) => {
            // 不是合法 UTF-8 也要把信息带回去，方便判断是丢字节还是编码问题
            let lossy = String::from_utf8_lossy(err.as_bytes()).into_owned();
            Ok(ImeEcho {
                bytes: bytes_len,
                text: lossy,
                valid_utf8: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echoes_cjk_bytes_exactly() {
        // "你好" 的 UTF-8 字节
        let echo = ime_echo("e4bda0e5a5bd".to_string()).unwrap();
        assert_eq!(echo.text, "你好");
        assert_eq!(echo.bytes, 6);
        assert!(echo.valid_utf8);
    }

    #[test]
    fn reports_invalid_utf8_instead_of_hiding_it() {
        let echo = ime_echo("ff41".to_string()).unwrap();
        assert!(!echo.valid_utf8);
        assert_eq!(echo.bytes, 2);
        assert!(echo.text.contains('\u{FFFD}'));
    }

    #[test]
    fn rejects_malformed_hex() {
        let err = ime_echo("zz".to_string()).unwrap_err();
        assert_eq!(err.code, crate::error::ErrCode::ValidationError);
    }
}
