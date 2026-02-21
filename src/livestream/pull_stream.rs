//! Core SRT stream pulling and segmentation logic.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::events::*;
use super::stream_info::StreamInfo;

use crate::core::context::Context;
use crate::core::input::SrtInputContext;
use crate::core::output::{HlsOutputContext, RtmpOutputContext};
use crate::core::packet::Packet;

use anyhow::{Result, anyhow};
use log::{info, warn};
use tokio::sync::broadcast;

/// Determines if a new segment should be created based on packet and duration.
fn should_segment(
    packet: &Packet,
    input_ctx: &impl Context,
    duration: f64,
    last_pts: &mut i64,
) -> bool {
    let current_pts = packet.pts().unwrap_or(0);
    let current_stream = input_ctx.stream(packet.stream_idx()).unwrap();
    if !current_stream.is_video_stream() || !packet.is_key_frame() {
        return false;
    }

    if (current_pts - *last_pts) as f64 * current_stream.time_base_f64() > duration {
        *last_pts = current_pts;
        return true;
    }

    false
}

/// Main loop for pulling SRT stream, segmenting, and writing to disk.
fn pull_srt_loop_impl(
    stream_msg_tx: broadcast::Sender<StreamMessage>,
    mut control_rx: broadcast::Receiver<StreamControlMessage>,
    stop_signal: Arc<AtomicBool>,
    info: StreamInfo,
) -> Result<()> {
    let live_id = info.live_id();
    let cache_dir = info.cache_dir();
    let segment_duration = info.segment_duration() as f64;

    let input_ctx = SrtInputContext::open(&info.srt_listener_url(), stop_signal)?;

    // TODO: FIXME
    let rtmp_output = RtmpOutputContext::create(info.rtmp_listener_url(), &input_ctx)?;

    let mut segment_id: u64 = 1;
    let mut hls_output = HlsOutputContext::create_segment(&cache_dir, &input_ctx, segment_id)?;

    let mut last_start_pts = 0;
    let mut stream_started_notified = false;

    while !control_rx
        .try_recv()
        .is_ok_and(|msg| msg.is_stop_stream() && msg.live_id() == live_id)
    {
        let packet = Packet::alloc()?;
        if packet.read_safely(&input_ctx) == 0 {
            info!("Stream ended for {}", live_id);
            break;
        }

        let cloned_packet = packet.clone();

        // Send stream started event on first successful packet read
        if !stream_started_notified {
            if let Err(e) = stream_msg_tx.send(StreamMessage::stream_started(live_id)) {
                warn!("Failed to send stream connected event: {}", e);
            }
            stream_started_notified = true;
        }

        if should_segment(&packet, &input_ctx, segment_duration, &mut last_start_pts) {
            on_segment_complete(&stream_msg_tx, live_id, &hls_output);

            segment_id += 1;
            hls_output = HlsOutputContext::create_segment(&cache_dir, &input_ctx, segment_id)?;
        }

        packet.rescale_ts_for_ctx(&input_ctx, &rtmp_output);
        if let Err(e) = packet.write(&rtmp_output) {
            warn!("Failed to write packet to FLV output: {}", e);
            on_segment_complete(&stream_msg_tx, live_id, &hls_output);
            return Err(anyhow!("Failed to write packet to FLV output: {}", e));
        }

        cloned_packet.rescale_ts_for_ctx(&input_ctx, &hls_output);
        if let Err(e) = cloned_packet.write(&hls_output) {
            warn!("Failed to write packet to TS output: {}", e);
            on_segment_complete(&stream_msg_tx, live_id, &hls_output);
            return Err(anyhow!("Failed to write packet to TS output: {}", e));
        }
    }

    on_segment_complete(&stream_msg_tx, live_id, &hls_output);

    fn on_segment_complete(
        stream_msg_tx: &broadcast::Sender<StreamMessage>,
        live_id: &str,
        hls_output: &HlsOutputContext,
    ) {
        let event = StreamMessage::segment_complete(live_id, hls_output.path());
        if let Err(e) = stream_msg_tx.send(event) {
            warn!("Failed to send final segment complete event: {}", e);
        }
    }

    Ok(())
}

/// Wrapper function that handles stream termination event.
pub(super) async fn pull_srt_loop(
    stream_msg_tx: broadcast::Sender<StreamMessage>,
    control_tx: broadcast::Sender<StreamControlMessage>,
    info: StreamInfo,
) -> Result<()> {
    let stop_signal = Arc::new(AtomicBool::new(false));

    let cloned_info = info.clone();
    let cloned_stop_signal = stop_signal.clone();
    let cloned_stream_msg_tx = stream_msg_tx.clone();
    let control_rx = control_tx.subscribe();
    let handle = tokio::task::spawn_blocking(move || {
        pull_srt_loop_impl(
            cloned_stream_msg_tx,
            control_rx,
            cloned_stop_signal,
            cloned_info,
        )
    });

    let mut control_rx = control_tx.subscribe();

    while !handle.is_finished() {
        if let Ok(msg) = control_rx.try_recv()
            && msg.is_stop_stream()
            && msg.live_id() == info.live_id()
        {
            info!("Received stop signal for stream {}", info.live_id());
            // handle.abort(); // `spawn_blocking` cannot be aborted
            stop_signal.store(true, Ordering::Relaxed);
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let error = match handle.await {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(e) if e.is_cancelled() => Some("Stream pulling task was cancelled".to_string()),
        Err(e) => Some(format!("Stream pulling task panicked: {:?}", e)),
    };

    if let Err(e) = stream_msg_tx.send(StreamMessage::stream_stopped(
        info.live_id(),
        error,
        info.cache_dir(),
    )) {
        warn!("Failed to send stream terminate event: {:?}", e);
        Err(anyhow!("Failed to send stream terminate event: {:?}", e))
    } else {
        Ok(())
    }
}
