//! SFTP 会话：独立建连、复用登录凭据。
//!
//! 为什么不一上来就复用终端的连接：SSH 通道共享同一个 TCP 窗口，
//! 上传大文件会把终端的输出窗口占满，用户看到的现象是「传文件时终端卡死」。
//! 多开一条连接是最便宜的解法。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use russh::client::Handle;
use russh::Disconnect;
use russh_sftp::client::SftpSession;

use crate::error::{AppError, AppResult, ErrCode};
use crate::ssh::client::{establish, ProbeHandler, ProbeRequest, ProbeResult, Purpose};
use crate::ssh::HostKeyDecision;

/// 一条已建立的 SFTP 连接。
pub struct SftpConn {
    pub session: Arc<SftpSession>,
    /// 登录后的家目录，作为文件面板的初始位置
    pub home: String,
    /// 只用于主动断开。取走后为 None，重复 close 是安全的。
    handle: Mutex<Option<Handle<ProbeHandler>>>,
}

impl SftpConn {
    /// 断开底层 SSH 连接。
    ///
    /// 只断连接不删 `SftpSession` —— 调用方可能还持有 `Arc`，
    /// 后续操作会自然失败并报 `SFTP_IO_ERROR`，不需要在这里处理生命周期。
    pub async fn close(&self) {
        let handle = self
            .handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();

        if let Some(handle) = handle {
            let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        }
    }
}

pub struct OpenedSftp {
    pub session_id: String,
    pub home: String,
    /// 首次连接且用户刚确认过的主机密钥，调用方需要落库
    pub newly_trusted: Option<(String, String)>,
    pub elapsed_ms: u64,
}

/// 所有活着的 SFTP 连接，按前端给的 session id 索引。
#[derive(Default)]
pub struct SftpManager {
    conns: Mutex<HashMap<String, Arc<SftpConn>>>,
}

impl SftpManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, session_id: String, conn: Arc<SftpConn>) {
        self.conns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id, conn);
    }

    pub fn get(&self, session_id: &str) -> AppResult<Arc<SftpConn>> {
        self.conns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                AppError::with_detail(
                    ErrCode::SftpIoError,
                    format!("SFTP 会话不存在: {session_id}"),
                )
            })
    }

    pub fn remove(&self, session_id: &str) -> Option<Arc<SftpConn>> {
        self.conns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id)
    }

    /// 关掉所有连接，返回关闭数量。幂等。
    ///
    /// 先把表整个取空再逐个 close：`close()` 里有 await，
    /// 持锁跨 await 会让 future 不 Send（这就是 M3 踩过的那个坑）。
    pub async fn close_all(&self) -> usize {
        let conns: Vec<Arc<SftpConn>> = self
            .conns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .map(|(_, conn)| conn)
            .collect();

        let count = conns.len();
        for conn in conns {
            conn.close().await;
        }
        count
    }

    pub fn ids(&self) -> Vec<String> {
        self.conns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .cloned()
            .collect()
    }
}

/// 建立一条 SFTP 连接。
///
/// 用的是和终端会话同一套 `establish`（指纹校验、认证、算法档位全部共用），
/// 只有最后一步不同：开 `sftp` 子系统而不是 PTY。
pub async fn open(
    session_id: String,
    probe: ProbeRequest,
) -> AppResult<(OpenedSftp, Arc<SftpConn>)> {
    let established = establish(&probe, Purpose::Session)
        .await
        .map_err(failure_to_error)?;

    let newly_trusted = if established.host_key.decision == HostKeyDecision::AcceptedByUser {
        Some((
            established.host_key.algo.clone(),
            established.host_key.fingerprint.clone(),
        ))
    } else {
        None
    };

    let handle = established.handle;

    let channel = handle.channel_open_session().await.map_err(|err| {
        AppError::with_detail(ErrCode::SftpIoError, format!("打开 SFTP 通道失败: {err}"))
    })?;

    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|err| {
            AppError::with_detail(
                ErrCode::SftpIoError,
                format!("服务器不接受 SFTP 子系统: {err}"),
            )
        })?;

    let session = SftpSession::new(channel.into_stream())
        .await
        .map_err(|err| {
            AppError::with_detail(ErrCode::SftpIoError, format!("SFTP 初始化失败: {err}"))
        })?;

    // 家目录拿不到就退回根目录，不让整个会话因此失败
    let home = session
        .canonicalize(".")
        .await
        .unwrap_or_else(|_| "/".to_string());

    let conn = Arc::new(SftpConn {
        session: Arc::new(session),
        home: home.clone(),
        handle: Mutex::new(Some(handle)),
    });

    Ok((
        OpenedSftp {
            session_id,
            home,
            newly_trusted,
            elapsed_ms: established.elapsed_ms,
        },
        conn,
    ))
}

