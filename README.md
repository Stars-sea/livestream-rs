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

- Rust 1.85+（`edition = 2024`）
- FFmpeg 开发库：`libavcodec`、`libavformat`、`libavutil`
- `clang`、`libclang-dev`、`pkg-config`
- `protobuf-compiler`（用于 `tonic` 生成代码）
- MinIO 或任意 S3 兼容对象存储

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

| 变量名 | 说明 | 默认值 |
|---|---|---|
| `INGEST_HOST` | 返回给客户端的 SRT 主机地址 / host reported in stream info | `0.0.0.0` |
| `INGEST_GRPCPORT` | gRPC 监听端口 | `50051` |
| `INGEST_RTMPPORT` | ingest 侧 RTMP 拉流端口（用于 `INPUT_PROTOCOL_RTMP`） | `1936` |
| `INGEST_SRTPORTS` | SRT 端口范围（`start-end`） | `4000-4100` |
| `INGEST_DURATION` | 分段时长（秒） | `10` |
| `INGEST_CALLBACK` | 回调 gRPC 地址（可选） | `""` |
| `PUBLISH_PORT` | RTMP 监听端口 | `1935` |
| `PUBLISH_APPNAME` | RTMP 应用名（路径段） | `lives` |
| `MINIO_URI` | MinIO/S3 地址 | 必填 |
| `MINIO_ACCESSKEY` | MinIO/S3 Access Key | 必填 |
| `MINIO_SECRETKEY` | MinIO/S3 Secret Key | 必填 |
| `MINIO_BUCKET` | MinIO/S3 Bucket | 必填 |
| `RUST_LOG` | 日志过滤级别 | `info` |
| `OTEL_SERVICE_NAME` | OTEL 服务名 | `livestream-rs` |

> `minio` 配置缺失会导致程序启动失败。

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

定义见 `proto/livestream.proto`：

- `StartPullStream(live_id, passphrase, input_protocol)`
- `StopPullStream(live_id)`
- `ListActiveStreams()`
- `GetStreamInfo(live_id)`

`input_protocol`：

- `INPUT_PROTOCOL_SRT`（默认值）
- `INPUT_PROTOCOL_RTMP`

`passphrase` 规则：

- 当 `input_protocol=INPUT_PROTOCOL_SRT` 时必填，且限制为 10~79 位字母数字（`^[a-zA-Z0-9]{10,79}$`）。
- 当 `input_protocol=INPUT_PROTOCOL_RTMP` 时可为空。

请求示例（SRT）：

```json
{
  "live_id": "live_1001",
  "passphrase": "abc123def456",
  "input_protocol": "INPUT_PROTOCOL_SRT"
}
```

请求示例（RTMP）：

```json
{
  "live_id": "live_1001",
  "passphrase": "",
  "input_protocol": "INPUT_PROTOCOL_RTMP"
}
```

## 推拉流说明 / Stream flow

- 当 `input_protocol=INPUT_PROTOCOL_SRT` 时，调用 `StartPullStream` 后服务会分配一个 SRT 监听端口。
- SRT 端使用 `live_id` 作为 `streamid`，`passphrase` 为加密口令。
- 当 `input_protocol=INPUT_PROTOCOL_RTMP` 时，服务会从 `rtmp://<INGEST_HOST>:<INGEST_RTMPPORT>/<PUBLISH_APPNAME>/<live_id>` 拉流。
- RTMP 播放地址格式：`rtmp://<server>:<PUBLISH_PORT>/<PUBLISH_APPNAME>/<live_id>`。

> 注意：`INGEST_RTMPPORT` 与 `PUBLISH_PORT` 不能相同，程序启动时会校验并拒绝相同配置。

## 回调接口 / Callback API

当设置 `INGEST_CALLBACK`（例如 `http://127.0.0.1:50052`）时，会调用 `proto/livestream_callback.proto` 中的回调方法：

- `NotifyStreamStarted`
- `NotifyStreamStopped`
- `NotifyStreamRestarting`
- `NotifyPullerStarted`
- `NotifyPullerStopped`

## Telemetry

程序默认启用 OTLP（gRPC）导出器（日志/指标/追踪）。常见环境变量：

- `OTEL_EXPORTER_OTLP_ENDPOINT`（如 `http://localhost:4317`）
- `OTEL_EXPORTER_OTLP_PROTOCOL`（通常为 `grpc`）

## License

See [LICENSE](LICENSE).
