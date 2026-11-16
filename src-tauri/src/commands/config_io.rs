//! 连接配置导入导出的 command。
//!
//! 只做「读文件 / 写文件 / 汇报数字」，格式与校验都在 `config_io` 模块里（那边有完整测试）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::config_io;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::storage::connections;

/// 导入文件的大小上限，与 `config_io` 内部一致；这里是为了**读取前**就拒绝，
/// 不去把一个 500 MiB 的文件读进内存。
const MAX_IMPORT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSummary {
    pub connections: usize,
    pub path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub imported: usize,
    /// 因为库里已有同一台（主机+端口+用户名）而跳过的条数
    pub skipped: usize,
}

/// 导出全部连接到 JSON。**不含密码与主机指纹**（见 `config_io` 模块头）。
#[tauri::command]
pub async fn config_export(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> AppResult<ExportSummary> {
    let target = writable_target(&path)?;

    let records = state.db.call(|conn| connections::list(conn, None)).await?;
    if records.is_empty() {
        return Err(AppError::validation("还没有任何连接，没什么可导出的"));
    }

    let document = config_io::export_document(&records, &app.package_info().version.to_string());
    let json = config_io::to_json(&document)?;

    // 先写临时文件再改名：中途失败不会留下半截 JSON，
    // 也不会把用户原有的同名备份覆盖成半份。
    let temp = target.with_extension("json.tmp");
    std::fs::write(&temp, json).map_err(|err| {
        AppError::with_detail(
            crate::error::ErrCode::StorageError,
            format!("写文件失败: {err}"),
        )
    })?;
    std::fs::rename(&temp, &target).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        AppError::with_detail(
            crate::error::ErrCode::StorageError,
            format!("写文件失败: {err}"),
        )
    })?;

    tracing::info!(count = records.len(), "已导出连接配置");

    Ok(ExportSummary {
        connections: records.len(),
        path: target.display().to_string(),
    })
}

/// 导入预览：文件里每条连接 + 库里现有连接（给「经由」下拉用）。
///
/// 存在的意义只有一个：导出文件里记的是跳板的**名字**，不是 id，
/// 跨机器无法自动还原。这里把线索摊给用户，让他点一下配哪台 —— 而不是悄悄丢掉依赖。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreviewEntry {
    /// 与判重同一套口径的稳定 key，导入时按它回传配对结果
    pub key: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: String,
    /// 文件里记录的跳板名字；`None` = 这条原本就是直连
    pub jump_name: Option<String>,
    /// 库里已有同一台（主机+端口+用户名），导入时会被跳过
    pub duplicate: bool,
    /// 同名且**唯一**的现有连接 id，供前端预选；有歧义时给 `None`
    pub suggested: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCandidate {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub entries: Vec<ImportPreviewEntry>,
    pub connections: Vec<ImportCandidate>,
}

#[tauri::command]
pub async fn config_import_preview(
    state: State<'_, AppState>,
    path: String,
) -> AppResult<ImportPreview> {
    let target = readable_target(&path)?;
    let entries = read_entries(&target)?;

    let (preview, records) = state
        .db
        .call(move |conn| {
            let records = connections::list(conn, None)?;
            let existing = config_io::existing_identities(conn)?;
            let candidates: Vec<(String, String)> = records
                .iter()
                .map(|rec| (rec.id.clone(), rec.name.clone()))
                .collect();
            let preview = config_io::preview(&entries, &existing, &candidates);
            Ok((preview, records))
        })
        .await?;

    Ok(ImportPreview {
        entries: preview
            .into_iter()
            .map(|item| ImportPreviewEntry {
                key: item.key,
                name: item.name,
                host: item.host,
                port: item.port,
                username: item.username,
                auth_type: item.auth_type,
                jump_name: item.jump_name,
                duplicate: item.duplicate,
                suggested: item.suggested,
            })
            .collect(),
        connections: records
            .into_iter()
            .map(|rec| ImportCandidate {
                id: rec.id,
                name: rec.name,
                host: rec.host,
                port: rec.port,
                username: rec.username,
            })
            .collect(),
    })
}

