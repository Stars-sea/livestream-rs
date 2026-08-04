# Transport-Pipeline Architecture

本文档说明 transport 与 pipeline 子系统的职责划分、协作方式与关键设计决策。
Updated 2026-08-04 to match current code (ProtocolServerCore, transcode, deferred HLS).

## 1. 子系统职责 / Subsystem Responsibilities

**Transport (`livestream-transport`)**:
- 协议接入：RTMP ingest/playback（基于 rml_rtmp）、RTSP ingest（基于 rtsp-types）
- 共享服务端核心：`ProtocolServerCore`（accept 循环 / 控制消息 / 预创建 TTL / 连接数信号量 / JoinSet 任务跟踪），RTMP 与 RTSP 复用
- 会话管理：`SessionRegistry` 全局注册表（descriptor + CancellationToken）
- 控制面：`TransportController`（PrecreateStream / StopStream），经 mpsc 驱动各协议服务器
- FLV 分发：`FlvEgressHub` + `FlvLiveChannel`（每流广播通道 + sequence header 缓存 + demand 信号）
- HTTP-FLV 播放：`HttpFlvServer` 提供 `/lives/{live_id}.flv`，以及独立于播放功能的 `/alive`、`/health`、`/health/stream/{live_id}`
- gRPC 接口：`GrpcServer` 实现 `StartLivestream` / `StopLivestream` / `ListLivestreams` / `GetLivestreamInfo` / `WatchLivestream` / `GetServiceInfo`，可选 Bearer token 认证（`GRPC__AUTH_TOKEN`）
- 事件广播：`EventDispatcher` 发射 `SessionEvent`（Started / Init / Ended + EndReason）

**Pipeline (`livestream-pipeline`)**:
- 管道构建：`PipelineFactory`（自由函数 `build_pipeline` / `build_rtsp_pipeline` / `build_encoded_chain`），持有共享依赖（MinIO uploader、SegmentConfig）
- 管道执行：`PipelineImpl` 管理 spawned tasks + shutdown 排空（5s 超时，late-registered 任务中止兜底）
- Processor 链：`OTelProbe` → `SeqCacheProbe` → fan-out → `FlvMux` / `HlsSegmenter`（RTMP 与转码源为延迟初始化）
- 转码：`TranscodeProcessor`（RTSP MJPEG → H.264，服务端转码）
- Sink：`FlvSink`（广播到 FlvEgressHub）、`MinIoSink`（TS + playlist 上传 MinIO，有限重试）
- 路径安全：`sanitize::sanitize_stream_id` 在文件系统路径与对象键中使用前消毒 live_id

## 2. 协作链路 / Collaboration Chain

```
1. gRPC StartLivestream → GrpcServer → TransportController → ControlMessage::PrecreateStream
2. TransportController → RtmpServer/RtspServer（协议服务器经 ProtocolServerCore 处理）
3. 服务器注册会话 → HandlerLifecycle::pending() → SessionRegistry::register_session(Pending)
4. 注册结果经 oneshot ack 返回 gRPC 调用方（权威结果，无需轮询 registry）
5. 客户端连接 → RtmpServer.handle_connection / RtspServer.handle_connection
6. 连接建立 → lifecycle.connecting()/connect() → EventDispatcher → SessionEvent::SessionStarted
7. 连接处理器 → PipelineFactory::build_pipeline/build_rtsp_pipeline → PipelineImpl (spawn tasks)
8. Source 产包 → PadSender → PadReceiver → Processor 链 → Sink
9. FlvSink.consume → FlvBroadcast::broadcast → FlvEgressHub → HTTP-FLV / RTMP playback
10. HlsSegmenter.process → TsSegment（keyframe 对齐）→ PadSender → MinIoSink.consume → MinIO upload
11. 断连/超时 → HandlerLifecycle::disconnect_with_reason → SessionEvent::SessionEnded
12. StopLivestream → TransportController::close_session → StopStream → 会话取消 → PipelineImpl::shutdown
```

## 3. 关键设计 / Key Design Decisions

### 3.1 Pipeline 不依赖 Transport
`livestream-pipeline` 不导入 `livestream-transport`。Pipeline 通过 trait（`FlvBroadcast`、`ObjectUploader`）接收 transport 侧依赖，transport 在构造时注入具体实现（`FlvEgressHub`、`PersistenceClient`/`NullUploader`）。

