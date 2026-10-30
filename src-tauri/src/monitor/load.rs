//! 服务器负载快照（看板用）。
//!
//! 和 `monitor::DiagKind` 的区别：诊断是「跑一条命令、看一张表」，
//! 看板是「一次问清所有指标、定时刷新、看趋势」。所以这里：
//!
//! 1. **一条复合命令采全部**：一次 exec 通道往返拿完，比每条指标开一次通道便宜得多
//!    （每次 exec 都是一次 channel 建立/关闭）。
//! 2. **CPU 与网络要两次采样**：`/proc/stat` 和 `/proc/net/dev` 都是累计计数器，
//!    只有差值才有意义。命令里 `sleep 0.5` 采两次，间隔用 `/proc/uptime` 算 ——
//!    不依赖 `date +%s%N`（busybox 不保证支持），也不假设 sleep 精确睡满 0.5 秒。
//! 3. **只读**：全部数据来自伪文件系统，不执行任何有副作用的命令。
//!
//! 解析全部按「标记分段 + 位置取值」，不做正则。这些格式（/proc/stat、
//! /proc/meminfo、/proc/net/dev、df -P）从 2.6 内核起就没变过，位置比正则可靠。

use std::collections::HashMap;

use serde::Serialize;

/// 采集命令。**分段标记必须独占一行**，解析靠它切段。
///
/// 注意几个刻意的写法：
/// - `sleep 0.5` 两次采样之间的间隔，靠 `/proc/uptime` 修正真实耗时
/// - `df` 排除 tmpfs/devtmpfs/overlay/squashfs：这些挂载点的使用率对判断没意义
/// - `ps` 用 `-eo` + `comm`：列少、进程名不折行（与诊断面板一致）
/// - 所有可能不存在的命令都带 `2>/dev/null`，缺了就是空段，不让整条命令失败
pub const SAMPLE_COMMAND: &str = "\
echo '@@up1'; cut -d' ' -f1 /proc/uptime 2>/dev/null; \
echo '@@stat1'; grep '^cpu' /proc/stat 2>/dev/null; \
echo '@@net1'; cat /proc/net/dev 2>/dev/null; \
sleep 0.5; \
echo '@@up2'; cut -d' ' -f1 /proc/uptime 2>/dev/null; \
echo '@@stat2'; grep '^cpu' /proc/stat 2>/dev/null; \
echo '@@net2'; cat /proc/net/dev 2>/dev/null; \
echo '@@mem'; cat /proc/meminfo 2>/dev/null; \
echo '@@load'; cat /proc/loadavg 2>/dev/null; \
echo '@@disk'; df -hP -x tmpfs -x devtmpfs -x overlay -x squashfs 2>/dev/null; \
echo '@@procs'; ps -eo pid,comm,pcpu,pmem --sort=-pcpu 2>/dev/null | head -6; \
echo '@@os'; grep '^PRETTY_NAME=' /etc/os-release 2>/dev/null; uname -sr 2>/dev/null; \
echo '@@who'; who 2>/dev/null | wc -l; \
echo '@@tcp'; (ss -tan 2>/dev/null || netstat -tan 2>/dev/null) | grep -c ESTAB";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSnapshot {
    /// 发行版名，如 `Ubuntu 24.04 LTS`；读不到为空
    pub os: String,
    /// 内核，如 `Linux 6.8.0-31-generic`
    pub kernel: String,
    /// 开机时长（秒）
    pub uptime_secs: u64,
    pub load1: f32,
    pub load5: f32,
    pub load15: f32,
    /// 可运行进程数 / 总进程数（来自 /proc/loadavg 后两段）
    pub running: u32,
    pub total_procs: u32,
    pub cpu: CpuLoad,
    pub memory: MemoryLoad,
    pub disks: Vec<DiskLoad>,
    pub nets: Vec<NetLoad>,
    pub processes: Vec<TopProc>,
    /// 登录会话数（`who` 的行数）
    pub sessions: u32,
    pub tcp_established: u32,
    /// 采样实际耗时（毫秒），前端用来解释速率的统计窗口
    pub window_ms: u64,
    /// 采集不完整时的说明（例如不是 Linux、没装 ps）。有值不代表失败
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuLoad {
    /// 总体使用率 0-100
    pub percent: f32,
    /// 每个核心的使用率，顺序与 `cpu0`/`cpu1` 一致
    pub cores: Vec<f32>,
    pub cores_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryLoad {
    pub total_kb: u64,
    pub used_kb: u64,
    pub available_kb: u64,
    pub cached_kb: u64,
    pub buffers_kb: u64,
    pub percent: f32,
    pub swap_total_kb: u64,
    pub swap_used_kb: u64,
    pub swap_percent: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskLoad {
    pub filesystem: String,
    pub mount: String,
    pub size: String,
    pub used: String,
    pub available: String,
    /// 0-100，用于前端画进度条和排序
    pub percent: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetLoad {
    pub name: String,
    /// 接收速率（字节/秒）
    pub rx_bps: f64,
    pub tx_bps: f64,
    /// 累计接收/发送字节，用于判断是不是新起来的接口
    pub rx_total: u64,
    pub tx_total: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopProc {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub mem: f32,
}

/// 解析采集输出。
///
/// 永不 panic：任何一段缺失只会让对应字段是默认值，同时在 `warning` 里说明原因。
/// 看板是「随时看一眼」的东西，为一段读不到的数据整块报错没有意义。
pub fn parse(text: &str) -> LoadSnapshot {
    let sections = split_sections(text);

    let mut warning: Option<String> = None;

    let up1 = first_f64(&sections, "up1");
    let up2 = first_f64(&sections, "up2");
    let window = match (up1, up2) {
        (Some(a), Some(b)) if b > a => b - a,
        _ => 0.5,
    };

    let cpu = parse_cpu(&sections, &mut warning);
    let memory = parse_memory(&sections, &mut warning);
    let (load1, load5, load15, running, total_procs) = parse_loadavg(&sections);
    let disks = parse_disks(&sections);
    let nets = parse_nets(&sections, window);
    let processes = parse_procs(&sections);
    let (os, kernel) = parse_os(&sections);

    if sections.get("stat1").is_none_or(|rows| rows.is_empty()) && warning.is_none() {
        warning = Some("读不到 /proc/stat —— 可能不是 Linux 系统".to_string());
    }

    LoadSnapshot {
        os,
        kernel,
        uptime_secs: up2.unwrap_or(0.0) as u64,
        load1,
        load5,
        load15,
        running,
        total_procs,
        cpu,
        memory,
        disks,
        nets,
        processes,
        sessions: first_u32(&sections, "who").unwrap_or(0),
        tcp_established: first_u32(&sections, "tcp").unwrap_or(0),
        window_ms: (window * 1000.0).round() as u64,
        warning,
    }
}

/// 把输出按 `@@name` 标记切成若干段。
///
/// 标记之外的行走属于它前面那个标记 —— 命令里每个标记恰好只输出一次。
fn split_sections(text: &str) -> HashMap<String, Vec<&str>> {
    let mut out: HashMap<String, Vec<&str>> = HashMap::new();
    let mut current: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("@@") {
            current = Some(name.to_string());
            out.entry(name.to_string()).or_default();
            continue;
        }

        if let Some(name) = &current {
            if !trimmed.is_empty() {
                out.entry(name.clone()).or_default().push(trimmed);
            }
        }
    }

    out
}

fn first_f64(sections: &HashMap<String, Vec<&str>>, name: &str) -> Option<f64> {
    sections.get(name)?.first()?.parse().ok()
}

fn first_u32(sections: &HashMap<String, Vec<&str>>, name: &str) -> Option<u32> {
    sections.get(name)?.first()?.parse().ok()
}

/// 一次 CPU 采样的累计值。
#[derive(Debug, Clone, Copy, Default)]
struct CpuTimes {
    total: u64,
    idle: u64,
}

impl CpuTimes {
    /// `/proc/stat` 的 `cpu` / `cpuN` 行：
    /// `user nice system idle iowait irq softirq steal guest guest_nice`
    ///
    /// total 只取前 8 项：guest 已经计入 user，再加会重复。
    /// idle 取 idle + iowait —— 等着 IO 的时间也是没在干活的时间。
    fn from_line(line: &str) -> Option<Self> {
        let mut parts = line.split_whitespace();
        let _label = parts.next()?;
        let values: Vec<u64> = parts.take(8).filter_map(|item| item.parse().ok()).collect();
        if values.len() < 4 {
            return None;
        }

        let idle = values[3] + values.get(4).copied().unwrap_or(0);
        Some(Self {
            total: values.iter().sum(),
            idle,
        })
    }

    /// 两帧之间算使用率。计数器重置（< 0）时当作 0，不返回负数。
    fn delta_percent(before: Self, after: Self) -> f32 {
        let total = after.total.saturating_sub(before.total);
        if total == 0 {
            return 0.0;
        }
        let idle = after.idle.saturating_sub(before.idle);
        let busy = total.saturating_sub(idle.min(total));
        (busy as f64 / total as f64 * 100.0) as f32
    }
}

fn parse_cpu(sections: &HashMap<String, Vec<&str>>, warning: &mut Option<String>) -> CpuLoad {
    let before = cpu_rows(sections.get("stat1"));
    let after = cpu_rows(sections.get("stat2"));

    if before.is_empty() || after.is_empty() {
        *warning = Some("CPU 采样不完整（/proc/stat 读不到）".to_string());
        return CpuLoad {
            percent: 0.0,
            cores: Vec::new(),
            cores_count: 0,
        };
    }

    let percent = match (before.get("cpu"), after.get("cpu")) {
        (Some(a), Some(b)) => CpuTimes::delta_percent(*a, *b),
        _ => 0.0,
    };

    // 核心按 cpu0、cpu1… 的顺序配对；两帧里对不上的核心直接跳过
    let mut core_ids: Vec<&String> = after.keys().filter(|key| *key != "cpu").collect();
    core_ids.sort_by_key(|key| key.trim_start_matches("cpu").parse::<u32>().unwrap_or(0));

    let cores: Vec<f32> = core_ids
        .into_iter()
        .filter_map(|id| {
            let a = before.get(id)?;
            let b = after.get(id)?;
            Some(CpuTimes::delta_percent(*a, *b))
        })
        .collect();

    CpuLoad {
        percent: round1(percent),
        cores_count: cores.len(),
        cores,
    }
}

fn cpu_rows(rows: Option<&Vec<&str>>) -> HashMap<String, CpuTimes> {
    let mut out = HashMap::new();
    for row in rows.into_iter().flatten() {
        let Some(label) = row.split_whitespace().next() else {
            continue;
        };
        if !label.starts_with("cpu") {
            continue;
        }
        if let Some(times) = CpuTimes::from_line(row) {
            out.insert(label.to_string(), times);
        }
    }
    out
}

fn parse_memory(sections: &HashMap<String, Vec<&str>>, warning: &mut Option<String>) -> MemoryLoad {
    let mut map: HashMap<&str, u64> = HashMap::new();
    for row in sections.get("mem").into_iter().flatten() {
        let mut parts = row.split_whitespace();
        let (Some(key), Some(value)) = (parts.next(), parts.next()) else {
            continue;
        };
        if let Ok(kb) = value.parse::<u64>() {
            map.insert(key.trim_end_matches(':'), kb);
        }
    }

    let total = map.get("MemTotal").copied().unwrap_or(0);
    let free = map.get("MemFree").copied().unwrap_or(0);
    let buffers = map.get("Buffers").copied().unwrap_or(0);
    let cached = map.get("Cached").copied().unwrap_or(0);
    // 3.14 之前没有 MemAvailable，用 Free + Buffers + Cached 近似
    let available = map
        .get("MemAvailable")
        .copied()
        .unwrap_or_else(|| free + buffers + cached);

    let swap_total = map.get("SwapTotal").copied().unwrap_or(0);
    let swap_free = map.get("SwapFree").copied().unwrap_or(0);

    if total == 0 && warning.is_none() {
        *warning = Some("读不到 /proc/meminfo".to_string());
    }

    let used = total.saturating_sub(available);
    MemoryLoad {
        total_kb: total,
        used_kb: used,
        available_kb: available,
        cached_kb: cached,
        buffers_kb: buffers,
        percent: percent_of(used, total),
        swap_total_kb: swap_total,
        swap_used_kb: swap_total.saturating_sub(swap_free),
        swap_percent: percent_of(swap_total.saturating_sub(swap_free), swap_total),
    }
}

fn parse_loadavg(sections: &HashMap<String, Vec<&str>>) -> (f32, f32, f32, u32, u32) {
    // `0.52 0.58 0.59 2/345 6789`
    let Some(row) = sections.get("load").and_then(|rows| rows.first()) else {
        return (0.0, 0.0, 0.0, 0, 0);
    };

    let parts: Vec<&str> = row.split_whitespace().collect();
    let load1 = parts.first().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let load5 = parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let load15 = parts.get(2).and_then(|v| v.parse().ok()).unwrap_or(0.0);

    // 第四段是 `running/total`，分不清就都是 0
    let (running, total) = parts
        .get(3)
        .and_then(|v| v.split_once('/'))
        .map(|(a, b)| (a.parse::<u32>().unwrap_or(0), b.parse::<u32>().unwrap_or(0)))
        .unwrap_or((0, 0));

    (load1, load5, load15, running, total)
}

fn parse_disks(sections: &HashMap<String, Vec<&str>>) -> Vec<DiskLoad> {
    let mut out: Vec<DiskLoad> = Vec::new();

    for row in sections.get("disk").into_iter().flatten() {
        let mut parts = row.split_whitespace();
        let (Some(filesystem), Some(size), Some(used), Some(available), Some(capacity)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };

        // 表头行（Filesystem Size Used ...）
        if filesystem == "Filesystem" {
            continue;
        }

        // 挂载点可能带空格，剩下的全部拼回去
        let mount = parts.collect::<Vec<_>>().join(" ");
        if mount.is_empty() {
            continue;
        }

        out.push(DiskLoad {
            filesystem: filesystem.to_string(),
            mount,
            size: size.to_string(),
            used: used.to_string(),
            available: available.to_string(),
            percent: capacity.trim_end_matches('%').parse().unwrap_or(0),
        });
    }

    // 使用率高的排前面：打开就想看哪个快满了
    out.sort_by_key(|disk| std::cmp::Reverse(disk.percent));
    out
}

/// `/proc/net/dev` 两行表头之后是每行一个接口：
/// `eth0: rx_bytes packets ... tx_bytes ...`（冒号前是接口名）
fn parse_net_rows(rows: Option<&Vec<&str>>) -> HashMap<String, (u64, u64)> {
    let mut out = HashMap::new();

    for row in rows.into_iter().flatten() {
        let Some((name, rest)) = row.split_once(':') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // 接收 8 项 + 发送 8 项，取第 1 项和第 9 项
        let (Some(rx), Some(tx)) = (fields.first(), fields.get(8)) else {
            continue;
        };
        let (Ok(rx), Ok(tx)) = (rx.parse::<u64>(), tx.parse::<u64>()) else {
            continue;
        };
        out.insert(name.trim().to_string(), (rx, tx));
    }

    out
}

fn parse_nets(sections: &HashMap<String, Vec<&str>>, window: f64) -> Vec<NetLoad> {
    let before = parse_net_rows(sections.get("net1"));
    let after = parse_net_rows(sections.get("net2"));

    let mut out: Vec<NetLoad> = after
        .into_iter()
        .map(|(name, (rx, tx))| {
            let (rx0, tx0) = before.get(&name).copied().unwrap_or((rx, tx));
            // 计数据回绕或接口重建时 after < before，按 0 处理，不出现过大的负值
            let rx_diff = rx.saturating_sub(rx0) as f64;
            let tx_diff = tx.saturating_sub(tx0) as f64;

            NetLoad {
                name,
                rx_bps: rx_diff / window,
                tx_bps: tx_diff / window,
                rx_total: rx,
                tx_total: tx,
            }
        })
        .collect();

    // 有流量的排前面，lo 这类零流量的自然沉底
    out.sort_by(|a, b| {
        (b.rx_bps + b.tx_bps)
            .partial_cmp(&(a.rx_bps + a.tx_bps))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn parse_procs(sections: &HashMap<String, Vec<&str>>) -> Vec<TopProc> {
    let mut out = Vec::new();

    for row in sections.get("procs").into_iter().flatten() {
        let mut parts = row.split_whitespace();
        let (Some(pid), Some(name), Some(cpu), Some(mem)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };

        // 表头行
        let (Ok(pid), Ok(cpu), Ok(mem)) =
            (pid.parse::<u32>(), cpu.parse::<f32>(), mem.parse::<f32>())
        else {
            continue;
        };

        out.push(TopProc {
            pid,
            name: name.to_string(),
            cpu,
            mem,
        });
    }

    out
}

fn parse_os(sections: &HashMap<String, Vec<&str>>) -> (String, String) {
    let rows = sections.get("os");
    let mut os = String::new();
    let mut kernel = String::new();

    for row in rows.into_iter().flatten() {
        if let Some(value) = row.strip_prefix("PRETTY_NAME=") {
            os = value.trim_matches('"').to_string();
        } else if row.starts_with("Linux ") {
            kernel = row.to_string();
        }
    }

    (os, kernel)
}

fn percent_of(part: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    round1((part as f64 / total as f64 * 100.0) as f32)
}

fn round1(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两帧之间：total 增长 200，idle 增长 100 → 忙 50%
    const STAT1: &str = "\
cpu  100 0 100 800 0 0 0 0 0 0
cpu0 50 0 50 400 0 0 0 0 0 0
cpu1 50 0 50 400 0 0 0 0 0 0";

    const STAT2: &str = "\
cpu  200 0 150 900 0 0 0 0 0 0
cpu0 100 0 75 450 0 0 0 0 0 0
cpu1 100 0 75 450 0 0 0 0 0 0";

    const MEMINFO: &str = "\
MemTotal:        8000000 kB
MemFree:          500000 kB
MemAvailable:    2000000 kB
Buffers:          100000 kB
Cached:          1500000 kB
SwapTotal:       2000000 kB
SwapFree:        1500000 kB";

    const NET1: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:    1000      10    0    0    0     0          0         0     1000      10    0    0    0     0       0          0
  eth0: 5000000    5000    0    0    0     0          0         0  2000000    2000    0    0    0     0       0          0";

    const NET2: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:    1000      10    0    0    0     0          0         0     1000      10    0    0    0     0       0          0
  eth0: 6000000    6000    0    0    0     0          0         0  3000000    3000    0    0    0     0       0          0";

    fn sample(stat1: &str, stat2: &str, net1: &str, net2: &str) -> String {
        format!(
            "@@up1\n1000.00 2000.00\n@@stat1\n{stat1}\n@@net1\n{net1}\n\
             @@up2\n1000.50 2001.00\n@@stat2\n{stat2}\n@@net2\n{net2}\n\
             @@mem\n{MEMINFO}\n@@load\n0.52 0.58 0.59 3/345 6789\n\
             @@disk\nFilesystem Size Used Avail Capacity Mounted on\n\
             /dev/vda1 40G 12G 26G 32% /\n/dev/vdb1 100G 85G 10G 85% /data\n\
             @@procs\nPID COMMAND %CPU %MEM\n 4211 nginx 12.5 3.1\n  812 dockerd 4.0 1.2\n\
             @@os\nPRETTY_NAME=\"Ubuntu 24.04 LTS\"\nLinux 6.8.0-31-generic\n\
             @@who\n2\n@@tcp\n17\n"
        )
    }

    #[test]
    fn cpu_percent_comes_from_two_samples() {
        let snapshot = parse(&sample(STAT1, STAT2, NET1, NET2));

        // 总 total 从 1000 → 1250（+250），idle 从 800 → 900（+100）→ 忙 60%
        assert_eq!(snapshot.cpu.percent, 60.0);
        assert_eq!(snapshot.cpu.cores_count, 2);
        assert_eq!(snapshot.cpu.cores.len(), 2);
        // 每核 total +125，idle +50 → 忙 60%
        assert_eq!(snapshot.cpu.cores[0], 60.0);
        assert_eq!(snapshot.window_ms, 500, "采样窗口来自 /proc/uptime 的差值");
    }

    #[test]
    fn cpu_delta_never_goes_negative_on_counter_reset() {
        // 第二帧比第一帧小：只能当 0，不能出现负数使用率
        let snapshot = parse(&sample(STAT2, STAT1, NET1, NET2));
        assert!(snapshot.cpu.percent >= 0.0, "{}", snapshot.cpu.percent);
    }

    #[test]
    fn memory_uses_available_not_free() {
        let snapshot = parse(&sample(STAT1, STAT2, NET1, NET2));

        assert_eq!(snapshot.memory.total_kb, 8_000_000);
        // used = total - MemAvailable = 6_000_000
        assert_eq!(snapshot.memory.used_kb, 6_000_000);
        assert_eq!(snapshot.memory.percent, 75.0);
        assert_eq!(
            snapshot.memory.swap_used_kb, 500_000,
            "swap 已用 = total - free"
        );
        assert_eq!(snapshot.memory.swap_percent, 25.0);
    }

    #[test]
    fn memory_falls_back_when_available_is_missing() {
        // 3.14 之前的内核没有 MemAvailable
        let old = "\
MemTotal:        8000000 kB
MemFree:          500000 kB
Buffers:          100000 kB
Cached:          1500000 kB";
        let text = format!(
            "@@up1\n1000.00\n@@stat1\n{STAT1}\n@@net1\n{NONE}\n@@up2\n1000.50\n\
             @@stat2\n{STAT2}\n@@net2\n{NONE}\n@@mem\n{old}\n",
            NONE = ""
        );

        let snapshot = parse(&text);
        assert_eq!(snapshot.memory.total_kb, 8_000_000);
        assert_eq!(
            snapshot.memory.available_kb, 2_100_000,
            "没有 MemAvailable 时用 Free + Buffers + Cached"
        );
    }

    #[test]
    fn network_rates_are_per_second_over_the_sample_window() {
        let snapshot = parse(&sample(STAT1, STAT2, NET1, NET2));

        let eth0 = snapshot
            .nets
            .iter()
            .find(|item| item.name == "eth0")
            .expect("应当有 eth0");
        // 0.5 秒内收 1_000_000 字节 → 2_000_000 B/s
        assert!((eth0.rx_bps - 2_000_000.0).abs() < 1.0, "{}", eth0.rx_bps);
        assert!((eth0.tx_bps - 2_000_000.0).abs() < 1.0);

        assert_eq!(snapshot.nets[0].name, "eth0", "有流量的接口排前面，lo 在后");
    }

    #[test]
    fn network_rate_is_zero_when_counters_reset() {
        let snapshot = parse(&sample(STAT1, STAT2, NET2, NET1));
        let eth0 = snapshot
            .nets
            .iter()
            .find(|item| item.name == "eth0")
            .unwrap();
        assert_eq!(eth0.rx_bps, 0.0);
        assert_eq!(eth0.tx_bps, 0.0);
    }

    #[test]
    fn load_average_is_split_into_three_windows() {
        let snapshot = parse(&sample(STAT1, STAT2, NET1, NET2));

        assert_eq!(snapshot.load1, 0.52);
        assert_eq!(snapshot.load5, 0.58);
        assert_eq!(snapshot.load15, 0.59);
        assert_eq!(snapshot.running, 3);
        assert_eq!(snapshot.total_procs, 345);
    }

    #[test]
    fn disks_are_sorted_by_usage_and_keep_the_mount_point() {
        let snapshot = parse(&sample(STAT1, STAT2, NET1, NET2));

        assert_eq!(snapshot.disks.len(), 2);
        assert_eq!(snapshot.disks[0].mount, "/data", "使用率高的排前面");
        assert_eq!(snapshot.disks[0].percent, 85);
        assert_eq!(snapshot.disks[1].mount, "/");
    }

    #[test]
    fn os_processes_and_counts_are_parsed() {
        let snapshot = parse(&sample(STAT1, STAT2, NET1, NET2));

        assert_eq!(snapshot.os, "Ubuntu 24.04 LTS", "引号要去掉");
        assert_eq!(snapshot.kernel, "Linux 6.8.0-31-generic");
        assert_eq!(snapshot.processes.len(), 2);
        assert_eq!(snapshot.processes[0].name, "nginx");
        assert_eq!(snapshot.processes[0].cpu, 12.5);
        assert_eq!(snapshot.sessions, 2);
        assert_eq!(snapshot.tcp_established, 17);
    }

    #[test]
    fn missing_sections_degrade_instead_of_panicking() {
        // 典型的非 Linux 机器：/proc 什么都没有
        let snapshot = parse("@@up1\n\n@@up2\n");

        assert_eq!(snapshot.cpu.percent, 0.0);
        assert_eq!(snapshot.memory.total_kb, 0);
        assert!(snapshot.disks.is_empty());
        assert!(snapshot.warning.is_some(), "读不到 /proc/stat 要说明原因");
    }

    #[test]
    fn garbage_lines_are_skipped_without_inventing_values() {
        let text = "\
@@stat1
这不是 cpu 行
cpu  1 2 3 4
@@stat2
cpu  bad values here
@@mem
乱码
@@disk
只 有 三 列
@@procs
PID COMMAND %CPU %MEM";

        let snapshot = parse(text);
        assert_eq!(snapshot.cpu.percent, 0.0);
        assert!(snapshot.disks.is_empty());
        assert!(snapshot.processes.is_empty(), "表头行不能被当成进程");
        assert_eq!(snapshot.memory.total_kb, 0);
    }

    #[test]
    fn sample_command_is_read_only() {
        // 看板每隔几秒跑一次，绝不能混进有副作用的命令。
        // 先把 `2>/dev/null`、`2>&1` 去掉再查 `>` —— 丢弃错误输出不是写文件，
        // 一开始没剔掉，这条测试自己报了假警。
        let sanitized = SAMPLE_COMMAND
            .replace("2>/dev/null", "")
            .replace("2>&1", "");

        for forbidden in [
            "rm ",
            "kill",
            "systemctl",
            "apt",
            ">",
            ">>",
            "mkfs",
            "shutdown",
        ] {
            assert!(
                !sanitized.contains(forbidden),
                "采集命令里出现了 {forbidden}，看板必须是只读的"
            );
        }
    }

    #[test]
    fn sample_command_is_a_single_line_without_continuation_artifacts() {
        // 命令里混进换行 = exec 只执行第一行，后面全部静默丢失；
        // 反斜杠残留则是行续接没被 Rust 吃掉，症状同样是「一部分指标永远空」。
        assert_eq!(SAMPLE_COMMAND.lines().count(), 1, "必须是一条单行命令");
        assert!(!SAMPLE_COMMAND.contains('\\'), "不该残留行续接的反斜杠");
    }

    #[test]
    fn sample_command_has_every_marker_the_parser_expects() {
        // 少一个标记 → 对应字段永远是默认值，而且没人会发现
        for marker in [
            "@@up1", "@@stat1", "@@net1", "@@up2", "@@stat2", "@@net2", "@@mem", "@@load",
            "@@disk", "@@procs", "@@os", "@@who", "@@tcp",
        ] {
            assert!(
                SAMPLE_COMMAND.contains(marker),
                "采集命令缺少分段标记 {marker}"
            );
        }
    }
}
