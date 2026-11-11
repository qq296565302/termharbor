//! MCP（Model Context Protocol）服务端。
//!
//! 形态与取舍见 `docs/开发计划.md` §17：**独立的无头二进制 + stdio + JSON-RPC 2.0**，
//! 复用 `storage` / `crypto` / `ssh` / `sftp`（这几层本来就不依赖 Tauri）。
//!
//! 为什么手写协议而不引 `rmcp`：M8-1 只需要 `initialize` / `tools/list` / `tools/call`
//! 三个方法，手写约三百行且每一步都能单测；官方 SDK 的 API 还在快速变化，一旦版本跳变，
//! 排障成本比省下的代码量大得多。
//!
//! **stdout 是协议通道**：任何日志、调试输出都必须走 stderr，否则宿主收到的就是一堆
//! 无法解析的行，症状是"连上了但工具全都不见了"。

pub mod audit;
pub mod config;
pub mod context;
pub mod exec;
pub mod jsonrpc;
pub mod server;
pub mod tools;

pub use context::Context;
pub use server::Server;

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

/// MCP 的配置与审计放 `%APPDATA%\TermHarbor\`，和快照同一个产品目录。
///
/// 0.1.28 及更早直接用了 `product_dir`（也就是 `%APPDATA%` 本身），
/// `mcp.json` / `audit.jsonl` 散在了 Roaming 根目录 —— 那不是我们的地盘，
/// 用户打开 `%APPDATA%` 会莫名其妙多出两个文件。0.1.29 起收进子目录，
/// 并在第一次取用时把旧位置的搬过来。
///
/// 搬迁失败时**整体退回旧目录**：宁可继续用旧位置，也不能让配置"凭空消失"
/// 变成全关（那会让用户以为自己的白名单被清空了）。
pub fn data_dir(product_dir: &Path) -> PathBuf {
    let target = product_dir.join(crate::state::PRODUCT_FOLDER);
    if target == product_dir {
        return target;
    }
    if std::fs::create_dir_all(&target).is_err() {
        return product_dir.to_path_buf();
    }

    // 顺序有讲究：配置先搬。它搬不动就直接回退，审计留在旧位置下次再试。
    for name in [config::FILE_NAME, audit::FILE_NAME] {
        let old = product_dir.join(name);
        let new = target.join(name);
        // 两边都有时以新位置为准 —— 说明已经迁移过，旧的多半是用户手工放的
        if !old.exists() || new.exists() {
            continue;
        }
        match std::fs::rename(&old, &new) {
            Ok(()) => tracing::info!(file = name, "MCP 文件已迁到 %APPDATA%\\TermHarbor"),
            Err(err) => {
                tracing::warn!(file = name, error = %err, "MCP 文件迁移失败，继续用旧位置");
                if name == config::FILE_NAME {
                    return product_dir.to_path_buf();
                }
            }
        }
    }

    target
}

/// stdio 主循环。带界面的主程序和独立的 `termharbor-mcp` 都调这一份。
///
/// **stdout 只写协议帧**：日志一律 `eprintln!`。混进去一行，宿主那边就是
/// "连接上了但工具全都不见了"，而且很难排查。
pub async fn serve_stdio() {
    // 日志只走 stderr。宿主会把它收进自己的日志里 —— "连服务器很慢"这种问题，
    // 各个阶段的耗时就靠这里看。
    let _ = crate::logging::init_stderr("info");

    // 路径算不出来（APPDATA 缺失之类）就直说，别让宿主看到"进程退出了但没原因"
    let ctx = match Context::new() {
        Ok(ctx) => ctx,
        Err(err) => {
            eprintln!("[termharbor-mcp] 初始化失败: {err}");
            return;
        }
    };

    tracing::info!(
        data_dir = %ctx.paths().data_dir.display(),
        "MCP 服务启动"
    );

    let server = Server::new(ctx);
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            // 读 stdin 出错（一般是宿主关管道），安静退出
            break;
        };

        let Some(response) = server.handle_line(&line).await else {
            continue;
        };

        if writeln!(stdout, "{response}").is_err() {
            break;
        }
        // 宿主是逐帧读的，不 flush 就会一直等，表现成"程序卡住了"
        if stdout.flush().is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("th-mcp-dir-{tag}-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn files_live_under_the_product_folder() {
        let root = temp_root("under");
        assert_eq!(data_dir(&root), root.join(crate::state::PRODUCT_FOLDER));
    }

    /// 0.1.28 把这两个文件散在了 `%APPDATA%` 根目录。搬进来，不能让用户的
    /// 白名单因为换了位置就"凭空消失"变成全关。
    #[test]
    fn moves_flat_files_into_the_folder() {
        let root = temp_root("move");
        std::fs::write(root.join(config::FILE_NAME), r#"{"version":1}"#).unwrap();
        std::fs::write(root.join(audit::FILE_NAME), "{}\n").unwrap();

        let dir = data_dir(&root);

        assert!(dir.join(config::FILE_NAME).exists(), "配置没搬过去");
        assert!(dir.join(audit::FILE_NAME).exists(), "审计没搬过去");
        assert!(!root.join(config::FILE_NAME).exists(), "旧位置该搬空了");
    }

    /// 已经迁过就别动。尤其是别拿旧位置的残留覆盖新位置 —— 那等于回滚配置。
    #[test]
    fn keeps_files_already_at_the_new_location() {
        let root = temp_root("keep");
        let dir = root.join(crate::state::PRODUCT_FOLDER);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(config::FILE_NAME), "new").unwrap();
        std::fs::write(root.join(config::FILE_NAME), "old").unwrap();

        let got = data_dir(&root);

        let body = std::fs::read_to_string(got.join(config::FILE_NAME)).unwrap();
        assert_eq!(body, "new", "新位置的配置被旧位置的覆盖了");
    }
}
