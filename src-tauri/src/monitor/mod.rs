//! 结构化诊断（把命令输出解析成表格）。
//!
//! 为什么要有这个模块：`ss -tulnp` 的输出是一屏文本，运维靠训练能一眼扫出重点，
//! 开发者和普通用户不行。这里把固定格式的输出解析成「列 + 行 + 一句人话摘要」，
//! 前端渲染成表格，用户不用再逐列对齐。
//!
//! 解析全靠**位置**而不是列宽：这些命令（df -P / free / ps -eo / ss）的输出格式
//! 几十年没变过，比 `column` 对齐可靠。解析失败时降级为只返回原始输出，不编造数据。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::ssh::exec;

/// 诊断类型。前端传 `"disk"` 这类字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagKind {
    Disk,
    Inode,
    Memory,
    Processes,
    Ports,
    ProcessTree,
}

impl DiagKind {
    /// 真正执行的命令。
    ///
    /// 都带 `-P` / `-o` 这类「固定格式」开关，避免因终端宽度不同而折行。
    /// 不加 `--color`：诊断面板要解析，颜色是给终端看的。
    pub fn command(self) -> &'static str {
        match self {
            DiagKind::Disk => "df -hP -x tmpfs -x devtmpfs",
            DiagKind::Inode => "df -iP -x tmpfs -x devtmpfs",
            DiagKind::Memory => "free -h",
            DiagKind::Processes => "ps -eo pid,user,pcpu,pmem,comm --sort=-pcpu | head -15",
            DiagKind::Ports => "ss -tulnp | head -30",
            // 不带 --forest | head：树形缩进和 ps 的列宽填充混在一起，切不干净。
            // 拿全量三元组回来，在 Rust 里按 ppid 重建，层级才可靠。
            DiagKind::ProcessTree => "ps -eo pid,ppid,comm",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            DiagKind::Disk => "磁盘使用",
            DiagKind::Inode => "inode 使用",
            DiagKind::Memory => "内存使用",
            DiagKind::Processes => "CPU 占用前 15",
            DiagKind::Ports => "端口与连接",
            DiagKind::ProcessTree => "进程树",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagResult {
    pub kind: DiagKind,
    pub title: String,
    pub command: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// 一句人话结论，比如「使用率最高的是 /data，85%」
    pub summary: String,
    /// 需要用户注意的事，没有则为 None
    pub warning: Option<String>,
    /// 原始输出，供「查看原始输出」展开
    pub raw: String,
    pub fetched_at: u64,
}

/// 跑一条诊断：执行 → 解析。
pub(crate) async fn run(
    conn: &russh::client::Handle<crate::ssh::client::ProbeHandler>,
    kind: DiagKind,
) -> crate::error::AppResult<DiagResult> {
    let raw = exec::exec_once(conn, kind.command()).await?;
    Ok(parse(kind, &raw))
}

/// 纯函数：把命令输出解析成表格。抽出来是为了能拿字符串样本直接测。
pub fn parse(kind: DiagKind, raw: &str) -> DiagResult {
    let text = raw.trim_end();

    let (columns, rows) = match kind {
        DiagKind::Disk => parse_df_like(text, ["挂载点", "总量", "已用", "可用", "使用率"]),
        DiagKind::Inode => parse_df_like(text, ["挂载点", "inode 总数", "已用", "可用", "使用率"]),
        DiagKind::Memory => parse_memory(text),
        DiagKind::Processes => parse_processes(text),
        DiagKind::Ports => parse_ports(text),
        DiagKind::ProcessTree => parse_process_tree(text),
    };

    let (summary, warning) = if rows.is_empty() {
        let warning = if text.is_empty() {
            "命令没有输出。可能是权限不足，或系统不支持该命令。".to_string()
        } else {
            format!("输出无法解析，已保留原始内容。首行：{}", first_line(text))
        };
        (String::from("没有可展示的数据"), Some(warning))
    } else {
        summarize(kind, &rows)
    };

    DiagResult {
        kind,
        title: kind.title().to_string(),
        command: kind.command().to_string(),
        columns,
        rows,
        summary,
        warning,
        raw: text.to_string(),
        fetched_at: crate::state::now_millis(),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(80)
        .collect::<String>()
}

fn lines_of(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

// ---------------------------------------------------------------- 各命令的解析

/// `df -hP` / `df -iP` 共用的解析。
///
/// ```text
/// Filesystem      Size  Used Avail Use% Mounted on
/// /dev/vda1        40G   12G   26G  32% /
/// ```
/// 跳过列名行（我们用自己的中文表头），挂载点放第一列 —— 用户关心「哪个分区满了」。
/// disk 与 inode 只差表头文字，解析逻辑完全一致。
fn parse_df_like(text: &str, headers: [&str; 5]) -> (Vec<String>, Vec<Vec<String>>) {
    let columns = headers.iter().map(|item| item.to_string()).collect();
    let mut rows: Vec<Vec<String>> = Vec::new();

    for line in lines_of(text).into_iter().skip(1) {
        let mut parts = line.split_whitespace();
        let (Some(_fs), Some(size), Some(used), Some(avail), Some(capacity)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };

        // 挂载点里可能有空格，剩下的全部拼回去
        let mount = parts.collect::<Vec<_>>().join(" ");
        if mount.is_empty() {
            continue;
        }

        rows.push(vec![
            mount,
            size.to_string(),
            used.to_string(),
            avail.to_string(),
            capacity.to_string(),
        ]);
    }

    // 使用率高的排前面：用户打开就想看哪个快满了
    rows.sort_by(|a, b| percent_of(&b[4]).cmp(&percent_of(&a[4])));
    (columns, rows)
}

/// `free -h`：
/// ```text
///               total        used        free      shared  buff/cache   available
/// Mem:           3.8Gi       1.2Gi       180Mi        10Mi       2.4Gi       2.3Gi
/// Swap:             0B          0B          0B
/// ```
fn parse_memory(text: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let columns = ["类型", "总量", "已用", "空闲", "缓存", "可用"]
        .iter()
        .map(|item| item.to_string())
        .collect();

    let mut rows = Vec::new();

    for line in lines_of(text) {
        if !(line.starts_with("Mem:") || line.starts_with("Swap:")) {
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(label) = parts.next() else { continue };
        let values: Vec<&str> = parts.collect();

        let name = label.trim_end_matches(':');
        let cell = |index: usize| values.get(index).copied().unwrap_or("—").to_string();

        rows.push(vec![
            name.to_string(),
            cell(0),
            cell(1),
            cell(2),
            // Swap 行没有 buff/cache 和 available
            cell(4),
            cell(5),
        ]);
    }

    (columns, rows)
}

/// `ps -eo pid,user,pcpu,pmem,comm`：
/// ```text
///     PID USER     %CPU %MEM COMMAND
///    2043 root     12.5  3.2 nginx
/// ```
fn parse_processes(text: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let columns = ["PID", "用户", "CPU%", "内存%", "命令"]
        .iter()
        .map(|item| item.to_string())
        .collect();

    let mut rows = Vec::new();

    for line in lines_of(text).into_iter().skip(1) {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(user), Some(cpu), Some(mem), Some(comm)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };

        rows.push(vec![
            pid.to_string(),
            user.to_string(),
            cpu.to_string(),
            mem.to_string(),
            comm.to_string(),
        ]);
    }

    (columns, rows)
}

/// 进程树。
///
/// 两个决定：
/// 1. **不解析 `ps --forest` 的缩进** —— 那段缩进和 ps 的列宽填充混在同一列，切不干净。
///    改成拿 `pid ppid comm` 三元组，在 Rust 里按 ppid 重建，层级才是真的。
/// 2. **用 `├─` / `└─` 连线而不是空格缩进** —— 空白字符在 HTML 里是否被折叠取决于
///    渲染细节（`white-space` 取值、是否 nbsp），而制表符号是可见字符，一定画得出来。
///    前端仍给这一列加 `white-space: pre`，保证连线之间的空格不被吃掉。
fn parse_process_tree(text: &str) -> (Vec<String>, Vec<Vec<String>>) {
    const MAX_ROWS: usize = 40;
    const MAX_DEPTH: usize = 8;

    let columns = ["PID", "PPID", "进程"]
        .iter()
        .map(|item| item.to_string())
        .collect();

    struct Proc {
        pid: u32,
        ppid: u32,
        comm: String,
    }

    let mut procs: Vec<Proc> = Vec::new();
    for line in lines_of(text).into_iter().skip(1) {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(comm)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else {
            continue;
        };
        procs.push(Proc {
            pid,
            ppid,
            comm: comm.to_string(),
        });
    }

    if procs.is_empty() {
        return (columns, Vec::new());
    }

    let known: HashSet<u32> = procs.iter().map(|proc_| proc_.pid).collect();
    let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();

    for (index, proc_) in procs.iter().enumerate() {
        // ppid 为 0 或指向一个已消失的进程，都算顶层
        if proc_.ppid == 0 || !known.contains(&proc_.ppid) {
            roots.push(index);
        } else {
            children.entry(proc_.ppid).or_default().push(index);
        }
    }

    // 数据异常（ppid 互相指）时可能一个顶层都没有，那就把它们都当顶层，别交白卷
    if roots.is_empty() {
        roots = (0..procs.len()).collect();
    }

    /// 一帧待展开的节点。`prefix` 是左侧已经画好的竖线，用来对齐下一层。
    struct Frame {
        index: usize,
        depth: usize,
        prefix: String,
        is_last: bool,
    }

    fn frame(index: usize, depth: usize, prefix: String, is_last: bool) -> Frame {
        Frame {
            index,
            depth,
            prefix,
            is_last,
        }
    }

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut visited: HashSet<u32> = HashSet::new();

    // 反序压栈，pop 出来就是正序
    let root_total = roots.len();
    let mut stack: Vec<Frame> = roots
        .iter()
        .enumerate()
        .rev()
        .map(|(order, &index)| frame(index, 0, String::new(), order + 1 == root_total))
        .collect();

    while let Some(current) = stack.pop() {
        if rows.len() >= MAX_ROWS {
            break;
        }

        let proc_ = &procs[current.index];
        // 数据异常时 ppid 可能成环，访问过的就不再展开
        if !visited.insert(proc_.pid) {
            continue;
        }

        // 顶层不画分支，直接从进程名开始
        let branch = if current.depth == 0 {
            ""
        } else if current.is_last {
            "└─ "
        } else {
            "├─ "
        };

        rows.push(vec![
            proc_.pid.to_string(),
            proc_.ppid.to_string(),
            format!("{}{}{}", current.prefix, branch, proc_.comm),
        ]);

        if current.depth + 1 >= MAX_DEPTH {
            continue;
        }

        let Some(kids) = children.get(&proc_.pid) else {
            continue;
        };

        // 子层左侧：父节点还有后续兄弟时画竖线，否则留空对齐
        let next_prefix = if current.depth == 0 {
            String::new()
        } else {
            format!(
                "{}{}",
                current.prefix,
                if current.is_last { "   " } else { "│  " }
            )
        };

        let kid_total = kids.len();
        for (order, &kid) in kids.iter().enumerate().rev() {
            stack.push(frame(
                kid,
                current.depth + 1,
                next_prefix.clone(),
                order + 1 == kid_total,
            ));
        }
    }

    (columns, rows)
}

/// `ss -tulnp`：
/// ```text
/// Netid State  Recv-Q Send-Q Local Address:Port  Peer Address:Port Process
/// tcp   LISTEN 0      511    0.0.0.0:80           0.0.0.0:*         users:(("nginx",pid=2043,fd=6))
/// ```
fn parse_ports(text: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let columns = ["协议", "状态", "本地地址", "对端", "进程"]
        .iter()
        .map(|item| item.to_string())
        .collect();

    let mut rows: Vec<Vec<String>> = Vec::new();

    for line in lines_of(text).into_iter().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 {
            continue;
        }

        let process = parts
            .get(6)
            .map(|field| process_of(field))
            .unwrap_or_else(|| "—".to_string());

        rows.push(vec![
            parts[0].to_string(),
            parts[1].to_string(),
            parts[4].to_string(),
            parts.get(5).unwrap_or(&"*").to_string(),
            process,
        ]);
    }

    // LISTEN 排前面：这才是「对外提供服务」，其余是已建立的连接
    rows.sort_by_key(|row| if row[1] == "LISTEN" { 0 } else { 1 });

    (columns, rows)
}

/// 从 `users:(("nginx",pid=2043,fd=6))` 里取出 `nginx (2043)`。
fn process_of(field: &str) -> String {
    let Some(name_start) = field.find("((\"").map(|index| index + 3) else {
        return "—".to_string();
    };
    let Some(name_end) = field[name_start..]
        .find('"')
        .map(|index| index + name_start)
    else {
        return "—".to_string();
    };
    let name = &field[name_start..name_end];

    let pid = field
        .find("pid=")
        .and_then(|index| {
            let start = index + 4;
            field[start..]
                .find([',', ')'])
                .map(|end| &field[start..start + end])
        })
        .unwrap_or("?");

    format!("{name} ({pid})")
}

// ---------------------------------------------------------------- 摘要

fn percent_of(cell: &str) -> u32 {
    cell.trim_end_matches('%')
        .parse::<f32>()
        .map(|value| value.round() as u32)
        .unwrap_or(0)
}

/// 生成一句人话结论 + 可选的告警。
fn summarize(kind: DiagKind, rows: &[Vec<String>]) -> (String, Option<String>) {
    match kind {
        DiagKind::Disk | DiagKind::Inode => {
            let worst = rows.first();
            let summary = match worst {
                Some(row) => format!("使用率最高的是 {}，{}", row[0], row[4]),
                None => "没有可用的挂载点".to_string(),
            };

            let full: Vec<&Vec<String>> = rows
                .iter()
                .filter(|row| percent_of(&row[4]) >= 80)
                .collect();

            let warning = if full.is_empty() {
                None
            } else {
                let names: Vec<&str> = full.iter().map(|row| row[0].as_str()).collect();
                Some(format!(
                    "{} 已超过 80% 使用率（{}），建议清理",
                    names.join("、"),
                    full.iter()
                        .map(|row| row[4].as_str())
                        .collect::<Vec<_>>()
                        .join("、")
                ))
            };
            (summary, warning)
        }

        DiagKind::Memory => {
            // 第一行是 Mem，Swap 在其后
            let mem = rows.first();
            let summary = match mem {
                Some(row) => format!("内存已用 {} / {}，可用 {}", row[2], row[1], row[5]),
                None => "没有读到内存信息".to_string(),
            };

            let warning = mem.and_then(|row| {
                let total = parse_size(&row[1]);
                let available = parse_size(&row[5]);
                if total > 0.0 && available / total < 0.15 {
                    Some(format!(
                        "可用内存只剩 {}，占总量 {:.0}%，注意 OOM",
                        row[5],
                        available / total * 100.0
                    ))
                } else {
                    None
                }
            });
            (summary, warning)
        }

        DiagKind::Processes => {
            let summary = match rows.first() {
                Some(row) => format!("CPU 占用最高的是 {}（PID {}），{}%", row[4], row[0], row[2]),
                None => "没有进程信息".to_string(),
            };

            let hot = rows.iter().find(|row| percent_of(&row[2]) >= 80);
            let warning = hot.map(|row| {
                format!(
                    "{}（PID {}）CPU 占用 {}%，持续高位就该看看了",
                    row[4], row[0], row[2]
                )
            });
            (summary, warning)
        }

        DiagKind::ProcessTree => {
            let truncated = rows.len() >= 40;
            let summary = if truncated {
                "按父子层级展开前 40 个进程（还有更多未显示）".to_string()
            } else {
                format!("按父子层级展开 {} 个进程", rows.len())
            };
            (summary, None)
        }

        DiagKind::Ports => {
            let listening = rows.iter().filter(|row| row[1] == "LISTEN").count();
            let summary = format!("共 {} 条连接记录，其中 {} 个在监听", rows.len(), listening);

            let warning = if listening == 0 {
                Some("没有任何监听端口，服务可能没起来".to_string())
            } else {
                None
            };
            (summary, warning)
        }
    }
}

/// `1.2Gi` / `180Mi` → 字节数，用来算比例。认不出就返回 0（后续判断会跳过）。
fn parse_size(cell: &str) -> f64 {
    let trimmed = cell.trim();
    let split_at = trimmed
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(trimmed.len());

    let (number, unit) = trimmed.split_at(split_at);
    let Ok(value) = number.parse::<f64>() else {
        return 0.0;
    };

    let multiplier = match unit.trim() {
        "B" | "" => 1.0,
        "Ki" | "K" => 1024.0,
        "Mi" | "M" => 1024.0 * 1024.0,
        "Gi" | "G" => 1024.0 * 1024.0 * 1024.0,
        "Ti" | "T" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => 0.0,
    };

    value * multiplier
}

#[cfg(test)]
mod tests {
    use super::*;

    const DF: &str = "\
Filesystem      Size  Used Avail Use% Mounted on
/dev/vda1        40G   12G   26G  32% /
/dev/vdb1       100G   85G   10G  85% /data
tmpfs           780M  1.4M  779M   1% /run
";

    const FREE: &str = "\
              total        used        free      shared  buff/cache   available
Mem:           3.8Gi       3.4Gi       180Mi        10Mi       2.4Gi       400Mi
Swap:             0B          0B          0B
";

    const PS: &str = "\
    PID USER     %CPU %MEM COMMAND
   2043 root     92.5  3.2 nginx
      1 root      0.0  0.1 systemd
";

    const SS: &str = "\
Netid State  Recv-Q Send-Q Local Address:Port  Peer Address:Port Process
udp   UNCONN 0      0      127.0.0.53%lo:53     0.0.0.0:*
tcp   LISTEN 0      511    0.0.0.0:80           0.0.0.0:*         users:((\"nginx\",pid=2043,fd=6))
tcp   ESTAB  0      0      10.0.0.2:22          10.0.0.9:51234
";

    #[test]
    fn disk_rows_are_sorted_by_usage_and_mount_is_first_column() {
        let result = parse(DiagKind::Disk, DF);
        assert_eq!(result.columns[0], "挂载点");
        assert_eq!(result.rows.len(), 3, "表头要跳过");
        assert_eq!(result.rows[0], vec!["/data", "100G", "85G", "10G", "85%"]);
        assert_eq!(result.rows[2][0], "/run", "使用率最低的排最后");
    }

    #[test]
    fn disk_summary_names_the_fullest_mount() {
        let result = parse(DiagKind::Disk, DF);
        assert!(result.summary.contains("/data"), "实际 {}", result.summary);
        assert!(result.summary.contains("85%"));
    }

    #[test]
    fn disk_warns_when_usage_crosses_eighty() {
        let result = parse(DiagKind::Disk, DF);
        assert!(
            result.warning.unwrap_or_default().contains("/data"),
            "85% 应当告警"
        );
    }

    #[test]
    fn healthy_disk_has_no_warning() {
        let text = "\
Filesystem      Size  Used Avail Use% Mounted on
/dev/vda1        40G   12G   26G  32% /
";
        assert!(parse(DiagKind::Disk, text).warning.is_none());
    }

    #[test]
    fn memory_maps_six_columns() {
        let result = parse(DiagKind::Memory, FREE);
        assert_eq!(result.rows.len(), 2, "Mem 与 Swap 各一行");
        assert_eq!(result.rows[0][0], "Mem");
        assert_eq!(result.rows[0][1], "3.8Gi");
        assert_eq!(result.rows[0][5], "400Mi", "available 是第 6 列");
    }

    #[test]
    fn memory_warns_when_available_is_low() {
        let result = parse(DiagKind::Memory, FREE);
        // 400Mi / 3.8Gi ≈ 10%，低于 15% 阈值
        assert!(result.warning.unwrap_or_default().contains("OOM"));
    }

    #[test]
    fn processes_keep_order_and_name_the_top_one() {
        let result = parse(DiagKind::Processes, PS);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0][4], "nginx");
        assert!(result.summary.contains("nginx"));
        assert!(result.warning.is_some(), "92% CPU 应当告警");
    }

    #[test]
    fn ports_extract_process_name_and_pid() {
        let result = parse(DiagKind::Ports, SS);
        assert_eq!(result.rows.len(), 3);

        let listen = result.rows.iter().find(|row| row[1] == "LISTEN").unwrap();
        assert_eq!(listen[2], "0.0.0.0:80");
        assert_eq!(listen[4], "nginx (2043)");
    }

    #[test]
    fn ports_put_listen_first() {
        let result = parse(DiagKind::Ports, SS);
        assert_eq!(result.rows[0][1], "LISTEN", "监听连接排最前");
    }

    #[test]
    fn port_without_process_shows_placeholder() {
        let result = parse(DiagKind::Ports, SS);
        let established = result.rows.iter().find(|row| row[1] == "ESTAB").unwrap();
        assert_eq!(established[4], "—", "没有权限看进程时不能假装知道");
    }

    #[test]
    fn empty_output_degrades_to_a_readable_warning() {
        let result = parse(DiagKind::Disk, "");
        assert!(result.rows.is_empty());
        assert!(result.warning.unwrap_or_default().contains("没有输出"));
        assert_eq!(result.summary, "没有可展示的数据");
    }

    #[test]
    fn unparsable_output_keeps_raw_and_explains() {
        let result = parse(DiagKind::Disk, "bash: df: command not found");
        assert!(result.rows.is_empty());
        let warning = result.warning.unwrap_or_default();
        assert!(warning.contains("无法解析"), "实际 {warning}");
        assert!(warning.contains("command not found"));
        assert_eq!(result.raw, "bash: df: command not found");
    }

    // ---------------------------------------------------------- 端到端（真实 SSH 服务端）

    mod e2e {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use super::*;
        use crate::events::OutputBatch;
        use crate::ssh::session::{self, OpenRequest, SessionManager};
        use crate::ssh::test_support::{spawn, HostKey};
        use crate::ssh::{probe, ProbeAuth, ProbeRequest, ProbeResult};

        const DF_SAMPLE: &str = "\
Filesystem      Size  Used Avail Use% Mounted on
/dev/vda1        40G   12G   26G  32% /
";

        fn probe_req(port: u16) -> ProbeRequest {
            ProbeRequest {
                host: "127.0.0.1".to_string(),
                port,
                username: "tester".to_string(),
                auth: ProbeAuth::Password("pw".to_string()),
                legacy_algorithms: false,
                known_fingerprint: None,
                trust_fingerprint: None,
            }
        }

        /// 建一个真实终端会话，返回管理器与 session_id。
        async fn open_session(
            port: u16,
        ) -> (Arc<SessionManager>, String, Arc<Mutex<Vec<OutputBatch>>>) {
            let mut request = probe_req(port);
            let fingerprint = match probe(request.clone()).await {
                ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
                other => panic!("期望 NeedsConfirmation，实际 {other:?}"),
            };
            request.trust_fingerprint = Some(fingerprint);

            let manager = Arc::new(SessionManager::new());
            let store: Arc<Mutex<Vec<OutputBatch>>> = Arc::new(Mutex::new(Vec::new()));
            let writer = Arc::clone(&store);
            let emit = Box::new(move |batch: OutputBatch| {
                writer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(batch);
                true
            });

            let opened = session::open(
                Arc::clone(&manager),
                OpenRequest {
                    session_id: "s-diag".to_string(),
                    connection_id: "c1".to_string(),
                    title: "诊断测试".to_string(),
                    host_label: "127.0.0.1".to_string(),
                    cols: 100,
                    rows: 30,
                    probe: request,
                },
                emit,
                Box::new(|_| {}),
            )
            .await
            .expect("会话应当能建立");

            (manager, opened.info.session_id, store)
        }

        fn terminal_text(store: &Arc<Mutex<Vec<OutputBatch>>>) -> String {
            store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .map(|batch| batch.data.clone())
                .collect()
        }

        #[tokio::test]
        async fn diag_parses_output_from_a_real_exec_channel() {
            let server = spawn("pw", HostKey::Ed25519, false);
            server
                .exec_outputs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(DiagKind::Disk.command().to_string(), DF_SAMPLE.to_string());

            let (manager, sid, _store) = open_session(server.port).await;
            let conn = manager.connection(&sid).expect("应当能取到连接句柄");

            let result = run(&conn, DiagKind::Disk).await.expect("诊断应当成功");

            assert_eq!(result.rows.len(), 1);
            assert_eq!(result.rows[0][0], "/"); // 挂载点在第一列
            assert_eq!(result.rows[0][4], "32%");
            assert!(result.summary.contains("/"));
            assert!(
                result.raw.contains("Filesystem"),
                "原始输出要留着给用户展开"
            );
        }

        #[tokio::test]
        async fn missing_command_degrades_without_inventing_rows() {
            // 不给服务器配置输出，测试服务端会回 "command not found"
            let server = spawn("pw", HostKey::Ed25519, false);
            let (manager, sid, _store) = open_session(server.port).await;
            let conn = manager.connection(&sid).unwrap();

            let result = run(&conn, DiagKind::Disk).await.expect("exec 本身不报错");

            assert!(result.rows.is_empty(), "解析不出东西时不能编造行");
            let warning = result.warning.unwrap_or_default();
            assert!(warning.contains("command not found"), "实际 {warning}");
        }

        #[tokio::test]
        async fn diagnostic_does_not_disturb_the_terminal() {
            // exec 走独立 channel：既不该把输出混进终端，也不该打断终端
            let server = spawn("pw", HostKey::Ed25519, false);
            server
                .exec_outputs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(DiagKind::Memory.command().to_string(), FREE.to_string());

            let (manager, sid, store) = open_session(server.port).await;
            let conn = manager.connection(&sid).unwrap();

            run(&conn, DiagKind::Memory).await.expect("诊断应当成功");
            tokio::time::sleep(Duration::from_millis(150)).await;

            let text = terminal_text(&store);
            assert!(
                !text.contains("Mem:"),
                "诊断输出不该混进终端画面，实际 {text:?}"
            );

            // 终端仍然可用
            manager
                .send_input(&sid, b"echo still-alive\r".to_vec())
                .expect("诊断之后终端仍应能写入");
            tokio::time::sleep(Duration::from_millis(200)).await;

            assert!(terminal_text(&store).contains("echo still-alive"));
        }
    }

    #[test]
    fn inode_uses_the_same_parser_with_different_headers() {
        let text = "\
Filesystem      Inodes  IUsed   IFree IUse% Mounted on
/dev/vda1      2621440 400000 2221440   16% /
/dev/vdb1      6553600 5898240  655360   90% /data
";
        let result = parse(DiagKind::Inode, text);

        assert_eq!(result.columns[1], "inode 总数");
        assert_eq!(result.rows[0][0], "/data", "使用率高的排前面");
        assert_eq!(result.rows[0][4], "90%");
        assert!(
            result.warning.unwrap_or_default().contains("/data"),
            "inode 90% 也该告警 —— 磁盘有空间却写不进文件就是这个原因"
        );
    }

    #[test]
    fn process_tree_rebuilds_the_hierarchy() {
        // 1 ├─ 412
        //   ├─ 800
        //   │  ├─ 900
        //   │  └─ 950
        //   └─ 960
        let text = "\
    PID    PPID COMMAND
      1       0 systemd
    412       1 systemd-journald
    800       1 sshd
    900     800 bash
    950     800 sh
    960       1 crond
";
        let result = parse(DiagKind::ProcessTree, text);

        assert_eq!(result.rows.len(), 6);
        assert_eq!(result.rows[0][0], "1");
        assert_eq!(result.rows[0][2], "systemd", "顶层不画分支");

        let line_of = |pid: &str| -> String {
            result
                .rows
                .iter()
                .find(|row| row[0] == pid)
                .unwrap_or_else(|| panic!("找不到 pid {pid}"))[2]
                .clone()
        };

        assert_eq!(line_of("412"), "├─ systemd-journald", "中间兄弟用 ├─");
        assert_eq!(line_of("800"), "├─ sshd", "后面还有兄弟，所以不是 └─");
        assert_eq!(
            line_of("900"),
            "│  ├─ bash",
            "父节点有后续兄弟，左侧要画竖线"
        );
        assert_eq!(line_of("950"), "│  └─ sh", "父节点最后一个子进程用 └─");
        assert_eq!(line_of("960"), "└─ crond", "最后一个顶层用 └─");
    }

    #[test]
    fn process_tree_survives_cyclic_ppids() {
        // 真实数据不会这样，但异常时不能死循环，也不能交白卷
        let text = "\
    PID    PPID COMMAND
      1       2 alpha
      2       1 beta
";
        let result = parse(DiagKind::ProcessTree, text);
        assert_eq!(result.rows.len(), 2, "环里的节点也要展示出来");
    }

    #[test]
    fn parse_size_understands_units() {
        assert_eq!(parse_size("1Ki"), 1024.0);
        assert_eq!(parse_size("1.5Gi"), 1.5 * 1024.0 * 1024.0 * 1024.0);
        assert_eq!(parse_size("512B"), 512.0);
        assert_eq!(parse_size("—"), 0.0);
    }
}
