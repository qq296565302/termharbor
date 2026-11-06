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

    // `--export-key`：在正常交互会话里把数据密钥导出一次，供后台会话里的 MCP 用。
    // 系统凭据管理器在非交互登录会话（服务/计划任务）里会报 ERROR_NO_SUCH_LOGON_SESSION。
    if std::env::args().skip(1).any(|arg| arg == "--export-key") {
        std::process::exit(export_key());
    }

    termharbor_lib::run()
}

fn export_key() -> i32 {
    let result = (|| -> termharbor_lib::error::AppResult<std::path::PathBuf> {
        let paths = termharbor_lib::state::AppPaths::resolve_headless()?;
        let file = termharbor_lib::crypto::export_key_to_devfile(&paths.data_dir)
            .map_err(termharbor_lib::error::AppError::from)?;
        Ok(file)
    })();

    match result {
        Ok(file) => {
            eprintln!("已导出数据密钥到：{}", file.display());
            eprintln!(
                "在 MCP 宿主里设置环境变量 TERMHARBOR_KEY_PROVIDER=devfile，后台会话就能读到同一把密钥。"
            );
            eprintln!("注意：这是明文文件，只靠用户目录 ACL 保护，用完请删除并恢复默认。");
            0
        }
        Err(err) => {
            eprintln!("导出失败：{err}");
            1
        }
    }
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
