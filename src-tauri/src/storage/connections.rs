//! 连接配置的读写。
//!
//! 约定：
//! - 密文字段（`secret_blob` / `key_pass_blob`）原样存取，加解密由 `crypto` 层在边界完成；
//! - 这一层不认识明文，也不做校验。参数校验和加解密都在 `commands/connections.rs`。

use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use crate::error::{AppError, AppResult, ErrCode};
use crate::storage::migrations::now_millis;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    Password,
    Key,
}

impl AuthType {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthType::Password => "password",
            AuthType::Key => "key",
        }
    }

    pub fn parse(raw: &str) -> AppResult<Self> {
        match raw {
            "password" => Ok(AuthType::Password),
            "key" => Ok(AuthType::Key),
            other => Err(AppError::validation(format!("不支持的认证方式: {other}"))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionRecord {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: AuthType,
    /// 加密后的密码
    pub secret_blob: Option<Vec<u8>>,
    /// 私钥路径（只存路径，不存私钥内容）
    pub key_path: Option<String>,
    /// 加密后的私钥口令
    pub key_pass_blob: Option<Vec<u8>>,
    /// S1 Spike 结论：兼容老服务器算法
    pub legacy_algorithms: bool,
    /// 跳板机：指向另一条连接。空 = 直连；多跳靠"跳板自己也有跳板"串成链
    pub jump_connection_id: Option<String>,
    pub last_used_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ConnectionRecord {
    pub fn new(
        name: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        auth_type: AuthType,
    ) -> Self {
        let now = now_millis();
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            name: name.into(),
            host: host.into(),
            port,
            username: username.into(),
            auth_type,
            secret_blob: None,
            key_path: None,
            key_pass_blob: None,
            legacy_algorithms: false,
            jump_connection_id: None,
            last_used_at: None,
            created_at: now,
            updated_at: now,
        }
    }
}

const COLUMNS: &str = "id, name, host, port, username, auth_type, secret_blob, key_path, \
                       key_pass_blob, legacy_algorithms, last_used_at, created_at, updated_at, \
                       jump_connection_id";

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<ConnectionRecord> {
    let auth_raw: String = row.get(5)?;
    Ok(ConnectionRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        host: row.get(2)?,
        port: row.get::<_, i64>(3)? as u16,
        username: row.get(4)?,
        // 库里的 CHECK 约束保证了只有这两种值，兜底给 Password 而不是 panic
        auth_type: if auth_raw == "key" {
            AuthType::Key
        } else {
            AuthType::Password
        },
        secret_blob: row.get(6)?,
        key_path: row.get(7)?,
        key_pass_blob: row.get(8)?,
        legacy_algorithms: row.get::<_, i64>(9)? != 0,
        last_used_at: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        jump_connection_id: row.get(13)?,
    })
}

/// 列出连接，`query` 非空时按名称/主机/用户名模糊匹配。
/// 排序：用过的排前面，其次按名称。
pub fn list(conn: &Connection, query: Option<&str>) -> AppResult<Vec<ConnectionRecord>> {
    let keyword = query.map(str::trim).filter(|q| !q.is_empty());

    let mut sql = format!("SELECT {COLUMNS} FROM connections");
    let mut args: Vec<String> = Vec::new();
    if let Some(k) = keyword {
        sql.push_str(" WHERE name LIKE ?1 OR host LIKE ?1 OR username LIKE ?1");
        args.push(format!("%{k}%"));
    }
    sql.push_str(" ORDER BY COALESCE(last_used_at, 0) DESC, name ASC");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), row_to_record)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn get(conn: &Connection, id: &str) -> AppResult<ConnectionRecord> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM connections WHERE id = ?1"),
        [id],
        row_to_record,
    )
    .optional()?
    .ok_or_else(|| AppError::with_detail(ErrCode::ConnectionNotFound, format!("id={id}")))
}

