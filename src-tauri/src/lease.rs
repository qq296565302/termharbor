//! 会话租约：把「客户端此刻正连着哪些服务器」写成一份文件，给 MCP 侧看。
//!
//! ## 为什么需要它
//!
//! 0.1.26 为了解决「宿主把 MCP 起在非交互会话里读不到密钥」，给 MCP 配了一条
//! 独立拿到密钥的路（密钥镜像 + 直接开库）。代价是 **MCP 完全不受客户端生命周期约束**：
//! 用户在界面上关掉连接、甚至退出整个应用，智能体照样能连服务器 —— 因为它是
//! 另一条 SSH 连接、用的是同一份库里存的凭据。
//!
//! 用户的诉求很直接：「我关了连接，它就不该还能连，直到我下次重连」。
//! 所以加一份**运行时租约**：客户端在跑的时候每 5 秒往产品目录写一次
//! 「当前打开着的连接 id」，MCP 每个联网工具执行前先看这份文件 ——
//! 连接不在名单里、或者心跳过期（客户端已退出/被杀），一律拒绝。
//!
//! ## 三个数字的由来
//!
//! - [`BEAT_MS`] 5 秒：一次约 200 字节的写，代价可以忽略。
//! - [`TTL_MS`] 15 秒：容忍三次丢拍。设太短会在系统卡顿时误判「客户端没了」，
//!   设太长则杀掉客户端后还能继续用一个不短的时间窗口。
//! - **退出时主动删文件**，所以正常关窗是立即失效，只有崩溃才走 TTL。
//!
//! ## 它不是什么
//!
//! 不是权限体系的一部分，是一道**在线闸门**。用户真要 7×24 无人值守访问，
//! 在设置里关掉「在线会话校验」即可（`mcp.json` 的 `requireLiveSession`），
//! 那时本模块整个不参与判断。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::{AppState, PRODUCT_FOLDER};

/// 租约文件名。和 `mcp.json` / `audit.jsonl` 放同一个目录（产品根目录下的 `TermHarbor`）。
pub const FILE_NAME: &str = "session-lease.json";
/// 心跳间隔。
pub const BEAT_MS: u64 = 5_000;
/// 超过这个时间没心跳就认为客户端已经不在了。
pub const TTL_MS: u64 = 15_000;

const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lease {
    version: u32,
    /// 写这份租约的进程。排查「是不是还有个残留进程在续租」时用得上。
    pid: u32,
    beat_ms: u64,
    connections: Vec<String>,
}

/// 租约目录。**必须和 `mcp::data_dir` 算出同一个位置**，否则 MCP 会去读一个
/// 永远不存在的文件 —— 症状是「什么都拒绝」，比不检查更让人抓狂。
pub fn dir(product_dir: &Path) -> PathBuf {
    product_dir.join(PRODUCT_FOLDER)
}

pub fn path(product_dir: &Path) -> PathBuf {
    dir(product_dir).join(FILE_NAME)
}

/// 心跳读出来的三种状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// 客户端在跑，这是它当前连着的连接 id
    Live(Vec<String>),
    /// 文件在但心跳停了：客户端被杀、或者从挂起里没醒过来
    Expired { age_ms: u64 },
    /// 没有文件：客户端没启动过，或者正常退出时删掉了
    Missing,
}

/// 某台机器现在能不能被用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Miss {
    /// 客户端没在运行
    NotRunning,
    /// 心跳过期（进程被杀 / 休眠）
    Expired,
    /// 客户端在跑，但这台机器当前没连
    NotConnected,
}

/// 原子写：先写临时文件再 rename。
///
/// 不能直接覆写 —— MCP 随时可能在读，读到半截 JSON 只会被当成「格式坏」，
/// 表现为一次莫名其妙的拒绝。
pub fn write(product_dir: &Path, connections: &[String]) -> io::Result<()> {
    let mut list = connections.to_vec();
    list.sort();
    list.dedup();

    let lease = Lease {
        version: VERSION,
        pid: std::process::id(),
        beat_ms: crate::state::now_millis(),
        connections: list,
    };
    let body = serde_json::to_vec(&lease).map_err(io::Error::other)?;

    let file = path(product_dir);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension("json.tmp");
    fs::write(&tmp, &body)?;
    // Windows 上 rename 会覆盖已存在的目标（MoveFileEx 带 REPLACE_EXISTING），
    // 所以这里不需要先删；先删反而会留出一个「文件不存在」的窗口。
    fs::rename(&tmp, &file)
}

/// 正常退出时删掉。删失败也不影响正确性 —— TTL 兜底。
pub fn clear(product_dir: &Path) {
    let _ = fs::remove_file(path(product_dir));
}

/// 读当前状态。`now_ms` 传进来而不是内部取，测试才能造出「过期」。
pub fn status(product_dir: &Path, now_ms: u64) -> Status {
    let Ok(raw) = fs::read(path(product_dir)) else {
        return Status::Missing;
    };
    // 解析失败一律当「不在」：宁可让 agent 找用户重连，也不能因为一份坏文件放行。
    let Ok(lease) = serde_json::from_slice::<Lease>(&raw) else {
        return Status::Missing;
    };
    if lease.version != VERSION {
        return Status::Missing;
    }

    // saturating：时钟被往回调整时不要算出天文数字
    let age_ms = now_ms.saturating_sub(lease.beat_ms);
    if age_ms > TTL_MS {
        return Status::Expired { age_ms };
    }
    Status::Live(lease.connections)
}

