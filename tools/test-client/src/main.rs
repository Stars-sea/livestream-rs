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

macro_rules! run_test {
    ($cond:expr, $label:expr, $failed:ident, $test:expr) => {
        if $cond {
            if let Err(e) = $test.await {
                tracing::error!("{} 测试失败: {e}", $label);
                $failed = true;
            }
        } else {
            tracing::info!("=== {} 跳过 (port=0) ===", $label);
        }
    };
}

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

fn spawn_push(input_file: &PathBuf, format_args: &[&str], push_url: &str) -> anyhow::Result<Child> {
    Command::new("ffmpeg")
        .args(["-re", "-i"])
        .arg(input_file)
        .args(format_args)
        .arg(push_url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("ffmpeg 推流失败")
}

async fn verify_connected(
    client: &mut LivestreamClient<tonic::transport::Channel>,
    live_id: &str,
) -> anyhow::Result<()> {
    let desc = client
        .get_livestream_info(GetLivestreamInfoRequest {
            live_id: live_id.to_string(),
        })
        .await
        .context("GetLivestreamInfo 失败")?
        .into_inner()
        .descriptor
        .context("无 descriptor")?;
    tracing::info!("状态: live_id={} status={}", desc.live_id, desc.status);
    Ok(())
}

/// In auto mode: pull stream via ffmpeg for `duration`, verify video frames are received.
async fn pull_and_verify(url: &str, label: &str, duration: Duration) -> anyhow::Result<()> {
    let stderr_file = tempfile::NamedTempFile::new().context("创建临时日志文件失败")?;

    tracing::info!(
        "拉流验证 ({label}): {url} (duration={}s)",
        duration.as_secs()
    );

    let mut child = Command::new("ffmpeg")
        .args(["-i", url, "-f", "null", "/dev/null"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_file.as_file().try_clone()?)
        .spawn()
        .with_context(|| format!("ffmpeg 拉流 ({label}) 启动失败"))?;

    tokio::time::sleep(duration).await;

    let _ = child.kill();
    let _ = child.wait();

    let stderr = std::fs::read_to_string(stderr_file.path()).context("读取拉流日志失败")?;

    if stderr.contains("frame=") {
        tracing::info!("拉流验证 ({label}): 成功 (检测到视频帧)");
        Ok(())
    } else if stderr.contains("Connection refused") || stderr.contains("Connection reset") {
        bail!("拉流验证 ({label}) 失败: 连接被拒绝");
    } else {
        let preview: String = stderr.chars().take(500).collect();
        bail!("拉流验证 ({label}) 失败: 未检测到视频帧\nstderr 前 500 字符:\n{preview}");
    }
}

async fn stop_livestream(client: &mut LivestreamClient<tonic::transport::Channel>, live_id: &str) {
    let resp = client
        .stop_livestream(StopLivestreamRequest {
            live_id: live_id.to_string(),
        })
        .await;
    match resp {
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
    auto: bool,
    duration: Duration,
) -> anyhow::Result<()> {
    tracing::info!("=== RTMP 测试 (port={}) ===", ports.rtmp);
    let live_id = format!("{stream_key}-rtmp");

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

    let mut push = spawn_push(input_file, &["-c", "copy", "-f", "flv"], &push_url)?;
    sleep(Duration::from_secs(3)).await;
    verify_connected(client, &live_id).await?;

    if auto {
        let rtmp_url = format!("rtmp://localhost:{}/lives/{}", ports.rtmp, live_id);
        let flv_url = format!("http://localhost:{}/lives/{}.flv", ports.http_flv, live_id);

        let rtmp_fut = pull_and_verify(&rtmp_url, "rtmp_pull", duration);
        let flv_fut = async {
            if ports.http_flv > 0 {
                Some(pull_and_verify(&flv_url, "http_flv", duration).await)
            } else {
                None
            }
        };

        let (rtmp_res, flv_res) = tokio::join!(rtmp_fut, flv_fut);
        if let Err(e) = rtmp_res {
            tracing::warn!("RTMP 拉流验证警告: {e}");
        }
        if let Some(Err(e)) = flv_res {
            tracing::warn!("HTTP-FLV 拉流验证警告: {e}");
        }
    } else {
        let rtmp_pull_url = format!("rtmp://localhost:{}/lives/{}", ports.rtmp, live_id);
        let mut rtmp_pull_proc = pull_cmd(no_gui, &rtmp_pull_url);

        let mut flv_proc = (ports.http_flv > 0)
            .then(|| {
                let flv_url = format!("http://localhost:{}/lives/{}.flv", ports.http_flv, live_id);
                pull_cmd(no_gui, &flv_url)
            })
            .flatten();

        wait_enter().await;
        for p in rtmp_pull_proc.iter_mut().chain(flv_proc.iter_mut()) {
            kill_and_wait(p);
        }
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
    auto: bool,
    duration: Duration,
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

    let mut push = spawn_push(
        input_file,
        &["-c", "copy", "-f", "rtsp", "-rtsp_transport", "tcp"],
        &push_url,
    )?;
    sleep(Duration::from_secs(3)).await;
    verify_connected(client, &live_id).await?;

    // RTSP server is ingest-only; playback via RTMP.
    if auto {
        if ports.rtmp > 0 {
            let rtmp_url = format!("rtmp://localhost:{}/lives/{}", ports.rtmp, live_id);
            if let Err(e) = pull_and_verify(&rtmp_url, "rtmp_pull_from_rtsp", duration).await {
                tracing::warn!("RTMP 拉流验证警告 (RTSP ingest): {e}");
            }
        }
    } else {
        tracing::info!("RTSP 拉流跳过 (ingest-only, 通过 RTMP 拉流)");
        let mut rtmp_pull_proc = (ports.rtmp > 0)
            .then(|| {
                let url = format!("rtmp://localhost:{}/lives/{}", ports.rtmp, live_id);
                pull_cmd(no_gui, &url)
            })
            .flatten();
        wait_enter().await;
        for p in rtmp_pull_proc.iter_mut() {
            kill_and_wait(p);
        }
    }

    kill_and_wait(&mut push);
    stop_livestream(client, &live_id).await;
    Ok(())
}

fn parse_duration(arg: &str) -> u64 {
    arg.parse().unwrap_or_else(|e| {
        eprintln!("无效的 --duration 值 '{arg}': {e}, 使用默认值 10");
        10
    })
}

#[tokio::main]
#[allow(clippy::excessive_nesting)]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = env::args().skip(1).collect();

    // Parse --auto and --duration flags, collecting remaining positional args
    let mut auto = false;
    let mut duration_secs: u64 = 10;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--auto" => {
                auto = true;
                i += 1;
            }
            "--duration" => {
                i += 1;
                duration_secs = if i < args.len() {
                    parse_duration(&args[i])
                } else {
                    eprintln!("--duration 缺少参数, 使用默认值 10");
                    10
                };
                i += 1;
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }

    let input_file: PathBuf = if positional.is_empty() {
        eprintln!("用法: test-client [--auto] [--duration <secs>] <input.mp4>");
        std::process::exit(1);
    } else {
        positional[0].clone().into()
    };

    if !input_file.exists() {
        eprintln!("文件不存在: {}", input_file.display());
        std::process::exit(1);
    }

    for cmd in &["ffmpeg"] {
        if Command::new("which").arg(cmd).output().is_err() {
            eprintln!("缺少 {cmd}");
            std::process::exit(1);
        }
    }

    let stream_key = env_or("STREAM_KEY", "demo");
    let no_gui = env::var("NO_GUI").is_ok();
    let duration = Duration::from_secs(duration_secs);

    // Connect
    tracing::info!("连接 gRPC: {GRPC_ADDR}");
    let channel = match Endpoint::from_shared(GRPC_ADDR.to_string()) {
        Ok(ep) => match ep.connect().await {
            Ok(ch) => ch,
            Err(e) => {
                tracing::error!("gRPC 连接失败: {e}");
                std::process::exit(1);
            }
        },
        Err(e) => {
            tracing::error!("gRPC endpoint 无效: {e}");
            std::process::exit(1);
        }
    };
    let mut client = LivestreamClient::new(channel);

    // GetServiceInfo
    let svc = match client.get_service_info(GetServiceInfoRequest {}).await {
        Ok(resp) => resp.into_inner(),
        Err(e) => {
            tracing::error!("GetServiceInfo 失败: {e}");
            std::process::exit(1);
        }
    };
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
    match client.list_livestreams(ListLivestreamsRequest {}).await {
        Ok(resp) => {
            let list = resp.into_inner();
            tracing::info!("活跃流: {} 个", list.streams.len());
        }
        Err(e) => {
            tracing::warn!("ListLivestreams 失败: {e}");
        }
    }

    let mut failed = false;

    run_test!(
        ports.rtmp > 0,
        "RTMP",
        failed,
        test_rtmp(
            &mut client,
            &ports,
            &stream_key,
            &input_file,
            no_gui,
            auto,
            duration
        )
    );

    run_test!(
        ports.rtsp > 0,
        "RTSP",
        failed,
        test_rtsp(
            &mut client,
            &ports,
            &stream_key,
            &input_file,
            no_gui,
            auto,
            duration
        )
    );

    tracing::info!("测试完成");
    if failed {
        std::process::exit(1);
    }
}
