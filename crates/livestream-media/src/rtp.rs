//! FFmpeg RTP demuxer — in-memory, no network sockets.
//!
//! Opens the SDP via a custom AVIO, then swaps in a ring-buffer AVIO for
//! feeding raw RTP packets. Uses `sdpflags=custom_io` so FFmpeg reads RTP
//! through `ctx->pb` instead of opening real sockets.

use std::ffi::{CString, c_int, c_void};
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use bytes::Bytes;
use ffmpeg_sys_next::*;
use tracing::warn;

use crate::packet::Packet;
use livestream_core::types::Codec;

use std::collections::VecDeque;

struct RtpBuf {
    /// Queue of complete RTP packets, each with its framing preserved.
    packets: VecDeque<Bytes>,
    /// Currently-being-read packet + offset for partial AVIO reads.
    current: Option<(Bytes, usize)>,
    /// Upper bound on total bytes across all queued packets.
    max_bytes: usize,
    /// Whether we've already handled RTP sequence probation.
    probation_satisfied: bool,
}
pub struct RtpDemuxContext {
    fmt_ctx: *mut AVFormatContext,
    rtp_io: *mut AVIOContext,
    _inner: Arc<Mutex<RtpBuf>>,
}

// SAFETY: RtpDemuxContext is the sole owner of its raw FFmpeg pointers
// (fmt_ctx, rtp_io) and the associated AVFormatContext/AVIOContext, so moving
// it across threads is safe.
// Sync is deliberately NOT implemented: &self methods mutate FFmpeg state
// (e.g. av_read_frame), so concurrent &self calls from multiple threads are
// not safe. Callers must serialize access (the pipeline wraps this type in a
// Mutex — see RtpDemuxProcessor).
unsafe impl Send for RtpDemuxContext {}

// ── SDP-serving state ──

struct SdpState {
    data: Vec<u8>,
    pos: usize,
}

/// Release an SDP-serving AVIO context together with its internal buffer and
/// opaque `SdpState`.
///
/// # Safety
///
/// `sdp_io` must be a valid `AVIOContext` returned by `avio_alloc_context()`
/// whose buffer and opaque state have not yet been released. Note that
/// `avio_context_free()` on a read-only (write_flag == 0) AVIO does NOT free
/// the internal buffer, so the buffer is freed here.
unsafe fn free_sdp_io(mut sdp_io: *mut AVIOContext) {
    let opaque = unsafe { (*sdp_io).opaque };
    let internal_buf = unsafe { (*sdp_io).buffer };
    unsafe { avio_context_free(&mut sdp_io) };
    // avio_context_free may have NULL'd the buffer; free what we captured.
    if !internal_buf.is_null() {
        unsafe { av_free(internal_buf as *mut c_void) };
    }
    if !opaque.is_null() {
        let _ = unsafe { Box::from_raw(opaque as *mut SdpState) };
    }
}

