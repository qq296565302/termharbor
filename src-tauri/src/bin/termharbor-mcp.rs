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

use termharbor_lib::mcp::Server;

fn main() {
    let server = Server::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            // 读 stdin 出错（一般是宿主关管道），安静退出，不要往 stdout 写任何东西
            break;
        };

        let Some(response) = server.handle_line(&line) else {
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
