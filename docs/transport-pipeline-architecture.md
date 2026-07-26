# Transport-Pipeline Architecture

本文档说明 transport 与 pipeline 子系统的职责划分、协作方式与关键设计决策。
Updated 2026-07-26 for v0.5 architecture (post Spec 02-10 alignment).

## 1. 子系统职责 / Subsystem Responsibilities

**Transport (`livestream-transport`)**:
- 协议接入：RTMP ingest（基于 rml_rtmp）、RTSP ingest（基于 rtsp-types）
- 会话管理：`SessionRegistry` 全局注册表（descriptor + CancellationToken + PipelineHandle）
- 控制面：`TransportController`（PrecreateStream / StopStream），驱动 RtmpServer/RtspServer
- FLV 分发：`FlvEgressHub` 向多订阅者广播 FLV tag
- HTTP-FLV 播放：`HttpFlvServer` 提供 `/lives/{live_id}.flv`
- gRPC 接口：`GrpcServer` 实现 `StartLivestream` / `StopLivestream` / `ListLivestreams` / `GetLivestreamInfo` / `WatchLivestream`
- 事件广播：`EventDispatcher` 发射 `SessionEvent`（Started / Init / Ended + EndReason）
- 生命周期：`TransportServer` 聚合所有协议 server 的统一入口

**Pipeline (`livestream-pipeline`)**:
- 管道构建：`PipelineFactory` 持有共享依赖（MinIO、SegmentConfig、FlvBroadcast）
- 管道执行：`PipelineImpl` 管理 spawned tasks + shutdown 排空
- Processor 链：`OTelProbe` → `SeqCacheProbe` → fan-out → `FlvMux` / `HlsSegmenter`
- Sink：`FlvSink`（广播到 FlvEgressHub）、`MinIoSink`（上传 TS 分段到 MinIO）
- 图验证：`PipelineGraph::validate()` 在构建期检查管道完整性
- 指标：`metric_pipeline_error!` 在 processor/sink 错误路径中 emit 计数器

## 2. 协作链路 / Collaboration Chain

```
1. gRPC StartLivestream → GrpcServer → TransportController → ControlMessage
2. TransportController → RtmpServer/RtspServer → 预创建会话 (SessionRegistry)
3. 客户端连接 → RtmpServer.handle_connection / RtspServer.handle_connection
4. 连接建立 → HandlerLifecycle → EventDispatcher → SessionEvent::SessionStarted
5. PipelineFactory.instantiate → PipelineImpl (spawn processor/sink tasks)
6. Source 产包 → PadSender → PadReceiver → Processor 链 → Sink
7. FlvSink.consume → FlvBroadcast::broadcast → FlvEgressHub → HTTP-FLV/RTMP playback
8. HlsSegmenter.process → TsSegment → PadSender → MinIoSink.consume → MinIO upload
9. 断连/超时 → HandlerLifecycle.disconnect_with_reason → SessionEvent::SessionEnded
10. StopLivestream → TransportController → cancel pipeline → PipelineImpl::shutdown
```

## 3. 关键设计 / Key Design Decisions

### 3.1 Pipeline 不依赖 Transport
`livestream-pipeline` 不导入 `livestream-transport`。Pipeline 通过 trait（`FlvBroadcast`、`ObjectUploader`）接收 transport 侧依赖。Transport 在构造时注入具体实现。

### 3.2 Source 与 Pipeline 分离
`RtmpSource`/`RtspSource` 定义在 `livestream-transport` 中，实现 `Source` trait。Pipeline 通过 `PadReceiver<T>` 接收数据，不关心协议层细节。RTSP 的 depacketization 拆分为独立的 `RtpDemuxProcessor`（使用 FFmpeg RTP demuxer）。

### 3.3 事件驱动会话管理
`EventDispatcher` 是单例（`dispatcher::INSTANCE`），任何组件可订阅 `SessionEvent`。`SessionEvent` 携带 `EndReason` enum（`ClientDisconnect` / `Error(String)` / `AdminStop` / `Timeout`），用于区分终止原因。

### 3.4 Processor 链与 Fan-out
Pipeline 是线性 graph + branch 分支。主链：Source → OTelProbe → SeqCacheProbe → fan-out：
- Branch 1: FlvMux → FlvSink（FLV 直播分发）
- Branch 2: HlsSegmenter → MinIoSink（HLS 持久化）

