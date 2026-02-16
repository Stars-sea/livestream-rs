//! Core SRT stream pulling and segmentation logic.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::events::*;
use super::stream_info::StreamInfo;

use crate::core::context::Context;
use crate::core::input::SrtInputContext;
use crate::core::output::TsOutputContext;
use crate::core::packet::Packet;

use anyhow::{Result, anyhow};
use log::{debug, info, warn};

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
    connected_tx: StreamConnectedTx,
    segment_complete_tx: SegmentCompleteTx,
    mut stop_rx: StopStreamRx,
    stop_signal: Arc<AtomicBool>,
    info: StreamInfo,
) -> Result<()> {
    let live_id = info.live_id();
    let cache_dir = info.cache_dir();
    let segment_duration = info.segment_duration() as f64;

    let input_ctx = SrtInputContext::open(&info.listener_url(), stop_signal)?;

    let mut segment_id: u64 = 1;
    let mut output_ctx = TsOutputContext::create_segment(&cache_dir, &input_ctx, segment_id)?;

    let mut last_start_pts = 0;
    let mut stream_started_notified = false;

    while !stop_rx.try_recv().is_ok_and(|id| id.live_id() == live_id) {
        let packet = Packet::alloc()?;
        let bytes_read = packet.read_safely(&input_ctx);

        if bytes_read == 0 {
            debug!("Stream ended for {}", live_id);
            break;
        }

        // Send stream started event on first successful packet read
        if !stream_started_notified {
            if let Err(e) = connected_tx.send(OnStreamConnected::new(live_id)) {
                warn!("Failed to send stream connected event: {}", e);
            }
            stream_started_notified = true;
        }

        if should_segment(&packet, &input_ctx, segment_duration, &mut last_start_pts) {
            output_ctx.release_and_close()?;

            if let Err(e) =
                segment_complete_tx.send(OnSegmentComplete::from_ctx(&live_id, &output_ctx))
            {
                warn!("Failed to send segment complete event: {}", e);
            }

            segment_id += 1;
            output_ctx = TsOutputContext::create_segment(&cache_dir, &input_ctx, segment_id)?;
        }

        packet.rescale_ts_for_ctx(&input_ctx, &output_ctx);
        packet.write(&output_ctx)?;
    }

    output_ctx.release_and_close()?;

    if let Err(e) = segment_complete_tx.send(OnSegmentComplete::from_ctx(&live_id, &output_ctx)) {
        warn!("Failed to send final segment complete event: {}", e);
    }

    Ok(())
}

/// Wrapper function that handles stream termination event.
pub(super) async fn pull_srt_loop(
    connected_tx: StreamConnectedTx,
    terminate_tx: StreamTerminateTx,
    segment_complete_tx: SegmentCompleteTx,
    stop_stream_tx: StopStreamTx,
    info: StreamInfo,
) -> Result<()> {
    let mut connected_rx = connected_tx.subscribe();
    let stop_signal = Arc::new(AtomicBool::new(false));

    let cloned_info = info.clone();
    let cloned_stop_signal = stop_signal.clone();
    let stop_rx = stop_stream_tx.subscribe();
    let handle = tokio::task::spawn_blocking(move || {
        pull_srt_loop_impl(
            connected_tx,
            segment_complete_tx,
            stop_rx,
            cloned_stop_signal,
            cloned_info,
        )
    });

    let mut stop_rx = stop_stream_tx.subscribe();

    loop {
        tokio::select! {
            stop_msg = stop_rx.recv() => {
                if let Ok(stop_info) = stop_msg && stop_info.live_id() == info.live_id() {
                    info!("Received stop signal for stream {}", info.live_id());
                    // handle.abort(); // `spawn_blocking` cannot be aborted
                    stop_signal.store(true, Ordering::Relaxed);
                    break;
                }
            }

            connected_msg = connected_rx.recv() => {
                if let Ok(connected_info) = connected_msg && connected_info.live_id() == info.live_id() {
                    info!("Stream {} connected", info.live_id());
                    break;
                }
            }
        }
    }

    let result = handle.await;
    let error = match result {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(e) if e.is_cancelled() => Some("Stream pulling task was cancelled".to_string()),
        Err(e) => Some(format!("Stream pulling task panicked: {:?}", e)),
    };

    if let Err(e) = terminate_tx.send(OnStreamTerminate::new(
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
