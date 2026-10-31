//! 连接配置导入导出的 command。
//!
//! 只做「读文件 / 写文件 / 汇报数字」，格式与校验都在 `config_io` 模块里（那边有完整测试）。

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

/// 从 JSON 导入连接。已存在的不覆盖，只新增。
#[tauri::command]
pub async fn config_import(state: State<'_, AppState>, path: String) -> AppResult<ImportSummary> {
    let target = readable_target(&path)?;

    let size = std::fs::metadata(&target)
        .map_err(|err| AppError::validation(format!("读不到文件: {err}")))?
        .len();
    if size > MAX_IMPORT_BYTES {
        return Err(AppError::validation("文件太大，不像是连接配置文件"));
    }

    let text = std::fs::read_to_string(&target)
        .map_err(|err| AppError::validation(format!("读不到文件: {err}")))?;
    let entries = config_io::parse(&text)?;

    let (imported, skipped) = state
        .db
        .call(move |conn| {
            let existing = config_io::existing_identities(conn)?;
            let plan = config_io::plan(entries, &existing);
            let imported = config_io::apply(conn, &plan)?;
            Ok((imported, plan.skipped))
        })
        .await?;

    tracing::info!(imported, skipped, "已导入连接配置");

    Ok(ImportSummary { imported, skipped })
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
