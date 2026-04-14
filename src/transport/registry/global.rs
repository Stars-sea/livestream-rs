use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::registry::global_registry as global_session;
use crate::transport::registry::state::{SessionDescriptor, SessionState};

pub async fn register_session(session: SessionDescriptor, ct: CancellationToken) -> Result<()> {
    let session = Arc::new(RwLock::new(session));
    global_session().await.register_session(session, ct).await
}

pub async fn get_session(stream_key: &str) -> Option<Arc<RwLock<SessionDescriptor>>> {
    global_session().await.get_session(stream_key)
}

pub async fn get_cancel_token(stream_key: &str) -> Option<CancellationToken> {
    global_session().await.get_cancel_token(stream_key)
}

pub async fn get_session_state(stream_key: &str) -> Option<SessionState> {
    global_session().await.get_state(stream_key).await
}

pub async fn get_session_descriptor(stream_key: &str) -> Option<SessionDescriptor> {
    global_session().await.get_descriptor(stream_key).await
}

pub async fn list_session_descriptors() -> Vec<SessionDescriptor> {
    global_session().await.list_descriptors().await
}

pub async fn update_session_state(stream_key: &str, new_state: SessionState) -> Result<()> {
    global_session()
        .await
        .update_state(stream_key, new_state)
        .await
}
