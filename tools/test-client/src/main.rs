use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, bail};
use tokio::time::sleep;
use tonic::transport::Endpoint;

mod proto {
    tonic::include_proto!("livestream");
}

use proto::livestream_client::LivestreamClient;
use proto::{
    GetLivestreamInfoRequest, ListLivestreamsRequest, StartLivestreamRequest, StopLivestreamRequest,
};

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let input_file: PathBuf = env::args()
        .nth(1)
        .context("用法: cargo run -p test-client -- <input.mp4>")?
        .into();
    if !input_file.exists() {
        bail!("文件不存在: {}", input_file.display());
    }

    // Check deps
    for cmd in &["ffmpeg"] {
        if Command::new("which").arg(cmd).output().is_err() {
            bail!("缺少 {cmd} — 请先安装");
        }
    }

    let grpc_host = env_or("GRPC_HOST", "http://127.0.0.1:50051");
    let rtmp_port = env_or("RTMP_PORT", "1935");
    let http_flv_enabled = env::var("HTTP_FLV_ENABLED").is_ok();
    let http_flv_port = env_or("HTTP_FLV_PORT", "8080");
    let stream_key = env_or("STREAM_KEY", "demo");
    let no_gui = env::var("NO_GUI").is_ok();

    // ── 1. Connect ──
    tracing::info!("连接 gRPC: {grpc_host}");
    let channel = Endpoint::from_shared(grpc_host.clone())?
        .connect()
        .await
        .context("gRPC 连接失败")?;
    let mut client = LivestreamClient::new(channel);

    // ── 2. StartLivestream ──
    tracing::info!("gRPC StartLivestream: {stream_key}");
    let resp = client
        .start_livestream(StartLivestreamRequest {
            live_id: stream_key.clone(),
            passphrase: None,
            input_protocol: 1, // RTMP
        })
        .await
        .context("StartLivestream 失败")?
        .into_inner();

    let rtmp_ep = resp
        .descriptor
        .as_ref()
        .and_then(|d| d.endpoints.as_ref())
        .and_then(|e| e.ingest.as_ref())
        .and_then(|i| i.rtmp.as_ref())
        .context("响应中缺少 RTMP endpoint")?;
    let rtmp_url = format!(
        "rtmp://localhost:{}/{}/{}",
        rtmp_ep.port, rtmp_ep.app_name, rtmp_ep.stream_key
    );
    tracing::info!("RTMP 推流地址: {rtmp_url}");

    // ── 3. ListLivestreams ──
    let list = client
        .list_livestreams(ListLivestreamsRequest {})
        .await
        .context("ListLivestreams 失败")?
        .into_inner();
    tracing::info!("活跃流: {} 个", list.streams.len());
    for s in &list.streams {
        tracing::info!("  {} ({})", s.live_id, s.status);
    }

    // ── 4. 推流 ──
    tracing::info!("启动推流...");
    let mut push_proc = Command::new("ffmpeg")
        .args(["-re", "-i"])
        .arg(&input_file)
        .args(["-c", "copy", "-f", "flv"])
        .arg(&rtmp_url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("ffmpeg 推流启动失败")?;

    sleep(Duration::from_secs(3)).await;

    // ── 5. 验证状态 ──
    let info = client
        .get_livestream_info(GetLivestreamInfoRequest {
            live_id: stream_key.clone(),
        })
        .await
        .context("GetLivestreamInfo 失败")?
        .into_inner();

    let desc = info.descriptor.context("响应缺少 descriptor")?;
    tracing::info!(
        "流状态: live_id={} status={} protocol={}",
        desc.live_id,
        desc.status,
        desc.input_protocol
    );

    if desc.status != 2 {
        // 2 = SESSION_STATUS_CONNECTED
        tracing::warn!("流状态不是 CONNECTED (got {})", desc.status);
    }

    // ── 6. 拉流 ──
    let pull_proc = if no_gui {
        let rtmp_pull = format!("rtmp://localhost:{rtmp_port}/lives/{stream_key}");
        tracing::info!("RTMP 拉流采样 (5s): {rtmp_pull}");
        let child = Command::new("timeout")
            .args(["5", "ffmpeg", "-i", &rtmp_pull, "-f", "null", "/dev/null"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("ffmpeg 拉流启动失败")?;
        Some(child)
    } else {
        let rtmp_pull = format!("rtmp://localhost:{rtmp_port}/lives/{stream_key}");
        tracing::info!("RTMP 拉流 (ffplay): {rtmp_pull}");
        match Command::new("ffplay")
            .arg(&rtmp_pull)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => Some(child),
            Err(e) => {
                tracing::warn!("ffplay 不可用 ({e}), 回退到 ffmpeg 采样");
                None
            }
        }
    };

    // HTTP-FLV pull
    let flv_proc = if http_flv_enabled {
        let flv_url = format!("http://localhost:{http_flv_port}/lives/{stream_key}.flv");
        if no_gui {
            tracing::info!("HTTP-FLV 拉流采样 (5s): {flv_url}");
            Command::new("timeout")
                .args(["5", "ffmpeg", "-i", &flv_url, "-f", "null", "/dev/null"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        } else {
            tracing::info!("HTTP-FLV 拉流 (ffplay): {flv_url}");
            Command::new("ffplay")
                .arg(&flv_url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        }
    } else {
        None
    };

    // ── 7. 等待用户 ──
    eprintln!("\n══════════════════════════════════════");
    eprintln!("  按 Enter 结束测试");
    eprintln!("══════════════════════════════════════\n");
    {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
    }

    // ── 8. 停止拉流 ──
    for mut proc in pull_proc.into_iter().chain(flv_proc.into_iter()) {
        tracing::info!("停止拉流进程...");
        let _ = proc.kill();
        let _ = proc.wait();
    }

    // ── 9. 停止推流 ──
    tracing::info!("停止推流...");
    let _ = push_proc.kill();
    let _ = push_proc.wait();

    // ── 10. StopLivestream ──
    tracing::info!("gRPC StopLivestream");
    match client
        .stop_livestream(StopLivestreamRequest {
            live_id: stream_key.clone(),
        })
        .await
    {
        Ok(resp) => {
            let inner = resp.into_inner();
            tracing::info!("StopLivestream 结果: is_success={}", inner.is_success);
        }
        Err(status) => {
            // 流可能已自行结束（视频播完、连接断开等），这是正常的
            tracing::warn!("StopLivestream 失败 (流可能已结束): {status}");
        }
    }

    tracing::info!("测试完成");
    Ok(())
}
