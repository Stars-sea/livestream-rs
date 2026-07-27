//! NAL unit data format enum — distinguishes AVCC (length-prefixed)
//! from Annex B (start-code delimited) H.264/H.265 bitstream data.
//!
//! At the pipeline level, all video `EncodedPacket`s carry a `NalData`
//! variant.  The source tags the format; downstream processors and sinks
//! match on it to decide whether conversion is needed.

use bytes::Bytes;

/// H.264 / H.265 NAL unit bitstream format.
#[derive(Clone, Debug, PartialEq)]
pub enum NalData {
    /// AVCC format — each NAL unit is prefixed with a 4-byte big-endian
    /// length (MP4 / FLV containers use this).
    Avcc(Bytes),
    /// Annex B format — each NAL unit is prefixed with `00 00 00 01`
    /// (or `00 00 01`) start code (TS / RTSP use this).
    AnnexB(Bytes),
}

impl NalData {
    /// Return the raw byte slice regardless of variant.
    pub fn as_bytes(&self) -> &Bytes {
        match self {
            NalData::Avcc(b) | NalData::AnnexB(b) => b,
        }
    }

    /// Length of the raw data in bytes.
    pub fn len(&self) -> usize {
        self.as_bytes().len()
    }

    /// Whether the raw data is empty.
    pub fn is_empty(&self) -> bool {
        self.as_bytes().is_empty()
    }

    /// Consume self and return the inner `Bytes`.
    pub fn into_bytes(self) -> Bytes {
        match self {
            NalData::Avcc(b) | NalData::AnnexB(b) => b,
        }
    }

    /// True if this data is in AVCC format.
    pub fn is_avcc(&self) -> bool {
        matches!(self, NalData::Avcc(_))
    }

    /// True if this data is in Annex B format.
    pub fn is_annex_b(&self) -> bool {
        matches!(self, NalData::AnnexB(_))
    }
}

impl From<NalData> for Bytes {
    fn from(d: NalData) -> Bytes {
        d.into_bytes()
    }
}
