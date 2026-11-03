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

pub mod context;
pub mod jsonrpc;
pub mod server;
pub mod tools;

pub use context::Context;
pub use server::Server;

use std::io::{self, BufRead, Write};

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
