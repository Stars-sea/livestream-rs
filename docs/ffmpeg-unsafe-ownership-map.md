# FFmpeg Unsafe Ownership Map / FFmpeg 非安全所有权映射

Date / 日期: 2026-04-01
Status / 状态: Updated from current source code (`src/infra/media/**`). / 基于当前代码（`src/infra/media/**`）更新。

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
| `AVFormatContext` (HLS output) | `avformat_alloc_output_context2` in `HlsOutputContext::create` | `HlsOutputContext` | `avformat_free_context` in `Drop` | Trailer written in `Drop` before free. / `Drop` 内先写 trailer 再释放。 |
| HLS `AVIOContext` / IO handle | `avio_open` through `OutputContext::open_io` | `HlsOutputContext` | `avio_closep` in `Drop` and header-failure cleanup path | Closed both in normal `Drop` and when header writing fails during create. / 正常 `Drop` 与 create 阶段写头失败时都会关闭。 |
| `AVFormatContext` (FLV output) | `avformat_alloc_output_context2` in `FlvOutputContext::create` | `FlvOutputContext` | `avformat_free_context` in `Drop` | Uses custom AVIO callback path. / 使用自定义 AVIO 回调路径。 |
| FLV custom AVIO buffer | `av_malloc` in `FlvOutputContext::open_io` | `FlvOutputContext` | `av_freep((*pb).buffer)` in unified cleanup (`Drop` + create-failure) | Must free buffer before `avio_context_free`. / 在 `avio_context_free` 前释放缓冲区。 |
| FLV `AVIOContext` | `avio_alloc_context` in `FlvOutputContext::open_io` | `FlvOutputContext` | `avio_context_free` in unified cleanup (`Drop` + create-failure) | Custom write callback writes to crossfire channel. / 回调写入 crossfire channel。 |
| FLV callback opaque (`FlvAvioOpaque`) | `Arc::into_raw` in `FlvOutputContext::create` | `FlvOutputContext` | `Arc::from_raw` in open-io error path or unified cleanup | Reclaimed exactly once in all paths. / 所有路径均保证只回收一次。 |

## Callback Safety Notes / 回调安全说明

- `write_packet` treats `opaque` as borrowed pointer and does not change ownership. / `write_packet` 将 `opaque` 视为借用指针，不改变所有权。
- Callback returns `AVERROR_EOF` when sender is disconnected to signal downstream stop. / 发送端断开时回调返回 `AVERROR_EOF`，用于通知下游停止。
- Input interrupt callback ownership: disable callback and detach `opaque` before close, then reclaim token once close completes. / 输入中断回调所有权：关闭前先禁用回调并摘除 `opaque`，close 完成后回收 token。

## Common Failure Modes / 常见故障模式

- Forgetting to reclaim `opaque` causes memory leak. / 忘记回收 `opaque` 会造成内存泄漏。
- Reclaiming `opaque` twice causes double-free crash. / 重复回收 `opaque` 会触发 double-free 崩溃。
- Freeing AVIO buffer after `avio_context_free` may corrupt memory. / 在 `avio_context_free` 之后再释放 AVIO 缓冲区可能破坏内存。
- Not closing HLS IO on header-write failure leaks file handles. / 写头失败时若未关闭 HLS IO 会泄漏文件句柄。

## Pipeline Integration Notes / 与 Pipeline 集成说明

- Keep FFmpeg pointer ownership inside media wrappers; pipeline should pass typed packets only. / FFmpeg 指针所有权应留在 media 包装层，pipeline 仅传递类型化包对象。
- If persistence stage creates additional output contexts, follow the same allocate-owner-drop pattern. / 若持久化阶段新增输出上下文，需遵循相同的分配-归属-释放模式。