use anyhow::Result;

use crate::infra::media::context::HlsOutputContext;
use crate::infra::media::packet::{FlvTag, FlvTagPacketizer, Packet};
use crate::infra::media::stream::StreamCollection;

#[derive(Default)]
pub(super) enum IngestState {
    #[default]
    Pending,
    PacketDriven,
    FlvDriven(FlvTagPacketizer),
}

impl IngestState {
    pub(super) fn start_packet_ingest(&mut self) {
        if matches!(self, Self::Pending) {
            *self = Self::PacketDriven;
        }
    }

    pub(super) fn packetize_flv_tag(
        &mut self,
        tag: &FlvTag,
        streams: &dyn StreamCollection,
    ) -> Result<Vec<Packet>> {
        if matches!(self, Self::Pending) {
            *self = Self::FlvDriven(FlvTagPacketizer::new());
        }

        if let Self::FlvDriven(packetizer) = self {
            packetizer.packetize(tag, streams)
        } else {
            anyhow::bail!("Mixed stream ingest types detected: expected FLV-driven state.");
        }
    }

    pub(super) fn apply_codec_extradata(&self, ctx: &HlsOutputContext) -> Result<()> {
        if let Self::FlvDriven(packetizer) = self {
            packetizer.apply_codec_extradata(ctx)?;
        }
        Ok(())
    }
}
