//! gRPC client helpers — connect, verify, stop.

use anyhow::Context;
use tonic::transport::Endpoint;

use crate::proto::{
    GetLivestreamInfoRequest, GetServiceInfoRequest, SessionStatus, StopLivestreamRequest,
    livestream_client::LivestreamClient,
};

/// Ports returned by the livestream service.
#[derive(Debug, Clone, Copy)]
pub struct ServicePorts {
    pub rtmp: u16,
    pub rtsp: u16,
    pub http_flv: u16,
}

/// Connect to the gRPC endpoint and fetch service ports.
pub async fn connect_and_get_info(
    grpc_addr: &str,
) -> anyhow::Result<(LivestreamClient<tonic::transport::Channel>, ServicePorts)> {
    let channel = Endpoint::from_shared(grpc_addr.to_string())
        .context("invalid gRPC endpoint")?
        .connect()
        .await
        .context("gRPC connection failed")?;
    let mut client = LivestreamClient::new(channel);

    let svc = client
        .get_service_info(GetServiceInfoRequest {})
        .await
        .context("GetServiceInfo failed")?
        .into_inner();

    Ok((
        client,
        ServicePorts {
            rtmp: svc.rtmp_port as u16,
            rtsp: svc.rtsp_port as u16,
            http_flv: svc.http_flv_port as u16,
        },
    ))
}

/// Verify a stream has connected successfully.
pub async fn verify_connected(
    client: &mut LivestreamClient<tonic::transport::Channel>,
    live_id: &str,
) -> anyhow::Result<()> {
    let desc = client
        .get_livestream_info(GetLivestreamInfoRequest {
            live_id: live_id.to_string(),
        })
        .await
        .context("GetLivestreamInfo failed")?
        .into_inner()
        .descriptor
        .context("no descriptor")?;
    if desc.status != SessionStatus::Connected as i32 {
        anyhow::bail!(
            "stream {live_id} is not connected: status = {} (expected {} = CONNECTED)",
            desc.status,
            SessionStatus::Connected as i32,
        );
    }
    tracing::info!(live_id = %live_id, status = desc.status, "stream verified");
    Ok(())
}

/// Stop a livestream via gRPC.
pub async fn stop_livestream(
    client: &mut LivestreamClient<tonic::transport::Channel>,
    live_id: &str,
) {
    let _ = client
        .stop_livestream(StopLivestreamRequest {
            live_id: live_id.to_string(),
        })
        .await;
}
