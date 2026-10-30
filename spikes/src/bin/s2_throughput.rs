//! S2：终端输出吞吐与背压。
//!
//! 要回答的问题：
//! 1. Rust 侧从 SSH 读到「解码 → 批量」这条链路能跑多快？
//! 2. 前端消费变慢时，链路会不会无界堆积内存？
//! 3. 终端输出用 `String` 走 JSON IPC，比 `Vec<u8>` 省多少？
//!
//! 服务端跑在**子进程**里，否则它的发送缓冲会污染本进程的 RSS 读数。

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use russh::client;
use russh::keys::{Algorithm, PublicKeyOrCertificate};
use russh::ChannelMsg;
use serde::Serialize;
use tokio::sync::mpsc;

use spikes::algorithms;
use spikes::batch::Batcher;
use spikes::rss::{fmt_bytes, RssSampler};
use spikes::utf8_stream::Utf8StreamDecoder;
use spikes::{free_port, mb};

/// 单次测量传输的总量
const TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// SSH 分包大小（贴近真实链路里 32KB 左右的分片）
const WIRE_CHUNK: usize = 32 * 1024;
/// 读取任务到处理任务之间的有界队列容量
const QUEUE_CAP: usize = 64;
/// 批处理阈值
const BATCH_MAX_BYTES: usize = 32 * 1024;
const BATCH_MAX_DELAY: Duration = Duration::from_millis(16);
/// 慢消费者每批的模拟处理耗时。
///
/// 注意别设成 2ms 这种小值：Windows 默认定时器精度约 15.6ms，
/// 小于一个 tick 的 sleep 会实打实睡满 15.6ms，测出来的数字就没有意义了。
const SLOW_CONSUMER_DELAY: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConsumerMode {
    /// 立即消费，测吞吐上限
    Fast,
    /// 每批 sleep，测背压是否生效
    Slow,
}

impl ConsumerMode {
    fn label(self) -> &'static str {
        match self {
            ConsumerMode::Fast => "快速消费者（不限速）",
            ConsumerMode::Slow => "慢速消费者（每批 20ms）",
        }
    }

    /// 慢模式总量小一点，否则要跑一分多钟。
    fn total_bytes(self) -> u64 {
        match self {
            ConsumerMode::Fast => TOTAL_BYTES,
            ConsumerMode::Slow => 16 * 1024 * 1024,
        }
    }
}

struct Probe;

impl client::Handler for Probe {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        Ok(true)
    }
}

#[derive(Serialize)]
struct BatchPayload<'a> {
    seq: u64,
    data: &'a str,
}

#[derive(Serialize)]
struct BatchPayloadBytes<'a> {
    seq: u64,
    data: &'a [u8],
}

#[derive(Default)]
struct Metrics {
    bytes: u64,
    chunks: u64,
    batches: u64,
    largest_batch: usize,
    invalid_utf8: u64,
    json_string_len: usize,
    json_bytes_len: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--serve") {
        let port: u16 = args[2].parse()?;
        let total: u64 = args[3].parse()?;
        return serve_mode(port, total).await;
    }

    println!("S2 · 终端输出吞吐与背压");
    println!(
        "总量 {:.0} MB / 分包 {} KB / 队列容量 {}",
        mb(TOTAL_BYTES),
        WIRE_CHUNK / 1024,
        QUEUE_CAP
    );
    println!();

    for mode in [ConsumerMode::Fast, ConsumerMode::Slow] {
        let result = run_case(mode).await?;
        print_case(mode, &result);
    }

    println!();
    println!("== 结论 ==");
    println!("· 见上方两组的 MB/s 对比：慢消费者模式的吞吐应被压到接近消费速率，");
    println!("  说明有界队列把压力原样传回了读取任务，也就是真正的背压。");
    println!("· RSS 峰值若远小于总量，说明内存没有随输出无界增长。");
    println!("· json_ratio 越大，越说明终端输出必须走 String 而不是 byte 数组。");

    Ok(())
}

async fn serve_mode(port: u16, total: u64) -> Result<()> {
    let (_port, _handle) = spikes::serve::spawn_server_at(
        port,
        algorithms::modern(),
        Algorithm::Ed25519,
        spikes::serve::StreamOptions {
            chunk: WIRE_CHUNK,
            pace: None,
            with_cjk: true,
        },
    )?;

    // 客户端推完就会断开；留足余量后自行退出（父进程也会 kill）
    tokio::time::sleep(Duration::from_secs(180)).await;
    println!("server {total} bytes done");
    Ok(())
}

