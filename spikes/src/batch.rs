//! 终端输出批处理器。
//!
//! 规则：攒满 `max_bytes` 或距上次 flush 超过 `flush_after` 就吐一批。
//! 目的是把「每个 SSH 分包一次 IPC」压成「每 16ms 一次 IPC」。
//!
//! 为什么不是"每条输出都一样"：`echo` 快速滚动时包极小极密，
//! 批量能把 IPC 次数降一到两个数量级；而 `top` 这种低频刷新几乎不受影响。

use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Batcher {
    buf: String,
    first_push_at: Option<Instant>,
    max_bytes: usize,
    flush_after: Duration,
    pub batches: u64,
    pub largest_batch: usize,
}

impl Batcher {
    pub fn new(max_bytes: usize, flush_after: Duration) -> Self {
        Self {
            buf: String::with_capacity(max_bytes),
            first_push_at: None,
            max_bytes,
            flush_after,
            batches: 0,
            largest_batch: 0,
        }
    }

    /// 压入一段文本，达到条件则返回一批可发送的数据。
    pub fn push(&mut self, s: &str) -> Option<String> {
        if s.is_empty() {
            return None;
        }
        if self.buf.is_empty() {
            self.first_push_at = Some(Instant::now());
        }
        self.buf.push_str(s);

        let too_big = self.buf.len() >= self.max_bytes;
        let too_old = self
            .first_push_at
            .map(|t| t.elapsed() >= self.flush_after)
            .unwrap_or(false);

        if too_big || too_old {
            return self.flush();
        }
        None
    }

    pub fn flush(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            self.first_push_at = None;
            return None;
        }
        let out = std::mem::replace(&mut self.buf, String::with_capacity(self.max_bytes));
        self.first_push_at = None;
        self.batches += 1;
        self.largest_batch = self.largest_batch.max(out.len());
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flushes_when_size_limit_reached() {
        let mut b = Batcher::new(10, Duration::from_secs(60));
        assert!(b.push("12345").is_none());
        assert!(b.push("67890").is_some());
        assert_eq!(b.batches, 1);
    }

    #[test]
    fn flushes_on_time_even_when_small() {
        let mut b = Batcher::new(1024, Duration::from_millis(0));
        let first = b.push("x");
        assert!(first.is_some(), "时间阈值到了就该吐出去，不能一直攒着");
    }

    #[test]
    fn empty_push_does_not_start_timer() {
        let mut b = Batcher::new(1024, Duration::from_millis(50));
        assert!(b.push("").is_none());
        assert!(b.first_push_at.is_none());
    }

    #[test]
    fn flush_is_idempotent() {
        let mut b = Batcher::new(1024, Duration::from_secs(1));
        b.push("abc");
        assert_eq!(b.flush().as_deref(), Some("abc"));
        assert!(b.flush().is_none());
        assert_eq!(b.batches, 1);
    }

    #[test]
    fn cjk_bytes_count_toward_limit() {
        let mut b = Batcher::new(6, Duration::from_secs(60));
        // "中文" = 6 字节
        assert!(b.push("中文").is_some());
    }
}
