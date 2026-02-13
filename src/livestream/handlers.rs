use log::warn;
use tokio_stream::StreamExt;

use super::events::{
    StreamConnectedRx, StreamConnectedStream, StreamTerminateRx, StreamTerminateStream,
};
use crate::services::redis::RedisClient;

pub(super) async fn stream_connected_handler(rx: StreamConnectedRx, redis: RedisClient) {
    let mut stream = StreamConnectedStream::new(rx);

    while let Some(Ok(connected)) = stream.next().await {
        println!("Received stream connected event: {:?}", connected);

        let result = redis
            .publish_stream_event(connected.live_id(), "connected")
            .await;
        if let Err(e) = result {
            warn!("Failed to publish stream event: {}", e);
        }
    }
}

pub(super) async fn stream_terminate_handler(rx: StreamTerminateRx, redis: RedisClient) {
    let mut stream = StreamTerminateStream::new(rx);

    while let Some(Ok(connected)) = stream.next().await {
        println!("Received stream terminate event: {:?}", connected);

        let result = redis
            .publish_stream_event(connected.live_id(), "terminate")
            .await;
        if let Err(e) = result {
            warn!("Failed to publish stream event: {}", e);
        }
    }
}
