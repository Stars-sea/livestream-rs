# FFmpeg Unsafe Ownership Map / FFmpeg 非安全所有权映射

Date / 日期: 2026-04-01 (updated 2026-08-04 for current code state)
Status / 状态: Updated for current architecture (transcode, RTP demux, BSF; FLV muxing is pure Rust).

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
| `AVPacket` | `av_packet_alloc` in `Packet::alloc` | `Packet` | `av_packet_free` in `Packet::drop` | `Packet::clone` uses `av_packet_clone` and asserts non-null result. / clone 使用 `av_packet_clone` 并断言返回非空。 |
| Temporary `AVCodecContext` (dummy stream params) | `avcodec_alloc_context3` in `OwnedCodecParams::create_dummy_*` | `OwnedCodecParams::create_dummy_*` local scope | `avcodec_free_context` on both success and error paths | Temporary context is always freed after `avcodec_parameters_from_context`. / 在参数复制后无论成功失败均释放临时上下文。 |
| Owned dummy `AVCodecParameters` | `avcodec_parameters_alloc` in `OwnedCodecParams::create_dummy_*` | `OwnedCodecParams` | `avcodec_parameters_free` in `Drop` | Ownership is transferred into `OwnedCodecParams` and released by RAII. / 所有权转移至 `OwnedCodecParams`，由 RAII 释放。 |
| `AVFrame` | `av_frame_alloc` in `Frame::new` | `Frame` | `av_frame_free` in `Drop` | Used by transcode pipeline (decoder output / scaler destination). / 转码链路使用（解码输出 / scaler 目标帧）。 |
| `Decoder` (`AVCodecContext`, decode) | `avcodec_find_decoder` + `avcodec_alloc_context3` in `Decoder::new` | `Decoder` | `avcodec_free_context` in `Drop` | MJPEG decoder in `TranscodeProcessor`; `Send` but not thread-safe — serialized behind a `Mutex`. 致命解码错误后重建（重建失败则 `None`，下一包再试）。 |
| `Encoder` (`AVCodecContext`, encode) | `avcodec_find_encoder`/`avcodec_find_encoder_by_name` + `avcodec_alloc_context3` in `Encoder::new`/`new_named` | `Encoder` | `avcodec_free_context` in `Drop` | H.264 encoder in `TranscodeProcessor`; `av_opt_set` declared manually as compat (missing in ffmpeg-sys-next 8.x bindings). / `av_opt_set` 为手动兼容声明。 |
| `Scaler` (`SwsContext`) | `sws_getContext` in `Scaler::new` | `Scaler` | `sws_freeContext` in `Drop` | YUV conversion in transcode path. / 转码路径色彩/尺寸转换。 |
| `H264Mp4ToAnnexb` (`AVBSFContext`) | `av_bsf_get_by_name` + `av_bsf_alloc` + `av_bsf_init` in `H264Mp4ToAnnexb::new` | `H264Mp4ToAnnexb` | `av_bsf_free` in `Drop` | ffmpeg-sys-next 8.x 无 AVBSFContext 绑定，使用本地 `#[repr(C)]` compat 声明；构造期由 `BsfContextGuard` 保证任何提前返回路径均释放。 |
| `RtpDemuxContext` (`AVFormatContext` + SDP/RTP 两个 `AVIOContext`) | `avformat_alloc_context` + `avio_alloc_context`×2 in `RtpDemuxContext::open` | `RtpDemuxContext` | `avformat_close_input` + `avio_context_free`×2 in `Drop` | `Drop` 先将 `fmt_ctx->pb` 置空再 close；SDP AVIO 在 `avformat_open_input` 成功后立即释放并摘除。 |
| SDP AVIO 内部 buffer (read-only AVIO) | `av_malloc` in SDP `avio_alloc_context` | `RtpDemuxContext` | `av_free` in `free_sdp_io` | `avio_context_free` 对只读（write_flag=0）AVIO **不**释放内部 buffer，必须手动 `av_free`（先于 `avio_context_free` 捕获指针）。 |
| SDP AVIO opaque (`SdpState`) | `Box::into_raw` | `RtpDemuxContext` | `Box::from_raw` in `free_sdp_io` | 只在 `free_sdp_io` 处回收一次（成功与失败路径均经此函数）。 |
| RTP AVIO 内部 buffer (write_flag=1) | `av_malloc` in RTP `avio_alloc_context` | `RtpDemuxContext` | `avio_context_free` 自动释放 | write_flag=1 的 AVIO close 时自动释放 buffer。 |
| RTP AVIO opaque (`Arc<Mutex<RtpBuf>>`) | `Arc::into_raw` | `RtpDemuxContext` | `Arc::from_raw` in `Drop` | 构造失败路径亦回收（见 open 错误分支）。 |
| `AVFormatContext` (HLS output) | `avformat_alloc_output_context2` in `HlsOutputContext::create` | `HlsOutputContext` | `avformat_free_context` in `Drop` | Trailer written via `write_trailer()` during segment rollover (NOT in Drop — Drop is safety net). / segment 切分时通过 `write_trailer()` 写 trailer（Drop 仅作安全兜底）。 |
| HLS `AVIOContext` / IO handle | `avio_alloc_context` in `HlsOutputContext::create` | `HlsOutputContext` | `avio_context_free` in `Drop` | 自定义 AVIO 回调写入**内存缓冲区**（`opaque` 指向 `Vec<u8>`）。/ Custom AVIO callback writes to in-memory buffer (`opaque` → `Vec<u8>`). |
| HLS custom AVIO buffer | `av_malloc` in `HlsOutputContext::create` | `HlsOutputContext` | `av_freep` before `avio_context_free` in `Drop` | 必须**先**释放 AVIO buffer，**再**释放 AVIOContext。 |
| HLS callback opaque (`Vec<u8>`) | `&mut Vec<u8>` (借用于 `ts_buffer`，由 `HlsSegmenter` 持有) | `HlsSegmenter` | N/A — opaque 不被 HlsOutputContext 拥有 | Opaque 只是对 `HlsSegmenter.ts_buffer` 的借用指针。`HlsOutputContext` 不拥有它。生命周期由 Rust 借用检查保证。 |

