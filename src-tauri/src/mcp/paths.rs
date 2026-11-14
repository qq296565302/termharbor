//! 高危写入路径判定 —— 专门看「写进去之后，系统会不会自动执行」。
//!
//! ## 为什么和凭据黑名单分开
//!
//! `config::secret_path_reason` 那道门的判据是 **「读到它，等于拿到更大的访问权」**：
//! 私钥、`.pgpass`、`/etc/shadow` 这类是凭据载体，风险发生在「读」这一侧，
//! 属于信息泄露。
//!
//! 本模块的判据完全不同：**「写进去之后，系统会自动加载 / 执行」**。
//! `/etc/systemd/system/kantai.service` 里的一行 `ExecStart`，就是一个随开机
//! 拉起的 root 后门；`/etc/cron.d/x` 会按点被跑起来；`~/.bashrc` 下次开 shell
//! 就生效。这类位置本身不保密（谁都能 `cat` 一眼），危险来自「落盘」这个动作，
//! 属于持久化后门。两件事维度不同，硬塞进一张表会让拒绝文案自相矛盾：
//! `/etc/shadow` 该说「读到即泄密」、归凭据门，`/etc/passwd` 该说「改动影响权限
//! 判定」、归本门，合并之后用户只会看到一句含糊的「敏感文件」。
//! 分开还能守住一条边界：**读操作一律不进这道门** —— 读 `/etc/crontab` 看一眼
//! 计划任务是正常排查，不该被「写后门」的理由误伤。
//!
//! ## 为什么要组件边界
//!
//! 目录前缀若用裸 `starts_with` 比，`/etc/systemd/system` 会把
//! `/etc/systemd/system-old/x` 也吞进来，`/etc/crontab` 会把 `/etc/crontab.bak`
//! 也算进去。对安全名单而言 **误伤比漏挡更消耗信任**：用户被拦一次正常路径，
//! 就会把整个功能当成麻烦、转头去关掉它。因此所有前缀都必须落在组件边界上
//! （要么等于目录，要么以「目录 + `/`」开头），精确项则一律用相等比较。
//!
//! ## 其它约定
//!
//! - 远端是 POSIX：比较 **大小写敏感**，不做 lowercase 折叠。
//! - 比较前把 `\` 统一成 `/`、去掉尾部 `/`，让 Windows 侧传来的路径和目录本身
//!   （`/etc/systemd/system`）都能落到同一套判定上。
//! - 四类写操作（写入 / 上传 / 改名 / 删除）同等对待：它们都改变目标路径的最终
//!   状态，系统据此自动执行 / 加载，命中结果不因动作而异。`op` 只用来给拒绝文案
//!   挑一个动作措辞 —— 既真正用上参数（而不是 `let _ = op;` 敷衍），又不会误伤读。

/// systemd 单元目录。写成不带尾斜杠的形式，边界交给 [`under_dir`] 统一处理。
const SYSTEMD_DIRS: &[&str] = &[
    "/etc/systemd/system",
    "/etc/systemd/user",
    "/usr/lib/systemd/system",
    "/lib/systemd/system",
];

/// cron 相关目录（`/etc/crontab` 是精确项，单独判）。
const CRON_DIRS: &[&str] = &[
    "/etc/cron.d",
    "/etc/cron.daily",
    "/etc/cron.hourly",
    "/etc/cron.weekly",
    "/etc/cron.monthly",
    "/var/spool/cron",
];

/// 家目录登录脚本文件名：basename 精确相等才算命中。
const HOME_RC_NAMES: &[&str] = &[".bashrc", ".bash_profile", ".profile", ".zshrc"];

/// 写操作类型。只有这些会触发本判定；**读操作一律不拦**。
///
/// 「读」刻意不是本枚举的一员：读 `/etc/crontab` 看计划任务是正常排查，
/// 该走的是 `config::secret_path_reason` 那道「读到即泄密」的凭据门，
/// 而不是这道「写进去会被自动执行」的落点门。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOp {
    Write,
    Upload,
    Rename,
    Remove,
}

