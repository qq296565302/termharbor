//! 日志初始化（约束 A-07：默认脱敏）。
//!
//! 落盘到 `app_log_dir`，按天滚动。级别用 `TERMHARBOR_LOG` 覆盖，例如
//! `TERMHARBOR_LOG=termharbor_lib=debug,info`。
//!
//! 脱敏不是靠这里某个"过滤器"，而是靠两条纪律：
//! 1. 敏感值一律用 [`crate::redact::Redacted`] 包起来；
//! 2. 错误 detail 在构造时就已经过 [`crate::redact::redact`]。

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::error::{AppError, AppResult};

pub const ENV_FILTER: &str = "TERMHARBOR_LOG";

/// 持有后台写盘线程的 guard，drop 会导致日志丢失。必须挂到应用生命周期上。
pub struct LoggingGuard {
    _guard: Option<WorkerGuard>,
}

impl LoggingGuard {
    /// 测试或特殊场景下不落盘。
    pub fn disabled() -> Self {
        Self { _guard: None }
    }
}

pub fn init(log_dir: &Path, default_level: &str) -> AppResult<LoggingGuard> {
    std::fs::create_dir_all(log_dir)?;

    let appender = tracing_appender::rolling::daily(log_dir, "termharbor.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_env(ENV_FILTER)
        .or_else(|_| EnvFilter::try_new(default_level))
        .map_err(|e| AppError::internal(format!("日志过滤规则非法: {e}")))?;

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true);

    let registry = tracing_subscriber::registry().with(filter).with(file_layer);

    // 开发构建同时打到 stderr，省得每次去翻日志文件
    #[cfg(debug_assertions)]
    let registry = registry.with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr));

    // 重复初始化（例如测试）直接忽略，不 panic
    let _ = registry.try_init();

    Ok(LoggingGuard {
        _guard: Some(guard),
    })
}
