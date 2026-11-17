//! 本地端口转发（`ssh -L`）的规则读写。
//!
//! 约定与 `storage::connections` 一致：这一层只管存取，不做网络动作，也不认识凭据。
//! 真正开监听、建隧道的是 `ssh::tunnel`。

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::{AppError, AppResult, ErrCode};
use crate::storage::migrations::now_millis;

#[derive(Debug, Clone)]
pub struct TunnelRecord {
    pub id: String,
    pub connection_id: String,
    /// 本机监听端口（地址固定 127.0.0.1）
    pub local_port: u16,
    /// 目标地址由**跳板/远端**解析 —— 这正是它能把内网服务搬过来的原因
    pub target_host: String,
    pub target_port: u16,
    /// 应用启动时是否自动拉起这条规则
    pub auto_start: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl TunnelRecord {
    pub fn new(
        connection_id: impl Into<String>,
        local_port: u16,
        target_host: impl Into<String>,
        target_port: u16,
    ) -> Self {
        let now = now_millis();
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            connection_id: connection_id.into(),
            local_port,
            target_host: target_host.into(),
            target_port,
            // 默认不自动拉起：临时调试用的规则不该每次开机都被建起来
            auto_start: false,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_auto_start(mut self, auto_start: bool) -> Self {
        self.auto_start = auto_start;
        self
    }
}

const COLUMNS: &str =
    "id, connection_id, local_port, target_host, target_port, auto_start, created_at, updated_at";

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<TunnelRecord> {
    Ok(TunnelRecord {
        id: row.get(0)?,
        connection_id: row.get(1)?,
        local_port: row.get::<_, i64>(2)? as u16,
        target_host: row.get(3)?,
        target_port: row.get::<_, i64>(4)? as u16,
        auto_start: row.get::<_, i64>(5)? != 0,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

/// 列出某条连接的规则；`None` 表示全部（终端页/自检可能用到）。
pub fn list(conn: &Connection, connection_id: Option<&str>) -> AppResult<Vec<TunnelRecord>> {
    let mut out = Vec::new();
    match connection_id {
        Some(id) => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM tunnels WHERE connection_id = ?1 ORDER BY local_port ASC"
            ))?;
            let rows = stmt.query_map([id], row_to_record)?;
            for row in rows {
                out.push(row?);
            }
        }
        None => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM tunnels ORDER BY connection_id ASC, local_port ASC"
            ))?;
            let rows = stmt.query_map([], row_to_record)?;
            for row in rows {
                out.push(row?);
            }
        }
    }
    Ok(out)
}

pub fn get(conn: &Connection, id: &str) -> AppResult<TunnelRecord> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM tunnels WHERE id = ?1"),
        [id],
        row_to_record,
    )
    .optional()?
    .ok_or_else(|| AppError::with_detail(ErrCode::StorageError, format!("转发规则不存在: id={id}")))
}

pub fn insert(conn: &Connection, rec: &TunnelRecord) -> AppResult<()> {
    conn.execute(
        "INSERT INTO tunnels
         (id, connection_id, local_port, target_host, target_port, auto_start, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            rec.id,
            rec.connection_id,
            rec.local_port as i64,
            rec.target_host,
            rec.target_port as i64,
            if rec.auto_start { 1 } else { 0 },
            rec.created_at,
            rec.updated_at,
        ],
    )
    .map_err(|err| {
        // UNIQUE(connection_id, local_port) 撞了：说清是哪个端口，而不是抛 SQL 原文
        if err.to_string().contains("UNIQUE") {
            AppError::validation(format!(
                "这条连接上已经有指向本机 {} 端口的规则了",
                rec.local_port
            ))
        } else {
            AppError::with_detail(ErrCode::StorageError, err.to_string())
        }
    })?;
    Ok(())
}

/// 改某条规则的自动启动开关。
pub fn set_auto_start(conn: &Connection, id: &str, auto_start: bool) -> AppResult<()> {
    let affected = conn.execute(
        "UPDATE tunnels SET auto_start = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, if auto_start { 1 } else { 0 }, now_millis()],
    )?;
    if affected == 0 {
        return Err(AppError::with_detail(
            ErrCode::StorageError,
            format!("转发规则不存在: id={id}"),
        ));
    }
    Ok(())
}

/// 应用启动时该自动拉起的规则。
pub fn list_auto_start(conn: &Connection) -> AppResult<Vec<TunnelRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM tunnels WHERE auto_start = 1 ORDER BY connection_id ASC, local_port ASC"
    ))?;
    let rows = stmt.query_map([], row_to_record)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn delete(conn: &Connection, id: &str) -> AppResult<()> {
    let affected = conn.execute("DELETE FROM tunnels WHERE id = ?1", [id])?;
    if affected == 0 {
        return Err(AppError::with_detail(
            ErrCode::StorageError,
            format!("转发规则不存在: id={id}"),
        ));
    }
    Ok(())
}

