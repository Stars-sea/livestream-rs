//! FFmpeg AVPacket RAII wrapper.
//!
//! # Safety
//!
//! `Packet` wraps `*mut AVPacket`. Clone uses `av_packet_clone` (deep copy);
//! Drop calls `av_packet_free`.

use anyhow::Result;
use ffmpeg_sys_next::*;

/// Wrapper for FFmpeg `AVPacket` with safe operations.
///
/// Manages the lifecycle of `AVPacket` through RAII:
/// - Allocated in `alloc()` via `av_packet_alloc()`
/// - Freed in `Drop` via `av_packet_free()`
/// - Deep-copied in `Clone` via `av_packet_clone()`
#[derive(Debug)]
pub struct Packet {
    packet: *mut AVPacket,
}

// SAFETY: AVPacket is thread-safe — FFmpeg's av_packet_* functions do not
// have thread affinity. The pointer is only accessed through &self/&mut self
// (Rust borrow rules protect against races).
unsafe impl Send for Packet {}
unsafe impl Sync for Packet {}

impl Packet {
    /// Allocate an empty `AVPacket`.
    ///
    /// Only allocates the `AVPacket` struct, not the data buffer.
    /// Use `set_data()` to fill with payload data.
    pub fn alloc() -> Result<Self> {
        // SAFETY: av_packet_alloc returns a heap-allocated AVPacket or NULL.
        let pkt = unsafe { av_packet_alloc() };
        if pkt.is_null() {
            anyhow::bail!("Failed to allocate AVPacket");
        }
        Ok(Self { packet: pkt })
    }

    // ── Pointer access ──

    /// Returns an immutable pointer to the underlying `AVPacket`.
    pub fn as_ptr(&self) -> *const AVPacket {
        self.packet
    }

    /// Returns a mutable pointer to the underlying `AVPacket`.
    ///
    /// # Safety
    ///
    /// Caller must ensure the pointer is not used after `Drop`.
    pub fn as_mut_ptr(&mut self) -> *mut AVPacket {
        self.packet
    }

    // ── Data I/O ──

    /// Write payload data into the packet (copies from `data`).
    ///
    /// Safe to call multiple times — previous data is freed first.
    /// Uses `av_new_packet` internally to allocate a refcounted buffer.
    pub fn set_data(&mut self, data: &[u8]) -> Result<()> {
        let len = data.len();
        // SAFETY: av_packet_unref releases any existing buffer before we
        // allocate a new one. av_new_packet then allocates fresh memory.
        unsafe {
            av_packet_unref(self.packet);
            let ret = av_new_packet(self.packet, len as i32);
            if ret < 0 {
                anyhow::bail!(
                    "Failed to allocate packet memory ({} bytes): {}",
                    len,
                    crate::ffmpeg_error(ret)
                );
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), (*self.packet).data, len);
        }
        Ok(())
    }

    /// Borrow the packet's data as a byte slice.
    ///
    /// Returns an empty slice if the packet has no data.
    pub fn data(&self) -> &[u8] {
        // SAFETY: AVPacket data is valid for the packet's lifetime.
        unsafe {
            let size = (*self.packet).size;
            if size > 0 && !(*self.packet).data.is_null() {
                std::slice::from_raw_parts((*self.packet).data, size as usize)
            } else {
                &[]
            }
        }
    }

    // ── Timestamp manipulation ──

    /// Rescale packet timestamps from one timebase to another.
    pub fn rescale_ts(&mut self, from: AVRational, to: AVRational) {
        // SAFETY: av_packet_rescale_ts is a pure arithmetic transform.
        unsafe {
            av_packet_rescale_ts(self.packet, from, to);
        }
    }

    // ── Field accessors ──

    pub fn stream_idx(&self) -> usize {
        // SAFETY: reading a field from a valid AVPacket pointer.
        unsafe { (*self.packet).stream_index as usize }
    }

    pub fn set_stream_idx(&mut self, idx: i32) {
        // SAFETY: writing a field on a valid mutable AVPacket pointer.
        unsafe {
            (*self.packet).stream_index = idx;
        }
    }

    pub fn size(&self) -> i32 {
        // SAFETY: reading a field from a valid AVPacket pointer.
        unsafe { (*self.packet).size }
    }

    pub fn pts(&self) -> Option<i64> {
        // SAFETY: reading pts from a valid AVPacket pointer.
        unsafe {
            let pts = (*self.packet).pts;
            if pts != AV_NOPTS_VALUE {
                Some(pts)
            } else {
                None
            }
        }
    }

    pub fn set_pts(&mut self, pts: i64) {
        // SAFETY: writing pts on a valid mutable AVPacket pointer.
        unsafe {
            (*self.packet).pts = pts;
        }
    }

    pub fn dts(&self) -> Option<i64> {
        // SAFETY: reading dts from a valid AVPacket pointer.
        unsafe {
            let dts = (*self.packet).dts;
            if dts != AV_NOPTS_VALUE {
                Some(dts)
            } else {
                None
            }
        }
    }

    pub fn set_dts(&mut self, dts: i64) {
        // SAFETY: writing dts on a valid mutable AVPacket pointer.
        unsafe {
            (*self.packet).dts = dts;
        }
    }

    pub fn duration(&self) -> Option<i64> {
        // SAFETY: reading duration from a valid AVPacket pointer.
        unsafe {
            let dur = (*self.packet).duration;
            if dur > 0 { Some(dur) } else { None }
        }
    }

    pub fn is_key_frame(&self) -> bool {
        // SAFETY: reading flags from a valid AVPacket pointer.
        unsafe { (*self.packet).flags & AV_PKT_FLAG_KEY != 0 }
    }

    pub fn set_key_frame(&mut self, is_key: bool) {
        // SAFETY: writing flags on a valid mutable AVPacket pointer.
        unsafe {
            if is_key {
                (*self.packet).flags |= AV_PKT_FLAG_KEY;
            } else {
                (*self.packet).flags &= !AV_PKT_FLAG_KEY;
            }
        }
    }
}