/// 从 JSON 导入连接。已存在的不覆盖，只新增。
///
/// `jumps` 是用户在预览里确认过的跳板配对（`key → 现有连接 id`）：
/// 没给的 key 就按直连导入，不会自己去猜。
#[tauri::command]
pub async fn config_import(
    state: State<'_, AppState>,
    path: String,
    jumps: Option<HashMap<String, String>>,
) -> AppResult<ImportSummary> {
    let target = readable_target(&path)?;
    let entries = read_entries(&target)?;
    let jumps = jumps.unwrap_or_default();
    let jump_count = jumps.len();

    let (imported, skipped) = state
        .db
        .call(move |conn| {
            let existing = config_io::existing_identities(conn)?;

            // 指定的跳板必须是真存在的连接：前端可能拿的是过期列表
            if !jumps.is_empty() {
                let ids: Vec<String> = connections::list(conn, None)?
                    .into_iter()
                    .map(|rec| rec.id)
                    .collect();
                for (key, jump_id) in &jumps {
                    if !ids.iter().any(|id| id == jump_id) {
                        return Err(AppError::validation(format!(
                            "导入时指定的跳板不存在（{key} → {jump_id}），请重新选一次"
                        )));
                    }
                }
            }

            let plan = config_io::plan(entries, &existing, &jumps);
            let imported = config_io::apply(conn, &plan)?;
            Ok((imported, plan.skipped))
        })
        .await?;

    tracing::info!(imported, skipped, jumps = jump_count, "已导入连接配置");

    Ok(ImportSummary { imported, skipped })
}

/// 读取并解析导入文件（大小闸 + 解析校验都在这里，两个入口共用）。
fn read_entries(target: &Path) -> AppResult<Vec<config_io::ImportEntry>> {
    let size = std::fs::metadata(target)
        .map_err(|err| AppError::validation(format!("读不到文件: {err}")))?
        .len();
    if size > MAX_IMPORT_BYTES {
        return Err(AppError::validation("文件太大，不像是连接配置文件"));
    }

    let text = std::fs::read_to_string(target)
        .map_err(|err| AppError::validation(format!("读不到文件: {err}")))?;
    config_io::parse(&text)
}

/// 导出目标：绝对路径 + 父目录存在。
///
/// 前端路径来自系统保存对话框，这里是第二道闸 —— 和下载目标同一套理由。
fn writable_target(raw: &str) -> AppResult<PathBuf> {
    let path = absolute(raw, "导出路径")?;

    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| AppError::validation("导出路径缺少目录"))?;

    if !parent.is_dir() {
        return Err(AppError::validation("导出目录不存在"));
    }

    Ok(path)
}

/// 导入源：绝对路径 + 必须是已存在的普通文件。
fn readable_target(raw: &str) -> AppResult<PathBuf> {
    let path = absolute(raw, "导入路径")?;

    let meta = std::fs::metadata(&path)
        .map_err(|err| AppError::validation(format!("读不到文件: {err}")))?;
    if !meta.is_file() {
        return Err(AppError::validation("请选择一个文件，而不是目录"));
    }

    Ok(path)
}

fn absolute(raw: &str, label: &str) -> AppResult<PathBuf> {
    let path = Path::new(raw.trim());
    if raw.trim().is_empty() {
        return Err(AppError::validation(format!("{label}不能为空")));
    }
    if !path.is_absolute() {
        return Err(AppError::validation(format!("{label}必须是绝对路径")));
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_rejected() {
        assert!(absolute("config.json", "导出路径").is_err());
        assert!(absolute("", "导出路径").is_err());
    }

    #[test]
    fn import_requires_an_existing_file() {
        let missing = std::env::temp_dir().join("termharbor-missing-import.json");
        let _ = std::fs::remove_file(&missing);
        assert!(readable_target(&missing.display().to_string()).is_err());

        // 目录也要拒绝（用户可能手输了一个目录）
        let dir = std::env::temp_dir();
        assert!(readable_target(&dir.display().to_string()).is_err());
    }

    #[test]
    fn export_requires_an_existing_parent_directory() {
        let missing_dir = std::env::temp_dir().join("termharbor-no-such-dir-xyz");
        let target = missing_dir.join("out.json");
        assert!(writable_target(&target.display().to_string()).is_err());
    }
}
