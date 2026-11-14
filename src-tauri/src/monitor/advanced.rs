//! 高级监控快照（对标 FinalShell 专业版）。
//!
//! 和 `monitor::load`（看板）的区别：看板只关心几个趋势数字，这里要一次把
//! 「系统信息 / 网卡 / 全量进程 / 端口连接」四张表都交给前端，让用户不用再逐条敲命令。
//!
//! 设计约束（都是被真实运维环境逼出来的）：
//!
//! 1. **一条复合命令采全部**：一次 exec 通道往返拿完。每条指标各开一次 channel 的话，
//!    光握手就是十几倍开销，而且中间服务器状态变了，四张表会对不上。命令用
//!    `echo '@@xxx'` 分段，和 `monitor::load` 保持同一套切段风格。
//! 2. **工具缺失只降级、不整块失败**：`ss` / `netstat` / `ip` / `ifconfig` / `ps` /
//!    `systemd-detect-virt` 在精简镜像、busybox、容器里都可能没有。命令里所有可选工具
//!    都带 `2>/dev/null`，缺了就是空段；`@@cap` 段专门探测工具是否存在，用于生成
//!    `warnings` 里的中文提示。解析永远不因为「某段是空的」而报错或 panic。
//! 3. **解析靠位置、不靠正则**：`df -kP` / `ps -eo` / `ss` / `ip -o addr` /
//!    `/proc/net/dev` 的格式几十年没变，按列取值比正则稳。每一列都用
//!    `parse().unwrap_or(default)`，读不到就是 0 / 空串，绝不 `unwrap()`。
//! 4. **列表有安全上限**：进程和连接最多各 2000 条。远端 `/proc` 被挂马或输出被截断时，
//!    不让几十万行文本被解析成 DTO 打爆内存。
//! 5. **路径补全杜绝注入**：远端目录名交给 shell 之前必须用单引号转义函数包住，
//!    加 `--` 防止以 `-` 开头的名字被当成选项；换行直接拒绝，否则一行一条输出会错位。

use std::collections::{HashMap, HashSet};

use serde::Serialize;

/// 进程与连接列表的安全上限。超过就截断，避免异常输出把内存吃光。
const MAX_PROCESSES: usize = 2000;
const MAX_CONNECTIONS: usize = 2000;

/// 采集命令。**分段标记必须独占一行**，解析靠它切段。
///
/// 几个刻意的写法：
/// - 全部用 `;` 串联而不是 `&&`：任何一条工具不存在都不能阻断后面的采集。
/// - 可选命令一律 `2>/dev/null`：错误输出（command not found / Permission denied）
///   混进数据段会把按列解析搅乱，宁可空段也不要脏行。
/// - `@@cap` 单独探测工具是否存在，这样 `warnings` 才能说清「是没装 ss」还是
///   「装了 ss 但这台机器现在没有连接」，两者给人的行动建议完全不同。
/// - `ss -tulnp || netstat -tulnp`：ss 存在但退出非 0 时才降级 netstat，
///   不会把两份输出叠在一起产生重复行。
/// - `/proc/cpuinfo` 的 flags 行用来兜底判断 hypervisor：容器里常常没有
///   `systemd-detect-virt`，但 CPU flags 里一定有 hypervisor 标记。
/// - 命令里不出现裸换行、不出现会伪装成分段标记的 `@@` 数据行，
///   也不混入任何有副作用的命令（采集是只读的）。
pub const SAMPLE_COMMAND: &str = "\
echo '@@host'; hostname 2>/dev/null || cat /proc/sys/kernel/hostname 2>/dev/null; \
echo '@@os'; grep -E '^(PRETTY_NAME|NAME|VERSION)=' /etc/os-release 2>/dev/null; \
echo '@@kernel'; uname -sr 2>/dev/null; \
echo '@@arch'; uname -m 2>/dev/null; \
echo '@@cpu'; grep -m1 '^model name' /proc/cpuinfo 2>/dev/null; grep -c '^processor' /proc/cpuinfo 2>/dev/null; grep -m1 '^flags' /proc/cpuinfo 2>/dev/null; \
echo '@@virt'; systemd-detect-virt 2>/dev/null; \
echo '@@uptime'; cat /proc/uptime 2>/dev/null; \
echo '@@mem'; grep -E '^(MemTotal|SwapTotal):' /proc/meminfo 2>/dev/null; \
echo '@@load'; cat /proc/loadavg 2>/dev/null; \
echo '@@who'; who 2>/dev/null | wc -l; \
echo '@@disk'; df -kP 2>/dev/null; \
echo '@@netdev'; cat /proc/net/dev 2>/dev/null; \
echo '@@ipaddr'; ip -o addr 2>/dev/null; \
echo '@@ifconfig'; ifconfig -a 2>/dev/null; \
echo '@@sysnet'; grep -H '' /sys/class/net/*/address /sys/class/net/*/operstate 2>/dev/null; \
echo '@@cap'; if command -v ss >/dev/null 2>&1; then echo 'ss=yes'; else echo 'ss=no'; fi; if command -v netstat >/dev/null 2>&1; then echo 'netstat=yes'; else echo 'netstat=no'; fi; if command -v ip >/dev/null 2>&1; then echo 'ip=yes'; else echo 'ip=no'; fi; \
echo '@@sslisten'; ss -tulnp 2>/dev/null || netstat -tulnp 2>/dev/null; \
echo '@@ssall'; ss -tanp 2>/dev/null || netstat -tanp 2>/dev/null; \
echo '@@ps'; ps -eo pid,ppid,user,stat,pcpu,pmem,rss,etime,args --sort=-pcpu 2>/dev/null";

