//! 命令片段（需求 5.7，M6：只读命令库 + 插入终端）。
//!
//! 三个设计判断：
//!
//! 1. **内置命令写死在代码里，不进数据库。**
//!    需求原文是「随安装包 seed，首次启动导入，升级时合并」，但那只在「用户可编辑内置片段」时才成立。
//!    内置只读命令本质上是程序的一部分（和快捷键表同一性质）：升级时自然拿到新版，
//!    不需要合并逻辑，也不会出现「内置命令被用户改坏」的状态。M7 的自定义片段才进 `snippets` 表，
//!    届时列表 = 内置（代码）+ 自定义（DB），来源分明。
//!
//! 2. **风险等级不写死，而是每次由 [`classify_risk`] 算出来。**
//!    标注和判定共用一个函数，就不可能不一致 —— SEC-SN-10 要求的「以后端判定为准」在结构上成立。
//!    代价是判定函数必须够准，所以有测试逐条校验内置库全为只读。
//!
//! 3. **插入不回车**（A-12）。前端拿到 `render` 的文本后只是写进终端，
//!    是否执行由用户按回车决定。

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::monitor::DiagKind;

/// 风险级别。前端不得自行降级（SEC-SN-10）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Readonly,
    Dangerous,
    Critical,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetDto {
    pub id: String,
    pub name: String,
    pub command: String,
    pub description: String,
    pub group: String,
    pub risk: RiskLevel,
    /// 有些片段除了能插入终端，还能在诊断面板里渲染成表格
    pub diag: Option<DiagKind>,
}

struct Builtin {
    id: &'static str,
    name: &'static str,
    command: &'static str,
    description: &'static str,
    group: &'static str,
}

/// 内置只读诊断库。
///
/// 只收**无参数**、**只读**、**主流发行版都有**的命令。
/// 需要传参的（指定端口、指定服务、指定日志文件）留给 M7 的变量占位符，
/// 现在硬塞一个示例值只会让用户误以为可以直接执行。
const BUILTIN: &[Builtin] = &[
    Builtin {
        id: "sys.uname",
        name: "系统信息",
        command: "uname -a",
        description: "内核版本、主机名、硬件架构",
        group: "系统",
    },
    Builtin {
        id: "sys.release",
        name: "发行版信息",
        command: "cat /etc/os-release",
        description: "发行版名称与版本号",
        group: "系统",
    },
    Builtin {
        id: "sys.uptime",
        name: "运行时长与负载",
        command: "uptime",
        description: "已运行时长、登录用户数、1/5/15 分钟平均负载",
        group: "系统",
    },
    Builtin {
        id: "res.memory",
        name: "内存使用",
        command: "free -h",
        description: "内存与 swap 的已用/可用（含 buff/cache）",
        group: "资源",
    },
    Builtin {
        id: "res.disk",
        name: "磁盘使用",
        command: "df -hP -x tmpfs -x devtmpfs",
        description: "各挂载点容量与使用率（已滤掉 tmpfs 噪音）。使用率超过 80% 要留意，90% 以上该清理",
        group: "资源",
    },
    Builtin {
        id: "res.inode",
        name: "inode 使用",
        command: "df -iP -x tmpfs -x devtmpfs",
        description: "inode 使用率。磁盘明明有空间却写不进文件时先看这里",
        group: "资源",
    },
    Builtin {
        id: "res.dirsize",
        name: "当前目录各项占用",
        command: "du -sh ./* 2>/dev/null | sort -h | tail -20",
        description: "按大小排序，找占空间的大头",
        group: "资源",
    },
    Builtin {
        id: "proc.cpu",
        name: "CPU 占用前 20",
        command: "ps -eo pid,user,pcpu,pmem,comm --sort=-pcpu | head -15",
        description: "CPU 占用最高的 15 个进程。相比 ps aux 少了 6 列，进程名不会折行",
        group: "进程",
    },
    Builtin {
        id: "proc.mem",
        name: "内存占用前 20",
        command: "ps -eo pid,user,pcpu,pmem,comm --sort=-pmem | head -15",
        description: "内存占用最高的 15 个进程",
        group: "进程",
    },
    Builtin {
        id: "proc.tree",
        name: "进程树",
        command: "ps -eo pid,ppid,comm",
        description: "父子关系视图，按层级缩进展示前 40 个进程。极简系统（busybox）不支持 -eo，改用 ps aux",
        group: "进程",
    },
    Builtin {
        id: "net.listen",
        name: "端口监听",
        command: "ss -tulnp | head -30",
        description: "TCP/UDP 监听与已建立连接。看 State 列：LISTEN 才是对外提供服务；无 ss 时用 netstat -tulnp",
        group: "网络",
    },
    Builtin {
        id: "net.summary",
        name: "连接统计",
        command: "ss -s",
        description: "TCP/UDP 连接数汇总，排查连接数暴涨",
        group: "网络",
    },
    Builtin {
        id: "net.lsof",
        name: "占用端口的进程",
        command: "lsof -i -P -n | head -30",
        description: "按端口反查进程；需要 lsof，未安装时用 ss -tulnp",
        group: "网络",
    },
    Builtin {
        id: "log.journal",
        name: "系统日志尾部",
        command: "journalctl -n 40 --no-pager -o short-iso",
        description: "最近 40 条 systemd 日志，ISO 短时间戳一行一条。只看时间是否连续就能判断服务有没有反复重启；非 systemd 系统改用 tail -n 40 /var/log/messages",
        group: "日志",
    },
];