impl RtpDemuxContext {
    pub fn new(sdp: &str) -> Result<Self> {
        // 1. Build SDP AVIO with a proper read callback.
        let sdp_state = Box::new(SdpState {
            data: sdp.as_bytes().to_vec(),
            pos: 0,
        });
        let sdp_opaque = Box::into_raw(sdp_state) as *mut c_void;

        let buf_sz = 4096usize;
        let buf = unsafe { av_malloc(buf_sz + AV_INPUT_BUFFER_PADDING_SIZE as usize) as *mut u8 };
        if buf.is_null() {
            let _ = unsafe { Box::from_raw(sdp_opaque as *mut SdpState) };
            anyhow::bail!("av_malloc SDP");
        }
        let mut sdp_io = unsafe {
            avio_alloc_context(
                buf,
                buf_sz as c_int,
                0,
                sdp_opaque,
                Some(sdp_read),
                None,
                None,
            )
        };
        if sdp_io.is_null() {
            unsafe { av_free(buf as *mut c_void) };
            let _ = unsafe { Box::from_raw(sdp_opaque as *mut SdpState) };
            anyhow::bail!("avio_alloc_context SDP");
        }

        // 2. Allocate format context, attach AVIO, force "sdp" format.
        let mut fmt_ctx = unsafe { avformat_alloc_context() };
        if fmt_ctx.is_null() {
            unsafe { free_sdp_io(sdp_io) };
            anyhow::bail!("avformat_alloc_context");
        }
        unsafe {
            (*fmt_ctx).pb = sdp_io;
            (*fmt_ctx).flags |= AVFMT_FLAG_CUSTOM_IO;
        }

        // 3. Open with explicit "sdp" format + custom_io flag.
        let iformat_name = CString::new("sdp").unwrap();
        let iformat = unsafe { av_find_input_format(iformat_name.as_ptr()) };
        if iformat.is_null() {
            unsafe {
                avio_context_free(&mut sdp_io);
                avformat_free_context(fmt_ctx);
            }
            anyhow::bail!("av_find_input_format('sdp') returned NULL — SDP demuxer not available");
        }

        let mut opts: *mut AVDictionary = null_mut();
        unsafe {
            av_dict_set(
                &mut opts,
                CString::new("sdp_flags").unwrap().as_ptr(),
                CString::new("custom_io").unwrap().as_ptr(),
                0,
            );
            // Disable reorder queue — with custom_io we feed one packet at a time.
            av_dict_set(
                &mut opts,
                CString::new("reorder_queue_size").unwrap().as_ptr(),
                CString::new("0").unwrap().as_ptr(),
                0,
            );
            // Disable max_delay to prevent reorder queue activation.
            av_dict_set(
                &mut opts,
                CString::new("max_delay").unwrap().as_ptr(),
                CString::new("0").unwrap().as_ptr(),
                0,
            );
        }

        let e = CString::new("").unwrap();
        let ret = unsafe { avformat_open_input(&mut fmt_ctx, e.as_ptr(), iformat, &mut opts) };
        unsafe { av_dict_free(&mut opts) };

        if ret < 0 {
            // On failure avformat_open_input() frees the context and writes
            // NULL into *ps (i.e. fmt_ctx) — so fmt_ctx must NOT be touched or
            // freed here. However, because we attach the SDP AVIO with
            // AVFMT_FLAG_CUSTOM_IO, FFmpeg does not close it on the failure
            // path (and avformat_free_context never frees pb), so the SDP
            // AVIO, its buffer, and its opaque SdpState are still owned by us
            // and must be released.
            unsafe { free_sdp_io(sdp_io) };
            anyhow::bail!("avformat_open_input: {}", crate::ffmpeg_error(ret));
        }

        // Success: SDP AVIO + state no longer needed.
        // avio_context_free(write_flag=0) does NOT free the internal buffer
        // (only write_flag=1 AVIOs free the buffer on close). Free buf separately.
        unsafe { free_sdp_io(sdp_io) };
        // Detach freed SDP AVIO from fmt_ctx to prevent dangling pointer.
        unsafe {
            (*fmt_ctx).pb = null_mut();
        }
        // Enable FFmpeg timestamp generation. The SDP demuxer may not set
        // PTS/DTS on decoded frames. GENPTS tells FFmpeg to generate missing
        // timestamps from DTS when available, preventing "non-monotonically
        // increasing dts" errors in downstream muxers.
        // SAFETY: fmt_ctx is a valid, initialized AVFormatContext from
        // avformat_alloc_context() + avformat_open_input(), not aliased.
        unsafe {
            (*fmt_ctx).flags |= AVFMT_FLAG_GENPTS;
        }

        let inner = Arc::new(Mutex::new(RtpBuf {
            packets: VecDeque::new(),
            current: None,
            max_bytes: 1024 * 1024,
            probation_satisfied: false,
        }));
        let rtp_opaque = Arc::into_raw(Arc::clone(&inner)) as *mut c_void;
        let ios = 64 * 1024 + AV_INPUT_BUFFER_PADDING_SIZE as usize;
        let ib = unsafe { av_malloc(ios) as *mut u8 };
        if ib.is_null() {
            let _ = unsafe { Arc::from_raw(rtp_opaque as *const Mutex<RtpBuf>) };
            unsafe { avformat_close_input(&mut fmt_ctx) };
            anyhow::bail!("av_malloc RTP");
        }
        let rtp_io = unsafe {
            avio_alloc_context(
                ib,
                (ios - AV_INPUT_BUFFER_PADDING_SIZE as usize) as c_int,
                1,
                rtp_opaque,
                Some(rtp_read),
                Some(rtp_write_discard),
                None,
            )
        };
        if rtp_io.is_null() {
            let _ = unsafe { Arc::from_raw(rtp_opaque as *const Mutex<RtpBuf>) };
            unsafe {
                av_free(ib as *mut c_void);
                avformat_close_input(&mut fmt_ctx);
            }
            anyhow::bail!("avio_alloc_context RTP");
        }

        unsafe { (*fmt_ctx).pb = rtp_io };

        Ok(Self {
            fmt_ctx,
            rtp_io,
            _inner: inner,
        })
    }