### 3.2 Source 与 Pipeline 分离
`RtmpSource`/`RtspSource` 定义在 `livestream-transport` 中，实现 `Source` trait。RTMP 源在边界将 FLV 的 AVCC NAL 转成 Annex B（管道内部约定，与 RTP depacketizer 输出一致；`FlvMux` 输出时再转回 AVCC）。RTSP 的 depacketization 是独立的 `RtpDemuxProcessor`（FFmpeg RTP demuxer）。

### 3.3 事件驱动会话管理
`EventDispatcher` 是全局单例，提供 `subscribe_global()` 与按 live_id 的 `subscribe()`。`SessionEvent` 携带 `EndReason` enum（`ClientDisconnect` / `Error(String)` / `AdminStop` / `Timeout`）。`HandlerLifecycle` 用 AtomicBool 保证 disconnect 幂等（防双释放），`Drop` 兜底补发。

### 3.4 Processor 链与 Fan-out
主链：Source → OTelProbe → SeqCacheProbe → fan-out：
- Branch 1: FlvMux → FlvSink（FLV 直播分发，纯 Rust 编码，无 FFmpeg 依赖）
- Branch 2: HlsSegmenter → MinIoSink（HLS 持久化）

`SeqCacheProbe` 缓存 sequence header + 最近 keyframe；`FlvLiveChannel` 侧再缓存 FLV 形式的 seq tag，晚到订阅者拿到缓存即可起播。fan-out 的 pad 发送为尽力而为：`Full` 丢该输出副本，`Closed` 停止该输出，互不影响。

### 3.5 HLS 延迟初始化（deferred init）
RTMP 源的 codec 参数（SPS/PPS、ASC）随流内 sequence header 到达，晚于管道构建；MJPEG 转码流的 H.264 序列头由转码器合成，同样晚于构建。因此 HLS 分支由 `deferred_hls_init` 异步构建：
- 从 sequence-header 包收集每 codec 一份 `CodecParams`（视频 + 音频）
- 双 codec 齐备后构建；纯音频/纯视频流在首个媒体包到达且已有至少一份参数时启动
- 延迟构建的任务句柄通过共享 `Arc<Mutex<Vec<JoinHandle>>>` 注册进 `PipelineImpl`，shutdown 时一并排空/中止
- HLS 构建失败仅打 warning，FLV 路径不受影响

### 3.6 转码（MJPEG → H.264）
RTSP 源 ANNOUNCE 的 SDP 声明 MJPEG（RFC 2435）时，`build_rtsp_pipeline` 在 `RtpDemuxProcessor` 与编码链之间插入 `TranscodeProcessor`：
- 解码器在管道构建期（RECORD 前）即打开，配置错误对推流方立即可见
- 所有 FFmpeg 状态（decoder/encoder/scaler/frames）串行化在单个 `Mutex` 后
- 输出为 Annex B H.264，首帧合成 avcC 序列头；fps 按 `transcode.fps` 降采样，无 PTS 帧以 33ms 步进
- 解码器致命错误后自动重建并继续
- 转码流与 RTMP 源一样走延迟 HLS 初始化（参数来自转码器自身输出）

### 3.7 管道生命周期与 Shutdown
- `PipelineState`：Initializing → Running → Draining → Terminated（AtomicU8）
- `PipelineImpl::shutdown()`：cancel token → drain tasks（5s 总预算，逐任务剩余时间等待）→ 中止超时/迟到注册任务 → Terminated
- 每个 task 的 `run_processor`/`run_sink` 在 select! 中检查 cancel token，退出时调用 `Processor::close()`（HlsSegmenter 在此 flush 最终 segment 并写 `#EXT-X-ENDLIST` playlist）

### 3.8 错误处理分层
- Processor/Sink 错误：`tracing::warn!` + `metric_pipeline_error!`，丢弃当前包，继续循环
- Channel 关闭（PadSender disconnected）：停止向该 pad 输出，继续其他 pad
- Source 错误：cancel token → pipeline shutdown
- Transport 错误：`HandlerLifecycle` 管理会话状态转换（幂等断开 + EndReason）
- MinIO 上传瞬时失败：MinIoSink 内重试（200ms/500ms 退避，共 2 次），成功或最终失败后清理暂存文件

## 4. 组件清单 / Component Inventory

### Transport

