//! 连接配置的导入导出（**不含密码**）。
//!
//! ## 这个模块的边界
//!
//! 只导出「怎么连到哪台机器」：名称、主机、端口、用户名、认证方式、私钥路径、兼容开关。
//! **绝不导出**：密码、私钥口令、主机指纹、内部 id、使用时间。
//!
//! 为什么密码不进文件：本地密码是用**机器绑定**的密钥（系统钥匙串里那把）加密的，
//! 换了机器那把密钥不存在，密文就是废数据。真要带密码，只能用用户口令派生一把新密钥
//! 重新加密整个包（Argon2id），风险面大一个量级 —— 那是下一步，不是这里。
//!
//! 为什么主机指纹不进文件：导入的文件是**外来数据**。如果它带指纹，
//! 攻击者递给你一个「配置包」，里面 pin 的是他代理的指纹，你导入后第一次连接
//! 就"指纹匹配"，中间人永远不会被发现 —— 等于绕过全部主机密钥防护。
//! 所以导入的连接第一次连时必须重新走一遍 TOFU 确认，这是刻意的。
//!
//! ## 结构上没有的东西，就不需要「记得别写」
//!
//! `Entry` 里根本没有 password 字段，所以不存在"哪天顺手加进去"。
//! 有一条测试断言导出的 JSON 里不含 `password` / `passphrase` / `secret` 这些词。

use std::collections::HashSet;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::storage::connections::{self, AuthType, ConnectionRecord};

/// 文件里的格式标识。改名等于换格式，必须同时递增 `VERSION`。
pub const FORMAT: &str = "termharbor.connections";
/// 当前格式版本。读到更高版本直接拒绝，不去猜。
pub const VERSION: u32 = 1;

/// 单个文件的最大字节数。配置是纯文本，4 MiB 已经远超正常规模。
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 500;
const NAME_MAX: usize = 60;
const HOST_MAX: usize = 255;
const USER_MAX: usize = 128;
const KEY_PATH_MAX: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub format: String,
    pub version: u32,
    /// 导出时刻（Unix 毫秒），只用于展示
    pub exported_at: i64,
    /// 导出它的客户端版本，供排查
    pub app_version: String,
    pub connections: Vec<Entry>,
}

/// 文件里的一条连接。
///
/// **这里刻意没有 password / passphrase 字段** —— 结构上没有，就不可能被写出去。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// `password` | `key`
    pub auth_type: String,
    #[serde(default)]
    pub key_path: Option<String>,
    #[serde(default)]
    pub legacy_algorithms: bool,
}

/// 校验后的一条连接：认证方式已解析，字符串已裁剪。
#[derive(Debug, Clone, PartialEq)]
pub struct ImportEntry {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthType,
    pub key_path: Option<String>,
    pub legacy_algorithms: bool,
}

/// 导入计划：要插入的记录，以及因重复被跳过的条数。
#[derive(Debug, Default)]
pub struct Plan {
    pub to_insert: Vec<ConnectionRecord>,
    pub skipped: usize,
}

/// 把库里的连接转成可导出的文档。
pub fn export_document(records: &[ConnectionRecord], app_version: &str) -> Document {
    Document {
        format: FORMAT.to_string(),
        version: VERSION,
        exported_at: crate::state::now_millis() as i64,
        app_version: app_version.to_string(),
        connections: records
            .iter()
            .map(|rec| Entry {
                name: rec.name.clone(),
                host: rec.host.clone(),
                port: rec.port,
                username: rec.username.clone(),
                auth_type: rec.auth_type.as_str().to_string(),
                key_path: rec.key_path.clone(),
                legacy_algorithms: rec.legacy_algorithms,
            })
            .collect(),
    }
}

/// 序列化成文本。缩进两格，用户看不懂也能用文本编辑器改。
pub fn to_json(document: &Document) -> AppResult<String> {
    serde_json::to_string_pretty(document)
        .map_err(|err| AppError::internal(format!("导出序列化失败: {err}")))
}