    pub fn feed(&self, data: &[u8]) -> Result<()> {
        // Poisoned-lock recovery: a panic inside the guard must not kill
        // the ingest task on the next packet (RTP hot path).
        let mut inner = self._inner.lock().unwrap_or_else(|e| e.into_inner());
        // RTP sequence probation: FFmpeg drops the first two packets
        // (requires two consecutive in-sequence packets). Duplicate the
        // first real packet so that the third dequeued packet (real pkt2
        // with seq = pkt1.seq + 1) passes the probation check.
        if !inner.probation_satisfied && !inner.packets.is_empty() {
            // First packet already queued; this is the second feed.
            // Clone the first packet → both probation rounds consume it,
            // then the real second packet passes.
            let first = inner.packets[0].clone();
            inner.packets.push_front(first);
            inner.probation_satisfied = true;
        }
        // Evict oldest packets until total bytes are under the cap.
        // Recompute current_bytes each iteration — the sum changes
        // as packets are popped.
        loop {
            let current_bytes: usize = inner.packets.iter().map(|p| p.len()).sum::<usize>()
                + inner.current.as_ref().map_or(0, |(p, _)| p.len());
            if current_bytes + data.len() <= inner.max_bytes {
                break;
            }
            if inner.packets.pop_front().is_none() {
                break;
            }
        }
        inner.packets.push_back(Bytes::copy_from_slice(data));
        Ok(())
    }

    pub fn read_frame(&self) -> Result<Option<(Packet, Codec, AVRational)>> {
        let mut av_pkt: AVPacket = unsafe { std::mem::zeroed() };
        unsafe { av_init_packet(&mut av_pkt) };

        let ret = unsafe { av_read_frame(self.fmt_ctx, &mut av_pkt) };
        if ret == AVERROR_EOF || ret == AVERROR(EAGAIN) {
            unsafe { av_packet_unref(&mut av_pkt) };
            return Ok(None);
        }
        if ret < 0 {
            unsafe { av_packet_unref(&mut av_pkt) };
            anyhow::bail!("av_read_frame: {}", crate::ffmpeg_error(ret));
        }

        // Validate stream_index before dereferencing.  A malformed or
        // truncated RTP stream could cause FFmpeg to return a stream index
        // beyond bounds.
        let nb_streams = unsafe { (*self.fmt_ctx).nb_streams as usize };
        if av_pkt.stream_index < 0 || av_pkt.stream_index as usize >= nb_streams {
            unsafe { av_packet_unref(&mut av_pkt) };
            anyhow::bail!(
                "av_read_frame returned stream_index {} but nb_streams is {}",
                av_pkt.stream_index,
                nb_streams,
            );
        }

        let (codec, tb) = unsafe {
            let sp = *(*self.fmt_ctx).streams.add(av_pkt.stream_index as usize);
            let st = &*sp;
            let c = st
                .codecpar
                .as_ref()
                .map(|p| codec_id_to_codec(p.codec_id))
                .unwrap_or(Codec::H264);
            (c, st.time_base)
        };
        let pkt = Packet::from_raw(av_pkt);
        Ok(Some((pkt, codec, tb)))
    }

