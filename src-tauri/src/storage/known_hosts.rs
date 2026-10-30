//! 主机指纹（known_hosts）存取。
//!
//! 策略是 TOFU（Trust On First Use）+ 变更即阻断：
//! - 首次连接：指纹不存在 → 前端弹确认框 → 用户确认后写入；
//! - 再次连接：指纹必须完全一致，不一致直接拒绝，**不允许静默覆盖**（那是 MITM 的高速公路）。
//!
//! 记录格式刻意对齐 OpenSSH，方便用户拿 `ssh-keygen -F` 互相印证。

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::AppResult;

/// known_hosts 里的主机键。
///
/// 22 端口只写主机名，非 22 端口写成 `[host]:port` —— 和 OpenSSH 的习惯保持一致，
/// 否则用户拿着 `ssh-keygen -F example.com:22` 对不上会以为是 bug。
pub fn host_key_id(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHostRecord {
    pub host_key: String,
    pub algo: String,
    /// 形如 `SHA256:xxxx`，与 `ssh-keygen -lf` 输出一致
    pub fingerprint: String,
}

pub fn find(conn: &Connection, host_key: &str) -> AppResult<Option<KnownHostRecord>> {
    Ok(conn
        .query_row(
            "SELECT host_key, algo, fingerprint FROM known_hosts WHERE host_key = ?1",
            [host_key],
            |row| {
                Ok(KnownHostRecord {
                    host_key: row.get(0)?,
                    algo: row.get(1)?,
                    fingerprint: row.get(2)?,
                })
            },
        )
        .optional()?)
}

pub fn upsert(conn: &Connection, rec: &KnownHostRecord) -> AppResult<()> {
    conn.execute(
        "INSERT INTO known_hosts (host_key, algo, fingerprint, added_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(host_key) DO UPDATE SET algo = ?2, fingerprint = ?3, added_at = ?4",
        params![
            rec.host_key,
            rec.algo,
            rec.fingerprint,
            crate::storage::migrations::now_millis()
        ],
    )?;
    Ok(())
}

/// 忘记某台主机的指纹。指纹变了之后，用户必须走这条路才能重连 —— 强制一个显式动作。
pub fn forget(conn: &Connection, host_key: &str) -> AppResult<bool> {
    Ok(conn.execute("DELETE FROM known_hosts WHERE host_key = ?1", [host_key])? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Db;

    #[test]
    fn host_key_id_follows_openssh_convention() {
        assert_eq!(host_key_id("example.com", 22), "example.com");
        assert_eq!(host_key_id("example.com", 2222), "[example.com]:2222");
        assert_eq!(host_key_id("10.0.0.1", 22), "10.0.0.1");
    }

    #[tokio::test]
    async fn upsert_is_idempotent_and_overwrites() {
        let db = Db::open_in_memory().unwrap();
        let key = host_key_id("h", 22);
        let first = KnownHostRecord {
            host_key: key.clone(),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:aaa".into(),
        };
        let second = KnownHostRecord {
            host_key: key.clone(),
            algo: "rsa-sha2-256".into(),
            fingerprint: "SHA256:bbb".into(),
        };

        db.call({
            let first = first.clone();
            move |c| upsert(c, &first)
        })
        .await
        .unwrap();
        db.call({
            let second = second.clone();
            move |c| upsert(c, &second)
        })
        .await
        .unwrap();

        let loaded = db
            .call({
                let key = key.clone();
                move |c| find(c, &key)
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.fingerprint, "SHA256:bbb");
        assert_eq!(loaded.algo, "rsa-sha2-256");
    }

    #[tokio::test]
    async fn find_missing_returns_none() {
        let db = Db::open_in_memory().unwrap();
        let got = db.call(|c| find(c, "nope")).await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn forget_removes_entry() {
        let db = Db::open_in_memory().unwrap();
        let rec = KnownHostRecord {
            host_key: "h".into(),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:ccc".into(),
        };
        db.call({
            let rec = rec.clone();
            move |c| upsert(c, &rec)
        })
        .await
        .unwrap();

        let removed = db.call(|c| forget(c, "h")).await.unwrap();
        assert!(removed);
        let again = db.call(|c| forget(c, "h")).await.unwrap();
        assert!(!again);
    }
}