// ---------------------------------------------------------------- DTO

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdvancedSnapshot {
    pub info: SystemInfo,
    pub interfaces: Vec<NetInterface>,
    /// 完整进程列表，不截断（超过 2000 条时取前 2000 条安全上限）
    pub processes: Vec<ProcessInfo>,
    /// 监听 + 已建立 + 其它，最多 2000 条
    pub connections: Vec<NetConnection>,
    /// 工具缺失等降级说明，例如「服务器没有 ss」「ip 命令不可用」
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub hostname: String,
    pub os_name: String,
    pub kernel: String,
    pub arch: String,
    pub cpu_model: String,
    pub cpu_cores: u32,
    pub uptime_sec: u64,
    pub total_mem_kb: u64,
    pub total_swap_kb: u64,
    pub load_avg: Vec<f64>,
    pub process_count: u32,
    pub logged_users: u32,
    pub virtualization: String,
    pub disks: Vec<DiskInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskInfo {
    pub filesystem: String,
    pub mount: String,
    pub size_kb: u64,
    pub used_kb: u64,
    pub avail_kb: u64,
    pub use_percent: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetInterface {
    pub name: String,
    pub ipv4: String,
    pub ipv6: String,
    pub mac: String,
    pub state: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub user: String,
    pub stat: String,
    pub cpu_percent: f64,
    pub mem_percent: f64,
    pub mem_kb: u64,
    pub elapsed: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetConnection {
    pub proto: String,
    pub state: String,
    pub local_addr: String,
    pub remote_addr: String,
    pub pid: u32,
    pub process: String,
    /// "listen" | "established" | "other"
    pub kind: String,
}

// ---------------------------------------------------------------- 入口

/// 解析采集输出。
///
/// 永不 panic：任何一段缺失只会让对应字段是默认值，同时在 `warnings` 里说明原因。
/// 这是给用户「随时看一眼」的面板，为一段读不到的数据整块报错没有意义。
pub fn parse(raw: &str) -> AdvancedSnapshot {
    let sections = split_sections(raw);

    // 进程列表先算出来：连接的「按 pid 关联进程名」要复用它，避免解析两遍。
    let processes = parse_processes(&sections);
    // process_count 独立数一遍有效行，不受 2000 条展示上限影响。
    let process_count = rows(&sections, "ps")
        .iter()
        .filter(|row| {
            row.split_whitespace()
                .next()
                .and_then(|value| value.parse::<u32>().ok())
                .is_some()
        })
        .count() as u32;

    AdvancedSnapshot {
        info: parse_info(&sections, process_count),
        interfaces: parse_interfaces(&sections),
        connections: parse_connections(&sections, &processes),
        processes,
        warnings: build_warnings(&sections),
    }
}

/// 把输出按 `@@name` 标记切成若干段。
///
/// 标记之外的行属于它前面那个标记。这里用拥有所有权的 `String` 而不是 `&str`，
/// 是为了让后面所有解析函数不必和生命周期搏斗 —— 快照一次才几百行，多分配一点无关紧要。
/// 空行直接丢弃：工具缺失时 `echo` 之间会留下空行，留着只会让「第一段取值」取到空串。
type Sections = HashMap<String, Vec<String>>;

fn split_sections(text: &str) -> Sections {
    let mut out: Sections = HashMap::new();
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
                out.entry(name.clone())
                    .or_default()
                    .push(trimmed.to_string());
            }
        }
    }

    out
}

fn rows<'a>(sections: &'a Sections, name: &str) -> &'a [String] {
    sections.get(name).map(|v| v.as_slice()).unwrap_or(&[])
}

fn first_line<'a>(sections: &'a Sections, name: &str) -> Option<&'a str> {
    rows(sections, name).first().map(|s| s.as_str())
}

fn first_tokens<'a>(sections: &'a Sections, name: &str) -> Vec<&'a str> {
    first_line(sections, name)
        .map(|s| s.split_whitespace().collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- 系统信息

fn parse_info(sections: &Sections, process_count: u32) -> SystemInfo {
    let (cpu_model, cpu_cores, hypervisor) = parse_cpu(sections);
    let (total_mem_kb, total_swap_kb) = parse_mem(sections);

    SystemInfo {
        hostname: first_line(sections, "host").unwrap_or("").to_string(),
        os_name: parse_os_name(sections),
        kernel: first_line(sections, "kernel").unwrap_or("").to_string(),
        arch: first_line(sections, "arch").unwrap_or("").to_string(),
        cpu_model,
        cpu_cores,
        // /proc/uptime 是「开机秒数 空闲秒数」两个数，只能取第一个 token。
        uptime_sec: first_tokens(sections, "uptime")
            .first()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(0.0) as u64,
        total_mem_kb,
        total_swap_kb,
        load_avg: parse_load_avg(sections),
        process_count,
        logged_users: first_line(sections, "who")
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0),
        virtualization: parse_virt(sections, hypervisor),
        disks: parse_disks(sections),
    }
}

fn parse_os_name(sections: &Sections) -> String {
    let mut name = String::new();
    let mut version = String::new();

    for row in rows(sections, "os") {
        // PRETTY_NAME 已经把发行版和版本拼好了，优先用它。
        if let Some(value) = row.strip_prefix("PRETTY_NAME=") {
            return value.trim_matches('"').to_string();
        }
        if let Some(value) = row.strip_prefix("NAME=") {
            name = value.trim_matches('"').to_string();
        }
        if let Some(value) = row.strip_prefix("VERSION=") {
            version = value.trim_matches('"').to_string();
        }
    }

    // 老系统没有 PRETTY_NAME，就用 NAME + VERSION 拼一个可读名字；都没有就空串。
    if name.is_empty() {
        return String::new();
    }
    if version.is_empty() {
        name
    } else {
        format!("{name} {version}")
    }
}

/// 从 `@@cpu` 段取 CPU 型号、核数、以及是否带 hypervisor 标记。
///
/// 这一段故意混了三种行：`model name : ...`（有冒号）、`grep -c` 的纯数字（无冒号）、
/// `flags : ...`（有冒号且很长）。按「有没有冒号 + 纯数字」区分，比再开三段采集省事。
fn parse_cpu(sections: &Sections) -> (String, u32, bool) {
    let mut model = String::new();
    let mut cores = 0u32;
    let mut hypervisor = false;

    for row in rows(sections, "cpu") {
        let line = row.as_str();
        if let Some((key, value)) = line.split_once(':') {
            match key.trim() {
                "model name" => model = value.trim().to_string(),
                // ARM 用 Features 而不是 flags，两个都认。
                "flags" | "Features" => {
                    hypervisor = value.split_whitespace().any(|flag| flag == "hypervisor");
                }
                _ => {}
            }
        } else if let Ok(count) = line.trim().parse::<u32>() {
            cores = count;
        }
    }

    (model, cores, hypervisor)
}