/// 命中即判高危（SEC-SN-05）。
///
/// 这里的匹配是**宽松**的：宁可把只读命令误判成危险（用户多看一眼），
/// 也不能把危险命令漏成只读。测试会校验内置库没有被误判。
const CRITICAL_PATTERNS: &[&str] = &[
    "rm -rf",
    "rm -fr",
    "rm -r -f",
    "mkfs",
    "dd if=",
    "of=/dev/",
    "wipefs",
    "shutdown",
    "reboot",
    "poweroff",
    "halt",
    "kill -9",
    "killall -9",
    ":(){",
];

/// 有副作用但不到灾难级。
const DANGEROUS_PATTERNS: &[&str] = &[
    "systemctl stop",
    "systemctl restart",
    "systemctl disable",
    "systemctl enable",
    "service ",
    "kill",
    "pkill",
    "killall",
    "rm ",
    "rmdir",
    "mv ",
    "truncate",
    "chmod",
    "chown",
    "iptables",
    "ufw ",
    "> ",
    ">> ",
];

/// 判定一条命令的风险等级。
///
/// 只看文本，不理解语义 —— 这不是沙箱，只是给用户一个「要不要再看一眼」的信号。
pub fn classify_risk(command: &str) -> RiskLevel {
    let lowered = command.to_lowercase();

    if CRITICAL_PATTERNS
        .iter()
        .any(|pattern| lowered.contains(pattern))
    {
        return RiskLevel::Critical;
    }

    if DANGEROUS_PATTERNS
        .iter()
        .any(|pattern| lowered.contains(pattern))
    {
        return RiskLevel::Dangerous;
    }

    RiskLevel::Readonly
}

/// 哪些片段能在诊断面板里以表格展示。
///
/// 单独一张表而不是塞进 `Builtin` 结构：映射集中在一处，加新片段只改这里。
/// 有一条测试保证「登记了诊断的片段，它的命令必须和 `DiagKind::command` 完全一致」——
/// 否则用户点「插入」和点「查看」执行的就是两条不同的命令。
fn diag_of(id: &str) -> Option<DiagKind> {
    match id {
        "res.disk" => Some(DiagKind::Disk),
        "res.inode" => Some(DiagKind::Inode),
        "res.memory" => Some(DiagKind::Memory),
        "proc.cpu" => Some(DiagKind::Processes),
        "net.listen" => Some(DiagKind::Ports),
        "proc.tree" => Some(DiagKind::ProcessTree),
        _ => None,
    }
}

/// 列出片段，可按名称/命令/描述/分组模糊过滤。
pub fn list(query: Option<&str>) -> Vec<SnippetDto> {
    let keyword = query
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);

    BUILTIN
        .iter()
        .filter(|item| match &keyword {
            None => true,
            Some(keyword) => {
                item.name.to_lowercase().contains(keyword)
                    || item.command.to_lowercase().contains(keyword)
                    || item.description.to_lowercase().contains(keyword)
                    || item.group.to_lowercase().contains(keyword)
            }
        })
        .map(|item| SnippetDto {
            id: item.id.to_string(),
            name: item.name.to_string(),
            command: item.command.to_string(),
            description: item.description.to_string(),
            group: item.group.to_string(),
            // 每次现算，保证「标注」与「判定」永远是同一个结论
            risk: classify_risk(item.command),
            diag: diag_of(item.id),
        })
        .collect()
}