    /// Extract codec extradata (SPS+PPS for H.264, ASC for AAC) from a stream.
    ///
    /// # Safety
    ///
    /// The caller must ensure `self.fmt_ctx` is valid and has been opened with
    /// `avformat_open_input`. The stream array and codecpar pointers are valid
    /// for the lifetime of the format context.
    pub fn extradata(&self, stream_index: usize) -> Option<bytes::Bytes> {
        // SAFETY: fmt_ctx is a valid, opened AVFormatContext. The stream array
        // and codecpar are owned by fmt_ctx and valid for its lifetime.
        unsafe {
            let nb = (*self.fmt_ctx).nb_streams as usize;
            if stream_index >= nb {
                return None;
            }
            let sp = *(*self.fmt_ctx).streams.add(stream_index);
            let st = &*sp;
            let par = st.codecpar.as_ref()?;
            if par.extradata_size > 0 && !par.extradata.is_null() {
                Some(bytes::Bytes::copy_from_slice(std::slice::from_raw_parts(
                    par.extradata,
                    par.extradata_size as usize,
                )))
            } else {
                None
            }
        }
    }

    /// Returns the codec for a given stream index.
    ///
    /// # Safety
    ///
    /// The caller must ensure `self.fmt_ctx` is valid and opened. The stream
    /// array is valid for the format context's lifetime.
    pub fn codec_for_stream(&self, stream_index: usize) -> Option<Codec> {
        // SAFETY: fmt_ctx is owned and valid; stream array and codecpar
        // are valid for its lifetime.
        unsafe {
            let nb = (*self.fmt_ctx).nb_streams as usize;
            if stream_index >= nb {
                return None;
            }
            let sp = *(*self.fmt_ctx).streams.add(stream_index);
            let st = &*sp;
            st.codecpar.as_ref().map(|p| codec_id_to_codec(p.codec_id))
        }
    }
    pub fn stream_count(&self) -> usize {
        unsafe { (*self.fmt_ctx).nb_streams as usize }
    }

    /// Returns the time_base for a given stream index.
    pub fn time_base_for_stream(&self, stream_index: usize) -> Option<AVRational> {
        unsafe {
            let nb = (*self.fmt_ctx).nb_streams as usize;
            if stream_index >= nb {
                return None;
            }
            let sp = *(*self.fmt_ctx).streams.add(stream_index);
            let st = &*sp;
            Some(st.time_base)
        }
    }
}

// ── C callbacks ──

extern "C" fn sdp_read(opaque: *mut c_void, buf: *mut u8, buf_size: c_int) -> c_int {
    if opaque.is_null() || buf.is_null() || buf_size <= 0 {
        return AVERROR(EINVAL);
    }
    // SAFETY: opaque was set to Box::into_raw(Box<SdpState>) in new().
    // It is valid for the lifetime of the AVIOContext.
    let s: &mut SdpState = unsafe { &mut *(opaque as *mut SdpState) };
    let remaining = s.data.len() - s.pos;
    if remaining == 0 {
        return 0; // EOF
    }
    let n = (buf_size as usize).min(remaining);
    // SAFETY: buf is FFmpeg-provided buffer, s.data[s.pos..] is valid.
    unsafe { std::ptr::copy_nonoverlapping(s.data.as_ptr().add(s.pos), buf, n) };
    s.pos += n;
    n as c_int
}