fn parse_mem(sections: &Sections) -> (u64, u64) {
    let mut total = 0u64;
    let mut swap = 0u64;

    for row in rows(sections, "mem") {
        let mut parts = row.split_whitespace();
        let (Some(key), Some(value)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(kb) = value.parse::<u64>() else {
            continue;
        };
        match key.trim_end_matches(':') {
            "MemTotal" => total = kb,
            "SwapTotal" => swap = kb,
            _ => {}
        }
    }

    (total, swap)
}

/// `cat /proc/loadavg`：`0.52 0.58 0.59 2/345 6789`。
/// 固定返回三个值，缺的补 0，让前端不必处理长度变化。
fn parse_load_avg(sections: &Sections) -> Vec<f64> {
    let tokens = first_tokens(sections, "load");
    (0..3)
        .map(|i| {
            tokens
                .get(i)
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or(0.0)
        })
        .collect()
}

/// 虚拟化：优先 `systemd-detect-virt` 的具体值（kvm/docker/...），
/// 没有就用 CPU flags 的 hypervisor 标记兜底，什么都没有如实写 unknown。
fn parse_virt(sections: &Sections, hypervisor: bool) -> String {
    let virt_rows = rows(sections, "virt");
    for row in virt_rows {
        let value = row.trim();
        if value.is_empty() || value == "none" {
            continue;
        }
        return value.to_string();
    }

    if hypervisor {
        return "hypervisor".to_string();
    }
    if virt_rows.iter().any(|row| row.trim() == "none") {
        "none".to_string()
    } else {
        "unknown".to_string()
    }
}

/// `df -kP`：
/// `Filesystem 1024-blocks Used Available Capacity Mounted on`
/// `/dev/vda1 41943040 12582912 27262976 30% /`
///
/// 挂载点可能带空格，前 5 列取完后剩下的全部拼回去。
fn parse_disks(sections: &Sections) -> Vec<DiskInfo> {
    let mut out = Vec::new();

    for row in rows(sections, "disk") {
        let mut parts = row.split_whitespace();
        let (Some(filesystem), Some(size), Some(used), Some(avail), Some(capacity)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        if filesystem == "Filesystem" {
            continue;
        }
        let mount = parts.collect::<Vec<_>>().join(" ");
        if mount.is_empty() {
            continue;
        }

        out.push(DiskInfo {
            filesystem: filesystem.to_string(),
            mount,
            size_kb: size.parse::<u64>().unwrap_or(0),
            used_kb: used.parse::<u64>().unwrap_or(0),
            avail_kb: avail.parse::<u64>().unwrap_or(0),
            use_percent: capacity.trim_end_matches('%').parse::<f64>().unwrap_or(0.0),
        });
    }

    out
}

// ---------------------------------------------------------------- 网卡

/// 组装网卡表用的小工具：按名字找到（或新建）一行，保持 `/proc/net/dev` 的原始顺序。
struct IfaceBuilder {
    list: Vec<NetInterface>,
    index: HashMap<String, usize>,
}

impl IfaceBuilder {
    fn new() -> Self {
        Self {
            list: Vec::new(),
            index: HashMap::new(),
        }
    }

    fn slot(&mut self, name: &str) -> &mut NetInterface {
        if let Some(&i) = self.index.get(name) {
            return &mut self.list[i];
        }
        let i = self.list.len();
        self.index.insert(name.to_string(), i);
        self.list.push(NetInterface {
            name: name.to_string(),
            ipv4: String::new(),
            ipv6: String::new(),
            mac: String::new(),
            state: String::new(),
            rx_bytes: 0,
            tx_bytes: 0,
            rx_errors: 0,
            tx_errors: 0,
        });
        &mut self.list[i]
    }
}

fn append_addr(target: &mut String, value: &str) {
    if value.is_empty() {
        return;
    }
    if target.is_empty() {
        *target = value.to_string();
    } else if !target.split(", ").any(|item| item == value) {
        target.push_str(", ");
        target.push_str(value);
    }
}

/// 网卡信息来自四个来源，按「谁最可靠谁先填」的顺序合并：
/// 1. `/proc/net/dev` —— 接口名单 + 累计字节/错误包，Linux 一定有，作为名单基准；
/// 2. `ip -o addr` —— IPv4/IPv6；
/// 3. `ifconfig -a` —— 仅当没有 ip 时降级使用（best-effort）；
/// 4. `/sys/class/net/*/{address,operstate}` —— MAC 与 up/down 状态。
///
/// 任何一段缺失都只是让对应字段为空，其他来源照常出结果。
fn parse_interfaces(sections: &Sections) -> Vec<NetInterface> {
    let mut builder = IfaceBuilder::new();

    // 1) /proc/net/dev：每行 `eth0: rx_bytes packets errs ... tx_bytes ... errs ...`
    //    接收 8 项、发送 8 项，取第 0/2 与第 8/10 项。表头两行没有冒号，自然跳过。
    for row in rows(sections, "netdev") {
        let Some((name, rest)) = row.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 11 {
            continue;
        }
        let slot = builder.slot(name);
        slot.rx_bytes = fields[0].parse::<u64>().unwrap_or(0);
        slot.rx_errors = fields[2].parse::<u64>().unwrap_or(0);
        slot.tx_bytes = fields[8].parse::<u64>().unwrap_or(0);
        slot.tx_errors = fields[10].parse::<u64>().unwrap_or(0);
    }

    // 2) ip -o addr：`2: eth0  inet6 2001:db8::1/64 scope global ...`
    //    只取第 1/2/3 个 token，`-o` 把续行折成一行，末尾残留的反斜杠不影响。
    let mut ip_had_rows = false;
    for row in rows(sections, "ipaddr") {
        let parts: Vec<&str> = row.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }
        let name = parts[1];
        if name.is_empty() {
            continue;
        }
        ip_had_rows = true;
        let slot = builder.slot(name);
        match parts[2] {
            "inet" => append_addr(&mut slot.ipv4, parts[3]),
            "inet6" => append_addr(&mut slot.ipv6, parts[3]),
            _ => {}
        }
    }

    // 3) 没有 ip 命令时降级 ifconfig。再不济也至少有 `/proc/net/dev` 给出的名字，
    //    符合「只填名字」的最低要求，不会交白卷。
    if !ip_had_rows {
        apply_ifconfig(sections, &mut builder);
    }

    // 4) /sys/class/net：grep -H 输出 `路径:内容`，路径倒数第二段是网卡名、
    //    最后一段是 address/operstate。注意 MAC 本身含冒号，所以只在第一个冒号处切。
    for row in rows(sections, "sysnet") {
        let Some((path, value)) = row.split_once(':') else {
            continue;
        };
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() < 2 {
            continue;
        }
        let name = parts[parts.len() - 2];
        let field = parts[parts.len() - 1];
        if name.is_empty() {
            continue;
        }
        let slot = builder.slot(name);
        // 用 match guard 而不是 arm 里再套一层 if：/sys 的 address 段可能重复出现，
        // 只为第一个非空值赋值，符号链接别名不应该覆盖真实 MAC。
        match field {
            "address" if slot.mac.is_empty() => slot.mac = value.to_string(),
            "operstate" => slot.state = value.to_string(),
            _ => {}
        }
    }

    builder.list
}

/// ifconfig 降级解析（best-effort）。
///
/// 两种常见格式：`eth0: flags=...` 与 `eth0  Link encap:Ethernet`，标题行靠
/// `flags=` / `Link encap` 认；地址行靠 `inet ` / `inet addr:` / `inet6 ` 前缀认。
/// 因为 `split_sections` 会 trim 掉缩进，不能用「是否以空白开头」区分标题行，
/// 只能按内容特征判断。认不出来就只留网卡名，不编造地址。
fn apply_ifconfig(sections: &Sections, builder: &mut IfaceBuilder) {
    let mut current = String::new();

    for row in rows(sections, "ifconfig") {
        let line = row.trim();

        if line.contains("flags=") || line.contains("Link encap") {
            current = line
                .split(|ch: char| ch == ':' || ch.is_whitespace())
                .next()
                .unwrap_or("")
                .to_string();
            if !current.is_empty() {
                builder.slot(&current);
            }
            continue;
        }
        if current.is_empty() {
            continue;
        }

        let slot = builder.slot(&current);
        // busybox 写 `inet addr:10.0.0.2`，新版写 `inet 10.0.0.2`；
        // 必须先试带 addr: 的，否则会被 `inet ` 先匹配成 `addr:10.0.0.2`。
        if let Some(rest) = line
            .strip_prefix("inet addr:")
            .or_else(|| line.strip_prefix("inet "))
        {
            if let Some(addr) = rest.split_whitespace().next() {
                append_addr(&mut slot.ipv4, addr);
            }
        } else if let Some(rest) = line
            .strip_prefix("inet6 addr:")
            .or_else(|| line.strip_prefix("inet6 "))
        {
            if let Some(addr) = rest.split_whitespace().next() {
                append_addr(&mut slot.ipv6, addr);
            }
        } else if let Some(rest) = line.strip_prefix("ether ") {
            if slot.mac.is_empty() {
                slot.mac = rest.split_whitespace().next().unwrap_or("").to_string();
            }
        } else if let Some(pos) = line.find("HWaddr ") {
            if slot.mac.is_empty() {
                slot.mac = line[pos + 7..]
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string();
            }
        }
    }
}

// ---------------------------------------------------------------- 进程

/// 把一行按「前 fixed 列各占一个 token，剩下的整体是最后一列」切开。
///
/// `ps -eo ... args` 的 args 含空格，用 `split_whitespace().collect().join(" ")` 会
/// 把多空格压成一个空格。这里改成扫到前 8 列的结束位置，直接取原串剩余部分，
/// 命令行的原始间距才不会被改写。列数不够返回 None，交给调用方跳过该行。
fn split_command_columns(line: &str, fixed: usize) -> Option<(Vec<&str>, &str)> {
    let mut tokens = Vec::with_capacity(fixed);
    let mut rest = line.trim_start();

    for _ in 0..fixed {
        let end = rest
            .find(|ch: char| ch.is_whitespace())
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        tokens.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }

    Some((tokens, rest.trim_end()))
}

/// `ps -eo pid,ppid,user,stat,pcpu,pmem,rss,etime,args --sort=-pcpu`
///
/// 表头行第一列是 `PID`，`parse::<u32>()` 失败就被跳过，不会混进进程列表。
fn parse_processes(sections: &Sections) -> Vec<ProcessInfo> {
    let mut out = Vec::new();

    for row in rows(sections, "ps") {
        if out.len() >= MAX_PROCESSES {
            break;
        }
        let Some((cols, command)) = split_command_columns(row.as_str(), 8) else {
            continue;
        };
        let Ok(pid) = cols[0].parse::<u32>() else {
            continue;
        };

        out.push(ProcessInfo {
            pid,
            ppid: cols[1].parse::<u32>().unwrap_or(0),
            user: cols[2].to_string(),
            stat: cols[3].to_string(),
            cpu_percent: cols[4].parse::<f64>().unwrap_or(0.0),
            mem_percent: cols[5].parse::<f64>().unwrap_or(0.0),
            mem_kb: cols[6].parse::<u64>().unwrap_or(0),
            elapsed: cols[7].to_string(),
            command: command.to_string(),
        });
    }

    out
}

fn proc_short_name(command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or("");
    first.rsplit('/').next().unwrap_or(first).to_string()
}

// ---------------------------------------------------------------- 端口与连接

fn is_proto(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "tcp" | "tcp6" | "udp" | "udp6" | "raw" | "raw6" | "unix" | "dccp" | "sctp"
    )
}