/// `establish` 的失败分支 → 用户可读错误。
fn failure_to_error(failure: ProbeResult) -> AppError {
    match failure {
        ProbeResult::Failed(err) => err,
        ProbeResult::NeedsConfirmation { fingerprint, .. } => AppError::with_detail(
            ErrCode::SshHostKeyUnknown,
            format!("需要确认主机指纹: {fingerprint}"),
        ),
        ProbeResult::HostKeyChanged { stored, .. } => AppError::with_detail(
            ErrCode::SshHostKeyMismatch,
            format!("记录中的指纹为 {stored}"),
        ),
        ProbeResult::CertificateUnsupported => AppError::new(ErrCode::SshHostKeyUnknown),
        // establish 成功时不会走到这里
        ProbeResult::Connected(_) => AppError::internal("establish 返回了已连接状态"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::sftp::list;
    use crate::sftp::ops;
    use crate::sftp::transfer::{self, ProgressFn, TransferProgress};
    use crate::ssh::probe;
    use crate::ssh::test_support::spawn_sftp;
    use crate::ssh::ProbeAuth;
    use tokio_util::sync::CancellationToken;

    /// 测试用临时目录，Drop 时清掉。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            // 唯一性由 test_support 统一保证，别再各处自己拼时间戳
            let path = crate::ssh::test_support::unique_path(&format!("local-{tag}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("无法创建本地临时目录");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn probe_req(port: u16) -> ProbeRequest {
        ProbeRequest {
            host: "127.0.0.1".to_string(),
            port,
            username: "tester".to_string(),
            auth: ProbeAuth::Password("pw".to_string()),
            legacy_algorithms: false,
            known_fingerprint: None,
            trust_fingerprint: None,
        }
    }

    /// 走完整流程建立 SFTP 连接：先探一次拿指纹（TOFU），再带指纹正式连。
    async fn connect(port: u16) -> Arc<SftpConn> {
        let fingerprint = match probe(probe_req(port)).await {
            ProbeResult::NeedsConfirmation { fingerprint, .. } => fingerprint,
            other => panic!("期望 NeedsConfirmation，实际 {other:?}"),
        };

        let mut request = probe_req(port);
        request.trust_fingerprint = Some(fingerprint);

        let (opened, conn) = open("test".to_string(), request)
            .await
            .expect("SFTP 会话应当能建立");
        assert_eq!(opened.home, "/");
        conn
    }

    fn collector() -> (Arc<Mutex<Vec<TransferProgress>>>, ProgressFn) {
        let store: Arc<Mutex<Vec<TransferProgress>>> = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&store);
        let emit: ProgressFn = Box::new(move |progress| {
            writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(progress);
        });
        (store, emit)
    }

    fn states(store: &Arc<Mutex<Vec<TransferProgress>>>) -> Vec<&'static str> {
        store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|item| item.state)
            .collect()
    }

    #[tokio::test]
    async fn lists_directories_before_files() {
        let (server, root) = spawn_sftp();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::create_dir(root.join("alpha")).unwrap();
        std::fs::write(root.join("b.txt"), b"hi").unwrap();
        std::fs::write(root.join("A.txt"), b"yo").unwrap();

        let conn = connect(server.port).await;
        let entries = list::read_dir(&conn.session, "/").await.unwrap();

        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha", "sub", "A.txt", "b.txt"],
            "目录优先，同级按名称不区分大小写排序"
        );

        let file = entries.iter().find(|entry| entry.name == "b.txt").unwrap();
        assert!(!file.is_dir);
        assert_eq!(file.size, 2);
        assert_eq!(file.path, "/b.txt");
        assert!(file.mode.is_some(), "应当带回权限位供前端显示");
    }

    #[tokio::test]
    async fn upload_writes_through_part_file_then_renames() {
        let (server, root) = spawn_sftp();
        let local = TempDir::new("upload");
        let payload: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(local.join("payload.bin"), &payload).unwrap();

        let conn = connect(server.port).await;
        let (events, emit) = collector();

        let written = transfer::upload(
            &conn.session,
            &local.join("payload.bin"),
            "/payload.bin",
            "t1".to_string(),
            CancellationToken::new(),
            emit,
            &[],
        )
        .await
        .expect("上传应当成功");

        assert_eq!(written, payload.len() as u64);
        assert_eq!(
            std::fs::read(root.join("payload.bin")).unwrap(),
            payload,
            "远端内容必须与本地逐字节一致（跨了 3 个 128KB 块）"
        );
        assert!(
            !root.join("payload.bin.part").exists(),
            "成功改名后不该留下 .part"
        );
        assert_eq!(states(&events).last(), Some(&"done"));
    }

    #[tokio::test]
    async fn download_reproduces_remote_content() {
        let (server, root) = spawn_sftp();
        let payload: Vec<u8> = (0..200 * 1024).map(|i| (i % 97) as u8).collect();
        std::fs::write(root.join("remote.bin"), &payload).unwrap();

        let local = TempDir::new("download");
        let target = local.join("remote.bin");

        let conn = connect(server.port).await;
        let (events, emit) = collector();

        let written = transfer::download(
            &conn.session,
            "/remote.bin",
            &target,
            "t2".to_string(),
            CancellationToken::new(),
            emit,
        )
        .await
        .expect("下载应当成功");

        assert_eq!(written, payload.len() as u64);
        assert_eq!(std::fs::read(&target).unwrap(), payload);
        assert!(!target.with_extension("bin.part").exists());
        assert_eq!(states(&events).last(), Some(&"done"));
    }

    #[tokio::test]
    async fn cancelled_upload_leaves_nothing_behind() {
        let (server, root) = spawn_sftp();
        let local = TempDir::new("cancel");
        std::fs::write(local.join("big.bin"), vec![1u8; 1024 * 1024]).unwrap();

        let conn = connect(server.port).await;
        let (events, emit) = collector();

        // 传输前就取消：验证第一个块边界就能退出，且清理了 .part
        let token = CancellationToken::new();
        token.cancel();

        let err = transfer::upload(
            &conn.session,
            &local.join("big.bin"),
            "/big.bin",
            "t3".to_string(),
            token,
            emit,
            &[],
        )
        .await
        .expect_err("取消后应当报错");

        assert_eq!(err.code, crate::error::ErrCode::SftpCancelled);
        assert!(!root.join("big.bin").exists(), "不该产生目标文件");
        assert!(!root.join("big.bin.part").exists(), "不该留下 .part");
        assert_eq!(states(&events).last(), Some(&"canceled"));
    }

    #[tokio::test]
    async fn upload_to_missing_directory_reports_failure() {
        let (server, _root) = spawn_sftp();
        let local = TempDir::new("missing");
        std::fs::write(local.join("a.txt"), b"x").unwrap();

        let conn = connect(server.port).await;
        let (events, emit) = collector();

        let err = transfer::upload(
            &conn.session,
            &local.join("a.txt"),
            "/nope/a.txt",
            "t4".to_string(),
            CancellationToken::new(),
            emit,
            &[],
        )
        .await
        .expect_err("目录不存在应当失败");

        assert_eq!(err.code, crate::error::ErrCode::SftpNotFound);
        // 失败也必须发一次终态事件，否则前端进度条永远转
        assert_eq!(states(&events).last(), Some(&"failed"));
    }

    #[tokio::test]
    async fn operations_match_filesystem_effects() {
        let (server, root) = spawn_sftp();
        let conn = connect(server.port).await;

        ops::create_dir(&conn.session, "/logs").await.unwrap();
        assert!(root.join("logs").is_dir());

        std::fs::write(root.join("logs/a.log"), b"x").unwrap();
        std::fs::create_dir(root.join("logs/nested")).unwrap();
        std::fs::write(root.join("logs/nested/b.log"), b"y").unwrap();

        // 非递归删非空目录必须失败，不能悄悄递归删掉
        let err = ops::remove(&conn.session, "/logs", false)
            .await
            .expect_err("非空目录不该被非递归删除");
        assert!(root.join("logs/nested/b.log").exists());
        assert_ne!(err.code, crate::error::ErrCode::SftpCancelled);

        ops::rename(&conn.session, "/logs/a.log", "/logs/renamed.log")
            .await
            .unwrap();
        assert!(!root.join("logs/a.log").exists());
        assert!(root.join("logs/renamed.log").exists());

        ops::remove(&conn.session, "/logs", true).await.unwrap();
        assert!(!root.join("logs").exists(), "递归删除应当清空整棵树");
    }

    #[tokio::test]
    async fn stat_tells_missing_paths_apart() {
        let (server, root) = spawn_sftp();
        std::fs::write(root.join("exists.txt"), b"123").unwrap();

        let conn = connect(server.port).await;

        let found = ops::stat(&conn.session, "/exists.txt").await.unwrap();
        assert_eq!(found, Some((false, 3)));

        let missing = ops::stat(&conn.session, "/nope.txt").await.unwrap();
        assert_eq!(missing, None, "不存在的路径应当返回 None 而不是报错");
    }

    #[tokio::test]
    async fn closing_a_session_is_idempotent() {
        let (server, _root) = spawn_sftp();
        let conn = connect(server.port).await;

        let manager = SftpManager::new();
        manager.insert("f1".to_string(), Arc::clone(&conn));

        assert!(manager.get("f1").is_ok());
        manager.remove("f1").unwrap().close().await;
        assert!(manager.get("f1").is_err(), "移除后不该还能取到会话");

        // 重复 close 不能 panic
        conn.close().await;
    }

    #[tokio::test]
    async fn close_all_clears_every_session() {
        // 场景：WebView 整页重载，前端 sessionId 全丢，后端连接还在。
        // 不清就是泄漏，反复重载会在服务器上堆一串没人用的 sshd 会话。
        let (server, _root) = spawn_sftp();
        let manager = SftpManager::new();
        let conn = connect(server.port).await;

        manager.insert("f1".to_string(), Arc::clone(&conn));
        manager.insert("f2".to_string(), Arc::clone(&conn));
        assert_eq!(manager.ids().len(), 2);

        assert_eq!(manager.close_all().await, 2);
        assert!(manager.ids().is_empty(), "清理后不该还有会话");

        // 幂等：没有会话时返回 0，不 panic
        assert_eq!(manager.close_all().await, 0);
    }

    #[tokio::test]
    async fn session_survives_a_round_trip_after_idle() {
        // 与终端会话同一条约定：SFTP 也不能因空闲被回收
        let (server, root) = spawn_sftp();
        std::fs::write(root.join("probe.txt"), b"ok").unwrap();

        let conn = connect(server.port).await;
        tokio::time::sleep(Duration::from_millis(1200)).await;

        let entries = list::read_dir(&conn.session, "/").await.unwrap();
        assert!(entries.iter().any(|entry| entry.name == "probe.txt"));
    }
}
