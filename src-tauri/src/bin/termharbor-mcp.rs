//! 独立的 MCP 服务端二进制。
//!
//! **普通用户不需要单独装它**：主程序支持 `termharbor.exe --mcp`，走的是同一份
//! `mcp::serve_stdio()`。留这个二进制是为了排障（干净的控制台、不和 GUI 抢）
//! 以及某些宿主只认独立命令行的场景。
//!
//! stdout 只写协议帧，日志一律走 stderr —— 混进去一行，宿主那边就是
//! "连接成功但工具一个都看不到"。

#[tokio::main]
async fn main() {
    termharbor_lib::mcp::serve_stdio().await;
}
