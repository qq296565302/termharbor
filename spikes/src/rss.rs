//! 进程 RSS 采样（Windows / macOS / Linux 通用）。
//!
//! S2 要证明"慢消费者不会让内存无限涨"，所以需要一个真实的内存读数，
//! 而不只是队列深度这种间接指标。

use sysinfo::{Pid, ProcessesToUpdate, System};

pub struct RssSampler {
    sys: System,
    pid: Pid,
}

impl RssSampler {
    pub fn new() -> Self {
        let pid = Pid::from_u32(std::process::id());
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        Self { sys, pid }
    }

    /// 返回当前进程 RSS（字节）。
    pub fn sample(&mut self) -> u64 {
        self.sys
            .refresh_processes(ProcessesToUpdate::Some(&[self.pid]), true);
        self.sys.process(self.pid).map(|p| p.memory()).unwrap_or(0)
    }
}

impl Default for RssSampler {
    fn default() -> Self {
        Self::new()
    }
}

pub fn fmt_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    format!("{:.1} MB", bytes as f64 / MB)
}
