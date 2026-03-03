# livestream-rs

使用 Rust 编写的 SRT 接入与 RTMP 分发服务。  
A Rust-based service for SRT ingest and RTMP distribution.

相关项目 / Related project: <https://github.com/Stars-sea/Medicloud.Streaming>

## 功能特性 / Features

- SRT Listener 接入（按需分配端口）/ SRT listener ingest with on-demand port allocation
- gRPC 控制平面（启动/停止/查询流）/ gRPC control plane (start/stop/query streams)
- TS 分段并上传 MinIO / TS segmentation and MinIO upload
- 内置 RTMP 服务器用于播放分发 / Built-in RTMP server for playback distribution
- OpenTelemetry 日志、指标、链路追踪 / OpenTelemetry logs, metrics, traces

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
grpcport = 50051
rtmpport = 1936
srtports = "4000-4100"
duration = 10
callback = ""

[publish]
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
| `INGEST_GRPCPORT` | gRPC 监听端口 / gRPC listen port | `50051` |
| `INGEST_RTMPPORT` | ingest 侧 RTMP 拉流端口（用于 `INPUT_PROTOCOL_RTMP`）<br> ingest-side RTMP pull port (for `INPUT_PROTOCOL_RTMP`) | `1936` |
| `INGEST_SRTPORTS` | SRT 端口范围（`start-end`）/ SRT port range (`start-end`) | `4000-4100` |
| `INGEST_DURATION` | 分段时长（秒）/ segment duration (seconds) | `10` |
| `INGEST_CALLBACK` | 回调 gRPC 地址（可选）/ callback gRPC address (optional) | `""` |
| `PUBLISH_PORT` | RTMP 监听端口 / RTMP listen port | `1935` |
| `PUBLISH_APPNAME` | RTMP 应用名（路径段）/ RTMP app name (path segment) | `lives` |
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
export INGEST_GRPCPORT=50051
export INGEST_RTMPPORT=1936
export INGEST_SRTPORTS=4000-4100
export INGEST_DURATION=10
export PUBLISH_PORT=1935
export PUBLISH_APPNAME=lives
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

- `StartPullStream(live_id, passphrase, input_protocol)`
- `StopPullStream(live_id)`
- `ListActiveStreams()`
- `GetStreamInfo(live_id)`

`input_protocol`:

- `INPUT_PROTOCOL_SRT`（默认值）
- `INPUT_PROTOCOL_RTMP`

`passphrase` 规则 / `passphrase` rules:

- 当 `input_protocol=INPUT_PROTOCOL_SRT` 时必填，且限制为 10~79 位字母数字（`^[a-zA-Z0-9]{10,79}$`）  
Required when `input_protocol=INPUT_PROTOCOL_SRT`, must be 10-79 alphanumeric chars (`^[a-zA-Z0-9]{10,79}$`).
- 当 `input_protocol=INPUT_PROTOCOL_RTMP` 时可为空  
Optional when `input_protocol=INPUT_PROTOCOL_RTMP`.

## 推拉流说明 / Stream flow

- 当 `input_protocol=INPUT_PROTOCOL_SRT` 时，调用 `StartPullStream` 后服务会分配一个 SRT 监听端口  
When `input_protocol=INPUT_PROTOCOL_SRT`, the service allocates an SRT listen port after `StartPullStream`
- SRT 端使用 `live_id` 作为 `srt_streamid`，`passphrase` 为加密口令   
SRT uses `live_id` as `srt_streamid` and `passphrase` as the encryption secret.
- 当 `input_protocol=INPUT_PROTOCOL_RTMP` 时，服务会从 `rtmp://<INGEST_HOST>:<INGEST_RTMPPORT>/<PUBLISH_APPNAME>/<live_id>` 拉流  
When `input_protocol=INPUT_PROTOCOL_RTMP`, the service pulls from `rtmp://<INGEST_HOST>:<INGEST_RTMPPORT>/<PUBLISH_APPNAME>/<live_id>`
- RTMP 播放地址格式：`rtmp://<server>:<PUBLISH_PORT>/<PUBLISH_APPNAME>/<live_id>`  
RTMP play URL format: `rtmp://<server>:<PUBLISH_PORT>/<PUBLISH_APPNAME>/<live_id>`

### 网关 URL 拼接规则（示例）

- `input_protocol=INPUT_PROTOCOL_SRT`：
  - ingest/push：`srt://<host>:<port>?srt_streamid=<live_id>&passphrase=<passphrase>&&pbkeylen=32`
- `input_protocol=INPUT_PROTOCOL_RTMP`：
  - ingest/push：`rtmp://<host>:<port>/<rtmp_appname>/<live_id>`

> **注意**：`INGEST_RTMPPORT` 与 `PUBLISH_PORT` 不能相同，程序启动时会校验并拒绝相同配置  
> **Note**: `INGEST_RTMPPORT` must differ from `PUBLISH_PORT`; startup validation rejects identical values.

## 回调接口 / Callback API

当设置 `INGEST_CALLBACK`（例如 `http://127.0.0.1:50052`）时，会调用 `proto/livestream_callback.proto` 中的回调方法  
When `INGEST_CALLBACK` is set (for example `http://127.0.0.1:50052`), callback methods in `proto/livestream_callback.proto` are invoked.

- `NotifyStreamStarted`
- `NotifyStreamStopped`
- `NotifyStreamRestarting`
- `NotifyPullerStarted`
- `NotifyPullerStopped`

## Telemetry

程序默认启用 OTLP（gRPC）导出器（日志/指标/追踪）。常见环境变量：  
OTLP (gRPC) exporters (logs/metrics/traces) are enabled by default. Common environment variables:

- `OTEL_EXPORTER_OTLP_ENDPOINT`（如 `http://localhost:4317`）
- `OTEL_EXPORTER_OTLP_PROTOCOL`（通常为 `grpc`）

## License

See [LICENSE](LICENSE).