/// 解析并校验一个导入文件。
///
/// 校验是**严格**的：任何一条不合法就整体拒绝，并指出是第几条、错在哪。
/// 理由：这个文件正常来自我们自己的导出，出现非法值说明它被改过或不是我们的文件；
/// 半导入的状态比拒绝更难解释。
pub fn parse(text: &str) -> AppResult<Vec<ImportEntry>> {
    if text.len() > MAX_BYTES {
        return Err(AppError::validation(format!(
            "文件超过 {} MiB，不像是连接配置文件",
            MAX_BYTES / 1024 / 1024
        )));
    }

    let document: Document = serde_json::from_str(text)
        .map_err(|err| AppError::validation(format!("不是有效的连接配置文件: {err}")))?;

    if document.format != FORMAT {
        return Err(AppError::validation(format!(
            "文件格式不对（期望 {}，实际 {}）",
            FORMAT, document.format
        )));
    }

    if document.version > VERSION {
        return Err(AppError::validation(format!(
            "这个文件来自更新的版本（格式 v{}，当前支持 v{}），请先升级客户端",
            document.version, VERSION
        )));
    }

    if document.connections.len() > MAX_ENTRIES {
        return Err(AppError::validation(format!(
            "文件里有 {} 条连接，超过上限 {MAX_ENTRIES} 条",
            document.connections.len()
        )));
    }

    let mut out = Vec::with_capacity(document.connections.len());
    for (index, entry) in document.connections.into_iter().enumerate() {
        out.push(validate_entry(index + 1, entry)?);
    }
    Ok(out)
}

fn validate_entry(position: usize, entry: Entry) -> AppResult<ImportEntry> {
    let host = entry.host.trim();
    if host.is_empty() {
        return Err(AppError::validation(format!(
            "第 {position} 条：主机不能为空"
        )));
    }
    if host.chars().count() > HOST_MAX {
        return Err(AppError::validation(format!(
            "第 {position} 条：主机名超过 {HOST_MAX} 个字符"
        )));
    }

    let username = entry.username.trim();
    if username.is_empty() {
        return Err(AppError::validation(format!(
            "第 {position} 条：用户名不能为空"
        )));
    }
    if username.chars().count() > USER_MAX {
        return Err(AppError::validation(format!(
            "第 {position} 条：用户名超过 {USER_MAX} 个字符"
        )));
    }

    if entry.port == 0 {
        return Err(AppError::validation(format!(
            "第 {position} 条：端口必须在 1-65535 之间"
        )));
    }

    // 名称为空时退回主机名：我们自己的表单要求填名称，但外部文件可能缺，
    // 这种情况没必要整份拒绝 —— 列表里显示主机名比显示空白有用。
    let name = entry.name.trim();
    let name = if name.is_empty() { host } else { name };
    if name.chars().count() > NAME_MAX {
        return Err(AppError::validation(format!(
            "第 {position} 条：名称超过 {NAME_MAX} 个字符"
        )));
    }

    let auth = AuthType::parse(entry.auth_type.trim()).map_err(|_| {
        AppError::validation(format!(
            "第 {position} 条：认证方式只能是 password 或 key（实际 {}）",
            entry.auth_type
        ))
    })?;

    // 私钥路径只做长度限制，不检查文件是否存在：
    // 这个文件大概率来自另一台机器，那边的路径在本机当然不存在。
    let key_path = entry
        .key_path
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(path) = &key_path {
        if path.chars().count() > KEY_PATH_MAX {
            return Err(AppError::validation(format!(
                "第 {position} 条：私钥路径超过 {KEY_PATH_MAX} 个字符"
            )));
        }
    }

    Ok(ImportEntry {
        name: name.to_string(),
        host: host.to_string(),
        port: entry.port,
        username: username.to_string(),
        auth,
        key_path,
        legacy_algorithms: entry.legacy_algorithms,
    })
}

/// 生成导入计划：跳过库里已有的、以及本文件内重复的。
///
/// 判重按 `(主机, 端口, 用户名)`：
/// - 主机不区分大小写（`GitHub.com` 和 `github.com` 是同一台）
/// - 用户名保留大小写（Linux 上 `Root` 和 `root` 是两个账户）
///
/// 不做覆盖：导入永远只新增。同名不同主机是合法情况，
/// 而"悄悄改掉你已有的连接"比"多出一条让你自己删"糟得多。
pub fn plan(entries: Vec<ImportEntry>, existing: &[(String, u16, String)]) -> Plan {
    let mut seen: HashSet<(String, u16, String)> = existing
        .iter()
        .map(|(host, port, user)| identity(host, *port, user))
        .collect();

    let mut plan = Plan::default();
    for entry in entries {
        let key = identity(&entry.host, entry.port, &entry.username);
        if !seen.insert(key) {
            plan.skipped += 1;
            continue;
        }

        let mut record = ConnectionRecord::new(
            entry.name,
            entry.host,
            entry.port,
            entry.username,
            entry.auth,
        );
        // 密码与口令一律留空：文件里没有，用户第一次连接时自己填
        record.key_path = entry.key_path;
        record.legacy_algorithms = entry.legacy_algorithms;
        plan.to_insert.push(record);
    }

    plan
}

fn identity(host: &str, port: u16, username: &str) -> (String, u16, String) {
    (host.trim().to_ascii_lowercase(), port, username.to_string())
}

