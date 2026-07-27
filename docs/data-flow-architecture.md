# Livestream-RS: Data Flow & Component Architecture

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
│ RTMP/RTSP/   │◄─┤ Processor/Sink  │  │ flv, codec, stream,      │
│ HTTP-FLV,    │  │ impls, Factory, │  │ decode/encode/scaler,    │
│ gRPC, FLV    │  │ PipelineImpl,   │  │ rtp demux                │
│ Hub,Registry │  │ Task loops      │  └──────────┬───────────────┘
└────────┬─────┘  └────────┬────────┘             │
         │                 │ depends on            │
         │  depends on     ▼                      ▼
         │          ┌─────────────────────────────────────┐
         │          │ core                                 │
         ├─────────►│ traits (Source/Processor/Sink/       │
         │          │         Pipeline/Node)               │
         │          │ types (CodecParams, Protocol,        │
         │          │         Codec, MediaPacket)          │
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
│ OTel metrics,    │◄──── used by pipeline task loops
│ error counters   │
└──────────────────┘
```

No circular crate dependencies. All arrows point to `core` or `codec`.

## Key Abstractions

### Traits (core)

| Trait | Purpose | Implementors |
|-------|---------|-------------|
| `Node` | Human-readable `name()` for logging/metrics | All processors and sinks |
| `Source` | Produces encoded media packets | `RtmpSource`, `RtspSource` |
| `Processor` | `Input → Vec<Output>` transform, demand-aware | `FlvMux`, `HlsSegmenter`, `RtpDemuxProcessor`, `OTelProbe`, `SeqCacheProbe`, `Transcode` |
| `Sink` | Terminates pipeline — consumes items | `FlvSink`, `MinIoSink` |
| `Pipeline` | Lifecycle: `run()` / `shutdown()` / `handle()` | `PipelineImpl` |

### Cross-Cutting Interfaces

| Trait | Crate | Purpose |
|-------|-------|---------|
| `FlvBroadcast` | pipeline | Send FLV tags to subscribers. Transport-side impl: `FlvEgressHub` |
| `ObjectUploader` | pipeline/sink | Upload segments to S3/MinIO. Transport-side impls: MinIO client, `NullUploader` (dev) |
| `StreamCollection` | media | Codec stream lookup from FFmpeg |
| `MediaPacket` | core | Common interface for all pipeline data types |

### Channels

| Type | Backend | Use |
|------|---------|-----|
| `PadSender<T>` / `PadReceiver<T>` | Direct (same-task) or crossfire mpsc (cross-task) | Pipeline node connections |
| `MpscTx<T>` / `MpscRx<T>` | crossfire mpsc | Control channels (server ↔ controller) |
| `BroadcastTx<T>` / `BroadcastRx<T>` | tokio broadcast | FLV tag distribution to subscribers |
| `FlvLiveChannel` | tokio broadcast + cache | Per-stream FLV delivery with cached sequence headers |

### State Machines

| State Machine | States | Location |
|--------------|--------|----------|
| `PipelineState` | Initializing → Running → Draining → Terminated | core/traits/pipeline.rs |
| `SessionState` | Pending → Connecting → Connected → Disconnected | transport/registry/state.rs |
| `RtspSession` | WaitAnnounce → WaitSetup → WaitRecord → Recording → Teardown | transport/rtsp/session.rs |

## Entry Point Data Flows

### 1. RTMP Publish (Ingest)

```
Client TCP connect
  → RtmpConnection::handshake() [rml_rtmp handshake]
  → RtmpServer::accept_client() spawns connection handler
  → SessionGuard handles RTMP connect protocol + stream key extraction
  → HandlerBuilder::build() creates PublishHandler
  → PublishHandler receives VideoDataReceived/AudioDataReceived events
  → Converts to RtmpRawFrame, sends to RtmpSource (via mpsc channel)
  → RtmpSource::start() converts RtmpRawFrame → EncodedPacket (AVCC→Annex B for NALs)
  → PipelineFactory::build_pipeline() constructs:
      EncodedPacket → OTelProbe → SeqCacheProbe → FlvMux → FlvSink → FlvEgressHub
                                                 → (deferred) HlsSegmenter → MinIoSink
  → Deferred HLS: waits for first seq header with SPS/PPS, then constructs HLS branch
```

### 2. RTSP Ingest

```
Client TCP connect → RtspServer::accept_client()
  → RTSP handshake: read_message() parses headers + SDP body
  → RtspSession state machine: OPTIONS → ANNOUNCE → SETUP → RECORD
  → ANNOUNCE extracts SDP → parsed into CodecParams (video/audio)
  → SETUP assigns RTP interleaved channels
  → RECORD starts RTP feed
  → RtpInterleavedReader parses $<channel><length><payload> frames
  → RtspSource converts to RtpPacket, sends downstream
  → PipelineFactory::build_rtsp_pipeline():
      RtpPacket → RtpDemuxProcessor → EncodedPacket
      → OTelProbe → SeqCacheProbe → FlvMux → FlvSink → FlvEgressHub
                                   → HlsSegmenter → MinIoSink (immediate, codec params from SDP)