pub fn insert(conn: &Connection, rec: &ConnectionRecord) -> AppResult<()> {
    conn.execute(
        "INSERT INTO connections
         (id, name, host, port, username, auth_type, secret_blob, key_path, key_pass_blob,
          legacy_algorithms, last_used_at, created_at, updated_at, jump_connection_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            rec.id,
            rec.name,
            rec.host,
            rec.port as i64,
            rec.username,
            rec.auth_type.as_str(),
            rec.secret_blob,
            rec.key_path,
            rec.key_pass_blob,
            rec.legacy_algorithms as i64,
            rec.last_used_at,
            rec.created_at,
            rec.updated_at,
            rec.jump_connection_id,
        ],
    )?;
    Ok(())
}

pub fn update(conn: &Connection, rec: &ConnectionRecord) -> AppResult<()> {
    let affected = conn.execute(
        "UPDATE connections SET
           name = ?2, host = ?3, port = ?4, username = ?5, auth_type = ?6,
           secret_blob = ?7, key_path = ?8, key_pass_blob = ?9, legacy_algorithms = ?10,
           jump_connection_id = ?11, updated_at = ?12
         WHERE id = ?1",
        params![
            rec.id,
            rec.name,
            rec.host,
            rec.port as i64,
            rec.username,
            rec.auth_type.as_str(),
            rec.secret_blob,
            rec.key_path,
            rec.key_pass_blob,
            rec.legacy_algorithms as i64,
            rec.jump_connection_id,
            now_millis(),
        ],
    )?;
    ensure_affected(affected, &rec.id)
}

/// 删除连接，并把它作为跳板的那些连接改成直连。
///
/// 不清引用的话，剩下的连接会永远指向一个不存在的 id，每次连接都报"跳板不存在"，
/// 而用户完全不知道该改哪 —— 宁可静默降级成直连，也不要留一个修不好的死链。
pub fn delete(conn: &Connection, id: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE connections SET jump_connection_id = NULL WHERE jump_connection_id = ?1",
        [id],
    )?;
    let affected = conn.execute("DELETE FROM connections WHERE id = ?1", [id])?;
    ensure_affected(affected, id)
}

/// 收集 `id` 的跳板链，**最近的一跳排在最前**（目标 → B → A 得到 `[B, A]`）。
///
/// 三条硬规则，都是"脏配置不许把连接搞挂"：
/// - 最多 `max_hops` 跳，超了报错 —— 免得一条畸形链让连接卡到超时；
/// - 链上出现重复 id 说明成环，报错 —— 手改过库也挡得住；
/// - 跳板 id 查不到记录就报错 —— 正常删除会清引用，走到这里就是脏数据。
pub fn jump_chain(
    conn: &Connection,
    id: &str,
    max_hops: usize,
) -> AppResult<Vec<ConnectionRecord>> {
    let mut out: Vec<ConnectionRecord> = Vec::new();
    let mut seen: Vec<String> = vec![id.to_string()];
    let mut cursor = get(conn, id)?.jump_connection_id;

    while let Some(next) = cursor {
        if out.len() >= max_hops {
            return Err(AppError::validation(format!(
                "跳板最多 {max_hops} 层，请检查「经由」是不是配错了"
            )));
        }
        if seen.iter().any(|s| s == &next) {
            return Err(AppError::validation(
                "「经由」配置成环了：这条连接最终又绕回它自己，请改掉其中一条",
            ));
        }
        seen.push(next.clone());

        let rec = get(conn, &next).map_err(|_| {
            AppError::validation("「经由」指定的连接不存在，可能已被删除，请重新选择")
        })?;
        cursor = rec.jump_connection_id.clone();
        out.push(rec);
    }

    Ok(out)
}

pub fn touch_last_used(conn: &Connection, id: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE connections SET last_used_at = ?2 WHERE id = ?1",
        params![id, now_millis()],
    )?;
    Ok(())
}