/// 把计划写进库：一个事务，要么全进要么全不进。
pub fn apply(conn: &Connection, plan: &Plan) -> AppResult<usize> {
    if plan.to_insert.is_empty() {
        return Ok(0);
    }

    let tx = conn.unchecked_transaction()?;
    for record in &plan.to_insert {
        connections::insert(&tx, record)?;
    }
    tx.commit()?;

    Ok(plan.to_insert.len())
}

/// 读出库里所有连接的判重键，供 `plan` 使用。
pub fn existing_identities(conn: &Connection) -> AppResult<Vec<(String, u16, String)>> {
    let mut stmt = conn.prepare("SELECT host, port, username FROM connections")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)? as u16,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Db;

    fn sample_record(name: &str, host: &str) -> ConnectionRecord {
        let mut record = ConnectionRecord::new(name, host, 22, "root", AuthType::Password);
        // 故意塞上密文：导出的 JSON 里一个字都不该出现
        record.secret_blob = Some(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        record.key_pass_blob = Some(vec![0xCA, 0xFE]);
        record.key_path = Some("/home/u/.ssh/id_ed25519".to_string());
        record
    }

    fn document_json(entries: &str) -> String {
        format!(
            r#"{{"format":"{FORMAT}","version":{VERSION},"exportedAt":1,"appVersion":"0.1.9","connections":[{entries}]}}"#
        )
    }

    #[test]
    fn export_never_writes_secret_fields() {
        let records = vec![sample_record("阿里云", "1.2.3.4")];
        let json = to_json(&export_document(&records, "0.1.9")).unwrap();

        // 只查**字段名**形式（`"password":`），不查裸词 ——
        // `authType: "password"` 是认证方式的取值，本来就该在文件里。
        // 上一版按裸词查，这条测试自己报了假警。
        let lowered = json.to_ascii_lowercase();
        for forbidden in [
            "\"password\":",
            "\"passphrase\":",
            "\"secret",
            "\"blob",
            "\"key_pass",
            "deadbeef",
        ] {
            assert!(
                !lowered.contains(forbidden),
                "导出的文件里出现了字段 {forbidden}: {json}"
            );
        }
        assert!(json.contains("1.2.3.4"), "该有的字段要有");
    }

    #[test]
    fn export_drops_ids_and_host_key_fingerprints() {
        // 指纹不导出是安全要求：外来文件不能替用户 pin 指纹（见模块头注释）
        let json = to_json(&export_document(&[sample_record("a", "h")], "0.1.9")).unwrap();
        for forbidden in ["fingerprint", "\"id\"", "lastUsedAt", "known_host"] {
            assert!(!json.contains(forbidden), "不该出现 {forbidden}");
        }
    }

    #[test]
    fn roundtrip_keeps_the_connection_fields() {
        let records = vec![sample_record("阿里云", "1.2.3.4")];
        let json = to_json(&export_document(&records, "0.1.9")).unwrap();

        let entries = parse(&json).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "阿里云");
        assert_eq!(entries[0].host, "1.2.3.4");
        assert_eq!(entries[0].port, 22);
        assert_eq!(entries[0].auth, AuthType::Password);
        assert_eq!(
            entries[0].key_path.as_deref(),
            Some("/home/u/.ssh/id_ed25519")
        );
    }

    #[test]
    fn rejects_foreign_or_newer_files() {
        let wrong_format =
            r#"{"format":"other","version":1,"exportedAt":1,"appVersion":"x","connections":[]}"#;
        assert!(parse(wrong_format).is_err());

        let newer = document_json("").replace(
            &format!("\"version\":{VERSION}"),
            &format!("\"version\":{}", VERSION + 1),
        );
        let err = parse(&newer).unwrap_err();
        assert!(
            err.detail.unwrap_or_default().contains("升级"),
            "应当提示需要升级客户端"
        );
    }

    #[test]
    fn rejects_malformed_json_without_panicking() {
        for text in [
            "",
            "not json",
            "{}",
            r#"{"format":"termharbor.connections"}"#,
        ] {
            assert!(parse(text).is_err(), "应当拒绝: {text:?}");
        }
    }

    #[test]
    fn rejects_oversized_files_and_too_many_entries() {
        let huge = "x".repeat(MAX_BYTES + 1);
        assert!(parse(&huge).is_err());

        let entries = (0..MAX_ENTRIES + 1)
            .map(|index| {
                format!(
                    r#"{{"name":"n{index}","host":"h{index}","port":22,"username":"root","authType":"password"}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse(&document_json(&entries)).is_err());
    }

    #[test]
    fn rejects_invalid_fields_with_the_entry_number() {
        let cases = [
            (
                r#"{"name":"a","host":"","port":22,"username":"root","authType":"password"}"#,
                "主机",
            ),
            (
                r#"{"name":"a","host":"h","port":0,"username":"root","authType":"password"}"#,
                "端口",
            ),
            (
                r#"{"name":"a","host":"h","port":22,"username":"","authType":"password"}"#,
                "用户名",
            ),
            (
                r#"{"name":"a","host":"h","port":22,"username":"root","authType":"telnet"}"#,
                "认证方式",
            ),
        ];

        for (entry, expected) in cases {
            let err = parse(&document_json(entry)).unwrap_err();
            let text = err.detail.unwrap_or_default();
            assert!(text.contains(expected), "期望提到{expected}，实际 {text}");
            assert!(text.contains("第 1 条"), "要指明是第几条: {text}");
        }
    }

    #[test]
    fn empty_name_falls_back_to_host() {
        let text = document_json(
            r#"{"name":"  ","host":"10.0.0.9","port":22,"username":"root","authType":"password"}"#,
        );
        let entries = parse(&text).unwrap();
        assert_eq!(entries[0].name, "10.0.0.9");
    }

    #[test]
    fn plan_skips_duplicates_against_existing_and_within_the_file() {
        let entries = vec![
            ImportEntry {
                name: "新的".into(),
                host: "10.0.0.1".into(),
                port: 22,
                username: "root".into(),
                auth: AuthType::Password,
                key_path: None,
                legacy_algorithms: false,
            },
            // 与库里已有的重复（大小写不同也算同一台）
            ImportEntry {
                name: "重复".into(),
                host: "GitHub.com".into(),
                port: 2222,
                username: "git".into(),
                auth: AuthType::Key,
                key_path: Some("/k".into()),
                legacy_algorithms: false,
            },
            // 与本文件第一条重复
            ImportEntry {
                name: "文件内重复".into(),
                host: "10.0.0.1".into(),
                port: 22,
                username: "root".into(),
                auth: AuthType::Password,
                key_path: None,
                legacy_algorithms: false,
            },
        ];

        let existing = vec![("github.com".to_string(), 2222u16, "git".to_string())];
        let plan = plan(entries, &existing);

        assert_eq!(plan.to_insert.len(), 1);
        assert_eq!(plan.skipped, 2);
        assert_eq!(plan.to_insert[0].host, "10.0.0.1");
        assert!(
            plan.to_insert[0].secret_blob.is_none() && plan.to_insert[0].key_pass_blob.is_none(),
            "导入的连接不该带任何凭据"
        );
    }

    #[test]
    fn plan_generates_fresh_ids_so_import_never_overwrites() {
        let make = || ImportEntry {
            name: "a".into(),
            host: "10.0.0.1".into(),
            port: 22,
            username: "root".into(),
            auth: AuthType::Password,
            key_path: None,
            legacy_algorithms: false,
        };

        let first = plan(vec![make()], &[]);
        let second = plan(vec![make()], &[]);

        assert_ne!(
            first.to_insert[0].id, second.to_insert[0].id,
            "每次导入都要新 id，否则会撞掉已有记录"
        );
    }

    #[tokio::test]
    async fn apply_inserts_rows_without_credentials() {
        let db = Db::open_in_memory().unwrap();
        let records = vec![sample_record("阿里云", "1.2.3.4")];
        let json = to_json(&export_document(&records, "0.1.9")).unwrap();
        let entries = parse(&json).unwrap();

        let imported = db
            .call(move |conn| {
                let existing = existing_identities(conn)?;
                let plan = plan(entries, &existing);
                apply(conn, &plan)
            })
            .await
            .unwrap();

        assert_eq!(imported, 1);

        let (total, with_secret) = db
            .call(|conn| {
                let total: i64 =
                    conn.query_row("SELECT COUNT(*) FROM connections", [], |row| row.get(0))?;
                let with_secret: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM connections WHERE secret_blob IS NOT NULL",
                    [],
                    |row| row.get(0),
                )?;
                Ok((total, with_secret))
            })
            .await
            .unwrap();

        assert_eq!(total, 1);
        assert_eq!(with_secret, 0, "导入的记录不该有任何密文");
    }

    #[tokio::test]
    async fn importing_the_same_file_twice_is_idempotent() {
        let db = Db::open_in_memory().unwrap();
        let records = vec![sample_record("阿里云", "1.2.3.4")];
        let json = to_json(&export_document(&records, "0.1.9")).unwrap();

        for expected in [1usize, 0] {
            let entries = parse(&json).unwrap();
            let imported = db
                .call(move |conn| {
                    let existing = existing_identities(conn)?;
                    let plan = plan(entries, &existing);
                    apply(conn, &plan)
                })
                .await
                .unwrap();
            assert_eq!(imported, expected);
        }

        let total = db
            .call(|conn| {
                Ok(
                    conn.query_row("SELECT COUNT(*) FROM connections", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(total, 1, "重复导入不该产生第二条");
    }
}