`SeqCacheProbe` 缓存最近的 sequence header + keyframe，用于 HTTP-FLV 新订阅者快速启动。

### 3.5 管道生命周期与 Shutdown
- `PipelineState`：Initializing → Running → Draining → Terminated（AtomicU8）
- `PipelineImpl::shutdown()`：cancel token → drain tasks（with 5s timeout）→ set Terminated
- 每个 task 的 `run_processor`/`run_sink` 在 select! 中检查 cancel token，退出时自动调用 `Processor::close()`（HlsSegmenter 在此 flush final segment）

### 3.6 错误处理分层
- Processor/Sink 错误：`tracing::warn!` + `metric_pipeline_error!`，丢弃当前包，继续循环
- Channel 关闭（PadSender disconnected）：停止向该 pad 输出，继续其他 pad
- Source 错误：cancel token → pipeline shutdown
- Transport 错误：`HandlerLifecycle` 管理会话状态转换

## 4. 组件清单 / Component Inventory

### Transport

| 组件 | 文件 | 职责 |
|------|------|------|
| `TransportServer` | `transport/src/server.rs` | 聚合 RTMP/RTSP/HTTP-FLV 生命周期 |
| `TransportController` | `transport/src/controller.rs` | 控制面命令（MPSC） |
| `RtmpServer` | `transport/src/rtmp/server.rs` | RTMP ingest，预创建 TTL |
| `RtspServer` | `transport/src/rtsp/server.rs` | RTSP ingest（rtsp-types） |
| `RtspSession` | `transport/src/rtsp/session.rs` | ANNOUNCE/SETUP/RECORD/TEARDOWN |
| `RtpInterleavedReader` | `transport/src/rtsp/rtp.rs` | $ + channel + length 帧解析 |
| `SdpParser` | `transport/src/rtsp/sdp.rs` | SDP → CodecParams 提取 |
| `HttpFlvServer` | `transport/src/http_flv/server.rs` | HTTP-FLV + health endpoints |
| `GrpcServer` | `transport/src/grpc/server.rs` | gRPC 控制面 |
| `FlvEgressHub` | `transport/src/flv/hub.rs` | 多订阅者 FLV 广播 |
| `SessionRegistry` | `transport/src/registry/session.rs` | 全局会话注册表 |
| `EventDispatcher` | `transport/src/dispatcher/` | SessionEvent 广播 |
| `HandlerLifecycle` | `transport/src/lifecycle.rs` | 会话状态机 |

### Pipeline

| 组件 | 文件 | 职责 |
|------|------|------|
| `PipelineFactory` | `pipeline/src/factory.rs` | 持有共享依赖，构建管道 |
| `PipelineGraph` | `pipeline/src/graph.rs` | 管道拓扑 + validate |
| `PipelineBuilder` | `pipeline/src/graph.rs` | 类型安全构建 API |
| `PipelineImpl` | `pipeline/src/engine.rs` | 运行时管理 + shutdown |
| `OTelProbe` | `pipeline/src/processor/otel.rs` | 指标探针（passthrough） |
| `SeqCacheProbe` | `pipeline/src/processor/seq_cache.rs` | Seq header + keyframe 缓存 |
| `FlvMux` | `pipeline/src/processor/flv_mux.rs` | EncodedPacket → FlvTag |
| `HlsSegmenter` | `pipeline/src/processor/hls_segment.rs` | EncodedPacket → TsSegment |
| `RtpDemuxProcessor` | `pipeline/src/processor/rtp_depack/` | RtpPacket → EncodedPacket |
| `FlvSink` | `pipeline/src/sink/flv.rs` | FLV 广播到 FlvEgressHub |
| `MinIoSink` | `pipeline/src/sink/minio.rs` | TS 分段上传 MinIO |
| `Transcode` | `pipeline/src/processor/transcode.rs` | 桩代码（Phase 4.5/6） |

## 5. Crate 依赖 / Crate Dependencies

```
binary (livestream-rs)
  └── livestream-transport
        ├── livestream-pipeline
        │     ├── livestream-media
        │     │     └── livestream-codec
        │     │           └── livestream-core
        │     └── livestream-codec
        └── livestream-codec
  └── livestream-telemetry
```

- `livestream-media` 是唯一依赖 `ffmpeg-sys-next` 的 crate
- `livestream-pipeline` 不依赖 `livestream-transport`（通过 trait 注入）