/// 命中返回「这是什么位置」的中文说明（会直接进拒绝文案给用户看），没命中返回 `None`。
///
/// 说明文案要让人一眼懂危险，例如「systemd 服务单元（写进去会在重启后被执行）」。
/// 文案末尾会带上本次 `op` 的动作措辞（写入 / 上传 / 改名 / 删除），因此四类操作的
/// **命中结果完全一致**、仅措辞随 `op` 变化 —— 这让 `op` 真正参与返回值，而不是被忽略。
pub fn system_path_reason(path: &str, op: WriteOp) -> Option<&'static str> {
    // 远端是 POSIX，但 agent 可能带反斜杠过来，统一一下再判（与 config.rs 一致）。
    let normalized = path.replace('\\', "/");
    // 去掉尾部斜杠：目录本身与「目录下的项」才能共用同一套前缀判定。
    let trimmed = normalized.trim_end_matches('/');
    reason_for_path(trimmed, op)
}

/// 便捷判定：是否属于高危系统写入位置。
///
/// 固定按 [`WriteOp::Write`] 判空即可 —— 四类写操作命中结果一致，
/// 用哪一个当代表都不影响结论。
pub fn is_system_write(path: &str) -> bool {
    system_path_reason(path, WriteOp::Write).is_some()
}

/// 把「是什么位置」与「本次是什么写操作」拼成最终拒绝文案。
///
/// 四类写操作同等对待（命中结果一致），这里只让文案里的动作措辞随 `op` 变化：
/// 既真正用到 `op`、避免 unused 警告，又向用户点明触发的是哪类动作。
/// `concat!` 在编译期拼接字面量，返回值仍是 `&'static str`，无运行时分配、无 panic。
macro_rules! reason_for {
    ($op:expr, $base:literal) => {
        match $op {
            WriteOp::Write => concat!($base, "；本次动作：写入"),
            WriteOp::Upload => concat!($base, "；本次动作：上传"),
            WriteOp::Rename => concat!($base, "；本次动作：改名"),
            WriteOp::Remove => concat!($base, "；本次动作：删除"),
        }
    };
}

/// 前缀命中，但严格遵守**组件边界**：
/// 只有「路径恰好等于目录」或「路径以 目录 + `/` 开头」才算落在目录里。
///
/// 这样 `/etc/systemd/system/foo` 命中，而 `/etc/systemd/system-old/foo` 不会
/// 被 `/etc/systemd/system` 误伤 —— 单纯 `starts_with` 会把后者也吞进来。
/// `dir` 与 `path` 都应是已归一化、不含尾斜杠的形式。
fn under_dir(path: &str, dir: &str) -> bool {
    if path == dir {
        return true;
    }
    match path.strip_prefix(dir) {
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}

/// 取路径最后一段。全程无 unwrap / panic：找不到 `/` 时整串就是 basename。
fn basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(pos) => &path[pos + 1..],
        None => path,
    }
}

/// 登录 rc 文件：只认「路径含 `/` 且 basename 精确等于那几个名字」。
///
/// 加「含 `/`」约束是为了挡住裸相对名 `.bashrc`；basename 精确相等则保证
/// `/opt/app/.bashrc.bak` 这类带后缀的不会被误伤。`~/.bashrc`、
/// `/home/foo/.bashrc`、`/root/.bashrc` 的 basename 都是 `.bashrc`，一并命中。
fn is_login_rc(path: &str) -> bool {
    if !path.contains('/') {
        return false;
    }
    HOME_RC_NAMES.contains(&basename(path))
}

