//! 命令片段 command。
//!
//! 只做转发：风险判定、过滤、渲染都在 `snippets` 模块里，
//! 那边有完整的单元测试，这里不重复逻辑。

use crate::error::AppResult;
use crate::snippets::{self, SnippetDto};

/// 列出内置片段。`query` 为空时返回全部。
#[tauri::command]
pub async fn snippet_list(query: Option<String>) -> AppResult<Vec<SnippetDto>> {
    Ok(snippets::list(query.as_deref()))
}

/// 取片段最终要写进终端的文本。
///
/// 前端拿到后只写入终端、**不自动回车**（A-12）。
/// 走这一层而不是直接用 list 里的 command，是为了让「渲染 + 校验」留在 Rust 侧（A-11），
/// M7 加变量替换时前端不用改。
#[tauri::command]
pub async fn snippet_render(id: String) -> AppResult<String> {
    snippets::render(&id)
}
