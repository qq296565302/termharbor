//! SSH 相关实现。
//!
//! M1 只做「连接探测」（握手 + 认证 + 指纹校验）。真正的交互式会话在 M2：
//! `session.rs`（会话管理）、`decoder.rs`（UTF-8 增量解码）、`output.rs`（批量与背压）。

pub mod client;
pub mod decoder;
pub mod exec;
pub mod hostkey;
pub mod profile;
pub mod session;

#[cfg(test)]
pub(crate) mod test_support;

pub use client::{probe, ProbeAuth, ProbeRequest, ProbeResult, ProbeSuccess};
pub use hostkey::{HostKeyDecision, HostKeyInfo};