fn is_state(value: &str) -> bool {
    const STATES: &[&str] = &[
        "LISTEN",
        "ESTAB",
        "ESTABLISHED",
        "SYN-SENT",
        "SYN_SENT",
        "SYN-RECV",
        "SYN_RECV",
        "FIN-WAIT-1",
        "FIN_WAIT1",
        "FIN-WAIT-2",
        "FIN_WAIT2",
        "TIME-WAIT",
        "TIME_WAIT",
        "CLOSE-WAIT",
        "CLOSE_WAIT",
        "LAST-ACK",
        "LAST_ACK",
        "CLOSING",
        "UNCONN",
        "UNKNOWN",
        "CLOSED",
        "CLOSE",
        "NONE",
    ];
    let upper = value.to_ascii_uppercase();
    STATES.contains(&upper.as_str())
}

fn is_conn_header(row: &str) -> bool {
    matches!(
        row.split_whitespace().next().unwrap_or(""),
        "Netid" | "State" | "Proto" | "Active"
    )
}

/// `kind` 按状态分类。`ss -tulnp` 段的 UDP 行状态是 UNCONN、netstat 的 UDP 行
/// 状态列干脆是空的，但这一段专门查「监听端口」，所以把它们也归为 listen。
fn kind_of(state: &str, listen_section: bool) -> &'static str {
    let upper = state.to_ascii_uppercase();
    if upper == "LISTEN" || (listen_section && (upper == "UNCONN" || upper.is_empty())) {
        "listen"
    } else if upper.starts_with("ESTAB") {
        "established"
    } else {
        "other"
    }
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "listen" => 0,
        "established" => 1,
        _ => 2,
    }
}