extern "C" fn rtp_read(opaque: *mut c_void, buf: *mut u8, buf_size: c_int) -> c_int {
    if opaque.is_null() || buf.is_null() || buf_size <= 0 {
        return AVERROR(EINVAL);
    }
    // SAFETY: opaque was set to Arc::into_raw(Arc<Mutex<RtpBuf>>) in new().
    // It is valid for the lifetime of the AVIOContext.
    let b: &Mutex<RtpBuf> = unsafe { &*(opaque as *const Mutex<RtpBuf>) };
    let mut g = match b.lock() {
        Ok(g) => g,
        Err(_) => return AVERROR_EOF,
    };
    let buf_size = buf_size as usize;
    // Continue a partial read if one is in-flight.
    if let Some((pkt, offset)) = &mut g.current {
        let remaining = pkt.len() - *offset;
        let n = buf_size.min(remaining);
        // SAFETY: buf is a valid FFmpeg-provided buffer of buf_size bytes.
        // pkt[offset..] is a valid slice with at least remaining bytes.
        unsafe { std::ptr::copy_nonoverlapping(pkt[*offset..].as_ptr(), buf, n) };
        *offset += n;
        if *offset >= pkt.len() {
            g.current = None;
        }
        return n as c_int;
    }
    // Start a new packet.
    match g.packets.pop_front() {
        Some(pkt) => {
            let n = buf_size.min(pkt.len());
            // SAFETY: buf is a valid FFmpeg-provided buffer of buf_size bytes.
            // pkt.as_ptr() is valid for pkt.len() bytes.
            unsafe { std::ptr::copy_nonoverlapping(pkt.as_ptr(), buf, n) };
            if n < pkt.len() {
                g.current = Some((pkt, n));
            }
            n as c_int
        }
        None => AVERROR(EAGAIN),
    }
}

extern "C" fn rtp_write_discard(_o: *mut c_void, _b: *const u8, s: c_int) -> c_int {
    s
}

fn codec_id_to_codec(id: AVCodecID) -> Codec {
    match id {
        AVCodecID::AV_CODEC_ID_H264 => Codec::H264,
        AVCodecID::AV_CODEC_ID_HEVC => Codec::H265,
        AVCodecID::AV_CODEC_ID_AAC => Codec::Aac,
        AVCodecID::AV_CODEC_ID_MP3 => Codec::Mp3,
        AVCodecID::AV_CODEC_ID_OPUS => Codec::Opus,
        AVCodecID::AV_CODEC_ID_AV1 => Codec::Av1,
        AVCodecID::AV_CODEC_ID_MJPEG => Codec::Mjpeg,
        _ => {
            warn!(codec_id=?id, "Unknown codec→H264");
            Codec::H264
        }
    }
}

impl Drop for RtpDemuxContext {
    fn drop(&mut self) {
        unsafe {
            if !self.fmt_ctx.is_null() {
                (*self.fmt_ctx).pb = null_mut();
                avformat_close_input(&mut self.fmt_ctx);
                self.fmt_ctx = null_mut();
            }
            free_rtp_io(&mut self.rtp_io);
        }
    }
}

/// Release an RTP demux AVIO context and the `Arc<Mutex<RtpBuf>>` stored in
/// its opaque field.
///
/// # Safety
///
/// `rtp_io` must be a valid `AVIOContext` created by this module whose opaque
/// pointer is a `Box`-ed `Arc<Mutex<RtpBuf>>` (or null).
unsafe fn free_rtp_io(rtp_io: &mut *mut AVIOContext) {
    if rtp_io.is_null() {
        return;
    }
    let opaque = unsafe { (**rtp_io).opaque };
    if !opaque.is_null() {
        let _ = unsafe { Arc::from_raw(opaque as *const Mutex<RtpBuf>) };
    }
    unsafe { avio_context_free(rtp_io) };
}

#[cfg(test)]
mod tests {
    use super::*;

    static SDP: &str = "\
        v=0\r\n\
        o=- 0 0 IN IP4 127.0.0.1\r\n\
        s=T\r\n\
        c=IN IP4 127.0.0.1\r\n\
        t=0 0\r\n\
        m=video 5000 RTP/AVP 96\r\n\
        a=rtpmap:96 H264/90000\r\n";

    #[test]
    fn create() {
        let c = RtpDemuxContext::new(SDP);
        assert!(c.is_ok(), "{:?}", c.err());
        assert!(c.unwrap().stream_count() > 0);
    }

    #[test]
    fn empty_read() {
        let c = RtpDemuxContext::new(SDP).unwrap();
        assert!(c.read_frame().unwrap().is_none());
    }

    #[test]
    fn feed() {
        let c = RtpDemuxContext::new(SDP).unwrap();
        c.feed(&[0x80, 0x60, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert!(!c._inner.lock().unwrap().packets.is_empty());
    }
}