```

### 3. HTTP-FLV Playback

```
Client HTTP GET /lives/{live_id}.flv
  → HttpFlvServer handler
  → FlvEgressHub::subscribe(live_id) → (broadcast::Receiver<FlvTag>, Vec<FlvTag> cached)
  → Stream FLV header (encode_flv_header)
  → Stream cached tags (sequence headers for late joiners)
  → Loop: select { receiver.recv() → encode_flv_tag → write to response body }
  → On lag: skip to next keyframe via should_skip_while_waiting_keyframe()
  → On client disconnect: demand handle dropped → FlvMux skips processing
```

### 4. RTMP Playback

```
Client RTMP connect → SessionGuard handles play protocol
  → HandlerBuilder::build() creates PlayHandler
  → PlayHandler subscribes to FlvEgressHub broadcast channel
  → Same tag-receive + keyframe-recovery logic as HTTP-FLV
  → Sends FLV tags via SessionGuard::send_flv_tag() (RTMP chunk protocol)
```

### 5. gRPC Control Plane

```
gRPC client → GrpcServer (tonic)
  → IngestGrpcService implements Livestream trait:
      StartLivestream → TransportController::precreate_stream()
                       → sends PrecreateStream to RTMP/RTSP server via mpsc
                       → oneshot ack
      StopLivestream  → TransportController::close_stream()
                       → sends StopStream to server → HandleLifecycle::disconnect_with_reason(AdminStop)
      ListLivestreams → SessionRegistry snapshot
      WatchLivestreams → EventDispatcher subscription stream
```

## Component Relationships

### Session Lifecycle

```
TransportController::precreate_stream(live_id, protocol)
  → sends ControlMessage::PrecreateStream to server
  → server creates HandlerLifecycle in pending state
  → stores in DashMap<String, HandlerLifecycle> (pending_lifecycle)
  → HandlerLifecycle::pending() → SessionRegistry::insert(Pending)
  → EventDispatcher::broadcast(SessionStarted)

Client connects (RTMP/RTSP)
  → handler extracts live_id from stream_key/SDP
  → looks up HandlerLifecycle from pending_lifecycle
  → lifecycle.connecting() → SessionRegistry::update(Connecting)
  → lifecycle.connect() → SessionRegistry::update(Connected)
  → EventDispatcher::broadcast(SessionInit{streams})

Client disconnects or error
  → lifecycle.disconnect_with_reason(reason) [idempotent via AtomicBool]
  → SessionRegistry::update(Disconnected)
  → EventDispatcher::broadcast(SessionEnded{reason})
  → Pipeline::shutdown() → cancel token → drain tasks with 5s timeout
  → FlvEgressHub::remove_channel(live_id)
```

### Error Propagation

```
Processor/Sink error
  → metric_pipeline_error!() counter incremented
  → tracing::warn!() logged
  → Item dropped; pipeline continues (non-fatal by design)

Fatal errors (channel closure, unexpected state)
  → anyhow::Error propagated
  → Session ends with EndReason::Error(msg)
  → Pipeline shutdown triggered

Infrastructure errors (MinIO upload, network)
  → Retried by tokio channel backpressure
  → Logged as warnings
  → Session continues unless fatal
```

## Robustness Considerations

### Input Validation Boundaries

- **RTSP**: Content-Length bounded at 64 KiB (defense against OOM). Integer overflow protection on `pos + 4 + Content-Length`.
- **RTP**: `stream_index` bounds-checked against `nb_streams` before FFmpeg pointer dereference.
- **FLV**: Tags use 24-bit size fields (max ~16 MiB), naturally bounded.
- **gRPC**: tonic provides built-in message size limits.
- **RTMP**: rml_rtmp library handles protocol-level validation.

### Graceful Degradation

- **No MinIO**: `NullUploader` drops HLS segments with a warning — FLV path continues normally.
- **No codec params (RTMP)**: HLS deferred until first sequence header; FLV path works immediately.
- **HLS construction failure**: Warns, continues with FLV only.
- **No subscribers**: `FlvMux::should_process()` returns false; `FlvEgressHub::broadcast()` silently drops.

### Task Lifetime Management

- All pipeline tasks tracked via `Arc<Mutex<Vec<JoinHandle<()>>>>` — including deferred HLS tasks.
- `PipelineImpl::shutdown()` cancels all tokens, then drains with 5s timeout.
- `HandlerLifecycle` uses `AtomicBool` flags for idempotent disconnect — protects against double-free of session registry state.
- Session registry cleanup tasks are spawned with cancel token awareness.

### Known Gaps

- HTTP-FLV has no per-client backpressure; a slow reader can cause unbounded broadcast channel queuing.
- `let _ = channel.send(...)` silently drops events in the dispatcher and controller when receivers are full/slow.
- RTMP/RTSP connection handler spawns are fire-and-forget; panics are logged by tokio's default panic hook but not explicitly handled.
- No rate limiting on RTSP/RTMP connection accept loops.
