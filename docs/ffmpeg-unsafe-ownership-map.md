# FFmpeg Unsafe Ownership Map / FFmpeg 非安全所有权映射

Date / 日期: 2026-04-01 (updated 2026-07-24 for v0.4.0 refactoring)
Status / 状态: Updated for v0.4.0 refactored architecture. / 基于 v0.4.0 重构后架构更新。
Entries marked ~~strikethrough~~ are removed in v0.4.0 (see Spec 06 Migration Plan). / 标记 ~~删除线~~ 的条目在 v0.4.0 中移除（见 Spec 06 迁移计划）。

## Purpose / 目的

- Document acquire/free responsibilities around FFmpeg raw pointers. / 记录 FFmpeg 原始指针的获取与释放责任。
- Prevent double-free, leak, and use-after-free across wrappers. / 防止在包装层出现重复释放、泄漏和悬垂访问。

## Ownership Rules / 所有权规则

- Rule A: The type that allocates is responsible for releasing unless ownership is explicitly transferred. / 规则 A：谁分配谁释放，除非代码明确转移所有权。
- Rule B: Wrapper `Drop` must be idempotent by nulling internal pointers after free. / 规则 B：包装类型的 `Drop` 应在释放后置空指针，保证幂等。
- Rule C: FFI callback `opaque` ownership must be reclaimed exactly once. / 规则 C：FFI 回调 `opaque` 的所有权必须且只能回收一次。

## Acquire/Release Table / 获取与释放对照表

| Resource / 资源 | Acquire / 获取 | Owner / 拥有者 | Release / 释放 | Notes / 说明 |
|---|---|---|---|---|
| `AVFormatContext` (input) | `avformat_alloc_context` in `alloc_input_context` | `InputContext` | `avformat_close_input` in `free_input_context` | `interrupt_callback.opaque` holds boxed `CancellationToken`. / `opaque` 保存装箱 `CancellationToken`。 |
| Input interrupt opaque token | `Box::into_raw(cancel_token)` | `InputContext` | `Box::from_raw` in `free_input_context` | Callback is disabled and `opaque` is detached before close; token is reclaimed after close returns. / 关闭前先禁用回调并摘除 `opaque`，待 close 返回后再回收 token。 |
| `AVPacket` | `av_packet_alloc` in `Packet::alloc` | `Packet` | `av_packet_free` in `Packet::drop` | `Packet::clone` uses `av_packet_clone` and asserts non-null result. / clone 使用 `av_packet_clone` 并断言返回非空。 |
| Temporary `AVCodecContext` (dummy stream params) | `avcodec_alloc_context3` in `OwnedCodecParams::create_dummy_*` | `OwnedCodecParams::create_dummy_*` local scope | `avcodec_free_context` on both success and error paths | Temporary context is always freed after `avcodec_parameters_from_context`. / 在参数复制后无论成功失败均释放临时上下文。 |
| Owned dummy `AVCodecParameters` | `avcodec_parameters_alloc` in `OwnedCodecParams::create_dummy_*` | `OwnedCodecParams` | `avcodec_parameters_free` in `Drop` | Ownership is transferred into `OwnedCodecParams` and released by RAII. / 所有权转移至 `OwnedCodecParams`，由 RAII 释放。 |
| `AVFormatContext` (HLS output) | `avformat_alloc_output_context2` in `HlsOutputContext::create` | `HlsOutputContext` | `avformat_free_context` in `Drop` | Trailer written via `write_trailer()` during segment rollover (NOT in Drop — Drop is safety net). / segment 切分时通过 `write_trailer()` 写 trailer（Drop 仅作安全兜底）。 |
| HLS `AVIOContext` / IO handle | `avio_alloc_context` in `HlsOutputContext::create` | `HlsOutputContext` | `avio_context_free` in `Drop` | 自定义 AVIO 回调写入**内存缓冲区**（`opaque` 指向 `Vec<u8>`）。/ Custom AVIO callback writes to in-memory buffer (`opaque` → `Vec<u8>`). |
| HLS custom AVIO buffer | `av_malloc` in `HlsOutputContext::create` | `HlsOutputContext` | `av_freep` before `avio_context_free` in `Drop` | 必须**先**释放 AVIO buffer，**再**释放 AVIOContext。同 FLV 路径规则。 / Must free buffer BEFORE `avio_context_free` — same rule as FLV. |
| HLS callback opaque (`Vec<u8>`) | `&mut Vec<u8>` (借用于 `ts_buffer`，由 `HlsSegmenter` 持有) | `HlsSegmenter` | N/A — opaque 不被 HlsOutputContext 拥有 | Opaque 只是对 `HlsSegmenter.ts_buffer` 的借用指针。`HlsOutputContext` 不拥有它。生命周期由 Rust 借用检查保证：`ts_buffer` 的存活期必须长于 `HlsOutputContext`。 / Opaque is a borrowed pointer to `HlsSegmenter.ts_buffer`. Not owned by HlsOutputContext. Lifetime enforced by Rust borrow checker. |
| `AVFormatContext` (FLV output) | `avformat_alloc_output_context2` in `FlvOutputContext::create` | `FlvOutputContext` | `avformat_free_context` in `Drop` | Uses custom AVIO callback path. / 使用自定义 AVIO 回调路径。 |
| FLV custom AVIO buffer | `av_malloc` in `FlvOutputContext::open_io` | `FlvOutputContext` | `av_freep((*pb).buffer)` in unified cleanup (`Drop` + create-failure) | Must free buffer before `avio_context_free`. / 在 `avio_context_free` 前释放缓冲区。 |
| FLV `AVIOContext` | `avio_alloc_context` in `FlvOutputContext::open_io` | `FlvOutputContext` | `avio_context_free` in unified cleanup (`Drop` + create-failure) | Custom write callback writes to crossfire channel. / 回调写入 crossfire channel。 |
| FLV callback opaque (`FlvAvioOpaque`) | `Arc::into_raw` in `FlvOutputContext::create` | `FlvOutputContext` | `Arc::from_raw` in open-io error path or unified cleanup | Reclaimed exactly once in all paths. / 所有路径均保证只回收一次。 |

