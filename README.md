# livestream-rs

使用 Rust 编写的 SRT 接入与 RTMP 分发服务。  
A Rust-based service for SRT ingest and RTMP distribution.

相关项目 / Related project: <https://github.com/Stars-sea/Medicloud.Streaming>

## 功能特性 / Features

- SRT Listener 接入（按需分配端口）/ SRT listener ingest with on-demand port allocation
- gRPC 控制平面（启动/停止/查询流）/ gRPC control plane (start/stop/query streams)
- TS 分段并上传 MinIO / TS segmentation and MinIO upload
- 内置统一 RTMP 服务器：ingest 接收 + egress 出流 / Built-in unified RTMP server: ingest receive + egress output
- 统一 server 运行时编排（gRPC + RTMP）/ Unified server runtime orchestration (gRPC + RTMP)
- OpenTelemetry 日志、指标、链路追踪 / OpenTelemetry logs, metrics, traces

## 运行时组件 / Runtime Components

- `SrtWorker`：负责 SRT ingest（FFmpeg 输入、分段、FLV/HLS 写出）
- `RtmpWorker`：负责 RTMP ingest（Publish 接收、FLV tag 封包、EOS 清理）
- `RtmpEgressHandler`：负责 RTMP egress（Play 输出）
- `AppServer` 统一创建 `stream message bus`（`stream_msg_tx/rx`），并注入 `StreamManager` 与 `RtmpServer`，避免由某个业务组件反向提供总线

## 系统要求 / Requirements

- Rust 1.85+ (`edition = 2024`)
- FFmpeg 开发库 / FFmpeg development libraries：`libavcodec`、`libavformat`、`libavutil`
- `clang`、`libclang-dev`、`pkg-config`
- `protobuf-compiler` (用于 `tonic` 生成代码 / used by `tonic` codegen)
- MinIO 或任意 S3 兼容对象存储 / MinIO or any S3-compatible object storage

## 构建 / Build

### 本地构建 / Local build

```bash
# Ubuntu / Debian
sudo apt-get install -y build-essential clang libclang-dev pkg-config \
  libssl-dev libavcodec-dev libavformat-dev libavutil-dev protobuf-compiler

cargo build --release
```

### Docker 构建 / Docker build

```bash
docker build -t livestream-rs .
```

## 配置 / Configuration

当前代码通过 `config.toml` + 环境变量加载配置（环境变量会覆盖文件配置）。  
Configuration is loaded from `config.toml` and environment variables (env overrides file).

### 配置结构 / Config schema

```toml
[ingest]
host = "0.0.0.0"
srtports = "4000-4100"
duration = 10

[grpc]
port = 50051
callback = ""

[egress]
port = 1935
appname = "lives"

[minio]
uri = "http://localhost:9000"
accesskey = "minioadmin"
secretkey = "miniokey"
bucket = "videos"
```

### 环境变量 / Environment variables

| 变量名 / Name | 说明 / Description | 默认值 / Default |
|---|---|---|
| `INGEST_HOST` | 返回给客户端的 SRT 主机地址 / host reported in stream info | `0.0.0.0` |
| `INGEST_SRTPORTS` | SRT 端口范围（`start-end`）/ SRT port range (`start-end`) | `4000-4100` |
| `INGEST_DURATION` | 分段时长（秒）/ segment duration (seconds) | `10` |
| `GRPC_PORT` | gRPC 监听端口 / gRPC listen port | `50051` |
| `GRPC_CALLBACK` | 回调 gRPC 地址（可选）/ callback gRPC address (optional) | `""` |
| `EGRESS_PORT` | 统一 RTMP 服务端口（ingest+egress）/ unified RTMP server port (ingest+egress) | `1935` |
| `EGRESS_APPNAME` | RTMP 应用名（ingest/egress 共用）/ RTMP app name shared by ingest+egress | `lives` |
| `MINIO_URI` | MinIO/S3 地址 / MinIO/S3 endpoint | 必填 / required |
| `MINIO_ACCESSKEY` | MinIO/S3 Access Key | 必填 / required |
| `MINIO_SECRETKEY` | MinIO/S3 Secret Key | 必填 / required |
| `MINIO_BUCKET` | MinIO/S3 Bucket | 必填 / required |
| `RUST_LOG` | 日志过滤级别 / log filter level | `info` |
| `OTEL_SERVICE_NAME` | OTEL 服务名 / OTEL service name | `livestream-rs` |

> `minio` 配置缺失会导致程序启动失败 / Missing `minio` config causes startup failure.

## 运行 / Run

```bash
export INGEST_HOST=0.0.0.0
export INGEST_SRTPORTS=4000-4100
export INGEST_DURATION=10
export GRPC_PORT=50051
export GRPC_CALLBACK=
export EGRESS_PORT=1935
export EGRESS_APPNAME=lives
export MINIO_URI=http://localhost:9000
export MINIO_ACCESSKEY=minioadmin
export MINIO_SECRETKEY=miniokey
export MINIO_BUCKET=videos
export RUST_LOG=info

./target/release/livestream-rs
```

## gRPC API

定义见 `proto/livestream.proto`  
API definitions are in `proto/livestream.proto`.