/// 取一条片段最终要写进终端的文本。
///
/// M6 没有变量，`render` 只做存在性检查与控制字符校验。
/// 校验不能省：命令里如果混进换行，等于一条"片段"能塞进多条命令，
/// 用户按一次回车就执行了没预览过的东西（A-12 / SEC-SN-09）。
pub fn render(id: &str) -> AppResult<String> {
    let snippet = BUILTIN
        .iter()
        .find(|item| item.id == id)
        .ok_or_else(|| AppError::validation(format!("片段不存在: {id}")))?;

    let has_control = snippet
        .command
        .chars()
        .any(|ch| (ch.is_control() && ch != '\t') || ch == '\r');

    if has_control {
        return Err(AppError::internal(format!(
            "片段 {} 含有控制字符，已拒绝渲染",
            snippet.id
        )));
    }

    Ok(snippet.command.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_snippet_is_readonly() {
        // 内置库的守门人：谁往 BUILTIN 里加一条带 rm/kill/重定向的命令都会被拦下来，
        // 要么改命令，要么承认它该走危险流程。
        for item in BUILTIN {
            assert_eq!(
                classify_risk(item.command),
                RiskLevel::Readonly,
                "内置片段「{}」被判成了非只读: {}",
                item.name,
                item.command
            );
        }
    }

    #[test]
    fn builtin_ids_are_unique() {
        let mut ids: Vec<&str> = BUILTIN.iter().map(|item| item.id).collect();
        ids.sort_unstable();
        let total = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), total, "片段 id 不能重复");
    }

    #[test]
    fn critical_keywords_are_caught() {
        for command in [
            "rm -rf /var/log",
            "sudo mkfs.ext4 /dev/sdb1",
            "dd if=/dev/zero of=/dev/sda",
            "shutdown -h now",
            "reboot",
            "kill -9 1234",
        ] {
            assert_eq!(
                classify_risk(command),
                RiskLevel::Critical,
                "应当判为高危: {command}"
            );
        }
    }

    #[test]
    fn side_effecting_commands_are_dangerous() {
        for command in [
            "systemctl stop nginx",
            "rm /tmp/a.txt",
            "mv /a /b",
            "chmod 777 /var/www",
        ] {
            assert_eq!(
                classify_risk(command),
                RiskLevel::Dangerous,
                "应当判为危险: {command}"
            );
        }
    }

    #[test]
    fn readonly_commands_stay_readonly() {
        for command in [
            "df -h",
            "free -h",
            "uptime",
            "ss -tulnp",
            "ps aux --sort=-%cpu | head -20",
            "cat /etc/os-release",
            "journalctl -n 100 --no-pager",
        ] {
            assert_eq!(
                classify_risk(command),
                RiskLevel::Readonly,
                "应当判为只读: {command}"
            );
        }
    }

    #[test]
    fn list_filters_by_name_command_and_group() {
        // 搜索覆盖名称/命令/描述/分组四类，所以一条命中多个是正常的
        let disk = list(Some("磁盘"));
        assert!(disk.iter().any(|item| item.id == "res.disk"), "按名称匹配");
        assert!(
            disk.iter().any(|item| item.id == "res.inode"),
            "描述里提到磁盘的也该命中"
        );

        assert!(list(Some("df -h")).iter().any(|item| item.id == "res.disk"));

        let network = list(Some("网络"));
        for id in ["net.listen", "net.summary", "net.lsof"] {
            assert!(
                network.iter().any(|item| item.id == id),
                "按分组匹配应包含 {id}"
            );
        }

        assert_eq!(list(Some("   ")).len(), BUILTIN.len(), "空白查询等于不过滤");
        assert!(list(Some("不存在的关键词")).is_empty());
    }

    #[test]
    fn render_returns_the_registered_command() {
        // 不写死字符串：命令本身会随可读性调整而变化，这里只保证「取到的就是库里那条」
        let expected = BUILTIN
            .iter()
            .find(|item| item.id == "res.disk")
            .expect("res.disk 应当存在");
        assert_eq!(render("res.disk").unwrap(), expected.command);
    }

    #[test]
    fn render_rejects_unknown_id() {
        let err = render("nope").unwrap_err();
        assert_eq!(err.code, crate::error::ErrCode::ValidationError);
    }

    #[test]
    fn diag_snippets_run_exactly_the_same_command() {
        // 同一张卡片，点「插入」和点「查看」必须是同一条命令，
        // 否则用户会以为自己在终端里看到的就是面板里那份结果。
        for item in BUILTIN {
            let Some(kind) = diag_of(item.id) else {
                continue;
            };
            assert_eq!(
                item.command,
                kind.command(),
                "片段「{}」的命令与诊断命令不一致",
                item.name
            );
        }
    }

    #[test]
    fn six_snippets_have_diagnostics() {
        let with_diag = list(None)
            .into_iter()
            .filter(|item| item.diag.is_some())
            .count();
        assert_eq!(with_diag, 6, "磁盘 / inode / 内存 / 进程 / 端口 / 进程树");
    }

    #[test]
    fn list_marks_every_entry_readonly_in_this_version() {
        // 前端按 risk 上色；这里出现非只读说明 M6 的范围被突破了
        for item in list(None) {
            assert_eq!(item.risk, RiskLevel::Readonly);
        }
    }
}
