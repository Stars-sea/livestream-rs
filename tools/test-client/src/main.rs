use std::env;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, bail};
use tokio::time::sleep;
use tonic::transport::Endpoint;

mod proto {
    tonic::include_proto!("livestream");
}

use proto::livestream_client::LivestreamClient;
use proto::{
    GetLivestreamInfoRequest, GetServiceInfoRequest, InputProtocol, ListLivestreamsRequest,
    StartLivestreamRequest, StopLivestreamRequest,
};

const GRPC_ADDR: &str = "http://127.0.0.1:50051";

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

struct ServicePorts {
    rtmp: u16,
    rtsp: u16,
    http_flv: u16,
}

fn pull_cmd(no_gui: bool, url: &str) -> Option<Child> {
    if no_gui {
        tracing::info!("拉流采样 (5s): {url}");
        Command::new("timeout")
            .args(["5", "ffmpeg", "-i", url, "-f", "null", "/dev/null"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .ok()
    } else {
        tracing::info!("拉流 (ffplay): {url}");
        match Command::new("ffplay")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!("ffplay 不可用 ({e}), 回退到 ffmpeg 采样");
                None
            }
        }
    }
}

async fn stop_livestream(client: &mut LivestreamClient<tonic::transport::Channel>, live_id: &str) {
    match client
        .stop_livestream(StopLivestreamRequest {
            live_id: live_id.to_string(),
        })
        .await
    {
        Ok(resp) => {
            tracing::info!(
                "StopLivestream({live_id}): is_success={}",
                resp.into_inner().is_success
            );
        }
        Err(s) => {
            tracing::warn!("StopLivestream({live_id}) 失败 (流可能已结束): {s}");
        }
    }
}

fn kill_and_wait(proc: &mut Child) {
    let _ = proc.kill();
    let _ = proc.wait();
}

async fn wait_enter() {
    eprintln!("\n══════════════════════════════════════");
    eprintln!("  按 Enter 继续");
    eprintln!("══════════════════════════════════════\n");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
}