/// MCP 侧的闸门：这台机器此刻能不能连。
pub fn check(product_dir: &Path, connection_id: &str, now_ms: u64) -> Result<(), Miss> {
    match status(product_dir, now_ms) {
        Status::Missing => Err(Miss::NotRunning),
        Status::Expired { .. } => Err(Miss::Expired),
        Status::Live(open) => {
            if open.iter().any(|id| id == connection_id) {
                Ok(())
            } else {
                Err(Miss::NotConnected)
            }
        }
    }
}

/// 把当前真实状态写进租约。GUI 在连接建立/断开后立即调一次，心跳线程定时调。
///
/// **不另做一份「打开中的连接」簿记**，直接问两个 manager —— 簿记两份必然会对不上，
/// 而这里对不上的后果是「界面上明明连着，智能体却说没连」。
pub fn publish(state: &AppState) {
    let mut ids: Vec<String> = state
        .sessions
        .list()
        .into_iter()
        .map(|info| info.connection_id)
        .collect();
    ids.extend(state.sftp.connection_ids());

    if let Err(err) = write(&state.paths.product_dir, &ids) {
        tracing::warn!(error = %err, "写会话租约失败，智能体可能被误判为不可用");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        // 用 test_support 的序号生成器，不用时间戳：Windows 的 SystemTime 只有
        // 毫秒精度，并行跑测试时会撞名。
        let dir = crate::ssh::test_support::unique_path("lease");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn roundtrip_keeps_open_connection_ids() {
        let dir = temp_dir();
        write(&dir, &ids(&["c1", "c2"])).expect("写租约");

        let st = status(&dir, crate::state::now_millis());
        assert_eq!(st, Status::Live(ids(&["c1", "c2"])));
        assert_eq!(check(&dir, "c1", crate::state::now_millis()), Ok(()));
    }

    /// 关闭连接后（列表里没有它了）必须立刻拒绝 —— 这是这个功能存在的意义。
    #[test]
    fn closed_connection_is_rejected_immediately() {
        let dir = temp_dir();
        write(&dir, &ids(&["c1"])).expect("写租约");
        // 用户关掉 c1，只剩 c2
        write(&dir, &ids(&["c2"])).expect("重写租约");

        let now = crate::state::now_millis();
        assert_eq!(check(&dir, "c1", now), Err(Miss::NotConnected));
        assert_eq!(check(&dir, "c2", now), Ok(()));
    }

    #[test]
    fn no_file_means_client_not_running() {
        let dir = temp_dir();
        assert_eq!(status(&dir, crate::state::now_millis()), Status::Missing);
        assert_eq!(
            check(&dir, "c1", crate::state::now_millis()),
            Err(Miss::NotRunning)
        );
    }

    /// 客户端被强杀时文件还在，但心跳停了 —— 这种情况必须也判死，
    /// 否则「杀进程」就等于绕过了整个闸门。
    #[test]
    fn stale_heartbeat_expires() {
        let dir = temp_dir();
        write(&dir, &ids(&["c1"])).expect("写租约");
        let later = crate::state::now_millis() + TTL_MS + 1_000;

        assert!(matches!(status(&dir, later), Status::Expired { .. }));
        assert_eq!(check(&dir, "c1", later), Err(Miss::Expired));
    }

    /// TTL 边界：刚好等于 TTL 还不算过期，多 1 毫秒就算。
    ///
    /// 这里不用 `write()`（它写的是当前时间，和测试里的 now 会差几毫秒），
    /// 直接落一份 beatMs 固定的租约，边界才是确定的。
    #[test]
    fn heartbeat_expires_exactly_after_ttl() {
        let root = temp_dir();
        let beat = 1_000_000u64;
        let body = serde_json::json!({
            "version": 1,
            "pid": 1,
            "beatMs": beat,
            "connections": ["c1"],
        });
        fs::create_dir_all(dir(&root)).unwrap();
        fs::write(path(&root), serde_json::to_vec(&body).unwrap()).unwrap();

        assert!(matches!(status(&root, beat + TTL_MS), Status::Live(_)));
        assert!(matches!(
            status(&root, beat + TTL_MS + 1),
            Status::Expired { .. }
        ));
    }

    /// 文件被写坏时按「不在」处理，绝不能因为解析失败而放行。
    #[test]
    fn corrupted_file_fails_closed() {
        let dir = temp_dir();
        write(&dir, &ids(&["c1"])).expect("先写一份正常的");
        fs::write(path(&dir), b"{ not json").expect("写坏文件");

        assert_eq!(status(&dir, crate::state::now_millis()), Status::Missing);
        assert_eq!(
            check(&dir, "c1", crate::state::now_millis()),
            Err(Miss::NotRunning)
        );
    }

    #[test]
    fn clear_removes_the_file() {
        let dir = temp_dir();
        write(&dir, &ids(&["c1"])).expect("写租约");
        clear(&dir);
        assert!(!path(&dir).exists(), "退出后不该留下租约");
        assert_eq!(
            check(&dir, "c1", crate::state::now_millis()),
            Err(Miss::NotRunning)
        );
    }

    /// 重复 id 要去重：两次心跳之间连接数不该让文件变大。
    #[test]
    fn duplicate_ids_are_collapsed() {
        let dir = temp_dir();
        write(&dir, &ids(&["c1", "c1", "c1"])).expect("写租约");
        assert_eq!(
            status(&dir, crate::state::now_millis()),
            Status::Live(ids(&["c1"]))
        );
    }
}