fn make_conn(
    proto: String,
    state: String,
    local: &str,
    remote: &str,
    pid: u32,
    process: String,
    kind: &str,
) -> NetConnection {
    NetConnection {
        proto,
        state,
        local_addr: local.to_string(),
        remote_addr: remote.to_string(),
        pid,
        process,
        kind: kind.to_string(),
    }
}

/// 解析一行连接。
///
/// 这里同时兼容三种列布局，靠「第 0/1 个 token 是什么」判断，不依赖表头：
/// - `ss -tulnp` 带 Netid 列：`tcp LISTEN 0 511 0.0.0.0:80 0.0.0.0:* users:(...)`；
/// - `ss -tanp` 不带 Netid 列：`ESTAB 0 0 10.0.0.2:22 10.0.0.9:51234 ...`；
/// - `netstat -tulnp/-tanp`：`tcp 0 0 0.0.0.0:22 0.0.0.0:* LISTEN 123/sshd`，
///   状态在第 5 列而不是第 1 列，UDP 行甚至没有状态列。
///
/// 这样无论服务器装的是 ss 还是 netstat、用的是哪个版本，都能出结果。
fn parse_conn_row(row: &str, listen_section: bool) -> Option<NetConnection> {
    let parts: Vec<&str> = row.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let first = parts[0];

    if is_proto(first) {
        if parts.get(1).copied().map(is_state).unwrap_or(false) {
            // ss 带 Netid：proto state recvq sendq local peer [process...]
            if parts.len() < 6 {
                return None;
            }
            let (pid, process) = parse_process_field(&parts[6..].join(" "));
            return Some(make_conn(
                first.to_string(),
                parts[1].to_string(),
                parts[4],
                parts[5],
                pid,
                process,
                kind_of(parts[1], listen_section),
            ));
        }

        // netstat：proto recvq sendq local foreign [state] [pid/prog]
        if parts.len() < 5 {
            return None;
        }
        let (state, proc_field) = if parts.len() >= 6 && is_state(parts[5]) {
            (parts[5].to_string(), parts.get(6).copied().unwrap_or(""))
        } else if parts.len() >= 6 {
            // UDP 行没有状态列，第 5 列直接就是 PID/Program name
            (String::new(), parts[5])
        } else {
            (String::new(), "")
        };
        let (pid, process) = parse_process_field(proc_field);
        return Some(make_conn(
            first.to_string(),
            state.clone(),
            parts[3],
            parts[4],
            pid,
            process,
            kind_of(&state, listen_section),
        ));
    }

    if is_state(first) {
        // ss 不带 Netid：state recvq sendq local peer [process...]
        if parts.len() < 5 {
            return None;
        }
        let (pid, process) = parse_process_field(&parts[5..].join(" "));
        return Some(make_conn(
            // 这一段来自 -t/-tanp，只有 tcp；带 udp 的输出一定有 Netid 列。
            "tcp".to_string(),
            first.to_string(),
            parts[3],
            parts[4],
            pid,
            process,
            kind_of(first, listen_section),
        ));
    }

    None
}

/// 从进程字段里取 pid 与进程名。
///
/// - ss：`users:(("nginx",pid=123,fd=6))`
/// - netstat：`123/sshd`，无权限时是 `-`
///
/// 认不出来返回 (0, "")，绝不把整段乱文本塞进 `process` 字段。
fn parse_process_field(field: &str) -> (u32, String) {
    let value = field.trim();
    if value.is_empty() || value == "-" {
        return (0, String::new());
    }

    if let Some(pos) = value.find("pid=") {
        let start = pos + 4;
        let pid = value[start..]
            .split([',', ')'])
            .next()
            .and_then(|text| text.parse::<u32>().ok())
            .unwrap_or(0);
        let name = value
            .find("((\"")
            .and_then(|index| {
                let begin = index + 3;
                value[begin..]
                    .find('"')
                    .map(|end| value[begin..begin + end].to_string())
            })
            .unwrap_or_default();
        return (pid, name);
    }

    if let Some((pid_text, name)) = value.split_once('/') {
        return (
            pid_text.trim().parse::<u32>().unwrap_or(0),
            name.trim().to_string(),
        );
    }

    (0, String::new())
}