async fn test_rtmp(
    client: &mut LivestreamClient<tonic::transport::Channel>,
    ports: &ServicePorts,
    stream_key: &str,
    input_file: &PathBuf,
    no_gui: bool,
) -> anyhow::Result<()> {
    tracing::info!("=== RTMP 测试 (port={}) ===", ports.rtmp);
    let live_id = format!("{stream_key}-rtmp");

    // StartLivestream
    let resp = client
        .start_livestream(StartLivestreamRequest {
            live_id: live_id.clone(),
            passphrase: None,
            input_protocol: InputProtocol::Rtmp as i32,
        })
        .await
        .context("StartLivestream RTMP 失败")?
        .into_inner();

    let rtmp_ep = resp
        .descriptor
        .and_then(|d| d.endpoints)
        .and_then(|e| e.ingest)
        .and_then(|i| i.rtmp)
        .context("响应缺少 RTMP ingest endpoint")?;

    let push_url = format!(
        "rtmp://localhost:{}/{}/{}",
        rtmp_ep.port, rtmp_ep.app_name, rtmp_ep.stream_key
    );
    tracing::info!("RTMP 推流: {push_url}");

    // Push
    let mut push = Command::new("ffmpeg")
        .args(["-re", "-i"])
        .arg(input_file)
        .args(["-c", "copy", "-f", "flv"])
        .arg(&push_url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("ffmpeg RTMP 推流失败")?;
    sleep(Duration::from_secs(3)).await;

    // Verify
    let info = client
        .get_livestream_info(GetLivestreamInfoRequest {
            live_id: live_id.clone(),
        })
        .await
        .context("GetLivestreamInfo 失败")?
        .into_inner();
    let desc = info.descriptor.context("无 descriptor")?;
    tracing::info!("状态: live_id={} status={}", desc.live_id, desc.status);

    // RTMP pull
    let rtmp_pull = format!("rtmp://localhost:{}/lives/{}", ports.rtmp, live_id);
    let mut rtmp_pull_proc = pull_cmd(no_gui, &rtmp_pull);

    // HTTP-FLV pull
    let mut flv_proc = if ports.http_flv > 0 {
        let flv_url = format!("http://localhost:{}/lives/{}.flv", ports.http_flv, live_id);
        pull_cmd(no_gui, &flv_url)
    } else {
        None
    };

    wait_enter().await;

    for p in rtmp_pull_proc.iter_mut().chain(flv_proc.iter_mut()) {
        kill_and_wait(p);
    }
    kill_and_wait(&mut push);
    stop_livestream(client, &live_id).await;
    Ok(())
}

async fn test_rtsp(
    client: &mut LivestreamClient<tonic::transport::Channel>,
    ports: &ServicePorts,
    stream_key: &str,
    input_file: &PathBuf,
    no_gui: bool,
) -> anyhow::Result<()> {
    tracing::info!("=== RTSP 测试 (port={}) ===", ports.rtsp);
    let live_id = format!("{stream_key}-rtsp");

    let resp = client
        .start_livestream(StartLivestreamRequest {
            live_id: live_id.clone(),
            passphrase: None,
            input_protocol: InputProtocol::Rtsp as i32,
        })
        .await
        .context("StartLivestream RTSP 失败")?
        .into_inner();

    let rtsp_ep = resp
        .descriptor
        .and_then(|d| d.endpoints)
        .and_then(|e| e.ingest)
        .and_then(|i| i.rtsp)
        .context("响应缺少 RTSP ingest endpoint")?;

    let path = rtsp_ep.path.trim_start_matches('/');
    let push_url = format!("rtsp://localhost:{}/{path}", rtsp_ep.port);
    tracing::info!("RTSP 推流: {push_url}");

    // RTSP push: ffmpeg -re -i input -c copy -f rtsp -rtsp_transport tcp url
    let mut push = Command::new("ffmpeg")
        .args(["-re", "-i"])
        .arg(input_file)
        .args(["-c", "copy", "-f", "rtsp", "-rtsp_transport", "tcp"])
        .arg(&push_url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("ffmpeg RTSP 推流失败")?;
    sleep(Duration::from_secs(3)).await;

    // Verify
    let info = client
        .get_livestream_info(GetLivestreamInfoRequest {
            live_id: live_id.clone(),
        })
        .await
        .context("GetLivestreamInfo 失败")?
        .into_inner();
    let desc = info.descriptor.context("无 descriptor")?;
    tracing::info!("状态: live_id={} status={}", desc.live_id, desc.status);

    // RTSP server is ingest-only; playback via RTMP.
    tracing::info!("RTSP 拉流跳过 (ingest-only, 通过 RTMP 拉流)");

    let mut rtmp_pull_proc = if ports.rtmp > 0 {
        let rtmp_pull = format!("rtmp://localhost:{}/lives/{}", ports.rtmp, live_id);
        pull_cmd(no_gui, &rtmp_pull)
    } else {
        None
    };

    wait_enter().await;

    for p in rtmp_pull_proc.iter_mut() {
        kill_and_wait(p);
    }
    kill_and_wait(&mut push);
    stop_livestream(client, &live_id).await;
    Ok(())
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

    for cmd in &["ffmpeg"] {
        if Command::new("which").arg(cmd).output().is_err() {
            bail!("缺少 {cmd}");
        }
    }

    let stream_key = env_or("STREAM_KEY", "demo");
    let no_gui = env::var("NO_GUI").is_ok();

    // Connect
    tracing::info!("连接 gRPC: {GRPC_ADDR}");
    let channel = Endpoint::from_shared(GRPC_ADDR.to_string())?
        .connect()
        .await
        .context("gRPC 连接失败")?;
    let mut client = LivestreamClient::new(channel);

    // GetServiceInfo
    let svc = client
        .get_service_info(GetServiceInfoRequest {})
        .await
        .context("GetServiceInfo 失败")?
        .into_inner();
    let ports = ServicePorts {
        rtmp: svc.rtmp_port as u16,
        rtsp: svc.rtsp_port as u16,
        http_flv: svc.http_flv_port as u16,
    };
    tracing::info!(
        "GetServiceInfo: rtmp={} rtsp={} http_flv={}",
        ports.rtmp,
        ports.rtsp,
        ports.http_flv
    );

    // List
    let list = client
        .list_livestreams(ListLivestreamsRequest {})
        .await
        .context("ListLivestreams 失败")?
        .into_inner();
    tracing::info!("活跃流: {} 个", list.streams.len());

    // RTMP
    if ports.rtmp > 0 {
        if let Err(e) = test_rtmp(&mut client, &ports, &stream_key, &input_file, no_gui).await {
            tracing::error!("RTMP 测试失败: {e}");
        }
    } else {
        tracing::info!("=== RTMP 跳过 (port=0) ===");
    }

    // RTSP
    if ports.rtsp > 0 {
        if let Err(e) = test_rtsp(&mut client, &ports, &stream_key, &input_file, no_gui).await {
            tracing::error!("RTSP 测试失败: {e}");
        }
    } else {
        tracing::info!("=== RTSP 跳过 (port=0) ===");
    }

    tracing::info!("测试完成");
    Ok(())
}
