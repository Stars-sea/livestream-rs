# Livestream-RS: Data Flow & Component Architecture

Updated 2026-08-04 to match current code.

## Crate Dependency Graph

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Binary (src/main.rs)                            │
│  config + FFmpeg init + spawns RTMP / RTSP / gRPC / HTTP-FLV       │
└──────┬────────────────────┬───────────────────┬────────────────────┘
       │                    │                   │
       ▼                    ▼                   ▼
┌──────────────┐  ┌─────────────────┐  ┌──────────────────────────┐
│ transport    │  │ pipeline        │  │ media (FFmpeg RAII)      │
│ RTMP/RTSP/   │◄─┤ Processor/Sink  │  │ Decoder/Encoder/Scaler,  │
│ HTTP-FLV,    │  │ impls, Factory, │  │ RTP demux, HLS muxer,    │
│ gRPC, FLV    │  │ PipelineImpl,   │  │ BSF, FLV encode (pure    │
│ Hub,Registry │  │ Task loops      │  │ Rust in pipeline)        │
└────────┬─────┘  └────────┬────────┘  └──────────┬───────────────┘
         │                 │ depends on            │
         │  depends on     ▼                      ▼
         │          ┌─────────────────────────────────────┐
         │          │ core                                 │
         ├─────────►│ traits (Source/Processor/Sink/       │
         │          │         Pipeline/Node)               │
         │          │ types (CodecParams, Protocol,        │
         │          │         Codec, MediaPacket)          │
         │          │ config (SegmentConfig, Transcode-    │
         │          │         Config, AppConfig)           │
         │          │ pad (PadSender/PadReceiver,          │
         │          │      DemandSignal/DemandHandle)      │
         │          │ channel (mpsc/broadcast wrappers)    │
         │          └────────────────┬────────────────────┘
         │                           │
         │          ┌────────────────▼────────────────────┐
         │          │ codec                                │
         └─────────►│ EncodedPacket, TsSegment,            │
                    │ RtpPacket, CodecParams, SegmentConfig │
                    └──────────────────────────────────────┘

┌──────────────────┐
│ telemetry        │
│ OTel metrics,    │◄──── used by pipeline + transport task loops
│ error counters   │
└──────────────────┘
```

No circular crate dependencies. All arrows point to `core` or `codec`.

## Key Abstractions

### Traits (core)

| Trait | Purpose | Implementors |
|-------|---------|--------------|
| `Node` | Human-readable `name()` for logging/metrics | All processors and sinks |
| `Source` | Produces encoded media packets | `RtmpSource`, `RtspSource` |
| `Processor` | `Input → Vec<Output>` transform, demand-aware | `FlvMux`, `HlsSegmenter`, `RtpDemuxProcessor`, `TranscodeProcessor`, `OTelProbe`, `SeqCacheProbe` |
| `Sink` | Terminates pipeline — consumes items | `FlvSink`, `MinIoSink` |
| `Pipeline` | Lifecycle: `run()` / `shutdown()` / `handle()` | `PipelineImpl` |

### Cross-Cutting Interfaces

| Trait | Crate | Purpose |
|-------|-------|---------|
| `FlvBroadcast` | pipeline | Send FLV tags to subscribers + register demand handles. Transport-side impl: `FlvEgressHub` |
| `ObjectUploader` | pipeline/sink | Upload segments to S3/MinIO. Transport-side impls: `PersistenceClient` (MinIO), `NullUploader` (dev) |
| `StreamCollection` | media | Codec stream lookup from FFmpeg |
| `MediaPacket` | core | Common interface for all pipeline data types |

### Channels

| Type | Backend | Use |
|------|---------|-----|
| `PadSender<T>` / `PadReceiver<T>` | crossfire mpsc | Pipeline node connections |
| `MpscTx<T>` / `MpscRx<T>` | crossfire mpsc | Control channels (server ↔ controller) |
| `BroadcastTx<T>` / `BroadcastRx<T>` | tokio broadcast | Session events (dispatcher), FLV tag distribution |
| `FlvLiveChannel` | tokio broadcast + cache | Per-stream FLV delivery with cached sequence headers |

### State Machines

| State Machine | States | Location |
|--------------|--------|----------|
| `PipelineState` | Initializing → Running → Draining → Terminated | core/traits/pipeline.rs |
| `SessionState` | Pending → Connecting → Connected → Disconnected | transport/registry/state.rs |
| `RtspSession` | WaitAnnounce → WaitSetup → WaitRecord → Recording → Teardown | transport/rtsp/session.rs |
| `HandlerLifecycle` | pending → connecting → connect → disconnect (AtomicBool 幂等) | transport/lifecycle.rs |

## Entry Point Data Flows

### 1. RTMP Publish (Ingest)

```
Client TCP connect
  → RtmpConnection::perform_handshake() [rml_rtmp handshake]
  → RtmpServer::run() accept loop (ProtocolServerCore) spawns connection handler
  → SessionGuard handles RTMP connect protocol + stream key extraction
  → HandlerBuilder::build() creates PublishHandler
  → PublishHandler receives VideoDataReceived/AudioDataReceived events
  → Converts to RtmpRawFrame, sends to RtmpSource (via mpsc channel)
  → RtmpSource::start() converts RtmpRawFrame → EncodedPacket (AVCC → Annex B for NALs)
  → PipelineFactory::build_pipeline() constructs:
      EncodedPacket → OTelProbe → SeqCacheProbe → FlvMux → FlvSink → FlvEgressHub
                                                 → (deferred) HlsSegmenter → MinIoSink
  → Deferred HLS: waits for first seq headers (SPS/PPS + ASC), then constructs HLS branch