/// `ss -tulnp` 和 `ss -tanp` 都会报 LISTEN 行，按五元组去重；
/// 命令没带进程名时按 pid 回查完整进程列表补上；最后按 kind 排个序方便前端渲染。
fn parse_connections(sections: &Sections, processes: &[ProcessInfo]) -> Vec<NetConnection> {
    let mut names: HashMap<u32, String> = HashMap::new();
    for process in processes {
        names
            .entry(process.pid)
            .or_insert_with(|| proc_short_name(&process.command));
    }

    let mut out: Vec<NetConnection> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    'outer: for (section, listen_section) in [("sslisten", true), ("ssall", false)] {
        for row in rows(sections, section) {
            if is_conn_header(row.as_str()) {
                continue;
            }
            let Some(mut conn) = parse_conn_row(row.as_str(), listen_section) else {
                continue;
            };
            let key = format!(
                "{}|{}|{}|{}|{}",
                conn.proto, conn.state, conn.local_addr, conn.remote_addr, conn.pid
            );
            if !seen.insert(key) {
                continue;
            }
            if conn.process.is_empty() && conn.pid > 0 {
                if let Some(name) = names.get(&conn.pid) {
                    conn.process = name.clone();
                }
            }
            out.push(conn);
            if out.len() >= MAX_CONNECTIONS {
                break 'outer;
            }
        }
    }

    out.sort_by_key(|conn| kind_rank(&conn.kind));
    out
}

// ---------------------------------------------------------------- 降级提示

fn parse_cap(sections: &Sections) -> HashMap<String, bool> {
    let mut out = HashMap::new();
    for row in rows(sections, "cap") {
        if let Some((key, value)) = row.split_once('=') {
            out.insert(key.trim().to_string(), value.trim() == "yes");
        }
    }
    out
}

fn build_warnings(sections: &Sections) -> Vec<String> {
    let cap = parse_cap(sections);
    let has = |key: &str| cap.get(key).copied().unwrap_or(false);
    let mut warnings = Vec::new();

    if !has("ss") {
        if has("netstat") {
            warnings.push("服务器没有 ss，端口与连接信息已降级使用 netstat".to_string());
        } else {
            warnings.push("服务器没有 ss，也没有 netstat，端口与连接信息为空".to_string());
        }
    }
    if !has("ip") {
        warnings.push("ip 命令不可用，网卡地址信息可能不完整".to_string());
    }
    if rows(sections, "ps").is_empty() {
        warnings.push("ps 命令不可用或没有输出，进程列表为空".to_string());
    }
    if rows(sections, "netdev").is_empty() {
        warnings.push("读不到 /proc/net/dev，网卡流量统计不可用".to_string());
    }

    warnings
}

// ---------------------------------------------------------------- 路径补全（纯函数，便于单测）

/// POSIX 单引号转义：把路径里每一个 `'` 换成 `'\''`（结束引号、反斜杠转义一个引号、
/// 重开引号），整体再用单引号包住。
///
/// 为什么不用双引号：双引号里 `$`、反引号、`\` 仍会被 shell 展开，路径里出现
/// `$(rm -rf ...)` 这类字符就是命令注入。单引号里除了单引号本身，一切都是字面量。
/// 调用方另加 `--`，防止以 `-` 开头的目录名被 `ls` 当成选项。
pub fn shell_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// 把用户输入的 prefix 拆成「展示用目录前缀」和「文件名前缀」。
///
/// `/etc/pass` -> ("/etc/", "pass")；`/etc/` -> ("/etc/", "")；`etc` -> ("", "etc")。
/// 展示用前缀原样拼回候选，保证返回值和用户输入的是同一种路径形式。
pub fn split_path_prefix(prefix: &str) -> (String, String) {
    match prefix.rfind('/') {
        Some(index) => (
            prefix[..=index].to_string(),
            prefix[index + 1..].to_string(),
        ),
        None => (String::new(), prefix.to_string()),
    }
}

pub fn build_ls_command(dir: &str) -> String {
    // -1 一行一条、-A 不含 . 与 ..、-p 给目录补 /；-- 与单引号共同防注入。
    format!("ls -1Ap -- {}", shell_quote(dir))
}

