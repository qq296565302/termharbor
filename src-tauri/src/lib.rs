//! TermHarbor / 终端港 —— 跨平台 SSH 终端与文件管理客户端。
//!
//! 模块划分严格对齐需求文档 4.4：`commands/` 只做转发，业务在各领域模块。M0 只落地了
//! 骨架与基础设施（错误码、日志、加密、存储），`ssh` / `sftp` / `monitor` / `snippets`
//! 先占位，按开发计划的里程碑逐步填充。

pub mod commands;
pub mod config_io;
pub mod crypto;
pub mod dbcli;
pub mod error;
pub mod events;
pub mod lease;
pub mod logging;
pub mod mcp;
pub mod monitor;
pub mod redact;
pub mod sftp;
pub mod snippets;
pub mod ssh;
pub mod state;
pub mod storage;
pub mod update;

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};

use crate::crypto::{default_provider, FieldCipher};
use crate::dbcli::DbLinks;
use crate::sftp::{SftpManager, Transfers};
use crate::ssh::session::SessionManager;
use crate::ssh::TunnelManager;
use crate::state::{AppPaths, AppState};
use crate::storage::Db;

/// 库空但快照有数据时打一条 warn。
fn warn_if_db_emptied(db: &Db, backup_dir: &std::path::Path) {
    let current = db
        .inner()
        .lock()
        .ok()
        .and_then(|conn| {
            conn.query_row("SELECT COUNT(*) FROM connections", [], |row| {
                row.get::<_, i64>(0)
            })
            .ok()
        })
        .unwrap_or(-1);

    if current != 0 {
        return;
    }

    let Ok(items) = crate::storage::backup::list(backup_dir) else {
        return;
    };

    if let Some(found) = items.iter().find(|item| item.connections.unwrap_or(0) > 0) {
        tracing::warn!(
            snapshot = %found.name,
            servers = ?found.connections,
            "当前没有任何连接，但历史快照里有数据 —— 可在设置页「数据备份」中恢复"
        );
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| -> Result<(), Box<dyn std::error::Error>> {
            let paths = AppPaths::resolve(app.handle())?;
            let logging = logging::init(&paths.log_dir, "info")?;

            tracing::info!(
                version = app.package_info().version.to_string(),
                data_dir = %paths.data_dir.display(),
                "TermHarbor 启动"
            );

            // 0.1.16 及更早把快照目录算错了一层，落在了 app_data_dir 里面 ——
            // 那正好是卸载器 `RmDir /r` 会删掉的目录，安全网形同虚设。
            // 这里做一次性搬迁，必须在快照与恢复之前。
            match crate::storage::backup::migrate_legacy(
                &crate::storage::backup::legacy_dir(&paths.data_dir),
                &paths.backup_dir,
            ) {
                Ok(0) => {}
                Ok(count) => tracing::info!(count, "已把老位置的快照搬到新目录"),
                Err(err) => tracing::warn!(error = %err, "搬迁老快照失败，先继续启动"),
            }

            // 有待恢复的快照就先换上去（用户在上次会话里点了「恢复」）。
            // 必须在 Db::open 之前：换文件时不能有连接开着。
            match crate::storage::backup::apply_pending_restore(&paths.db_file, &paths.backup_dir) {
                Ok(Some(file)) => tracing::warn!(file = %file.display(), "已恢复数据快照"),
                Ok(None) => {}
                Err(err) => tracing::error!(error = %err, "应用待恢复快照失败"),
            }

            let db = Arc::new(Db::open_with_snapshot(
                &paths.db_file,
                Some(paths.backup_dir.as_path()),
            )?);

            // 自检：当前库是空的、但历史快照里有数据 —— 几乎可以断定数据目录被清过
            // （装新版本时勾了「删除应用数据」就是这个症状）。留一条 warn，
            // 用户来问「数据没了」时日志里直接有答案，不用再翻文件时间戳。
            warn_if_db_emptied(&db, &paths.backup_dir);

            let provider = default_provider(&paths.data_dir);
            let key_source = provider.source();
            let cipher = match provider.load_or_create() {
                Ok(key) => {
                    // 顺手写一份后台可读的密钥镜像。MCP 宿主常把子进程起在非交互
                    // 会话里，那种会话打不开凭据管理器；镜像用 DPAPI 绑到用户主密钥，
                    // 后台也能解。写失败只 warn —— 镜像是加速路径，主流程不受影响。
                    let mirror_path = paths.data_dir.join(crypto::mirror::FILE_NAME);
                    match crypto::mirror::store(&key, &mirror_path) {
                        Ok(()) => tracing::info!(
                            path = %mirror_path.display(),
                            "已写好后台可读的密钥镜像"
                        ),
                        Err(err) => tracing::warn!(
                            error = %err,
                            "密钥镜像写入失败，后台会话可能读不到密钥"
                        ),
                    }
                    Some(Arc::new(FieldCipher::new(&key)))
                }
                Err(err) => {
                    // 拿不到密钥不该直接崩：应用仍可启动，只是处于未解锁状态，
                    // 需要加密的 command 会返回 CRYPTO_KEY_UNAVAILABLE。
                    let app_err = error::AppError::from(err);
                    tracing::error!(
                        code = app_err.code.as_str(),
                        detail = app_err.detail.as_deref().unwrap_or("-"),
                        "数据密钥不可用，进入未解锁状态"
                    );
                    None
                }
            };

            let tunnels = Arc::new(TunnelManager::new());
            {
                // 重连发生是在后台任务里、不经过任何 command，只能靠这条回调
                // 把状态送到界面。没有它，界面会一直停在最后一次点按钮的结果上。
                let handle = app.handle().clone();
                tunnels.set_notice(Arc::new(move |tunnel_id, state, message| {
                    let _ = handle.emit(
                        crate::events::EVT_TUNNEL_STATE,
                        serde_json::json!({
                            "tunnelId": tunnel_id,
                            "state": state,
                            "message": message,
                        }),
                    );
                }));
            }

            app.manage(AppState {
                db,
                cipher,
                paths,
                key_source,
                heartbeat: Mutex::new(None),
                next_seq: AtomicU64::new(0),
                sessions: Arc::new(SessionManager::new()),
                sftp: Arc::new(SftpManager::new()),
                transfers: Arc::new(Transfers::new()),
                tunnels: Arc::clone(&tunnels),
                dblinks: Arc::new(DbLinks::new()),
                started_at_ms: crate::state::now_millis(),
                _logging: logging,
            });

            // 会话租约心跳：把「此刻打开着的连接」写进产品目录，MCP 侧据此判断
            // 某台机器能不能用（见 `lease` 模块）。先立刻写一次，把上次崩溃
            // 留下的旧文件即时覆盖掉 —— 否则最长要等一个 TTL 才失效。
            lease::publish(&app.state::<AppState>());
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let interval = std::time::Duration::from_millis(lease::BEAT_MS);
                loop {
                    tokio::time::sleep(interval).await;
                    if let Some(state) = handle.try_state::<AppState>() {
                        lease::publish(&state);
                    }
                }
            });

            // 自动启动的转发规则：等窗口和网络栈就绪再拉。开机自启时网络常常还没起来，
            // 立刻拉会整批失败；失败只记在规则状态里，不影响终端和别的规则。
            let auto_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if let Some(state) = auto_handle.try_state::<AppState>() {
                    let started = crate::commands::tunnel::apply_auto_start(state.inner()).await;
                    if started > 0 {
                        tracing::info!(count = started, "自动启动的转发已拉起");
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::system::ping,
            commands::system::start_heartbeat,
            commands::system::stop_heartbeat,
            commands::dev::ime_echo,
            commands::connections::connection_list,
            commands::connections::connection_get,
            commands::connections::connection_create,
            commands::connections::connection_update,
            commands::connections::connection_delete,
            commands::connections::connection_test,
            commands::connections::known_host_forget,
            commands::terminal::session_open,
            commands::terminal::session_write,
            commands::terminal::session_resize,
            commands::terminal::session_close,
            commands::terminal::session_ack,
            commands::terminal::session_list,
            commands::terminal::session_close_all,
            commands::sftp::sftp_open,
            commands::sftp::sftp_close,
            commands::sftp::sftp_close_all,
            commands::sftp::sftp_list,
            commands::sftp::sftp_parent_of,
            commands::sftp::sftp_stat,
            commands::sftp::sftp_mkdir,
            commands::sftp::sftp_rename,
            commands::sftp::sftp_remove,
            commands::sftp::sftp_upload,
            commands::sftp::sftp_download,
            commands::sftp::sftp_cancel,
            commands::snippets::snippet_list,
            commands::snippets::snippet_render,
            commands::snippets::snippet_save,
            commands::snippets::snippet_delete,
            commands::snippets::snippet_classify,
            commands::backup::backup_list,
            commands::backup::backup_create,
            commands::backup::backup_restore,
            commands::backup::backup_dir,
            commands::snippets::env_probe,
            commands::diag::diag_run,
            commands::diag::load_snapshot,
            // 进阶功能（对标 FinalShell 高级版 / 专业版）
            commands::advanced::advanced_snapshot,
            commands::advanced::advanced_kill,
            commands::advanced::path_complete,
            commands::history::history_list,
            commands::history::history_push,
            commands::history::history_clear,
            commands::history::history_remove,
            commands::history::history_complete,
            commands::pack::pack_upload,
            commands::pack::pack_download,
            commands::config_io::config_export,
            commands::config_io::config_import,
            commands::config_io::config_import_preview,
            commands::tunnel::tunnel_list,
            commands::tunnel::tunnel_create,
            commands::tunnel::tunnel_delete,
            commands::tunnel::tunnel_start,
            commands::tunnel::tunnel_stop,
            commands::db::db_account_get,
            commands::db::db_account_save,
            commands::db::db_account_clear,
            commands::db::db_query,
            commands::db::db_databases,
            commands::update::update_check,
            commands::update::update_open_page,
            commands::settings::settings_get,
            commands::settings::settings_update,
            commands::settings::read_background_image,
            commands::mcp::mcp_config_get,
            commands::mcp::mcp_config_set,
            commands::mcp::mcp_audit_recent,
            commands::mcp::mcp_allow_command,
            commands::mcp::mcp_live_connections,
            commands::mcp::mcp_approval_grant,
            commands::mcp::mcp_approval_revoke,
            commands::mcp::mcp_token_regenerate,
        ])
        .build(tauri::generate_context!())
        .expect("TermHarbor 启动失败")
        .run(|handle, event| {
            // 正常退出时删掉租约文件：MCP 立刻失去「客户端在线」这个前提，
            // 不必等 15 秒 TTL 过期。被强杀的情况仍然靠 TTL 兜底。
            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(state) = handle.try_state::<AppState>() {
                    crate::lease::clear(&state.paths.product_dir);
                    // 转发是先收摊再走：否则端口会挂到进程真正退出为止
                    state.tunnels.stop_all();
                    // 「本次客户端会话」的放行到此为止。固定时长的放行保留到自然过期；
                    // 强杀来不及走这里，还有配置里的 8 小时兜底。
                    let store = crate::mcp::config::ConfigStore::new(&crate::mcp::data_dir(
                        &state.paths.product_dir,
                    ));
                    let mut cfg = store.load();
                    cfg.clear_session_approvals();
                    if let Err(err) = store.save(&cfg) {
                        tracing::warn!(error = %err, "退出时清理 MCP 放行记录失败");
                    }
                }
            }
        });
}
