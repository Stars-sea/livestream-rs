# FFmpeg Unsafe Ownership Map

This document defines ownership boundaries and wrapper rules for all unsafe FFmpeg interactions in this repository.

## Ownership Map

| Rust wrapper or scope | Native resource(s) | Acquire site | Release site | Ownership rule |
|---|---|---|---|---|
| InputContext | AVFormatContext (input) | media/input.rs alloc_input_context + avformat_open_input | media/input.rs free_input_context via avformat_close_input | InputContext has unique ownership of AVFormatContext pointer.
| InputContext interrupt callback opaque | Arc<AtomicBool> raw pointer in AVIOInterruptCB.opaque | media/input.rs alloc_input_context via Arc::into_raw | media/input.rs free_input_context via Arc::from_raw exactly once | into_raw/from_raw must stay paired 1:1.
| FlvOutputContext | AVFormatContext (output) | media/format/flv_output.rs create + alloc_output_ctx | media/format/flv_output.rs Drop via avformat_free_context | FlvOutputContext owns output format context exclusively.
| FlvOutputContext custom AVIO context | AVIOContext + AVIO buffer | media/format/flv_output.rs open_io via avio_alloc_context + av_malloc | media/format/flv_output.rs Drop via av_freep + avio_context_free | AVIO buffer must be released before AVIOContext free.
| FlvOutputContext AVIO opaque | Arc<FlvAvioOpaque> raw pointer in AVIOContext.opaque | media/format/flv_output.rs create via Arc::into_raw | media/format/flv_output.rs Drop via Arc::from_raw exactly once | Custom IO opaque must be reclaimed before avio_context_free.
| Stream | Borrowed AVStream pointer from AVFormatContext | media/context.rs stream() | Borrow only, no release | Stream never owns AVStream and must not free it.
| Context trait accessors | Borrowed pointers from AVFormatContext | media/context.rs | Borrow only, no release | Context methods do not transfer ownership.
| FFmpeg global logging callback | Function pointer registration | media/log.rs init_logging | process lifetime | Callback must not capture non-static references.

## Wrapper Conventions

1. Raw pointer ownership is unique unless explicitly marked borrowed.
2. Every Arc::into_raw must have exactly one matching Arc::from_raw in a deterministic cleanup path.
3. Drop implementations must be idempotent: check null pointers before free and null out internal pointers after free.
4. Functions returning borrowed FFmpeg pointers must document lifetime constraints and never free borrowed memory.
5. Unsafe blocks should be minimal and adjacent to the specific FFI call they justify.
6. On partial initialization failure, free all already-acquired native resources before returning Err.
7. Do not store opaque raw pointers without documenting owner and free site in this map.

## Audit Checklist for New Unsafe FFmpeg Code

- Identify owner type and single release function.
- Document acquire and release site in this file.
- Add or update Drop path tests when ownership logic changes.
- Confirm no double-free path exists under early-return errors.
- Confirm no leak path exists under early-return errors.
