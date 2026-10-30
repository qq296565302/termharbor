//! S1：russh 算法兼容性矩阵。
//!
//! 要回答的问题：
//! 1. russh 默认配置能不能连上"只有老算法"的服务器？
//! 2. 如果连不上，改 `Preferred` 能不能救回来？
//!
//! 服务端用 russh 自建（本机没有 Docker，也连不到真实老机器），
//! 但算法集是按 CentOS 6/7 的真实默认值手工限定的。

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use russh::client;
use russh::keys::{Algorithm, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect, Preferred};

use spikes::algorithms;
use spikes::serve::{spawn_server, StreamOptions};

struct Probe;

impl client::Handler for Probe {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        // Spike 里不校验主机密钥（指纹校验是 M1 的活，这里只测算法）
        Ok(true)
    }
}

/// 完整走一遍：TCP → 握手 → 密码认证 → 开通道 → exec → 收 exit status。
async fn attempt(port: u16, preferred: Preferred) -> std::result::Result<Duration, String> {
    let started = Instant::now();

    let config = Arc::new(client::Config {
        preferred,
        inactivity_timeout: Some(Duration::from_secs(10)),
        ..Default::default()
    });

    let mut session = client::connect(config, ("127.0.0.1", port), Probe)
        .await
        .map_err(|e| format!("握手失败: {e}"))?;

    let auth = session
        .authenticate_password("tester", "tester")
        .await
        .map_err(|e| format!("认证出错: {e}"))?;
    if !auth.success() {
        return Err("认证被拒绝".to_string());
    }

    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("开通道失败: {e}"))?;
    channel
        .exec(true, "stream 0")
        .await
        .map_err(|e| format!("exec 失败: {e}"))?;

    let mut got_exit = false;
    while let Some(msg) = channel.wait().await {
        match msg {
            // 注意：不能在这里收到 Eof 就 break —— 服务端是 eof → exit_status → close 的顺序，
            // 提前退出会漏掉退出状态，把"其实成功"误判成失败。
            ChannelMsg::ExitStatus { .. } => {
                got_exit = true;
                break;
            }
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    let _ = session
        .disconnect(Disconnect::ByApplication, "", "en")
        .await;

    if !got_exit {
        return Err("通道已建立但没收到退出状态".to_string());
    }
    Ok(started.elapsed())
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("S1 · russh 算法兼容性矩阵");
    println!("russh 版本: {}", env!("CARGO_PKG_VERSION"));
    println!();

    println!("客户端「默认」档位声明的算法：");
    println!("  {}", algorithms::describe(&algorithms::client_default()));
    println!();
    println!("客户端「放宽」档位声明的算法：");
    println!("  {}", algorithms::describe(&algorithms::client_relaxed()));
    println!();
    println!("russh 已实现但默认不启用的算法：");
    for item in algorithms::IMPLEMENTED_BUT_NOT_DEFAULT {
        println!("  · {item}");
    }
    println!();

    let servers: Vec<(&str, Preferred, Algorithm)> = vec![
        ("现代服务器", algorithms::modern(), Algorithm::Ed25519),
        (
            "全老算法服务器",
            algorithms::legacy(),
            Algorithm::Rsa { hash: None },
        ),
        (
            "只给 ssh-rsa 的服务器",
            algorithms::rsa_only(),
            Algorithm::Rsa { hash: None },
        ),
    ];

    let clients: Vec<(&str, Preferred)> = vec![
        ("默认", algorithms::client_default()),
        ("放宽", algorithms::client_relaxed()),
    ];

    println!("{:<26} {:<10} {}", "服务端", "客户端", "结果");
    println!("{}", "-".repeat(78));

    let mut rows: Vec<(String, String, bool, String)> = Vec::new();

    for (server_name, server_pref, host_alg) in servers {
        let (port, handle) = spawn_server(server_pref, host_alg, StreamOptions::default())?;

        for (client_name, client_pref) in &clients {
            let outcome = attempt(port, client_pref.clone()).await;
            let (ok, detail) = match outcome {
                Ok(elapsed) => (true, format!("通过（{:.0}ms）", elapsed.as_millis() as f64)),
                Err(e) => (false, format!("失败：{}", truncate(&e, 44))),
            };
            println!("{server_name:<26} {client_name:<10} {detail}");

            rows.push((server_name.to_string(), client_name.to_string(), ok, detail));
        }

        handle.abort();
    }

    println!();
    println!("== 结论 ==");

    let default_vs_legacy = rows
        .iter()
        .find(|(s, c, _, _)| s == "全老算法服务器" && c == "默认");
    let relaxed_vs_legacy = rows
        .iter()
        .find(|(s, c, _, _)| s == "全老算法服务器" && c == "放宽");

    match (default_vs_legacy, relaxed_vs_legacy) {
        (Some((_, _, false, why)), Some((_, _, true, _))) => {
            println!("· 默认配置连全老算法服务器：失败 —— {why}");
            println!("· 放宽 Preferred 后：成功。");
            println!("=> 结论：russh 具备老算法能力，只是默认策略保守。");
            println!("   产品侧需要提供「放宽算法兼容模式」开关，而不是换 ssh2 crate。");
        }
        (Some((_, _, true, _)), _) => {
            println!("· 默认配置连全老算法服务器：直接通过，兼容性风险低于预期。");
        }
        _ => {
            println!("· 放宽配置仍连不上，需要评估 ssh2 crate 或放弃老服务器支持。");
        }
    }

    let rsa_row = rows
        .iter()
        .find(|(s, c, _, _)| s == "只给 ssh-rsa 的服务器" && c == "默认");
    if let Some((_, _, ok, _)) = rsa_row {
        println!(
            "· 只给 ssh-rsa 主机密钥的服务器：默认配置{}。",
            if *ok {
                "可以通过（ssh-rsa 本就在默认 key 列表里）"
            } else {
                "通不过"
            }
        );
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}