```

### 2. RTSP Ingest

```
Client TCP connect → RtspServer::run() accept loop (ProtocolServerCore)
  → RTSP handshake: read_message() parses headers + SDP body (idle timeout, size cap)
  → RtspSession state machine: OPTIONS → ANNOUNCE → SETUP → RECORD
  → ANNOUNCE extracts SDP → parsed into CodecParams (video/audio)
  → SETUP assigns RTP interleaved channels
  → RECORD starts RTP feed
  → RtpInterleavedReader parses $<channel><length><payload> frames
  → RtspSource converts to RtpPacket, sends downstream
  → PipelineFactory::build_rtsp_pipeline():
      RtpPacket → RtpDemuxProcessor → EncodedPacket
      → [MJPEG 流] TranscodeProcessor (MJPEG → H.264, 输出自带 avcC 序列头)
      → OTelProbe → SeqCacheProbe → FlvMux → FlvSink → FlvEgressHub
                                   → HlsSegmenter → MinIoSink
      （普通 H.264/AAC 源：HLS 立即构建；MJPEG 转码源：HLS 延迟初始化）
```

### 3. HTTP-FLV Playback

```
Client HTTP GET /lives/{live_id}.flv
  → HttpFlvServer handler (连接数信号量检查; 会话状态必须 Connected)
  → FlvEgressHub::subscribe(live_id) → (broadcast::Receiver<FlvTag>, Vec<FlvTag> cached)
  → Stream FLV header (encode_flv_header, 依据缓存 tag 生成 hasAudio/hasVideo)
  → Stream cached tags (sequence headers for late joiners)
  → Loop: select { receiver.recv() → encode_flv_tag → write to response body }
  → On lag (RecvError::Lagged): 置 waiting_keyframe, 记 metric_listener_lag
    → should_skip_while_waiting_keyframe() 跳过非关键帧直至恢复
  → 客户端断开 → demand handle drop → FlvMux 感知 (should_process=false) → 不推流处理
```

### 4. RTMP Playback

```
Client RTMP connect → SessionGuard handles play protocol
  → HandlerBuilder::build() creates PlayHandler
  → PlayHandler subscribes to FlvEgressHub broadcast channel (+ cached tags)
  → 与 HTTP-FLV 相同的 tag 接收 + keyframe 恢复逻辑
  → 通过 SessionGuard::send_flv_tag() 发送 (RTMP chunk protocol)
  → 观众断开不影响推流 (commit 3c13db6: viewer disconnect no longer kills stream)
```

### 5. gRPC Control Plane

```
gRPC client → GrpcServer (tonic, 可选 Bearer auth: GRPC__AUTH_TOKEN)
  → IngestGrpcService implements Livestream trait:
      StartLivestream → TransportController::precreate_{rtmp,rtsp}_session()
                       → ControlMessage::PrecreateStream via mpsc
                       → 服务器注册会话后经 oneshot ack 返回权威 SessionDescriptor
                       → 重复 live_id → ALREADY_EXISTS; ack 超时(5s) → INTERNAL
      StopLivestream  → TransportController::close_session()
                       → StopStream 到对应协议服务器 → 会话取消 → wait_for_cleanup(2s)
                       → HandlerLifecycle::disconnect_with_reason(AdminStop)
      ListLivestreams → SessionRegistry 快照
      GetLivestreamInfo → 单会话查询 (不存在 → NOT_FOUND)
      WatchLivestream → 轮询 registry + 订阅 EventDispatcher, 流式返回 SessionStatus
      GetServiceInfo → 各协议监听端口 (未启用为 0)
  另: tonic reflection 启用; GetServiceInfo 由 e2e 用于验证服务身份
```

## Component Relationships

### Session Lifecycle

```
TransportController::precreate_stream(live_id, protocol)
  → sends ControlMessage::PrecreateStream to server (oneshot ack)
  → server creates HandlerLifecycle in pending state
  → stores in DashMap<String, HandlerLifecycle> (pending_lifecycle)
  → HandlerLifecycle::pending() → SessionRegistry::register_session(Pending)
  → ack 返回 SessionDescriptor; EventDispatcher::broadcast 事件在 connect 时发出

