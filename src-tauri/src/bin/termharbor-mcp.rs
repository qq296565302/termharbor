//! TermHarbor 的 MCP 服务端（stdio）。
//!
//! 由 MCP 宿主（Claude Desktop / WorkBuddy 等）以子进程方式拉起，用**换行分隔的
//! JSON-RPC 2.0** 通信。
//!
//! 规矩只有一条，但破了就全废：**stdout 只能写协议帧**。
//! 任何日志、警告、panic 信息都必须走 stderr —— 混进去一行，宿主那边就是
//! "连接成功但工具一个都看不到"，而且很难看出原因。
//!
//! 配置示例（宿主侧）：
//! ```json
//! {
//!   "mcpServers": {
//!     "termharbor": { "command": "C:\\path\\to\\termharbor-mcp.exe" }
//!   }
//! }
//! ```

use std::io::{self, BufRead, Write};

use termharbor_lib::mcp::{Context, Server};

#[tokio::main]
async fn main() {
    // 路径算不出来（APPDATA 缺失之类）就直说，别让宿主看到"进程退出了但没原因"
    let ctx = match Context::new() {
        Ok(ctx) => ctx,
        Err(err) => {
            eprintln!("[termharbor-mcp] 初始化失败: {err}");
            std::process::exit(2);
        }
    };

    eprintln!(
        "[termharbor-mcp] 启动，数据目录 {}",
        ctx.paths().data_dir.display()
    );

    let server = Server::new(ctx);
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            // 读 stdin 出错（一般是宿主关管道），安静退出，不要往 stdout 写任何东西
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