/// 从 `ls -1Ap` 的输出里挑出匹配文件名前缀的候选，拼成完整路径。
///
/// `-p` 已经给目录加了 `/`，这里再显式补一次（幂等），并对换行做最后一道过滤，
/// 免得远端文件名里藏控制字符把返回给前端的 JSON 弄乱。
pub fn complete_candidates(
    dir_display: &str,
    file_part: &str,
    listing: &str,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }

    let mut out = Vec::new();
    for line in listing.lines() {
        let entry = line.trim();
        if entry.is_empty() || entry.contains('\n') || entry.contains('\r') {
            continue;
        }
        let is_dir = entry.ends_with('/');
        let name = entry.trim_end_matches('/');
        if name.is_empty() || !name.starts_with(file_part) {
            continue;
        }

        let mut full = String::new();
        full.push_str(dir_display);
        full.push_str(name);
        if is_dir {
            full.push('/');
        }
        out.push(full);
        if out.len() >= limit {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------- 单测

#[cfg(test)]
mod tests {
    use super::*;

    /// 按 POSIX 单引号规则还原一个 token，用来单测里独立验证 shell_quote 的正确性
    /// （不是复用 shell_quote 的实现，避免「自己证明自己」）。只处理 ASCII 测试输入。
    fn posix_unquote(input: &str) -> String {
        let bytes = input.as_bytes();
        assert_eq!(bytes.first().copied(), Some(b'\''), "必须是单引号 token");
        let mut out = String::new();
        let mut i = 1usize;
        while i < bytes.len() {
            if bytes[i] == b'\'' {
                // `'\''` 是转义的单引号；否则这个引号就是结束引号
                if i + 3 < bytes.len()
                    && bytes[i + 1] == b'\\'
                    && bytes[i + 2] == b'\''
                    && bytes[i + 3] == b'\''
                {
                    out.push('\'');
                    i += 4;
                    continue;
                }
                break;
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    #[test]
    fn empty_output_never_panics_and_explains_degradation() {
        let snapshot = parse("");

        assert_eq!(snapshot.info.hostname, "");
        assert_eq!(snapshot.info.total_mem_kb, 0);
        assert!(snapshot.info.disks.is_empty());
        assert!(snapshot.interfaces.is_empty());
        assert!(snapshot.processes.is_empty());
        assert!(snapshot.connections.is_empty());
        assert!(!snapshot.warnings.is_empty(), "空输出也必须给出降级说明");
    }

    #[test]
    fn partial_sections_still_produce_a_snapshot() {
        let raw = "@@host\nweb-01\n@@kernel\nLinux 6.8.0\n@@arch\nx86_64\n";
        let snapshot = parse(raw);

        assert_eq!(snapshot.info.hostname, "web-01");
        assert_eq!(snapshot.info.kernel, "Linux 6.8.0");
        assert_eq!(snapshot.info.arch, "x86_64");
        // 其它段缺失保持默认值，不 panic、不编造
        assert_eq!(snapshot.info.total_mem_kb, 0);
        assert_eq!(snapshot.info.load_avg, vec![0.0, 0.0, 0.0]);
        assert!(snapshot.info.disks.is_empty());
    }

    #[test]
    fn process_args_with_spaces_are_kept_intact() {
        let raw = "\
@@ps
PID PPID USER STAT %CPU %MEM RSS ELAPSED COMMAND
 4211 1 root Ss 12.5 3.1 256000 03:12:07 python3 -u /opt/app/server.py --worker 2
";
        let snapshot = parse(raw);

        assert_eq!(snapshot.processes.len(), 1, "表头行不能被当成进程");
        let process = &snapshot.processes[0];
        assert_eq!(process.pid, 4211);
        assert_eq!(process.ppid, 1);
        assert_eq!(process.user, "root");
        assert_eq!(process.stat, "Ss");
        assert_eq!(process.cpu_percent, 12.5);
        assert_eq!(process.mem_percent, 3.1);
        assert_eq!(process.mem_kb, 256000);
        assert_eq!(process.elapsed, "03:12:07");
        assert_eq!(process.command, "python3 -u /opt/app/server.py --worker 2");
        assert_eq!(snapshot.info.process_count, 1);
    }

    #[test]
    fn ipv6_and_sysfs_mac_and_state_are_parsed() {
        let raw = "\
@@netdev
Inter-|   Receive | Transmit
 face |bytes packets errs | bytes packets errs
  eth0: 1000 10 2 0 0 0 0 0 2000 20 3 0 0 0 0 0
@@ipaddr
2: eth0    inet 10.0.0.2/24 brd 10.0.0.255 scope global eth0
2: eth0    inet6 2001:db8::1/64 scope global
@@sysnet
/sys/class/net/eth0/address:aa:bb:cc:dd:ee:ff
/sys/class/net/eth0/operstate:up
";
        let snapshot = parse(raw);
        let eth0 = snapshot
            .interfaces
            .iter()
            .find(|item| item.name == "eth0")
            .expect("应当有 eth0");

        assert_eq!(eth0.ipv4, "10.0.0.2/24");
        assert_eq!(eth0.ipv6, "2001:db8::1/64");
        assert_eq!(eth0.mac, "aa:bb:cc:dd:ee:ff");
        assert_eq!(eth0.state, "up");
        assert_eq!(eth0.rx_bytes, 1000);
        assert_eq!(eth0.tx_bytes, 2000);
        assert_eq!(eth0.rx_errors, 2);
        assert_eq!(eth0.tx_errors, 3);
    }

    #[test]
    fn ss_users_field_extracts_pid_and_process() {
        let raw = "\
@@ps
PID PPID USER STAT %CPU %MEM RSS ELAPSED COMMAND
 123 1 root Ss 0.0 0.1 1024 01:00:00 nginx: master process
@@sslisten
Netid State  Recv-Q Send-Q Local Address:Port Peer Address:Port Process
tcp   LISTEN 0      511    0.0.0.0:80      0.0.0.0:*        users:((\"nginx\",pid=123,fd=6))
";
        let snapshot = parse(raw);
        let conn = snapshot
            .connections
            .iter()
            .find(|item| item.local_addr == "0.0.0.0:80")
            .expect("应当解析出监听行");

        assert_eq!(conn.proto, "tcp");
        assert_eq!(conn.state, "LISTEN");
        assert_eq!(conn.remote_addr, "0.0.0.0:*");
        assert_eq!(conn.pid, 123);
        assert_eq!(conn.process, "nginx");
        assert_eq!(conn.kind, "listen");
    }

    #[test]
    fn pid_associates_process_name_from_full_process_list() {
        // 连接行只有 pid=321、没有进程名，应当回查 ps 段把名字补上。
        let raw = "\
@@ps
PID PPID USER STAT %CPU %MEM RSS ELAPSED COMMAND
 321 1 root Ss 0.0 0.1 2048 00:10:00 /usr/sbin/nginx -g daemon off
@@ssall
State Recv-Q Send-Q Local Address:Port Peer Address:Port Process
ESTAB 0 0 10.0.0.2:443 10.0.0.9:60000 pid=321
";
        let snapshot = parse(raw);
        let conn = snapshot
            .connections
            .iter()
            .find(|item| item.local_addr == "10.0.0.2:443")
            .expect("应当解析出已建立连接");

        assert_eq!(conn.pid, 321);
        assert_eq!(conn.process, "nginx", "按 pid 关联到 ps 里的进程名");
        assert_eq!(conn.kind, "established");
    }

    #[test]
    fn netstat_fallback_is_parsed_with_different_column_order() {
        let raw = "\
@@ps
PID PPID USER STAT %CPU %MEM RSS ELAPSED COMMAND
@@sslisten
Active Internet connections (only servers)
Proto Recv-Q Send-Q Local Address           Foreign Address         State       PID/Program name
tcp        0      0 0.0.0.0:22              0.0.0.0:*               LISTEN      456/sshd
udp        0      0 127.0.0.53:53           0.0.0.0:*                          789/systemd-resolve
@@ssall
Active Internet connections (w/o servers)
Proto Recv-Q Send-Q Local Address           Foreign Address         State       PID/Program name
tcp        0      0 10.0.0.2:22             10.0.0.9:51234         ESTABLISHED 200/sshd
";
        let snapshot = parse(raw);

        let tcp_listen = snapshot
            .connections
            .iter()
            .find(|item| item.local_addr == "0.0.0.0:22")
            .expect("netstat 的 tcp 监听行");
        assert_eq!(tcp_listen.proto, "tcp");
        assert_eq!(tcp_listen.state, "LISTEN");
        assert_eq!(tcp_listen.pid, 456);
        assert_eq!(tcp_listen.process, "sshd");
        assert_eq!(tcp_listen.kind, "listen");

        let udp = snapshot
            .connections
            .iter()
            .find(|item| item.local_addr == "127.0.0.53:53")
            .expect("netstat 的 udp 行");
        assert_eq!(udp.proto, "udp");
        assert_eq!(udp.pid, 789);
        assert_eq!(udp.process, "systemd-resolve");
        assert_eq!(udp.kind, "listen", "udp 无状态列，但来自监听段");

        let established = snapshot
            .connections
            .iter()
            .find(|item| item.local_addr == "10.0.0.2:22")
            .expect("netstat 的已建立连接");
        assert_eq!(established.state, "ESTABLISHED");
        assert_eq!(established.pid, 200);
        assert_eq!(established.kind, "established");

        assert!(
            snapshot.warnings.iter().any(|w| w.contains("没有 ss")),
            "cap 段缺 ss 时要给中文提示"
        );
    }

    #[test]
    fn single_quote_escaping_is_posix_correct() {
        assert_eq!(shell_quote("/etc"), "'/etc'");
        assert_eq!(shell_quote("/opt/a'b"), "'/opt/a'\\''b'");
        assert_eq!(shell_quote("say'hi'"), "'say'\\''hi'\\'''");

        // 用独立的 POSIX 还原器验证：任意含单引号的路径都能无损往返，
        // 说明路径不会被 shell 二次解释。
        for value in [
            "/etc",
            "/opt/a'b",
            "say'hi'",
            "a b c",
            "/x'y/z",
            "$(touch /tmp/pwn)",
            "-rf",
        ] {
            assert_eq!(
                posix_unquote(&shell_quote(value)),
                value,
                "转义后必须能无损还原 {value}"
            );
        }
    }

    #[test]
    fn ls_command_single_quotes_the_directory_and_uses_double_dash() {
        assert_eq!(build_ls_command("/etc"), "ls -1Ap -- '/etc'");
        assert_eq!(build_ls_command("/opt/a'b"), "ls -1Ap -- '/opt/a'\\''b'");
        assert!(build_ls_command("-rf").contains("-- '-rf'"), "-- 不能丢");
    }

    #[test]
    fn path_complete_filters_prefix_appends_slash_and_respects_limit() {
        let listing = "passwd\npasswd-\nshadow\ncron.d/\n";

        assert_eq!(
            complete_candidates("/etc/", "pass", listing, 50),
            vec!["/etc/passwd", "/etc/passwd-"]
        );
        assert_eq!(
            complete_candidates("/etc/", "cron", listing, 50),
            vec!["/etc/cron.d/"],
            "目录候选末尾要补 /"
        );
        assert_eq!(complete_candidates("/etc/", "", listing, 2).len(), 2);
        assert!(
            complete_candidates("/etc/", "", listing, 0).is_empty(),
            "limit 为 0 不应返回候选"
        );
        // 相对路径：展示前缀为空，返回值保持相对形式
        assert_eq!(
            complete_candidates("", "pass", listing, 50),
            vec!["passwd", "passwd-"]
        );
    }

    #[test]
    fn oversized_lists_are_capped_at_safe_limits() {
        let mut raw = String::from("@@ps\n");
        for i in 1..=2500u32 {
            raw.push_str(&format!("{i} 1 root S 0.0 0.0 100 00:01 p{i}\n"));
        }
        raw.push_str("@@ssall\n");
        for i in 1..=2500u32 {
            raw.push_str(&format!(
                "ESTAB 0 0 10.0.0.2:{} 10.0.0.9:{} \n",
                30000 + i,
                40000 + i
            ));
        }

        let snapshot = parse(&raw);
        assert_eq!(snapshot.processes.len(), MAX_PROCESSES);
        assert_eq!(snapshot.connections.len(), MAX_CONNECTIONS);
        // process_count 独立统计全部有效行，不受展示上限影响
        assert_eq!(snapshot.info.process_count, 2500);
    }

    #[test]
    fn garbage_input_never_panics() {
        let raw = "\
@@ps
@@netdev
乱码 not a number
@@sslisten
random junk
@@disk
只 有 三 列
@@sysnet
没有冒号的一行
";
        let snapshot = parse(raw);
        assert!(snapshot.processes.is_empty());
        assert!(snapshot.info.disks.is_empty());
        assert!(snapshot.connections.is_empty());
    }

    #[test]
    fn sample_command_is_single_line_with_every_marker() {
        assert_eq!(SAMPLE_COMMAND.lines().count(), 1, "必须是一条单行命令");
        assert!(!SAMPLE_COMMAND.contains('\n'));
        assert!(!SAMPLE_COMMAND.contains('\r'));

        for marker in [
            "@@host",
            "@@os",
            "@@kernel",
            "@@arch",
            "@@cpu",
            "@@virt",
            "@@uptime",
            "@@mem",
            "@@load",
            "@@who",
            "@@disk",
            "@@netdev",
            "@@ipaddr",
            "@@ifconfig",
            "@@sysnet",
            "@@cap",
            "@@sslisten",
            "@@ssall",
            "@@ps",
        ] {
            assert!(
                SAMPLE_COMMAND.contains(marker),
                "采集命令缺少分段标记 {marker}"
            );
        }
    }
}
