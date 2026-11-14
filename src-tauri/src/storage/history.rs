//! 命令历史 / 路径历史的读写。
//!
//! 对标 FinalShell 高级版「解除终端命令历史和路径历史的数量限制」：
//! 本模块**不设任何条数上限**，`list` 一次返回该 kind 的全部记录，`push` 不做
//! 任何清理或淘汰。只搬数据、不校验参数 —— kind 白名单、value 去空白/截断、
//! limit 的默认值与范围都在 `commands/history.rs` 里做，和 snippets/settings
//! 的「存储层只管 SQL，校验留到 command 层」分层保持一致。
//!
//! 表结构与索引见 `migrations/003_history.sql`：`UNIQUE(kind, connection_id, value)`
//! 是去重的依据，`INDEX(kind, ts_ms DESC)` 正好服务 `list` 的时间倒序。

use rusqlite::{params, Connection};

/// 一条历史记录。序列化成 camelCase 给前端（`connectionId` / `tsMs` / `useCount`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    /// `"command"` 或 `"path"`。
    pub kind: String,
    pub value: String,
    /// 没关联连接时为空串 `""`。
    pub connection_id: String,
    pub ts_ms: u64,
    pub use_count: u32,
}

/// 列出某个 kind 的**全部**记录，按最近使用时间倒序。
///
/// 为什么不加上限：这个功能的卖点就是「无限制历史记录」，FinalShell 免费版
/// 会把命令/路径历史截断到固定条数。这里刻意不写 `LIMIT`，也不在别处淘汰老数据，
/// 用户敲过的每一条都能翻回来。去重（见 `push`）已经保证了「反复执行同一条命令」
/// 不会让表膨胀成无数重复行，所以长期积累的是「不同的命令」，量级可控。
pub fn list(conn: &Connection, kind: &str) -> rusqlite::Result<Vec<HistoryEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, value, connection_id, ts_ms, use_count \
         FROM history WHERE kind = ?1 ORDER BY ts_ms DESC",
    )?;

    // ts_ms / use_count 在 SQLite 里是 INTEGER(i64)，结构体用 u64 / u32，
    // 读成 i64 再 cast；当前毫秒时间戳远小于 i64::MAX，不会溢出。
    let rows = stmt.query_map(params![kind], |row| {
        Ok(HistoryEntry {
            id: row.get(0)?,
            kind: row.get(1)?,
            value: row.get(2)?,
            connection_id: row.get(3)?,
            ts_ms: row.get::<_, i64>(4)? as u64,
            use_count: row.get::<_, i64>(5)? as u32,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 写入一条历史；相同 `(kind, connection_id, value)` 只保留一行。
///
/// 为什么去重：终端里 `git pull`、`cd /opt/web` 这类命令会被反复执行成百上千次，
/// 每行都存一份既撑大库、又让补全列表被同一条刷屏。这里用
/// `ON CONFLICT ... DO UPDATE` 把它折叠成一行：更新为最新时间、`use_count` 累加，
/// 于是既能「翻到全部不同的历史」，又能按「用得最多 / 最近用过」排序补全。
/// 冲突时不覆盖 `id`，保留首次写入的那个，前端据 id 删除才稳定。
///
/// 时间戳与 id 都在这一层生成（任务约定），command 层不必传。
pub fn push(
    conn: &Connection,
    kind: &str,
    value: &str,
    connection_id: &str,
) -> rusqlite::Result<()> {
    let id = uuid::Uuid::now_v7().to_string();
    let ts_ms = crate::state::now_millis() as i64;

    conn.execute(
        "INSERT INTO history (id, kind, value, connection_id, ts_ms, use_count) \
         VALUES (?1, ?2, ?3, ?4, ?5, 1) \
         ON CONFLICT(kind, connection_id, value) DO UPDATE SET \
           ts_ms = excluded.ts_ms, \
           use_count = use_count + 1",
        params![id, kind, value, connection_id, ts_ms],
    )?;

    Ok(())
}

/// 清空某个 kind 的全部记录。
///
/// `WHERE kind = ?1` 不是装饰：命令历史和路径历史共用一张表，
/// 少了它点「清空命令历史」会连路径历史一起删掉。
pub fn clear(conn: &Connection, kind: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM history WHERE kind = ?1", params![kind])?;
    Ok(())
}

/// 按 id 删除一条记录，且必须落在指定 kind 内。
///
/// 同样带上 `kind` 条件：不让前端拿命令历史的 id 误删路径历史那一行。
pub fn remove(conn: &Connection, kind: &str, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM history WHERE kind = ?1 AND id = ?2",
        params![kind, id],
    )?;
    Ok(())
}

/// 把 LIKE 的通配符按字面转义，配合 `ESCAPE '\'` 使用。
///
/// `%`、`_` 以及转义符本身 `\` 都要在前面加一个 `\`；顺序上先给 `\` 自身加前缀
/// 很关键，否则后面补的那些 `\` 会被二次转义。
fn escape_like(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for ch in query.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 取补全候选：按 value 模糊匹配，返回去重后的 value 列表。
///
/// 为什么 LIKE 要转义：`query` 是用户在输入框里敲的原文，`%` 和 `_` 在 SQL 的
/// LIKE 里是通配符。不转义的话，用户想搜字面量 `100%` 会被当成「匹配任意文本」，
/// 补全列表失真，等于把整张历史表都翻出来（也算一种信息外泄）。把这三个字符
/// 转义并用 `ESCAPE '\'` 声明转义符后，用户输入就只按字面匹配。整个 pattern
/// 通过参数绑定传入，绝不拼进 SQL。
///
/// 按 `(kind, connection_id, value)` 去重后，同一个 value 可能对应多个 connection，
/// 所以这里用 `GROUP BY value` 折叠，排序取该 value 所有行里最大的 `use_count`
/// 与最新的 `ts_ms` —— 即「用得最多、最近用过」的排在前面。query 为空时
/// `%%` 匹配全部，正好返回最常用 / 最近的若干个。
pub fn complete(
    conn: &Connection,
    kind: &str,
    query: &str,
    limit: u32,
) -> rusqlite::Result<Vec<String>> {
    let pattern = format!("%{}%", escape_like(query));
    // 兜底夹一次范围：即便调用方传了 0 或超大值，SQL 也不会变成 LIMIT 0（空结果）
    // 或一次捞太多。默认值（None -> 50）在 command 层处理。
    let limit = limit.clamp(1, 500) as i64;

    // 原始字符串里 `ESCAPE '\'` 就是一个反斜杠字符，正是 SQLite 要的转义符字面量；
    // 用原始字符串避免和 Rust 的 `\\` 转义混在一起看错。
    let mut stmt = conn.prepare(
        r#"SELECT value FROM history WHERE kind = ?1 AND value LIKE ?2 ESCAPE '\' GROUP BY value ORDER BY MAX(use_count) DESC, MAX(ts_ms) DESC LIMIT ?3"#,
    )?;

    let rows = stmt.query_map(params![kind, pattern, limit], |row| row.get(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::migrations;
    use rusqlite::Connection;

    fn memory_db() -> Connection {
        let mut conn = Connection::open_in_memory().expect("内存库应当能打开");
        // version 3 的 003_history.sql 会随 apply 一起建表
        migrations::apply(&mut conn).expect("迁移应当成功");
        conn
    }

    /// 直接改 ts_ms，让「按时间倒序」这类测试不依赖真实时钟的毫秒精度。
    fn set_ts(conn: &Connection, value: &str, ts_ms: i64) {
        conn.execute(
            "UPDATE history SET ts_ms = ?1 WHERE value = ?2",
            rusqlite::params![ts_ms, value],
        )
        .unwrap();
    }

    #[test]
    fn empty_table_reads_as_empty_list() {
        let conn = memory_db();
        assert!(
            list(&conn, "command").unwrap().is_empty(),
            "空库命令历史为空"
        );
        assert!(list(&conn, "path").unwrap().is_empty(), "空库路径历史为空");
        assert!(complete(&conn, "command", "", 50).unwrap().is_empty());
    }

    #[test]
    fn push_then_list_round_trips() {
        let conn = memory_db();
        push(&conn, "command", "ls -la", "conn-1").unwrap();

        let all = list(&conn, "command").unwrap();
        assert_eq!(all.len(), 1);
        let e = &all[0];
        assert_eq!(e.kind, "command");
        assert_eq!(e.value, "ls -la");
        assert_eq!(e.connection_id, "conn-1");
        assert_eq!(e.use_count, 1);
        assert!(!e.id.is_empty(), "id 应已生成");
        assert!(e.ts_ms > 0, "应写入毫秒时间戳");
    }

    #[test]
    fn repeated_push_deduplicates_and_increments_use_count() {
        let conn = memory_db();
        for _ in 0..3 {
            push(&conn, "command", "git pull", "").unwrap();
        }

        let all = list(&conn, "command").unwrap();
        assert_eq!(all.len(), 1, "同一条命令只占一行，不因反复执行而膨胀");
        assert_eq!(all[0].use_count, 3, "每执行一次 use_count 加一");
    }

    #[test]
    fn different_connection_id_keeps_separate_rows_and_complete_dedupes_value() {
        let conn = memory_db();
        // 唯一键含 connection_id：同一个命令在不同连接下各占一行
        push(&conn, "command", "ls", "conn-A").unwrap();
        push(&conn, "command", "ls", "conn-B").unwrap();

        let all = list(&conn, "command").unwrap();
        assert_eq!(all.len(), 2, "connection_id 不同不该被去重合并");
        assert!(all.iter().all(|e| e.value == "ls"));

        // complete 面向「补全文本」，按 value 去重后只该回一个 ls
        let hits = complete(&conn, "command", "ls", 50).unwrap();
        assert_eq!(hits, vec!["ls".to_string()], "补全结果按 value 去重");
    }

    #[test]
    fn command_and_path_are_isolated() {
        let conn = memory_db();
        push(&conn, "command", "ls", "").unwrap();
        push(&conn, "path", "/home", "").unwrap();
        // 同名 value 跨 kind 也互不冲突（唯一键含 kind）
        push(&conn, "command", "/home", "").unwrap();

        let cmds = list(&conn, "command").unwrap();
        let paths = list(&conn, "path").unwrap();
        assert_eq!(cmds.len(), 2, "command 类只放命令");
        assert!(cmds.iter().all(|e| e.kind == "command"));
        assert_eq!(paths.len(), 1, "path 类只放路径");
        assert_eq!(paths[0].value, "/home");
    }

    #[test]
    fn clear_only_deletes_given_kind() {
        let conn = memory_db();
        push(&conn, "command", "c1", "").unwrap();
        push(&conn, "path", "p1", "").unwrap();

        clear(&conn, "command").unwrap();

        assert!(list(&conn, "command").unwrap().is_empty(), "命令历史已清空");
        assert_eq!(list(&conn, "path").unwrap().len(), 1, "路径历史不受影响");
    }

    #[test]
    fn remove_targets_id_within_kind() {
        let conn = memory_db();
        push(&conn, "command", "one", "").unwrap();
        push(&conn, "command", "two", "").unwrap();
        let id = list(&conn, "command").unwrap()[0].id.clone();

        // 用错误的 kind 删不掉：kind 条件兜着
        remove(&conn, "path", &id).unwrap();
        assert_eq!(list(&conn, "command").unwrap().len(), 2, "不能跨 kind 误删");

        // 用正确的 kind 才删掉指定那一条
        remove(&conn, "command", &id).unwrap();
        let left = list(&conn, "command").unwrap();
        assert_eq!(left.len(), 1);
        assert_ne!(left[0].id, id, "删掉的应是指定 id 那行");
    }

    #[test]
    fn complete_fuzzy_match_and_escapes_percent() {
        let conn = memory_db();
        push(&conn, "command", "100% done", "").unwrap();
        push(&conn, "command", "git status", "").unwrap();
        push(&conn, "command", "percentless", "").unwrap();

        // 普通子串模糊匹配
        assert_eq!(
            complete(&conn, "command", "git", 50).unwrap(),
            vec!["git status".to_string()]
        );

        // query 里的 % 必须按字面匹配：不转义的话 '%%%' 会命中全部三条
        assert_eq!(
            complete(&conn, "command", "%", 50).unwrap(),
            vec!["100% done".to_string()],
            "% 不该被当成通配符，否则整张表都被翻出来"
        );
    }

    #[test]
    fn complete_escapes_underscore() {
        let conn = memory_db();
        push(&conn, "command", "a_b", "").unwrap();
        push(&conn, "command", "axb", "").unwrap();

        // _ 是「任意单字符」通配符，不转义的话 '%_%' 会把 axb 也命中
        assert_eq!(
            complete(&conn, "command", "_", 50).unwrap(),
            vec!["a_b".to_string()],
            "_ 必须按字面匹配"
        );
    }

    #[test]
    fn list_is_ordered_by_ts_desc() {
        let conn = memory_db();
        push(&conn, "command", "old", "").unwrap();
        push(&conn, "command", "new", "").unwrap();
        push(&conn, "command", "mid", "").unwrap();
        set_ts(&conn, "old", 100);
        set_ts(&conn, "new", 300);
        set_ts(&conn, "mid", 200);

        // 先绑定，避免借用 list 返回的临时 Vec 导致 E0716
        let all = list(&conn, "command").unwrap();
        let values: Vec<&str> = all.iter().map(|e| e.value.as_str()).collect();
        assert_eq!(values, vec!["new", "mid", "old"], "ts_ms 大的排前面");
    }

    #[test]
    fn complete_orders_by_use_count_and_clamps_limit() {
        let conn = memory_db();
        push(&conn, "command", "alpha", "").unwrap();
        push(&conn, "command", "alpha", "").unwrap();
        push(&conn, "command", "alpha", "").unwrap(); // use_count = 3
        push(&conn, "command", "beta", "").unwrap(); // use_count = 1

        // query 为空返回最常用 / 最近的若干个，用得多的排前面
        assert_eq!(
            complete(&conn, "command", "", 50).unwrap(),
            vec!["alpha".to_string(), "beta".to_string()]
        );

        // limit 0 被 clamp 到 1，超大值也不会越过 500
        assert_eq!(complete(&conn, "command", "", 0).unwrap().len(), 1);
        assert_eq!(complete(&conn, "command", "", 999).unwrap().len(), 2);
    }

    #[test]
    fn storage_does_not_truncate_long_value() {
        let conn = memory_db();
        let long = "x".repeat(5000);
        push(&conn, "command", &long, "").unwrap();

        let all = list(&conn, "command").unwrap();
        assert_eq!(
            all[0].value.chars().count(),
            5000,
            "存储层原样落库，4096 字符的截断是 command 层的职责"
        );
    }
}