- `StartIngestStream(live_id, passphrase, input_protocol)`
- `StopIngestStream(live_id)`
- `ListIngestStreams()`
- `GetIngestStreamInfo(live_id)`

`input_protocol`:

- `INPUT_PROTOCOL_SRT`（默认值）
- `INPUT_PROTOCOL_RTMP`

`passphrase` 规则 / `passphrase` rules:

- 当 `input_protocol=INPUT_PROTOCOL_SRT` 时必填，且限制为 10~79 位字母数字（`^[a-zA-Z0-9]{10,79}$`）  
Required when `input_protocol=INPUT_PROTOCOL_SRT`, must be 10-79 alphanumeric chars (`^[a-zA-Z0-9]{10,79}$`).
- 当 `input_protocol=INPUT_PROTOCOL_RTMP` 时可为空  
Optional when `input_protocol=INPUT_PROTOCOL_RTMP`.

## 推拉流说明 / Stream flow

- 当 `input_protocol=INPUT_PROTOCOL_SRT` 时，调用 `StartIngestStream` 后服务会分配一个 SRT 监听端口  
When `input_protocol=INPUT_PROTOCOL_SRT`, the service allocates an SRT listen port after `StartIngestStream`
- SRT 端使用 `live_id` 作为 `srt_streamid`，`passphrase` 为加密口令   
SRT uses `live_id` as `srt_streamid` and `passphrase` as the encryption secret.
- 当 `input_protocol=INPUT_PROTOCOL_RTMP` 时，服务会注册一个 RTMP ingest 入口，地址为 `rtmp://<INGEST_HOST>:<EGRESS_PORT>/<EGRESS_APPNAME>/<live_id>`  
When `input_protocol=INPUT_PROTOCOL_RTMP`, the service registers an RTMP ingest endpoint at `rtmp://<INGEST_HOST>:<EGRESS_PORT>/<EGRESS_APPNAME>/<live_id>`
- RTMP egress（播放）地址格式：`rtmp://<server>:<EGRESS_PORT>/<EGRESS_APPNAME>/<live_id>`  
RTMP egress (playback) URL format: `rtmp://<server>:<EGRESS_PORT>/<EGRESS_APPNAME>/<live_id>`

### 网关 URL 拼接规则（示例）

- `input_protocol=INPUT_PROTOCOL_SRT`：
  - ingest：`srt://<host>:<port>?srt_streamid=<live_id>&passphrase=<passphrase>&&pbkeylen=32`
  - egress: `rtmp://<host>:<EGRESS_PORT>/<rtmp_appname>/<live_id>`
- `input_protocol=INPUT_PROTOCOL_RTMP`：
  - ingest & egress: `rtmp://<host>:<EGRESS_PORT>/<rtmp_appname>/<live_id>`

## 回调接口 / Callback API

当设置 `GRPC_CALLBACK`（例如 `http://127.0.0.1:50052`）时，会调用 `proto/livestream_callback.proto` 中的回调方法  
When `GRPC_CALLBACK` is set (for example `http://127.0.0.1:50052`), callback methods in `proto/livestream_callback.proto` are invoked.

- `NotifyStreamStarted`
- `NotifyStreamStopped`
- `NotifyStreamRestarting`
- `NotifyIngestWorkerStarted`
- `NotifyIngestWorkerStopped`

## Telemetry

程序默认启用 OTLP（gRPC）导出器（日志/指标/追踪）。常见环境变量：  
OTLP (gRPC) exporters (logs/metrics/traces) are enabled by default. Common environment variables:

- `OTEL_EXPORTER_OTLP_ENDPOINT`（如 `http://localhost:4317`）
- `OTEL_EXPORTER_OTLP_PROTOCOL`（通常为 `grpc`）

当前核心指标 / Core metrics:

- `livestream_rtmp_sessions`：当前 RTMP 会话数（连接维度）
- `livestream_ingest_streams`：当前 ingest 流数量（流维度）
- `livestream_egress_connections`：当前 egress 播放连接数（下行分发维度）

关键 span 前缀 / Key span namespaces:

- `ingest.manager.*`：控制面与流状态管理（创建/停止/查询）
- `ingest.srt_worker.*`：SRT ingest 工作循环与流事件
- `ingest.rtmp_worker.*`：RTMP ingest Publish/音视频处理/断连清理
- `server.rtmp.*`：RTMP 服务监听与 FLV 分发处理
- `server.grpc.*` 与 `ingest.grpc.*`：gRPC 服务端与 ingest RPC 生命周期

### 观测排障建议 / Troubleshooting checklist

- `livestream_rtmp_sessions` 持续升高但 `livestream_ingest_streams` 不变：优先排查异常连接或握手失败重试
- `livestream_ingest_streams` 高、`network_bytes_in_rate` 低：优先排查上游推流质量或输入中断
- `livestream_egress_connections` 高、`network_bytes_out_rate` 低：优先排查下游播放端拉流异常/积压
- `grpc_requests_failed` 上升：结合 `ingest.grpc.*` span 的错误字段排查参数与回调可用性

## License

See [LICENSE](LICENSE).
