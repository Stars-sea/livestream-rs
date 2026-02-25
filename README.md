# livestream-rs

使用 Rust 重写的 SRT 拉流项目  
A project for SRT Streaming rewritten in Rust

<https://github.com/Stars-sea/Medicloud.Streaming>

## 功能特性 / Features

- 🚀 高性能 SRT 流拉取和转码 / High-performance SRT stream pulling and transcoding
- 📦 自动分段和上传到 MinIO / Automatic segmentation and upload to MinIO
- 🔌 gRPC API 接口 / gRPC API interface
- 🎯 智能端口分配 / Smart port allocation
- 📊 实时流状态监控 / Real-time stream status monitoring

## 系统要求 / Requirements

- Rust 1.70+
- FFmpeg libraries (libavcodec, libavformat, libavutil)
- pkg-config
- MinIO or S3-compatible storage

## 构建 / Building

### 本地构建 / Local Build

```bash
# Install dependencies (Ubuntu/Debian)
sudo apt-get install -y build-essential clang libclang-dev pkg-config \
    libssl-dev libavcodec-dev libavformat-dev libavutil-dev protobuf-compiler

# Build
cargo build --release
```

### Docker 构建 / Docker Build

```bash
docker build -t livestream-rs .
```

## 配置 / Configuration

### 环境变量 / Environment Variables

| 变量名 / Variable | 描述 / Description | 默认值 / Default |
|-------------------|-------------------|------------------|
| `HOST` | 服务主机名 / Service hostname | `srt.example.local` |
| `GRPC_PORT` | gRPC 服务端口 / gRPC server port | `50051` |
| `RTMP_PORT` | RTMP 服务端口 / RTMP server port | `1935` |
| `SRT_PORTS` | SRT 监听端口范围 / SRT listening port range | `4000-4100` |
| `MINIO_URI` | MinIO URI | `http://localhost:9000` |
| `MINIO_ACCESSKEY` | MinIO 访问密钥 / MinIO access key | `minioadmin` |
| `MINIO_SECRETKEY` | MinIO 密钥 / MinIO secret key | `miniokey` |
| `MINIO_BUCKET` | MinIO 存储桶 / MinIO bucket | `videos` |
| `RUST_LOG` | 日志级别 / Log level | `info` |
| `SEGMENT_TIME` | 分段时长 / Segment duration | `10` (sec) |

### settings.json

```json
{
  "host": "live.example.local",
  "srt_ports": "4000-4100",
  "grpc_callback": "",
  "cache_dir": "./cache",
  "segment_time": 10
}
```

- `host`: 服务主机名 / Service hostname
- `srt_ports`: SRT 监听端口范围 / SRT listening port range
- `grpc_callback`: gRPC 回调地址 / gRPC callback address
- `cache_dir`: 临时缓存目录 / Temporary cache directory
- `segment_time`: 分段时长（秒）/ Segment duration (seconds)

## 运行 / Running

```bash
# Set environment variables
export HOST=srt.example.local
export GRPC_PORT=50051
export RTMP_PORT=1935
export SRT_PORTS=4000-4100
export MINIO_URI=http://localhost:9000
export MINIO_ACCESSKEY=minioadmin
export MINIO_SECRETKEY=miniokey
export MINIO_BUCKET=videos
export RUST_LOG=info
export SEGMENT_TIME=10

# Run
./target/release/livestream-rs
```

## API 使用 / API Usage

服务提供以下 gRPC 接口 / The service provides the following gRPC interfaces:

- `StartPullStream`: 开始拉取 SRT 流 / Start pulling SRT stream
- `StopPullStream`: 停止拉取流 / Stop pulling stream
- `ListActiveStreams`: 列出活动流 / List active streams
- `GetStreamInfo`: 获取流信息 / Get stream information

## 许可证 / License

See LICENSE file for details.