Client connects (RTMP/RTSP)
  → handler extracts live_id from stream_key/SDP
  → looks up HandlerLifecycle from pending_lifecycle
  → lifecycle.connecting() → SessionRegistry::update(Connecting)
  → lifecycle.connect() [AtomicBool CAS] → SessionRegistry::update(Connected)
  → EventDispatcher::send(SessionStarted); init() 时 send(SessionInit{streams})

Client disconnects or error
  → lifecycle.disconnect_with_reason(reason) [幂等 via AtomicBool]
  → SessionRegistry::update(Disconnected)
  → EventDispatcher::send(SessionEnded{reason})
  → Pipeline::shutdown() → cancel token → drain tasks (5s) → Terminated
  → FlvEgressHub::remove_channel(live_id) (+ demand signal 清理)
```

### Error Propagation

```
Processor/Sink 错误 (非致命)
  → metric_pipeline_error!() counter incremented
  → tracing::warn!() logged
  → Item dropped; pipeline continues (non-fatal by design)

致命错误 (channel closure, 意外状态)
  → anyhow::Error 传播 → Session ends with EndReason::Error(msg)
  → Pipeline shutdown triggered

基础设施错误 (MinIO 上传)
  → MinIoSink 重试 (200ms/500ms 退避, 共 2 次)
  → 上传成功或最终失败后均删除暂存文件 (防磁盘堆积)
  → 会话继续 (除非致命)
```

## Robustness Considerations

### Input Validation Boundaries

- **RTSP**: 消息头大小上限 (MAX_RTSP_MESSAGE_SIZE, 防 OOM); 空闲超时; Content-Length 边界检查; 整数溢出防护
- **RTP**: `stream_index` 对 `nb_streams` 边界检查后再解引用 FFmpeg 指针
- **FLV**: 24-bit size 字段天然受限 (~16 MiB); `FlvTag::try_from` 校验完整块
- **RTMP**: chunk size 钳制到 [128, 16 MiB] (rml_rtmp 允许 2^31-1, 防 OOM DoS)
- **路径安全**: `sanitize_stream_id` 用于分段文件路径与 MinIO 对象键 (防 path traversal)
- **gRPC**: tonic 内置消息大小限制; 可选 Bearer auth 覆盖含 reflection 的全部请求
- **连接数**: RTMP/RTSP/HTTP-FLV 信号量限流 (0=无限制); accept 错误区分可重试/致命

### Graceful Degradation

- **无 MinIO**: `NullUploader` 丢弃 HLS 分段并打 warning — FLV 路径正常; bucket ensure 失败重试 3 次 (500ms/1s 退避), "已存在"竞态视为成功
- **无 codec 参数 (RTMP / 转码流)**: HLS 延迟到序列头就绪; FLV 路径立即工作
- **HLS 构建失败**: 打 warning, 仅 FLV
- **无订阅者**: `FlvMux::should_process()` 返回 false (demand 感知); `FlvEgressHub::broadcast()` 静默丢弃
- **HTTP-FLV 启动失败**: 降级为 warning, 进程继续 (健康端点不可用); gRPC 启动失败则致命 (控制面必需)

### Task Lifetime Management

- Pipeline 任务经 `Arc<Mutex<Vec<JoinHandle<()>>>>` 跟踪, 含延迟 HLS 任务 (`push_tasks`)
- `PipelineImpl::shutdown()`: cancel → 5s 总预算逐任务排空 → 超时 abort → 迟到注册任务 abort (logged)
- 每个 task 退出时调用 `Processor::close()` (HlsSegmenter flush 最终 segment + `#EXT-X-ENDLIST`)
- 协议服务器连接任务由 `ProtocolServerCore` 的 `JoinSet` 跟踪, 服务器关闭时 `shutdown().await` 排空
- `HandlerLifecycle` 用 AtomicBool 幂等断开, 防会话注册表双释放; `Drop` 兜底补发断开

### Known Gaps

- HTTP-FLV/RTMP 播放无逐客户端背压: 慢读者导致 broadcast 通道堆积, 通过 Lagged → 跳帧恢复, 但队列本身无界可增长
- `SEGMENT__MAX_STAGED_SEGMENTS` (LRU 淘汰) 尚未实现 — 上传成功/最终失败后清理暂存, 未做总量上限
- `EventDispatcher` 全局通道满时打 warning 并丢弃事件 (不阻塞); 按 live_id 子通道无订阅者时自动移除
- RTMP/RTSP 连接 accept 无速率限制 (仅连接数上限)
- `FlvTagPacketizer` (media) 为 FLV → FFmpeg Packet 转换器, 当前仅由单元测试覆盖, 无生产调用路径