/// 删连接时连带清掉它的转发规则（返回被删的 id，调用方好把运行中的隧道停掉）。
pub fn delete_for_connection(conn: &Connection, connection_id: &str) -> AppResult<Vec<String>> {
    let ids: Vec<String> = list(conn, Some(connection_id))?
        .into_iter()
        .map(|rec| rec.id)
        .collect();
    conn.execute(
        "DELETE FROM tunnels WHERE connection_id = ?1",
        [connection_id],
    )?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Db;

    fn sample(
        connection_id: &str,
        local_port: u16,
        target: &str,
        target_port: u16,
    ) -> TunnelRecord {
        TunnelRecord::new(connection_id, local_port, target, target_port)
    }

    #[tokio::test]
    async fn insert_then_list_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let rec = sample("c1", 13306, "10.0.3.21", 3306);

        db.call({
            let rec = rec.clone();
            move |conn| insert(conn, &rec)
        })
        .await
        .unwrap();

        let rows = db.call(|conn| list(conn, Some("c1"))).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].local_port, 13306);
        assert_eq!(rows[0].target_host, "10.0.3.21");
        assert_eq!(rows[0].target_port, 3306);

        // 按连接过滤要生效，别把别人的规则也吐出来
        let other = db.call(|conn| list(conn, Some("c2"))).await.unwrap();
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn same_connection_cannot_reuse_a_local_port() {
        let db = Db::open_in_memory().unwrap();
        let first = sample("c1", 13306, "10.0.3.21", 3306);
        let second = sample("c1", 13306, "10.0.3.22", 5432);

        let err = db
            .call({
                let first = first.clone();
                let second = second.clone();
                move |conn| {
                    insert(conn, &first)?;
                    insert(conn, &second)
                }
            })
            .await
            .unwrap_err();

        assert_eq!(err.code, ErrCode::ValidationError);
        assert!(
            err.detail.unwrap_or_default().contains("13306"),
            "文案要点名是哪个端口"
        );
    }

    #[tokio::test]
    async fn different_connections_may_share_a_local_port_on_paper() {
        // 存的时候不拦（不同机器各配 13306 是合理的），真撞了由运行时 bind 报
        let db = Db::open_in_memory().unwrap();
        let a = sample("c1", 13306, "10.0.3.21", 3306);
        let b = sample("c2", 13306, "10.0.3.31", 3306);

        db.call({
            let (a, b) = (a.clone(), b.clone());
            move |conn| {
                insert(conn, &a)?;
                insert(conn, &b)
            }
        })
        .await
        .unwrap();

        assert_eq!(db.call(|conn| list(conn, None)).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn delete_for_connection_returns_removed_ids() {
        let db = Db::open_in_memory().unwrap();
        let a = sample("c1", 13306, "10.0.3.21", 3306);
        let b = sample("c1", 15432, "10.0.3.21", 5432);
        let keep = sample("c2", 16000, "10.0.3.31", 6379);

        db.call({
            let (a, b, keep) = (a.clone(), b.clone(), keep.clone());
            move |conn| {
                insert(conn, &a)?;
                insert(conn, &b)?;
                insert(conn, &keep)
            }
        })
        .await
        .unwrap();

        let removed = db
            .call(|conn| delete_for_connection(conn, "c1"))
            .await
            .unwrap();
        assert_eq!(removed.len(), 2);
        assert_eq!(db.call(|conn| list(conn, None)).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn missing_rule_reports_storage_error() {
        let db = Db::open_in_memory().unwrap();
        let err = db.call(|conn| get(conn, "nope")).await.unwrap_err();
        assert_eq!(err.code, ErrCode::StorageError);
    }

    /// 默认不自动拉起：临时调试用的规则不该每次开机都被建起来。
    /// 打开开关后要能读回来，且能改回去。
    #[tokio::test]
    async fn auto_start_defaults_off_and_can_be_toggled() {
        let db = Db::open_in_memory().unwrap();
        let rec = sample("c1", 13306, "10.0.3.21", 3306);

        db.call({
            let rec = rec.clone();
            move |conn| insert(conn, &rec)
        })
        .await
        .unwrap();

        let stored = db
            .call({
                let id = rec.id.clone();
                move |conn| get(conn, &id)
            })
            .await
            .unwrap();
        assert!(!stored.auto_start, "新建规则默认不该自动启动");

        db.call({
            let id = rec.id.clone();
            move |conn| set_auto_start(conn, &id, true)
        })
        .await
        .unwrap();
        assert!(
            db.call({
                let id = rec.id.clone();
                move |conn| get(conn, &id)
            })
            .await
            .unwrap()
            .auto_start
        );

        db.call({
            let id = rec.id.clone();
            move |conn| set_auto_start(conn, &id, false)
        })
        .await
        .unwrap();
        assert!(
            !db.call({
                let id = rec.id.clone();
                move |conn| get(conn, &id)
            })
            .await
            .unwrap()
            .auto_start
        );
    }

    /// 启动时只该拉起打开开关的那几条。
    #[tokio::test]
    async fn list_auto_start_returns_only_enabled_rules() {
        let db = Db::open_in_memory().unwrap();
        let on = sample("c1", 13306, "10.0.3.21", 3306).with_auto_start(true);
        let off = sample("c2", 15432, "10.0.3.31", 5432);

        db.call({
            let (on, off) = (on.clone(), off.clone());
            move |conn| {
                insert(conn, &on)?;
                insert(conn, &off)
            }
        })
        .await
        .unwrap();

        let rows = db.call(list_auto_start).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, on.id);
        assert!(rows[0].auto_start);
    }

    /// 改一条不存在的规则要报错，不能静默成功。
    #[tokio::test]
    async fn set_auto_start_on_missing_rule_errors() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .call(|conn| set_auto_start(conn, "nope", true))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrCode::StorageError);
    }
}
