//! TermHarbor / 终端港 —— 跨平台 SSH 终端与文件管理客户端。
//!
//! 模块划分严格对齐需求文档 4.4：`commands/` 只做转发，业务在各领域模块。M0 只落地了
//! 骨架与基础设施（错误码、日志、加密、存储），`ssh` / `sftp` / `monitor` / `snippets`
//! 先占位，按开发计划的里程碑逐步填充。

pub mod commands;
pub mod crypto;
pub mod error;
pub mod events;
pub mod logging;
pub mod monitor;
pub mod redact;
pub mod sftp;
pub mod snippets;
pub mod ssh;
pub mod state;
pub mod storage;

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use tauri::Manager;

use crate::crypto::{default_provider, FieldCipher};
use crate::sftp::{SftpManager, Transfers};
use crate::ssh::session::SessionManager;
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
                Ok(key) => Some(Arc::new(FieldCipher::new(&key))),
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
                _logging: logging,
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
            commands::settings::settings_get,
            commands::settings::settings_update,
        ])
        .run(tauri::generate_context!())
        .expect("TermHarbor 启动失败");
}
