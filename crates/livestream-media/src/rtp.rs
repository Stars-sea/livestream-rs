//! FFmpeg RTP demuxer — in-memory, no network sockets.
//!
//! Opens the SDP via a custom AVIO, then swaps in a ring-buffer AVIO for
//! feeding raw RTP packets. Uses `sdpflags=custom_io` so FFmpeg reads RTP
//! through `ctx->pb` instead of opening real sockets.

use std::ffi::{CString, c_int, c_void};
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use bytes::BytesMut;
use ffmpeg_sys_next::*;
use tracing::warn;

use crate::packet::Packet;
use livestream_core::types::Codec;

// ── RTP ring buffer + state ──

struct RtpBuf {
    /// BytesMut: O(1) split_to (advances internal pointer) for reading,
    /// O(1) extend_from_slice (single memcpy) for writing.
    buf: BytesMut,
    max: usize,
}

pub struct RtpDemuxContext {
    fmt_ctx: *mut AVFormatContext,
    rtp_io: *mut AVIOContext,
    _inner: Arc<Mutex<RtpBuf>>,
}

unsafe impl Send for RtpDemuxContext {}
unsafe impl Sync for RtpDemuxContext {}

// ── SDP-serving state ──

struct SdpState {
    data: Vec<u8>,
    pos: usize,
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
            unsafe { avio_context_free(&mut sdp_io) };
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
        let k = CString::new("sdp_flags").unwrap();
        let v = CString::new("custom_io").unwrap();
        unsafe {
            av_dict_set(&mut opts, k.as_ptr(), v.as_ptr(), 0);
        }

        let e = CString::new("").unwrap();
        let ret = unsafe { avformat_open_input(&mut fmt_ctx, e.as_ptr(), iformat, &mut opts) };
        unsafe { av_dict_free(&mut opts) };

        // SDP AVIO + state no longer needed.
        // avio_context_free(write_flag=0) does NOT free the internal buffer
        // (only write_flag=1 AVIOs free the buffer on close). Free buf separately.
        unsafe {
            let opaque = (*sdp_io).opaque;
            let internal_buf = (*sdp_io).buffer;
            avio_context_free(&mut sdp_io);
            // avio_context_free may have NULL'd the buffer; free what we captured.
            if !internal_buf.is_null() {
                av_free(internal_buf as *mut c_void);
            }
            if !opaque.is_null() {
                let _ = Box::from_raw(opaque as *mut SdpState);
            }
        }
        // Detach freed SDP AVIO from fmt_ctx to prevent dangling pointer.
        unsafe {
            (*fmt_ctx).pb = null_mut();
        }

        if ret < 0 {
            unsafe { avformat_free_context(fmt_ctx) };
            anyhow::bail!("avformat_open_input: {}", crate::ffmpeg_error(ret));
        }

        // 4. Build the RTP ring-buffer AVIO (read-write).
        let inner = Arc::new(Mutex::new(RtpBuf {
            buf: BytesMut::with_capacity(256 * 1024),
            max: 1024 * 1024,
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
        let mut inner = self._inner.lock().unwrap();
        let to_add = data.len().min(inner.max.saturating_sub(inner.buf.len()));
        if to_add > 0 {
            inner.buf.extend_from_slice(&data[..to_add]);
        }
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

    pub fn stream_count(&self) -> usize {
        unsafe { (*self.fmt_ctx).nb_streams as usize }
    }
}

// ── C callbacks ──

extern "C" fn sdp_read(opaque: *mut c_void, buf: *mut u8, buf_size: c_int) -> c_int {
    if opaque.is_null() || buf.is_null() || buf_size <= 0 {
        return AVERROR(EINVAL);
    }
    let s: &mut SdpState = unsafe { &mut *(opaque as *mut SdpState) };
    let remaining = s.data.len() - s.pos;
    if remaining == 0 {
        return 0; // EOF
    }
    let n = (buf_size as usize).min(remaining);
    unsafe { std::ptr::copy_nonoverlapping(s.data.as_ptr().add(s.pos), buf, n) };
    s.pos += n;
    n as c_int
}

extern "C" fn rtp_read(opaque: *mut c_void, buf: *mut u8, buf_size: c_int) -> c_int {
    if opaque.is_null() || buf.is_null() || buf_size <= 0 {
        return AVERROR(EINVAL);
    }
    let b: &Mutex<RtpBuf> = unsafe { &*(opaque as *const Mutex<RtpBuf>) };
    let mut g = match b.lock() {
        Ok(g) => g,
        Err(_) => return AVERROR_EOF,
    };
    if g.buf.is_empty() {
        return AVERROR(EAGAIN);
    }
    let n = (buf_size as usize).min(g.buf.len());
    // split_to is O(1): advances an internal offset, no copy of the remaining data.
    let chunk = g.buf.split_to(n);
    unsafe { std::ptr::copy_nonoverlapping(chunk.as_ptr(), buf, n) };
    n as c_int
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
            if !self.rtp_io.is_null() {
                let o = (*self.rtp_io).opaque;
                if !o.is_null() {
                    let _ = Arc::from_raw(o as *const Mutex<RtpBuf>);
                }
                avio_context_free(&mut self.rtp_io);
                self.rtp_io = null_mut();
            }
        }
    }
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
        assert!(c._inner.lock().unwrap().buf.len() > 0);
    }
}
