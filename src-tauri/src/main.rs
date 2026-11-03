// release 构建不弹控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `--mcp` 走无头的 MCP stdio 服务，**不初始化 Tauri**（不开窗口、不加载 WebView2）。
    // 这样 MCP 宿主指向已安装的主程序就行，用户不用再单独装一个 exe。
    //
    // 注意：release 是 windows 子系统，自己不分配控制台；但宿主是用管道接的
    // stdin/stdout，子系统进程照样能读写这两个句柄。
    if std::env::args().skip(1).any(|arg| arg == "--mcp") {
        std::process::exit(run_mcp());
    }

    termharbor_lib::run()
}

fn run_mcp() -> i32 {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("[termharbor-mcp] 无法创建运行时: {err}");
            return 2;
        }
    };

    runtime.block_on(termharbor_lib::mcp::serve_stdio());
    0
}