/// 逐类判定。命中即返回带操作措辞的完整说明；`op` 经 [`reason_for!`] 消费。
fn reason_for_path(trimmed: &str, op: WriteOp) -> Option<&'static str> {
    // —— systemd：单元会在开机 / reload 时被拉起，等于常驻后门 ——
    if SYSTEMD_DIRS.iter().any(|dir| under_dir(trimmed, dir)) {
        return Some(reason_for!(
            op,
            "systemd 服务单元（写进去会在重启或 reload 后被执行）"
        ));
    }

    // —— cron：按时间被自动执行 ——
    if trimmed == "/etc/crontab" || CRON_DIRS.iter().any(|dir| under_dir(trimmed, dir)) {
        return Some(reason_for!(op, "系统计划任务（写进去会按时间被自动执行）"));
    }

    // —— 提权：sudoers ——
    if trimmed == "/etc/sudoers" || under_dir(trimmed, "/etc/sudoers.d") {
        return Some(reason_for!(
            op,
            "sudoers 提权配置（改动会直接放大可执行命令与身份）"
        ));
    }

    // —— SSH 服务端配置 ——
    if trimmed == "/etc/ssh/sshd_config" || under_dir(trimmed, "/etc/ssh/sshd_config.d") {
        return Some(reason_for!(
            op,
            "SSH 服务端配置（改动会改变远程登录与鉴权方式）"
        ));
    }

    // —— 动态链接劫持 ——
    if trimmed == "/etc/ld.so.preload" || under_dir(trimmed, "/etc/ld.so.conf.d") {
        return Some(reason_for!(
            op,
            "动态链接器预加载配置（写进去会在进程启动时被自动加载）"
        ));
    }

    // —— 登录即执行：/etc 下的全局脚本 + 家目录 rc 文件 ——
    if matches!(
        trimmed,
        "/etc/profile" | "/etc/bash.bashrc" | "/etc/environment"
    ) || under_dir(trimmed, "/etc/profile.d")
        || is_login_rc(trimmed)
    {
        return Some(reason_for!(
            op,
            "登录即执行的脚本或环境配置（下次登录或开 shell 会被自动执行）"
        ));
    }

    // —— 用户 / 组数据库。刻意不收 /etc/shadow：它归凭据黑名单，避免两道门文案打架 ——
    if matches!(trimmed, "/etc/passwd" | "/etc/group") {
        return Some(reason_for!(
            op,
            "系统用户/组数据库（改动会影响身份与权限判定）"
        ));
    }

    // —— 容器 / 编排守护进程配置 ——
    if under_dir(trimmed, "/etc/docker") || under_dir(trimmed, "/etc/kubernetes") {
        return Some(reason_for!(
            op,
            "容器/编排系统配置（改动会影响守护进程或集群行为）"
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_OPS: [WriteOp; 4] = [
        WriteOp::Write,
        WriteOp::Upload,
        WriteOp::Rename,
        WriteOp::Remove,
    ];

    fn assert_blocked(path: &str, keyword: &str) {
        let reason = system_path_reason(path, WriteOp::Write);
        assert!(reason.is_some(), "该命中：{path}");
        assert!(is_system_write(path), "is_system_write 该命中：{path}");
        assert!(
            reason.is_some_and(|s| s.contains(keyword)),
            "文案该含「{keyword}」：{path} -> {reason:?}"
        );
    }

    fn assert_allowed(path: &str) {
        assert_eq!(
            system_path_reason(path, WriteOp::Write),
            None,
            "不该命中：{path}"
        );
        assert!(!is_system_write(path), "is_system_write 不该命中：{path}");
    }

    #[test]
    fn systemd_units_are_blocked() {
        assert_blocked("/etc/systemd/system/kantai.service", "systemd 服务单元");
        assert_blocked("/etc/systemd/user/agent.service", "systemd 服务单元");
        assert_blocked("/usr/lib/systemd/system/x.service", "systemd 服务单元");
        assert_blocked("/lib/systemd/system/x.service", "systemd 服务单元");
        assert_blocked("/etc/systemd/system", "systemd 服务单元");
    }

    #[test]
    fn cron_paths_are_blocked() {
        assert_blocked("/etc/crontab", "系统计划任务");
        assert_blocked("/etc/cron.d/job", "系统计划任务");
        assert_blocked("/etc/cron.daily/backup", "系统计划任务");
        assert_blocked("/etc/cron.hourly/x", "系统计划任务");
        assert_blocked("/etc/cron.weekly/x", "系统计划任务");
        assert_blocked("/etc/cron.monthly/x", "系统计划任务");
        assert_blocked("/var/spool/cron/crontabs/root", "系统计划任务");
    }

    #[test]
    fn sudoers_paths_are_blocked() {
        assert_blocked("/etc/sudoers", "sudoers");
        assert_blocked("/etc/sudoers.d/deploy", "sudoers");
        assert_blocked("/etc/sudoers.d", "sudoers");
    }

    #[test]
    fn sshd_config_paths_are_blocked() {
        assert_blocked("/etc/ssh/sshd_config", "SSH 服务端配置");
        assert_blocked("/etc/ssh/sshd_config.d/50-x.conf", "SSH 服务端配置");
    }

    #[test]
    fn ld_so_paths_are_blocked() {
        assert_blocked("/etc/ld.so.preload", "动态链接器");
        assert_blocked("/etc/ld.so.conf.d/x.conf", "动态链接器");
    }

    #[test]
    fn login_etc_paths_are_blocked() {
        assert_blocked("/etc/profile", "登录即执行");
        assert_blocked("/etc/profile.d/x.sh", "登录即执行");
        assert_blocked("/etc/bash.bashrc", "登录即执行");
        assert_blocked("/etc/environment", "登录即执行");
    }

    #[test]
    fn home_rc_files_are_blocked_in_three_spellings() {
        // ~ 开头
        assert_blocked("~/.bashrc", "登录即执行");
        assert_blocked("~/.bash_profile", "登录即执行");
        assert_blocked("~/.zshrc", "登录即执行");
        // /home/<user>
        assert_blocked("/home/foo/.bashrc", "登录即执行");
        assert_blocked("/home/foo/.profile", "登录即执行");
        // /root
        assert_blocked("/root/.bashrc", "登录即执行");
        assert_blocked("/root/.zshrc", "登录即执行");
    }

    #[test]
    fn account_db_paths_are_blocked_but_shadow_is_not() {
        assert_blocked("/etc/passwd", "用户/组数据库");
        assert_blocked("/etc/group", "用户/组数据库");
        assert_allowed("/etc/shadow");
    }

    #[test]
    fn container_paths_are_blocked() {
        assert_blocked("/etc/docker/daemon.json", "容器/编排");
        assert_blocked("/etc/kubernetes/manifests/x.yaml", "容器/编排");
    }

    #[test]
    fn ordinary_business_paths_are_not_blocked() {
        for path in [
            "/opt/kantai/app.js",
            "/var/lib/kantai/db.sqlite",
            "/var/log/nginx/error.log",
            "/etc/nginx/nginx.conf",
            "/home/a/.ssh/known_hosts",
            "/tmp/x/update.sh",
        ] {
            assert_allowed(path);
        }
    }

    #[test]
    fn component_boundary_is_respected() {
        // 前缀不能吞掉「长得像」的兄弟目录 / 备份文件
        assert_allowed("/etc/systemd/system-old/x.service");
        assert_allowed("/etc/systemd/system.bak/x");
        assert_allowed("/etc/crontab.bak");
        assert_allowed("/etc/cron.daily-old/x");
        assert_allowed("/etc/sudoers.bak");
        assert_allowed("/etc/passwd-");
        assert_allowed("/etc/group.old");
        // rc 文件带后缀同样不算
        assert_allowed("/opt/app/.bashrc.bak");
        assert_allowed("/home/a/.zshrc.swp");
    }

    #[test]
    fn backslashes_are_normalized() {
        assert_blocked(r"\etc\systemd\system\kantai.service", "systemd 服务单元");
        assert_blocked(r"\etc\crontab", "系统计划任务");
        assert_blocked(r"\home\foo\.bashrc", "登录即执行");
    }

    #[test]
    fn trailing_slashes_are_trimmed() {
        assert_blocked("/etc/systemd/system/", "systemd 服务单元");
        assert_blocked("/etc/crontab/", "系统计划任务");
        assert_blocked("/etc/sudoers.d/", "sudoers");
        assert_blocked("/etc/docker/", "容器/编排");
        // 正常路径即便带尾斜杠也不该被误伤
        assert_allowed("/opt/kantai/app.js/");
    }

    #[test]
    fn directory_itself_counts_as_hit() {
        assert_blocked("/etc/systemd/system", "systemd 服务单元");
        assert_blocked("/etc/sudoers.d", "sudoers");
        assert_blocked("/etc/cron.d", "系统计划任务");
        assert_blocked("/etc/ssh/sshd_config.d", "SSH 服务端配置");
        assert_blocked("/etc/profile.d", "登录即执行");
        assert_blocked("/etc/docker", "容器/编排");
        assert_blocked("/etc/kubernetes", "容器/编排");
    }

    #[test]
    fn tilde_prefix_is_recognized() {
        assert_blocked("~/.bashrc", "登录即执行");
        assert_blocked("~/.profile", "登录即执行");
        // 单独一个裸文件名不含 '/'，按规则不判
        assert_allowed(".bashrc");
    }

    #[test]
    fn comparison_is_case_sensitive() {
        // 远端是 POSIX，/ETC 与 /etc 是两个不同路径，不做大小写折叠
        assert_allowed("/ETC/systemd/system/x.service");
        assert_allowed("/etc/Crontab");
        assert_allowed("/home/a/.BASHRC");
    }

    #[test]
    fn all_write_ops_agree_on_hit_or_miss() {
        // 「行为一致」= 命中结果一致：改名 / 删除同样是写，不能因动作不同就放行。
        // 文案里的动作措辞随 op 变化（见 reason_text_varies_by_op），不影响是否命中。
        let blocked = "/etc/systemd/system/kantai.service";
        for op in ALL_OPS {
            assert!(system_path_reason(blocked, op).is_some(), "{op:?} 应命中");
        }
        let allowed = "/opt/kantai/app.js";
        for op in ALL_OPS {
            assert_eq!(system_path_reason(allowed, op), None, "{op:?} 不该命中");
        }
        assert!(is_system_write(blocked));
        assert!(!is_system_write(allowed));
    }

    #[test]
    fn reason_text_varies_by_op() {
        // op 真的参与返回值：同一路径下四类操作的动作措辞互不相同，
        // 证明没有用 `let _ = op;` 敷衍，也不会有 unused 警告。
        let p = "/etc/crontab";
        let w = system_path_reason(p, WriteOp::Write);
        let u = system_path_reason(p, WriteOp::Upload);
        let r = system_path_reason(p, WriteOp::Rename);
        let d = system_path_reason(p, WriteOp::Remove);
        assert!(w.is_some() && u.is_some() && r.is_some() && d.is_some());
        assert_ne!(w, u);
        assert_ne!(w, r);
        assert_ne!(w, d);
        assert_ne!(u, r);
        assert_ne!(u, d);
        assert_ne!(r, d);
        // 四类文案都保留「这是什么位置」的核心说明
        for reason in [w, u, r, d] {
            assert!(
                reason.is_some_and(|s| s.contains("系统计划任务")),
                "文案该保留位置说明：{reason:?}"
            );
        }
    }

    #[test]
    fn is_system_write_matches_system_path_reason() {
        for path in [
            "/etc/systemd/system/x.service",
            "/etc/crontab",
            "/etc/shadow",
            "/opt/kantai/app.js",
            "/home/a/.ssh/known_hosts",
        ] {
            assert_eq!(
                is_system_write(path),
                system_path_reason(path, WriteOp::Write).is_some(),
                "便捷判定应与主判定一致：{path}"
            );
        }
    }
}