fn spawn_child(port: u16, total: u64) -> Result<Child> {
    let exe = std::env::current_exe()?;
    let child = Command::new(exe)
        .args(["--serve", &port.to_string(), &total.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(child)
}

async fn wait_port(port: u16) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("服务端子进程没能在 10 秒内起来")
}

struct CaseResult {
    elapsed: Duration,
    metrics: Metrics,
    base_rss: u64,
    peak_rss: u64,
}

async fn run_case(mode: ConsumerMode) -> Result<CaseResult> {
    let port = free_port();
    let mut child = spawn_child(port, mode.total_bytes())?;

    let outcome = async {
        wait_port(port).await?;
        measure(port, mode).await
    }
    .await;

    let _ = child.kill();
    let _ = child.wait();
    outcome
}

async fn measure(port: u16, mode: ConsumerMode) -> Result<CaseResult> {
    let base_rss = RssSampler::new().sample();
    let peak = Arc::new(AtomicU64::new(base_rss));
    let peak_writer = peak.clone();
    let sampler = tokio::spawn(async move {
        let mut sampler = RssSampler::new();
        loop {
            tokio::time::sleep(Duration::from_millis(40)).await;
            peak_writer.fetch_max(sampler.sample(), Ordering::Relaxed);
        }
    });

    let config = Arc::new(client::Config {
        preferred: algorithms::client_default(),
        inactivity_timeout: Some(Duration::from_secs(60)),
        nodelay: true,
        ..Default::default()
    });
    let mut session = client::connect(config, ("127.0.0.1", port), Probe).await?;
    session.authenticate_password("tester", "tester").await?;

    let mut channel = session.channel_open_session().await?;
    channel
        .exec(true, format!("stream {}", mode.total_bytes()))
        .await?;

    let (tx, mut rx) = mpsc::channel::<Bytes>(QUEUE_CAP);
    let reader = tokio::spawn(async move {
        let mut received = 0u64;
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } => {
                    received += data.len() as u64;
                    // 队列满了这里会 await —— 这正是背压生效的位置
                    if tx.send(data).await.is_err() {
                        break;
                    }
                }
                ChannelMsg::Eof | ChannelMsg::Close => break,
                _ => {}
            }
        }
        received
    });

    let started = Instant::now();
    let mut metrics = Metrics::default();
    let mut decoder = Utf8StreamDecoder::new();
    let mut batcher = Batcher::new(BATCH_MAX_BYTES, BATCH_MAX_DELAY);

    while let Some(chunk) = rx.recv().await {
        metrics.bytes += chunk.len() as u64;
        metrics.chunks += 1;

        let text = decoder.push(&chunk);
        if let Some(batch) = batcher.push(&text) {
            metrics.batches += 1;
            probe_json_size(&batch, &mut metrics);
            if mode == ConsumerMode::Slow {
                tokio::time::sleep(SLOW_CONSUMER_DELAY).await;
            }
        }
    }

    let mut tail = decoder.push(&[]);
    tail.push_str(&decoder.finish());
    if !tail.is_empty() {
        if let Some(batch) = batcher.push(&tail) {
            metrics.batches += 1;
            probe_json_size(&batch, &mut metrics);
        }
    }
    if let Some(batch) = batcher.flush() {
        metrics.batches += 1;
        probe_json_size(&batch, &mut metrics);
    }

    let elapsed = started.elapsed();
    metrics.largest_batch = batcher.largest_batch;
    metrics.invalid_utf8 = decoder.invalid_bytes();

    let _ = reader.await;
    sampler.abort();

    Ok(CaseResult {
        elapsed,
        metrics,
        base_rss,
        peak_rss: peak.load(Ordering::Relaxed),
    })
}

/// 只取第一个够大的批次做一次序列化对比，避免影响吞吐测量。
fn probe_json_size(batch: &str, metrics: &mut Metrics) {
    if metrics.json_string_len > 0 || batch.len() < 4096 {
        return;
    }
    let as_string = serde_json::to_vec(&BatchPayload {
        seq: 0,
        data: batch,
    })
    .map(|v| v.len());
    let as_bytes = serde_json::to_vec(&BatchPayloadBytes {
        seq: 0,
        data: batch.as_bytes(),
    })
    .map(|v| v.len());
    if let (Ok(a), Ok(b)) = (as_string, as_bytes) {
        metrics.json_string_len = a;
        metrics.json_bytes_len = b;
    }
}

fn print_case(mode: ConsumerMode, r: &CaseResult) {
    let secs = r.elapsed.as_secs_f64();
    let throughput = mb(r.metrics.bytes) / secs;
    let avg_batch = if r.metrics.batches > 0 {
        r.metrics.bytes / r.metrics.batches
    } else {
        0
    };

    println!("── {} ──", mode.label());
    println!(
        "  传输 {:.1} MB，耗时 {:.2}s，吞吐 {:.1} MB/s",
        mb(r.metrics.bytes),
        secs,
        throughput
    );
    println!(
        "  分包 {} 个，聚合批次 {} 个，平均批 {:.1} KB，最大批 {:.1} KB",
        r.metrics.chunks,
        r.metrics.batches,
        avg_batch as f64 / 1024.0,
        r.metrics.largest_batch as f64 / 1024.0
    );
    println!(
        "  UTF-8 替换符 {} 个{}",
        r.metrics.invalid_utf8,
        if r.metrics.invalid_utf8 == 0 {
            "（中文未被分包切断）"
        } else {
            "（有乱码！）"
        }
    );
    println!(
        "  RSS 基线 {} → 峰值 {}（增长 {}）",
        fmt_bytes(r.base_rss),
        fmt_bytes(r.peak_rss),
        fmt_bytes(r.peak_rss.saturating_sub(r.base_rss))
    );
    if r.metrics.json_string_len > 0 {
        println!(
            "  IPC 载荷：String {} B vs Vec<u8> {} B，膨胀 {:.1}×",
            r.metrics.json_string_len,
            r.metrics.json_bytes_len,
            r.metrics.json_bytes_len as f64 / r.metrics.json_string_len as f64
        );
    }
    println!();
}
