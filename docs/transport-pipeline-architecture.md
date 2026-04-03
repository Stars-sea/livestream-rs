# Transport-Pipeline Architecture

本文档深入说明当前代码中的 transport 与 pipeline 协作方式与关键设计要点。  
This document explains transport-pipeline collaboration and key architecture principles in current code.

## 1. 子系统职责 / Subsystem Responsibilities

- transport: 连接管理、协议接入、会话状态、控制命令处理。  
transport: connection handling, protocol ingest, session state, and control command handling.
- pipeline: 媒体包统一上下文、处理链执行、转发、分段、持久化事件发射。  
pipeline: unified media context, processing chain execution, forwarding, segmentation, and persistence event emission.

## 2. 关键协作链路 / Key Collaboration Chain

1. gRPC 调用进入 `GrpcServer`，由 `TransportController` 发送 `ControlMessage`。  
gRPC enters `GrpcServer`, then `TransportController` sends `ControlMessage`.
2. `RtmpServer` / `SrtServer` 处理控制命令，维护会话并上报 `StreamEvent`。  
`RtmpServer` / `SrtServer` handle control commands, manage sessions, and emit `StreamEvent`.
3. transport 层通过 dispatcher 广播 `SessionEvent`（如 `SessionInit`、`SessionEnded`）。  
transport broadcasts `SessionEvent` (for example `SessionInit`, `SessionEnded`) via dispatcher.
4. `PipeBus` 订阅事件，基于 `UnifiedPipeFactory` 动态构建或销毁 per-stream `Pipe`。  
`PipeBus` subscribes to events and dynamically builds/removes per-stream `Pipe` via `UnifiedPipeFactory`.
5. 数据包封装为 `UnifiedPacketContext` 后进入 `Pipe`，按中间件顺序处理。  
Packets are wrapped as `UnifiedPacketContext` and processed through `Pipe` middleware chain.
6. `SegmentMiddleware` 发送 `SegmentComplete`，`SegmentPersistenceHandler` 消费并上传 MinIO。  
`SegmentMiddleware` emits `SegmentComplete`, consumed by `SegmentPersistenceHandler` for MinIO upload.

## 3. 关键组件 / Key Components

### 3.1 transport

- `TransportServer`: 统一启动 RTMP/SRT server 与事件监听。  
`TransportServer`: boots RTMP/SRT servers and event listener.
- `TransportController`: 控制面命令入口（Precreate/Stop）。  
`TransportController`: control command entry (Precreate/Stop).
- `GrpcServer`: 外部 RPC 接口实现。  
`GrpcServer`: external RPC service implementation.
- Global Registry: 会话描述与取消令牌集中管理。  
Global Registry: centralized session descriptors and cancellation tokens.

### 3.2 pipeline

- `Pipe`: middleware 执行容器。  
`Pipe`: middleware execution container.
- `PipeBus`: 按流 ID 路由到对应管道。  
`PipeBus`: routes by stream ID to the matching pipeline.
- `UnifiedPipeFactory`: 创建统一中间件链。  
`UnifiedPipeFactory`: creates the unified middleware chain.
- `OTelMiddleware`: 指标打点与延迟记录。  
`OTelMiddleware`: metrics and latency recording.
- `FlvMuxForwardMiddleware`: FLV 复用并转发到 RTMP egress 通道。  
`FlvMuxForwardMiddleware`: FLV mux and forward to RTMP egress channel.
- `SegmentMiddleware`: 分段滚动写盘与完成事件发射。  
`SegmentMiddleware`: rolling segment write and completion event emission.

## 4. 架构设计要点 / Architecture Principles

- 分层职责明确：transport 处理连接与会话，pipeline 处理媒体与内容流程。  
Clear layered responsibility: transport handles connection/session, pipeline handles media/content flow.
- 事件驱动解耦：会话生命周期通过 dispatcher 广播，管道与持久化侧按事件订阅。  
Event-driven decoupling: session lifecycle is broadcast via dispatcher, while pipeline and persistence subscribe by event.
- 按流隔离：每个 `live_id` 拥有独立 `Pipe`，降低跨流干扰与状态污染风险。  
Per-stream isolation: each `live_id` owns an independent `Pipe`, reducing cross-stream interference and state pollution.
- 可插拔处理链：中间件按顺序组合，新增处理能力不需要改接入协议层。  
Composable processing chain: middlewares are ordered and pluggable, enabling feature growth without changing ingest protocol layer.
- 统一上下文：`UnifiedPacketContext` 抽象不同协议输入，降低上层处理分支复杂度。  
Unified context: `UnifiedPacketContext` abstracts protocol differences and reduces branching complexity.
- 有界队列反压：crossfire 有界通道控制突发流量，防止无界内存增长。  
Bounded-channel backpressure: crossfire bounded channels contain burst traffic and prevent unbounded memory growth.