## Callback Safety Notes / 回调安全说明

- `write_packet` (FLV) treats `opaque` as borrowed pointer and does not change ownership. / `write_packet` (FLV) 将 `opaque` 视为借用指针，不改变所有权。
- `ts_write_packet` (HLS) treats `opaque` as `&mut Vec<u8>` — appends data. Same borrow semantics. / `ts_write_packet` (HLS) 将 `opaque` 视为 `&mut Vec<u8>`，追加数据。同样的借用语义。
- FLV callback returns `AVERROR_EOF` when sender is disconnected to signal downstream stop. / FLV 发送端断开时回调返回 `AVERROR_EOF`，用于通知下游停止。
- HLS callback 不会返回错误：内存写入不会失败（除非 OOM，此时进程 abort）。 / HLS callback never returns error: in-memory write cannot fail (OOM aborts the process).
- Input interrupt callback ownership: disable callback and detach `opaque` before close, then reclaim token once close completes. / 输入中断回调所有权：关闭前先禁用回调并摘除 `opaque`，close 完成后回收 token。

## Common Failure Modes / 常见故障模式

- Forgetting to reclaim `opaque` causes memory leak. / 忘记回收 `opaque` 会造成内存泄漏。
- Reclaiming `opaque` twice causes double-free crash. / 重复回收 `opaque` 会触发 double-free 崩溃。
- Freeing AVIO buffer after `avio_context_free` may corrupt memory. / 在 `avio_context_free` 之后再释放 AVIO 缓冲区可能破坏内存。
- HLS opaque 是借用指针（`&mut Vec<u8>`）：确保 `ts_buffer` 的存活期长于 `HlsOutputContext`。Rust borrow checker 在编译期保证这一点；但若使用 `unsafe` 绕过（如 `Arc` 转 raw），需人工验证。 / HLS opaque is a borrowed pointer: ensure `ts_buffer` outlives `HlsOutputContext`. Rust borrow checker enforces this at compile time; if bypassed via `unsafe` (e.g., `Arc` to raw), verify manually.

## Pipeline Integration Notes / 与 Pipeline 集成说明

- Keep FFmpeg pointer ownership inside media wrappers; pipeline should pass typed packets only. / FFmpeg 指针所有权应留在 media 包装层，pipeline 仅传递类型化包对象。
- HLS TS muxer 写入**内存缓冲区**（custom AVIO callback → `Vec<u8>`），不在 pipeline core task 中做磁盘 I/O。Segment 切分时一次性将 buffer 写入磁盘 → stage → MinIoSink 异步上传。 / HLS TS muxer writes to in-memory buffer (custom AVIO callback → `Vec<u8>`), no disk I/O in pipeline core task. On rollover, buffer is flushed to disk once → staged → MinIoSink async upload.
- If persistence stage creates additional output contexts, follow the same allocate-owner-drop pattern. / 若持久化阶段新增输出上下文，需遵循相同的分配-归属-释放模式。