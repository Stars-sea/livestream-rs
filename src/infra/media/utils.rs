use anyhow::Result;
use ffmpeg_sys_next::*;

use crate::infra::media::codec::{CodecParamsDescriptorTrait, CodecParamsMutPtrTrait};
use crate::infra::media::context::Context;
use crate::infra::media::packet::Packet;
use crate::infra::media::stream::StreamCollection;

pub(crate) fn make_packet(
    payload: &[u8],
    timestamp: u32,
    time_base: AVRational,
    is_keyframe: bool,
    stream_idx: usize,
) -> Result<Packet> {
    let mut pkt = Packet::alloc()?;

    let len = payload.len();
    unsafe {
        let pkt_ref = pkt.ptr();

        let ret = av_new_packet(pkt_ref, len as i32);
        if ret < 0 {
            anyhow::bail!("Failed to allocate packet memory");
        }
        std::ptr::copy_nonoverlapping(payload.as_ptr(), (*pkt_ref).data, len);

        let pts = av_rescale_q(
            timestamp as i64,
            AVRational { num: 1, den: 1000 },
            time_base,
        );

        (*pkt_ref).stream_index = stream_idx as i32;
        (*pkt_ref).pts = pts;
        (*pkt_ref).dts = pts;

        if is_keyframe {
            (*pkt_ref).flags |= ffmpeg_sys_next::AV_PKT_FLAG_KEY;
        }
    }

    Ok(pkt)
}

pub(crate) fn set_stream_extradata(
    ctx: &impl Context,
    media_type: AVMediaType,
    data: &[u8],
) -> Result<()> {
    fn find_stream_index_by_media_type(
        streams: &dyn StreamCollection,
        media_type: AVMediaType,
    ) -> Option<usize> {
        for stream in streams {
            if stream.codec_params_ptr().codec_type() == media_type {
                return Some(stream.index());
            }
        }

        None
    }

    if !ctx.available() {
        anyhow::bail!("Output format context is null");
    }

    let stream_idx = find_stream_index_by_media_type(ctx, media_type).ok_or_else(|| {
        anyhow::anyhow!("Target stream for media type {:?} not found", media_type)
    })?;

    unsafe {
        let format_ctx = ctx.ptr();
        let stream = *(*format_ctx).streams.add(stream_idx);
        if stream.is_null() || (*stream).codecpar.is_null() {
            anyhow::bail!("Target stream {} or codec parameters are null", stream_idx);
        }

        let codecpar = &mut *(*stream).codecpar;
        copy_extradata_to_codecpar(codecpar, data)
    }
}

pub(crate) fn copy_extradata_to_codecpar(
    codecpar: &mut impl CodecParamsMutPtrTrait,
    data: &[u8],
) -> Result<()> {
    unsafe {
        let codecpar_ptr = codecpar.mut_ptr();

        if !(*codecpar_ptr).extradata.is_null() {
            av_freep(&mut (*codecpar_ptr).extradata as *mut _ as *mut _);
            (*codecpar_ptr).extradata_size = 0;
        }

        let alloc_size = data
            .len()
            .checked_add(AV_INPUT_BUFFER_PADDING_SIZE as usize)
            .ok_or_else(|| anyhow::anyhow!("Extradata size overflow"))?;

        let extradata = av_mallocz(alloc_size) as *mut u8;
        if extradata.is_null() {
            anyhow::bail!("Failed to allocate codec extradata");
        }

        extradata.copy_from_nonoverlapping(data.as_ptr(), data.len());
        (*codecpar_ptr).extradata = extradata;
        (*codecpar_ptr).extradata_size = data.len() as i32;

        Ok(())
    }
}

pub(crate) fn validate_aac_audio_config(data: &[u8]) -> Result<()> {
    if data.len() < 2 {
        anyhow::bail!("AAC AudioSpecificConfig too short: {}", data.len());
    }

    let audio_object_type = (data[0] >> 3) & 0x1F;
    if audio_object_type == 0 {
        anyhow::bail!("Invalid AAC audio object type: 0");
    }

    Ok(())
}