> FLV 输出路径已不再使用 FFmpeg：`FlvMux`（pipeline）是纯 Rust 实现（`encode_flv_tag` / `encode_flv_header`，无 FFmpeg 依赖），
> 旧版 `FlvOutputContext` / `FlvAvioOpaque`（custom AVIO → crossfire channel）已删除。

## Callback Safety Notes / 回调安全说明

- `ts_write_packet` (HLS) treats `opaque` as `&mut Vec<u8>` — appends data. Borrow semantics; never returns error (in-memory write cannot fail; OOM aborts the process). / HLS 回调将 `opaque` 视为 `&mut Vec<u8>` 追加数据，借用语义，永不报错。
- SDP/RTP AVIO callbacks feed FFmpeg's RTP demuxer: SDP opaque is a boxed `SdpState` (freed once in `free_sdp_io`); RTP opaque is an `Arc<Mutex<RtpBuf>>` (freed once in `Drop`). / SDP/RTP AVIO 回调供 FFmpeg RTP demuxer 使用，各 opaque 恰好回收一次。

## Common Failure Modes / 常见故障模式

- Forgetting to reclaim `opaque` causes memory leak. / 忘记回收 `opaque` 会造成内存泄漏。
- Reclaiming `opaque` twice causes double-free crash. / 重复回收 `opaque` 会触发 double-free 崩溃。
- Freeing AVIO buffer after `avio_context_free` may corrupt memory. / 在 `avio_context_free` 之后再释放 AVIO 缓冲区可能破坏内存。
- Read-only AVIO (`write_flag=0`) does NOT free its internal buffer on close — freeing it manually is mandatory, and the pointer must be captured before `avio_context_free` nulls it. / 只读 AVIO close 时不释放内部 buffer——必须先捕获指针再手动释放。
- HLS opaque 是借用指针（`&mut Vec<u8>`）：确保 `ts_buffer` 的存活期长于 `HlsOutputContext`。Rust borrow checker 在编译期保证这一点；若使用 `unsafe` 绕过（如 `Arc` 转 raw），需人工验证。 / HLS opaque is a borrowed pointer: ensure `ts_buffer` outlives `HlsOutputContext`. Rust borrow checker enforces this at compile time; if bypassed via `unsafe` (e.g., `Arc` to raw), verify manually.
- AVBSF/AVCodecContext 绑定缺失时使用本地 compat 声明（`bsf.rs`、`encoder.rs`）：保持 `#[repr(C)]` 字段顺序与 FFmpeg 头文件一致，升级 ffmpeg-sys-next 后优先替换为官方绑定。 / When bindings are missing, local compat declarations are used; keep `#[repr(C)]` layout in sync with FFmpeg headers and prefer official bindings after upgrading.

## Pipeline Integration Notes / 与 Pipeline 集成说明

- Keep FFmpeg pointer ownership inside media wrappers; pipeline should pass typed packets only. / FFmpeg 指针所有权应留在 media 包装层，pipeline 仅传递类型化包对象。
- HLS TS muxer 写入**内存缓冲区**（custom AVIO callback → `Vec<u8>`），不在 pipeline core task 中做磁盘 I/O。Segment 切分时一次性将 buffer 写入磁盘 → stage → MinIoSink 异步上传。 / HLS TS muxer writes to in-memory buffer (custom AVIO callback → `Vec<u8>`), no disk I/O in pipeline core task. On rollover, buffer is flushed to disk once → staged → MinIoSink async upload.
- 转码（`TranscodeProcessor`）持有 `Decoder` + `Encoder` + `Scaler` + `Frame`，全部串行化在单个 `Mutex` 后访问；AVCodecContext 非线程安全，禁止跨任务共享。 / Transcode holds Decoder+Encoder+Scaler+Frames behind a single `Mutex`; AVCodecContext is not thread-safe and must never be shared across tasks.
- RTP depacketization（`RtpDemuxContext`）在 `livestream-media` 内完成 FFmpeg 输入上下文生命周期管理，pipeline 的 `RtpDemuxProcessor` 仅持有类型化上下文对象。 / RTP demux context lifecycle stays inside `livestream-media`; the pipeline processor holds only the typed wrapper.
- If persistence stage creates additional output contexts, follow the same allocate-owner-drop pattern. / 若持久化阶段新增输出上下文，需遵循相同的分配-归属-释放模式。