| 组件 | 文件 | 职责 |
|------|------|------|
| `TransportController` | `transport/src/controller.rs` | 控制面命令（MPSC + oneshot ack） |
| `ProtocolServerCore` | `transport/src/protocol_server.rs` | RTMP/RTSP 共享 accept/控制/TTL 主循环 |
| `RtmpServer` | `transport/src/rtmp/server.rs` | RTMP ingest，预创建 TTL |
| `SessionGuard` | `transport/src/rtmp/session.rs` | RTMP 会话（rml_rtmp），chunk size 钳制 |
| `HandlerBuilder` / `PublishHandler` / `PlayHandler` | `transport/src/rtmp/handler/` | RTMP publish/play 分发 |
| `RtspServer` | `transport/src/rtsp/server.rs` | RTSP ingest |
| `RtspSession` | `transport/src/rtsp/session.rs` | ANNOUNCE/SETUP/RECORD/TEARDOWN 状态机 |
| `RtpInterleavedReader` | `transport/src/rtsp/rtp.rs` | $ + channel + length 帧解析；RECORD 后带内 RTSP 请求（TEARDOWN/OPTIONS）识别 |
| `SdpParser` | `transport/src/rtsp/sdp.rs` | SDP → CodecParams 提取 |
| `HttpFlvServer` | `transport/src/http_flv/server.rs` | HTTP-FLV + /alive + /health 端点 |
| `GrpcServer` | `transport/src/grpc/server.rs` | gRPC 控制面 + reflection + auth |
| `FlvEgressHub` | `transport/src/flv/hub.rs` | 多订阅者 FLV 广播（FlvBroadcast 实现） |
| `FlvLiveChannel` | `transport/src/flv/channel.rs` | 每流广播通道 + seq header 缓存 |
| `SessionRegistry` | `transport/src/registry/session.rs` | 全局会话注册表 |
| `EventDispatcher` | `transport/src/dispatcher/` | SessionEvent 广播 |
| `HandlerLifecycle` | `transport/src/lifecycle.rs` | 会话状态机（幂等断开） |
| `RtmpSource` / `RtspSource` | `transport/src/source/` | Source trait 实现，协议包 → 管道包 |

### Pipeline

| 组件 | 文件 | 职责 |
|------|------|------|
| `PipelineFactory` | `pipeline/src/factory.rs` | `build_pipeline` / `build_rtsp_pipeline` / `build_encoded_chain` / `null_uploader` |
| `PipelineImpl` | `pipeline/src/engine.rs` | 运行时管理 + shutdown 排空 |
| `OTelProbe` | `pipeline/src/processor/otel.rs` | 指标探针（passthrough） |
| `SeqCacheProbe` | `pipeline/src/processor/seq_cache.rs` | Seq header + keyframe 缓存 |
| `FlvMux` | `pipeline/src/processor/flv_mux.rs` | EncodedPacket → FlvTag（纯 Rust，demand 感知） |
| `HlsSegmenter` | `pipeline/src/processor/hls_segment.rs` | EncodedPacket → TsSegment（keyframe 对齐）+ playlist |
| `TranscodeProcessor` | `pipeline/src/processor/transcode.rs` | MJPEG → H.264 服务端转码 |
| `RtpDemuxProcessor` | `pipeline/src/processor/rtp_depack/` | RtpPacket → EncodedPacket |
| `FlvSink` | `pipeline/src/sink/flv.rs` | FLV 广播到 FlvEgressHub |
| `MinIoSink` | `pipeline/src/sink/minio.rs` | TS/playlist 上传 MinIO（重试 + 暂存清理） |
| `FlvBroadcast` trait | `pipeline/src/broadcast.rs` | transport 依赖注入接口 |
| `sanitize` | `pipeline/src/sanitize.rs` | stream_id 路径/键消毒 |

## 5. Crate 依赖 / Crate Dependencies

```
binary (livestream-rs)
  └── livestream-transport
        ├── livestream-pipeline
        │     ├── livestream-media
        │     │     ├── livestream-codec
        │     │     │     └── livestream-core
        │     │     └── livestream-core
        │     └── livestream-telemetry
        ├── livestream-media
        ├── livestream-telemetry
        └── livestream-codec
```

- `livestream-media` 是唯一直接持有 FFmpeg `unsafe` 访问的 crate（ffmpeg-sys-next）
- `livestream-pipeline` 不依赖 `livestream-transport`（通过 trait 注入）
- `livestream-telemetry` 仅依赖 `livestream-core`，被 pipeline/transport/binary 使用
- 无循环依赖，所有箭头最终指向 `core`