fn ensure_affected(affected: usize, id: &str) -> AppResult<()> {
    if affected == 0 {
        return Err(AppError::with_detail(
            ErrCode::ConnectionNotFound,
            format!("id={id}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Db;

    fn sample(name: &str, host: &str) -> ConnectionRecord {
        ConnectionRecord::new(name, host, 22, "root", AuthType::Password)
    }

    #[tokio::test]
    async fn insert_then_get_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let rec = sample("生产", "10.0.0.1");

        db.call({
            let rec = rec.clone();
            move |c| insert(c, &rec)
        })
        .await
        .unwrap();

        let loaded = db
            .call({
                let id = rec.id.clone();
                move |c| get(c, &id)
            })
            .await
            .unwrap();

        assert_eq!(loaded.name, "生产");
        assert_eq!(loaded.host, "10.0.0.1");
        assert_eq!(loaded.auth_type, AuthType::Password);
        assert!(!loaded.legacy_algorithms, "新连接默认不开兼容模式");
    }

    #[tokio::test]
    async fn get_missing_returns_not_found() {
        let db = Db::open_in_memory().unwrap();
        let err = db.call(|c| get(c, "nope")).await.unwrap_err();
        assert_eq!(err.code, ErrCode::ConnectionNotFound);
    }

    #[tokio::test]
    async fn search_matches_name_host_and_user() {
        let db = Db::open_in_memory().unwrap();
        let mut a = sample("生产组", "10.0.0.1");
        a.username = "deploy".into();
        let b = sample("测试组", "192.168.1.5");

        db.call({
            let a = a.clone();
            let b = b.clone();
            move |c| {
                insert(c, &a)?;
                insert(c, &b)
            }
        })
        .await
        .unwrap();

        let by_name = db.call(|c| list(c, Some("生产"))).await.unwrap();
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].name, "生产组");

        let by_host = db.call(|c| list(c, Some("192.168"))).await.unwrap();
        assert_eq!(by_host.len(), 1);

        let by_user = db.call(|c| list(c, Some("deploy"))).await.unwrap();
        assert_eq!(by_user.len(), 1);

        let all = db.call(|c| list(c, None)).await.unwrap();
        assert_eq!(all.len(), 2);

        let blank = db.call(|c| list(c, Some("   "))).await.unwrap();
        assert_eq!(blank.len(), 2, "空查询串应当等同于不过滤");
    }

    #[tokio::test]
    async fn update_changes_fields_and_touches_timestamp() {
        let db = Db::open_in_memory().unwrap();
        let rec = sample("旧名", "1.1.1.1");
        let id = rec.id.clone();

        db.call({
            let rec = rec.clone();
            move |c| insert(c, &rec)
        })
        .await
        .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        db.call({
            let id = id.clone();
            move |c| {
                let mut r = get(c, &id)?;
                r.name = "新名".into();
                r.legacy_algorithms = true;
                update(c, &r)
            }
        })
        .await
        .unwrap();

        let loaded = db
            .call({
                let id = id.clone();
                move |c| get(c, &id)
            })
            .await
            .unwrap();
        assert_eq!(loaded.name, "新名");
        assert!(loaded.legacy_algorithms);
        assert!(loaded.updated_at > loaded.created_at);
    }

    #[tokio::test]
    async fn delete_removes_and_second_delete_fails() {
        let db = Db::open_in_memory().unwrap();
        let rec = sample("待删", "2.2.2.2");
        let id = rec.id.clone();

        db.call({
            let rec = rec.clone();
            move |c| insert(c, &rec)
        })
        .await
        .unwrap();

        db.call({
            let id = id.clone();
            move |c| delete(c, &id)
        })
        .await
        .unwrap();

        let err = db
            .call({
                let id = id.clone();
                move |c| delete(c, &id)
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrCode::ConnectionNotFound);

        let all = db.call(|c| list(c, None)).await.unwrap();
        assert!(all.is_empty());
    }

    #[tokio::test]
    async fn auth_type_parse_rejects_unknown() {
        assert!(AuthType::parse("password").is_ok());
        assert!(AuthType::parse("key").is_ok());
        assert!(AuthType::parse("kerberos").is_err());
    }

    #[tokio::test]
    async fn jump_connection_roundtrips() {
        let db = Db::open_in_memory().unwrap();
        let mut hop = sample("堡垒机", "10.0.0.9");
        let mut inner = sample("内网库", "10.0.3.21");
        inner.jump_connection_id = Some(hop.id.clone());

        db.call({
            let hop = hop.clone();
            let inner = inner.clone();
            move |c| {
                insert(c, &hop)?;
                insert(c, &inner)
            }
        })
        .await
        .unwrap();

        let loaded = db
            .call({
                let id = inner.id.clone();
                move |c| get(c, &id)
            })
            .await
            .unwrap();
        assert_eq!(loaded.jump_connection_id, Some(hop.id.clone()));

        // 直连的那条必须是 None，别把空串当"有跳板"
        hop.jump_connection_id = None;
        let direct = db.call(|c| list(c, None)).await.unwrap();
        assert!(direct.iter().any(|r| r.jump_connection_id.is_none()));
    }

    #[tokio::test]
    async fn deleting_a_jump_turns_dependents_back_to_direct() {
        let db = Db::open_in_memory().unwrap();
        let hop = sample("堡垒机", "10.0.0.9");
        let mut inner = sample("内网库", "10.0.3.21");
        inner.jump_connection_id = Some(hop.id.clone());

        db.call({
            let hop = hop.clone();
            let inner = inner.clone();
            move |c| {
                insert(c, &hop)?;
                insert(c, &inner)
            }
        })
        .await
        .unwrap();

        db.call({
            let id = hop.id.clone();
            move |c| delete(c, &id)
        })
        .await
        .unwrap();

        let left = db
            .call({
                let id = inner.id.clone();
                move |c| get(c, &id)
            })
            .await
            .unwrap();
        assert!(
            left.jump_connection_id.is_none(),
            "跳板被删后必须降级成直连，不能留一个修不好的死链"
        );
    }

    #[tokio::test]
    async fn jump_chain_walks_nearest_first_and_stops_at_depth_limit() {
        let db = Db::open_in_memory().unwrap();
        let a = sample("A", "10.0.0.1");
        let mut b = sample("B", "10.0.0.2");
        b.jump_connection_id = Some(a.id.clone());
        let mut c = sample("C", "10.0.0.3");
        c.jump_connection_id = Some(b.id.clone());
        let mut d = sample("D", "10.0.0.4");
        d.jump_connection_id = Some(c.id.clone());

        db.call({
            let (a, b, c, d) = (a.clone(), b.clone(), c.clone(), d.clone());
            move |conn| {
                insert(conn, &a)?;
                insert(conn, &b)?;
                insert(conn, &c)?;
                insert(conn, &d)
            }
        })
        .await
        .unwrap();

        let chain = db
            .call({
                let id = d.id.clone();
                move |conn| jump_chain(conn, &id, 5)
            })
            .await
            .unwrap();
        assert_eq!(
            chain.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["C", "B", "A"],
            "最近的一跳必须排在最前"
        );

        let err = db
            .call({
                let id = d.id.clone();
                move |conn| jump_chain(conn, &id, 2)
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrCode::ValidationError);
    }

    #[tokio::test]
    async fn jump_chain_rejects_cycle() {
        let db = Db::open_in_memory().unwrap();
        let a = sample("A", "10.0.0.1");
        let mut b = sample("B", "10.0.0.2");
        b.jump_connection_id = Some(a.id.clone());
        let mut a_loop = a.clone();
        a_loop.jump_connection_id = Some(b.id.clone());

        db.call({
            let (a_loop, b) = (a_loop.clone(), b.clone());
            move |conn| {
                insert(conn, &a_loop)?;
                insert(conn, &b)
            }
        })
        .await
        .unwrap();

        let err = db
            .call({
                let id = b.id.clone();
                move |conn| jump_chain(conn, &id, 5)
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrCode::ValidationError);
        assert!(
            err.message.contains("成环") || err.detail.unwrap_or_default().contains("成环"),
            "文案要提到成环，别只说输入不合法"
        );
    }

    #[tokio::test]
    async fn jump_chain_reports_missing_jump() {
        let db = Db::open_in_memory().unwrap();
        let mut rec = sample("孤儿", "10.0.0.5");
        rec.jump_connection_id = Some("gone".into());

        db.call({
            let rec = rec.clone();
            move |conn| insert(conn, &rec)
        })
        .await
        .unwrap();

        let err = db
            .call({
                let id = rec.id.clone();
                move |conn| jump_chain(conn, &id, 5)
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrCode::ValidationError);
    }
}
