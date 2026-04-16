# livestream-rs

Rust 实现的直播接入与分发服务，支持 SRT/RTMP ingest、统一 RTMP egress，以及 TS 分段上传到 MinIO/S3。  
A Rust live ingest/distribution service with SRT/RTMP ingest, unified RTMP egress, and TS segment persistence to MinIO/S3.

## 功能特性 / Features

- SRT ingest（动态端口分配）/ SRT ingest with dynamic port allocation
- RTMP ingest + RTMP egress（同一服务内统一处理）/ RTMP ingest + RTMP egress in one service
- gRPC 控制面（Start/Stop/List/Get/Watch）/ gRPC control plane (Start/Stop/List/Get/Watch)
- 基于中间件链的统一媒体处理管道 / Unified media processing pipeline via middleware chain
- 分段完成事件驱动上传 MinIO/S3 / Event-driven segment upload to MinIO/S3
- OpenTelemetry 指标与链路（按环境变量启用）/ OpenTelemetry metrics and tracing (env-enabled)

## 快速开始 / Quick Start

### 本地构建 / Local Build

```bash
# Ubuntu / Debian
sudo apt-get install -y build-essential clang libclang-dev pkg-config \
  libssl-dev libavcodec-dev libavformat-dev libavutil-dev protobuf-compiler

cargo build --release
```

### 运行示例 / Run Example

```bash
export SRT_PORTS=4000-4100
export PERSISTENCE_DURATION=10
export PERSISTENCE_CACHEDIR=
export GRPC_PORT=50051
export RTMP_PORT=1935
export RTMP_APPNAME=lives
export RTMP_SESSION_TTL_SECS=30
export MINIO_URI=http://localhost:9000
export MINIO_ACCESSKEY=minioadmin
export MINIO_SECRETKEY=miniokey
export MINIO_BUCKET=videos
export RUST_LOG=info

./target/release/livestream-rs
```

### Docker 构建 / Docker Build

```bash
docker build -t livestream-rs .
```

## 配置 / Configuration

配置来源：`config.toml` + 环境变量（环境变量覆盖文件）。  
Configuration source: `config.toml` + environment variables (env overrides file).

关键配置项 / Key settings:

- `srt.ports` / `SRT_PORTS`
- `persistence.duration` / `PERSISTENCE_DURATION`
- `persistence.cachedir` / `PERSISTENCE_CACHEDIR`
- `grpc.port` / `GRPC_PORT`
- `rtmp.port` / `RTMP_PORT`
- `rtmp.appname` / `RTMP_APPNAME`
- `rtmp.session_ttl_secs` / `RTMP_SESSION_TTL_SECS`
- `queue.*` / `QUEUE_*`
- `minio.uri/accesskey/secretkey/bucket` / `MINIO_*`

`rtmp.ttl` 默认值为 30 秒，允许范围为 0..=86400 秒。  
`rtmp.ttl` defaults to 30 seconds, with a valid range of 0..=86400 seconds.

最小必需项：`MINIO_*` 必填，否则启动失败。  
Minimum required: `MINIO_*` must be provided, otherwise startup fails.

## gRPC API

定义见 `proto/livestream.proto`。  
Definitions are in `proto/livestream.proto`.

- `StartLivestream`
- `StopLivestream`
- `ListLivestreams`
- `GetLivestreamInfo`
- `WatchLivestream`

## 架构速览 / Architecture Snapshot

`transport` 负责连接与会话，`pipeline` 负责媒体处理，二者通过事件和有界通道解耦协作。  
`transport` handles connections/sessions, `pipeline` handles media processing, and both collaborate via events and bounded channels.

```mermaid
flowchart LR
  grpc[gRPC Livestream Service]
  tc[TransportController]
  rtmp[RtmpServer]
  srt[SrtServer]
  registry[Global Session Registry]
  disp[EventDispatcher]
  bus[PipeBus]
  factory[UnifiedPipeFactory]
  pipe[Per-stream Pipe]
  mw1[OTelMiddleware]
  mw2[FlvMuxForwardMiddleware]
  mw3[SegmentMiddleware]
  egress[RTMP Play Broadcaster]
  persist[SegmentPersistenceHandler]
  minio[MinIO/S3]

  grpc --> tc
  tc --> rtmp
  tc --> srt

  rtmp --> registry
  srt --> registry

  rtmp --> disp
  srt --> disp
  disp --> bus
  bus --> factory
  factory --> pipe
  pipe --> mw1 --> mw2 --> mw3

  mw2 --> egress
  mw3 --> disp
  disp --> persist --> minio
```

## transport 与 pipeline 的协作主线 / Main Collaboration Flow

1. gRPC 调用进入 `TransportController`，下发控制命令到 RTMP/SRT server。  
gRPC calls enter `TransportController`, which dispatches control commands to RTMP/SRT servers.
2. transport 侧在会话初始化后发布 `SessionInit/SessionStarted` 事件。  
transport publishes `SessionInit/SessionStarted` events after session initialization.
3. `PipeBus` 监听事件并按 `live_id` 创建独立管道。  
`PipeBus` listens to events and creates isolated per-`live_id` pipelines.
4. 媒体包进入中间件链，完成计量、转发、分段与持久化触发。  
Media packets go through middleware chain for metering, forwarding, segmentation, and persistence triggering.

## 文档 / Documentation

- 关键组件与架构设计要点：`docs/transport-pipeline-architecture.md`  
Detailed component and architecture-principles guide: `docs/transport-pipeline-architecture.md`
- 架构演进待办：`docs/TODOs.md`  
Architecture evolution backlog: `docs/TODOs.md`
- FFmpeg unsafe 所有权：`docs/ffmpeg-unsafe-ownership-map.md`  
FFmpeg unsafe ownership map: `docs/ffmpeg-unsafe-ownership-map.md`

## License

See [LICENSE](LICENSE).
