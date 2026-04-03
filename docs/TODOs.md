# TODOs

面向 transport 与 pipeline 架构演进的待办事项。  
Roadmap-oriented TODO items for transport and pipeline evolution.

## Short Term

- 引入协议适配层，将 RTMP/SRT 输入进一步统一为一致的 ingest 抽象。  
Introduce a protocol adapter layer to normalize RTMP/SRT inputs into a unified ingest abstraction.
- 为 pipeline 增加可选的质量分析 middleware（码率、关键帧间隔、抖动指标）。  
Add optional quality-analysis middleware in pipeline (bitrate, keyframe interval, jitter metrics).
- 补充会话生命周期相关的集成测试（start/stop race、重复创建、异常断连）。  
Add integration tests for session lifecycle (start/stop race, duplicate creation, abnormal disconnect).

## Mid Term

- 将 `WatchLivestream` 从轮询式状态读取演进为更细粒度事件流模型。  
Evolve `WatchLivestream` from polling-based state checks to finer-grained event-stream model.
- 为持久化链路增加批量上传与失败分级重试策略。  
Add batch upload and tiered retry strategy for persistence pipeline.
- 为 pipeline 引入更清晰的错误分类与错误预算指标。  
Introduce clearer error taxonomy and error-budget metrics for pipeline.

## Long Term

- 在不破坏现有接口的前提下探索多协议扩展（例如 WebRTC ingest 适配）。  
Explore multi-protocol extension (for example WebRTC ingest adapter) without breaking current interfaces.
- 增加跨节点扩展能力评估（会话注册、事件总线、分段持久化的分布式方案）。  
Evaluate multi-node scaling options for session registry, event bus, and segment persistence.
