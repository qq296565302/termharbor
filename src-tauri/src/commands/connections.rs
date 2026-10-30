//! 连接管理 command。
//!
//! 这一层只做三件事：校验输入、加解密、把领域调用串起来（需求 4.4）。
//! 明文密码只在这个文件的作用域里短暂存在，绝不写日志、绝不回传前端。

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::crypto::FieldCipher;
use crate::error::{AppError, AppResult};
use crate::ssh::{self, ProbeAuth, ProbeRequest, ProbeResult};
use crate::state::AppState;
use crate::storage::connections::{self, AuthType, ConnectionRecord};
use crate::storage::known_hosts::{self, host_key_id, KnownHostRecord};

// ---------------------------------------------------------------- DTO

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDto {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: String,
    /// 前端只知道"有没有密码"，永远拿不到密码本身
    pub has_password: bool,
    pub key_path: Option<String>,
    pub has_key_passphrase: bool,
    pub legacy_algorithms: bool,
    pub last_used_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<&ConnectionRecord> for ConnectionDto {
    fn from(rec: &ConnectionRecord) -> Self {
        Self {
            id: rec.id.clone(),
            name: rec.name.clone(),
            host: rec.host.clone(),
            port: rec.port,
            username: rec.username.clone(),
            auth_type: rec.auth_type.as_str().to_string(),
            has_password: rec.secret_blob.is_some(),
            key_path: rec.key_path.clone(),
            has_key_passphrase: rec.key_pass_blob.is_some(),
            legacy_algorithms: rec.legacy_algorithms,
            last_used_at: rec.last_used_at,
            created_at: rec.created_at,
            updated_at: rec.updated_at,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInput {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: String,
    /// `None` 表示"保持原值"，`Some("")` 表示"清空"（仅更新时有意义）
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub key_path: Option<String>,
    #[serde(default)]
    pub key_passphrase: Option<String>,
    #[serde(default)]
    pub legacy_algorithms: bool,
}

/// 敏感字段的更新语义。
enum SecretPatch {
    /// 不动库里的值
    Keep,
    /// 覆盖（`None` 表示清空）
    Set(Option<Vec<u8>>),
}

// ---------------------------------------------------------------- CRUD

#[tauri::command]
pub async fn connection_list(
    state: State<'_, AppState>,
    query: Option<String>,
) -> AppResult<Vec<ConnectionDto>> {
    let rows = state
        .db
        .call(move |conn| connections::list(conn, query.as_deref()))
        .await?;
    Ok(rows.iter().map(ConnectionDto::from).collect())
}

#[tauri::command]
pub async fn connection_get(state: State<'_, AppState>, id: String) -> AppResult<ConnectionDto> {
    let rec = state
        .db
        .call(move |conn| connections::get(conn, &id))
        .await?;
    Ok(ConnectionDto::from(&rec))
}

#[tauri::command]
pub async fn connection_create(
    state: State<'_, AppState>,
    input: ConnectionInput,
) -> AppResult<ConnectionDto> {
    let auth_type = validate(&input)?;
    let cipher = state.require_cipher()?.clone();

    let mut rec = ConnectionRecord::new(
        input.name.trim(),
        input.host.trim(),
        input.port,
        input.username.trim(),
        auth_type,
    );
    rec.legacy_algorithms = input.legacy_algorithms;

    match auth_type {
        AuthType::Password => {
            let password = input
                .password
                .as_deref()
                .filter(|p| !p.is_empty())
                .ok_or_else(|| AppError::validation("密码认证需要填写密码"))?;
            rec.secret_blob = Some(cipher.encrypt_str(password)?);
        }
        AuthType::Key => {
            rec.key_path = Some(input.key_path.unwrap_or_default().trim().to_string());
            if let Some(pass) = input.key_passphrase.as_deref().filter(|p| !p.is_empty()) {
                rec.key_pass_blob = Some(cipher.encrypt_str(pass)?);
            }
        }
    }

    state
        .db
        .call({
            let rec = rec.clone();
            move |conn| connections::insert(conn, &rec)
        })
        .await?;

    tracing::info!(id = %rec.id, name = %rec.name, "已保存连接配置");
    Ok(ConnectionDto::from(&rec))
}

#[tauri::command]
pub async fn connection_update(
    state: State<'_, AppState>,
    id: String,
    input: ConnectionInput,
) -> AppResult<ConnectionDto> {
    let auth_type = validate(&input)?;
    let cipher = state.require_cipher()?.clone();

    let password_patch = secret_patch(&cipher, input.password.as_deref())?;
    let passphrase_patch = secret_patch(&cipher, input.key_passphrase.as_deref())?;

    let rec = state
        .db
        .call(move |conn| {
            let mut rec = connections::get(conn, &id)?;

            rec.name = input.name.trim().to_string();
            rec.host = input.host.trim().to_string();
            rec.port = input.port;
            rec.username = input.username.trim().to_string();
            rec.legacy_algorithms = input.legacy_algorithms;

            // 切换认证方式时要清理另一侧的残留，否则库里会留下永远解不开的密文
            match auth_type {
                AuthType::Password => {
                    rec.key_path = None;
                    rec.key_pass_blob = None;
                    rec.secret_blob = match password_patch {
                        SecretPatch::Set(blob) => blob,
                        SecretPatch::Keep => rec.secret_blob.take(),
                    };
                }
                AuthType::Key => {
                    rec.secret_blob = None;
                    rec.key_path = Some(input.key_path.unwrap_or_default().trim().to_string());
                    rec.key_pass_blob = match passphrase_patch {
                        SecretPatch::Set(blob) => blob,
                        SecretPatch::Keep => rec.key_pass_blob.take(),
                    };
                }
            }
            rec.auth_type = auth_type;

            if rec.auth_type == AuthType::Password && rec.secret_blob.is_none() {
                return Err(AppError::validation("密码认证需要填写密码"));
            }

            connections::update(conn, &rec)?;
            Ok(rec)
        })
        .await?;

    tracing::info!(id = %rec.id, "已更新连接配置");
    Ok(ConnectionDto::from(&rec))
}

#[tauri::command]
pub async fn connection_delete(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state
        .db
        .call({
            let id = id.clone();
            move |conn| connections::delete(conn, &id)
        })
        .await?;
    tracing::info!(id = %id, "已删除连接配置");
    Ok(())
}

// ---------------------------------------------------------------- 连接测试

#[derive(Debug, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TestOutcome {
    Connected {
        elapsed_ms: u64,
        host_key_algo: String,
        fingerprint: String,
        newly_trusted: bool,
    },
    /// 首次连接，前端要弹指纹确认框
    ConfirmationRequired {
        host_key_algo: String,
        fingerprint: String,
    },
    /// 指纹变了，阻断
    HostKeyChanged {
        host_key_algo: String,
        fingerprint: String,
        stored_fingerprint: String,
    },
    CertificateUnsupported,
    Failed {
        code: String,
        message: String,
        detail: Option<String>,
    },
}

#[derive(Debug)]
struct Target {
    id: Option<String>,
    name: String,
    host: String,
    port: u16,
    username: String,
    auth: ProbeAuth,
    legacy_algorithms: bool,
}

/// 测试连接。
///
/// `id` 与 `input` 二选一：已保存的连接走 id（密码从库里解密），
/// 表单里的临时配置走 input（密码还是明文）。这样「保存前先测一下」也能用。
#[tauri::command]
pub async fn connection_test(
    state: State<'_, AppState>,
    id: Option<String>,
    input: Option<ConnectionInput>,
    trust_fingerprint: Option<String>,
) -> AppResult<TestOutcome> {
    let prepared = prepare_probe(state.inner(), id, input, trust_fingerprint).await?;
    let host_key = prepared.host_label.clone();
    let connection_id = prepared.connection_id.clone();

    match ssh::probe(prepared.request).await {
        ProbeResult::Connected(info) => {
            if info.trusted_by_user {
                let record = KnownHostRecord {
                    host_key: host_key.clone(),
                    algo: info.algo.clone(),
                    fingerprint: info.fingerprint.clone(),
                };
                state
                    .db
                    .call(move |conn| known_hosts::upsert(conn, &record))
                    .await?;
                tracing::info!(host = %host_key, fp = %info.fingerprint, "已记录新主机指纹");
            }
            if let Some(id) = connection_id {
                let _ = state
                    .db
                    .call(move |conn| connections::touch_last_used(conn, &id))
                    .await;
            }

            Ok(TestOutcome::Connected {
                elapsed_ms: info.elapsed_ms,
                host_key_algo: info.algo,
                fingerprint: info.fingerprint,
                newly_trusted: info.trusted_by_user,
            })
        }
        ProbeResult::NeedsConfirmation { algo, fingerprint } => {
            Ok(TestOutcome::ConfirmationRequired {
                host_key_algo: algo,
                fingerprint,
            })
        }
        ProbeResult::HostKeyChanged {
            algo,
            fingerprint,
            stored,
        } => Ok(TestOutcome::HostKeyChanged {
            host_key_algo: algo,
            fingerprint,
            stored_fingerprint: stored,
        }),
        ProbeResult::CertificateUnsupported => Ok(TestOutcome::CertificateUnsupported),
        ProbeResult::Failed(err) => Ok(TestOutcome::Failed {
            code: err.code.as_str().to_string(),
            message: err.message.to_string(),
            detail: err.detail,
        }),
    }
}

/// 让用户显式忘掉某台主机的指纹（服务器重装后必须走这一步才能重连）。
#[tauri::command]
pub async fn known_host_forget(
    state: State<'_, AppState>,
    host: String,
    port: u16,
) -> AppResult<()> {
    let host_key = host_key_id(&host, port);
    state
        .db
        .call({
            let host_key = host_key.clone();
            move |conn| known_hosts::forget(conn, &host_key)
        })
        .await?;
    tracing::warn!(host = %host_key, "用户已清除主机指纹记录");
    Ok(())
}

// ---------------------------------------------------------------- 辅助

fn validate(input: &ConnectionInput) -> AppResult<AuthType> {
    if input.name.trim().is_empty() {
        return Err(AppError::validation("连接名称不能为空"));
    }
    if input.host.trim().is_empty() {
        return Err(AppError::validation("主机地址不能为空"));
    }
    if input.host.chars().any(char::is_whitespace) {
        return Err(AppError::validation("主机地址不能包含空白字符"));
    }
    if input.username.trim().is_empty() {
        return Err(AppError::validation("用户名不能为空"));
    }
    if input.port == 0 {
        return Err(AppError::validation("端口必须在 1–65535 之间"));
    }

    let auth_type = AuthType::parse(&input.auth_type)?;
    if auth_type == AuthType::Key {
        let path = input.key_path.as_deref().unwrap_or("").trim();
        if path.is_empty() {
            return Err(AppError::validation("私钥认证需要选择私钥文件"));
        }
    }
    Ok(auth_type)
}

fn secret_patch(cipher: &FieldCipher, value: Option<&str>) -> AppResult<SecretPatch> {
    match value {
        None => Ok(SecretPatch::Keep),
        Some("") => Ok(SecretPatch::Set(None)),
        Some(v) => Ok(SecretPatch::Set(Some(cipher.encrypt_str(v)?))),
    }
}

fn decrypt_secret(cipher: &FieldCipher, blob: Option<&Vec<u8>>) -> AppResult<Option<String>> {
    match blob {
        None => Ok(None),
        Some(bytes) => Ok(Some(cipher.decrypt_string(bytes)?)),
    }
}

/// 决定「测什么」。
///
/// 前端总是把表单内容作为 `input` 传上来，同时带上 `id`。这样：
/// - 表单改了 host/port 也能立刻测到；
/// - 密码框留空时自动回落到库里存的密文，不用让用户为了测一下重新输一遍。
async fn resolve_target(
    state: &AppState,
    id: Option<String>,
    input: Option<ConnectionInput>,
) -> AppResult<Target> {
    let saved = match id {
        Some(id) => Some(
            state
                .db
                .call(move |conn| connections::get(conn, &id))
                .await?,
        ),
        None => None,
    };

    let Some(input) = input else {
        let rec = saved.ok_or_else(|| AppError::validation("缺少连接配置"))?;
        return saved_target(state, &rec);
    };

    let auth_type = validate(&input)?;

    let auth = match auth_type {
        AuthType::Password => {
            let password = match input.password.as_deref().filter(|p| !p.is_empty()) {
                Some(typed) => typed.to_string(),
                None => {
                    let rec = saved
                        .as_ref()
                        .ok_or_else(|| AppError::validation("密码认证需要填写密码"))?;
                    let cipher = state.require_cipher()?;
                    decrypt_secret(cipher, rec.secret_blob.as_ref())?
                        .ok_or_else(|| AppError::validation("这条连接没有保存密码"))?
                }
            };
            ProbeAuth::Password(password)
        }
        AuthType::Key => {
            let passphrase = match input.key_passphrase.as_deref().filter(|p| !p.is_empty()) {
                Some(typed) => Some(typed.to_string()),
                None => match saved.as_ref().filter(|rec| rec.key_pass_blob.is_some()) {
                    Some(rec) => {
                        decrypt_secret(state.require_cipher()?, rec.key_pass_blob.as_ref())?
                    }
                    None => None,
                },
            };
            ProbeAuth::PrivateKey {
                path: input.key_path.unwrap_or_default(),
                passphrase,
            }
        }
    };

    Ok(Target {
        id: saved.map(|rec| rec.id),
        name: input.name.trim().to_string(),
        host: input.host.trim().to_string(),
        port: input.port,
        username: input.username.trim().to_string(),
        auth,
        legacy_algorithms: input.legacy_algorithms,
    })
}

/// 已经解析好的探测目标：凭据、算法档位、已知指纹都就位，可以直接交给 ssh 层。
pub(crate) struct PreparedProbe {
    pub request: ProbeRequest,
    pub connection_id: Option<String>,
    pub display_name: String,
    pub host_label: String,
}

/// 把「连接 id / 表单草稿」统一解析成一次探测请求。
///
/// `connection_test` 和 `session_open` 共用它，保证"能测通"和"能开终端"
/// 走的是同一套凭据解析与指纹策略。
pub(crate) async fn prepare_probe(
    state: &AppState,
    id: Option<String>,
    input: Option<ConnectionInput>,
    trust_fingerprint: Option<String>,
) -> AppResult<PreparedProbe> {
    let target = resolve_target(state, id, input).await?;
    let host_label = host_key_id(&target.host, target.port);

    let known = state
        .db
        .call({
            let host_label = host_label.clone();
            move |conn| known_hosts::find(conn, &host_label)
        })
        .await?;

    Ok(PreparedProbe {
        request: ProbeRequest {
            host: target.host,
            port: target.port,
            username: target.username,
            auth: target.auth,
            legacy_algorithms: target.legacy_algorithms,
            known_fingerprint: known.map(|record| record.fingerprint),
            trust_fingerprint,
        },
        connection_id: target.id,
        display_name: target.name,
        host_label,
    })
}

fn saved_target(state: &AppState, rec: &ConnectionRecord) -> AppResult<Target> {
    let cipher = state.require_cipher()?;
    let auth = match rec.auth_type {
        AuthType::Password => ProbeAuth::Password(
            decrypt_secret(cipher, rec.secret_blob.as_ref())?
                .ok_or_else(|| AppError::validation("这条连接没有保存密码"))?,
        ),
        AuthType::Key => ProbeAuth::PrivateKey {
            path: rec
                .key_path
                .clone()
                .ok_or_else(|| AppError::validation("这条连接没有配置私钥路径"))?,
            passphrase: decrypt_secret(cipher, rec.key_pass_blob.as_ref())?,
        },
    };

    Ok(Target {
        id: Some(rec.id.clone()),
        name: rec.name.clone(),
        host: rec.host.clone(),
        port: rec.port,
        username: rec.username.clone(),
        auth,
        legacy_algorithms: rec.legacy_algorithms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_input() -> ConnectionInput {
        ConnectionInput {
            name: "生产".into(),
            host: "10.0.0.1".into(),
            port: 22,
            username: "root".into(),
            auth_type: "password".into(),
            password: Some("secret".into()),
            key_path: None,
            key_passphrase: None,
            legacy_algorithms: false,
        }
    }

    #[test]
    fn validate_accepts_password_target() {
        assert_eq!(validate(&base_input()).unwrap(), AuthType::Password);
    }

    #[test]
    fn validate_rejects_blank_fields() {
        for mutate in [
            |i: &mut ConnectionInput| i.name = "  ".into(),
            |i: &mut ConnectionInput| i.host = "".into(),
            |i: &mut ConnectionInput| i.username = "".into(),
            |i: &mut ConnectionInput| i.host = "10.0.0.1 22".into(),
            |i: &mut ConnectionInput| i.port = 0,
        ] {
            let mut input = base_input();
            mutate(&mut input);
            let err = validate(&input).unwrap_err();
            assert_eq!(err.code, crate::error::ErrCode::ValidationError);
        }
    }

    #[test]
    fn validate_requires_key_path_for_key_auth() {
        let mut input = base_input();
        input.auth_type = "key".into();
        let err = validate(&input).unwrap_err();
        assert!(err.detail.unwrap().contains("私钥"));

        input.key_path = Some("C:/keys/id_ed25519".into());
        assert_eq!(validate(&input).unwrap(), AuthType::Key);
    }

    #[test]
    fn secret_patch_distinguishes_keep_clear_and_set() {
        let cipher = FieldCipher::new(&crate::crypto::random_key());

        assert!(matches!(
            secret_patch(&cipher, None).unwrap(),
            SecretPatch::Keep
        ));
        assert!(matches!(
            secret_patch(&cipher, Some("")).unwrap(),
            SecretPatch::Set(None)
        ));
        match secret_patch(&cipher, Some("hunter2")).unwrap() {
            SecretPatch::Set(Some(blob)) => {
                assert_eq!(cipher.decrypt_string(&blob).unwrap(), "hunter2");
            }
            _ => panic!("应当产生 Set(Some(..))"),
        }
    }

    #[test]
    fn dto_never_exposes_secrets() {
        let mut rec = ConnectionRecord::new("x", "h", 22, "root", AuthType::Password);
        rec.secret_blob = Some(vec![1, 2, 3]);
        let dto = ConnectionDto::from(&rec);
        let json = serde_json::to_string(&dto).unwrap();

        assert!(dto.has_password);
        assert!(!json.contains("secret_blob"));
        assert!(!json.contains("[1,2,3]"));
    }
}