// ── Clone (deep copy) ──

impl Clone for Packet {
    fn clone(&self) -> Self {
        // SAFETY: av_packet_clone creates a deep copy of the AVPacket,
        // including refcounted side-data. Panics on allocation failure
        // (process is already out of memory).
        let pkt_ptr = unsafe { av_packet_clone(self.packet) };
        assert!(
            !pkt_ptr.is_null(),
            "av_packet_clone returned null (allocation failure)"
        );
        Self { packet: pkt_ptr }
    }
}

// ── Drop ──

impl Drop for Packet {
    fn drop(&mut self) {
        // SAFETY: av_packet_free frees the AVPacket and its refcounted data.
        // It also NULLs the pointer.
        unsafe {
            av_packet_free(&mut self.packet);
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_and_drop() {
        let _pkt = Packet::alloc().expect("alloc should succeed");
    }

    #[test]
    fn set_data_roundtrip() {
        let mut pkt = Packet::alloc().unwrap();
        let payload: &[u8] = &[0x00, 0x00, 0x01, 0x67, 0x42, 0x00];
        pkt.set_data(payload).unwrap();
        assert_eq!(pkt.data(), payload);
        assert_eq!(pkt.size(), 6);
    }

    #[test]
    fn clone_is_independent() {
        let mut pkt = Packet::alloc().unwrap();
        pkt.set_pts(100);
        pkt.set_key_frame(true);

        let mut cloned = pkt.clone();
        cloned.set_pts(200);
        cloned.set_key_frame(false);

        assert_eq!(pkt.pts(), Some(100));
        assert!(pkt.is_key_frame());
        assert_eq!(cloned.pts(), Some(200));
        assert!(!cloned.is_key_frame());
    }

    #[test]
    fn rescale_ts_correct() {
        let mut pkt = Packet::alloc().unwrap();
        pkt.set_pts(1000);
        pkt.rescale_ts(
            AVRational { num: 1, den: 1000 },
            AVRational { num: 1, den: 90000 },
        );
        assert_eq!(pkt.pts(), Some(90000));
    }

    #[test]
    fn flags_and_fields() {
        let mut pkt = Packet::alloc().unwrap();
        pkt.set_stream_idx(0);
        pkt.set_pts(42);
        pkt.set_dts(40);
        pkt.set_key_frame(true);

        assert_eq!(pkt.stream_idx(), 0);
        assert_eq!(pkt.pts(), Some(42));
        assert_eq!(pkt.dts(), Some(40));
        assert!(pkt.is_key_frame());
    }

    #[test]
    fn empty_packet_has_no_pts() {
        let pkt = Packet::alloc().unwrap();
        assert_eq!(pkt.pts(), None);
        assert_eq!(pkt.dts(), None);
        assert!(pkt.data().is_empty());
    }
}
