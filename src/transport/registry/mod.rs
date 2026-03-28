mod cancel_token;
mod session;

pub mod global {
    use std::sync::Arc;

    use anyhow::Result;
    use tokio::sync::RwLock;
    use tokio_util::sync::CancellationToken;

    use super::cancel_token::global_registry as global_tokens;
    use super::session::global_registry as global_session;
    use crate::transport::{SessionDescriptor, SessionState};

    pub async fn register_session(session: Arc<RwLock<SessionDescriptor>>) -> Result<()> {
        global_session().await.register_session(session).await
    }

    pub async fn remove_session(
        session: Arc<RwLock<SessionDescriptor>>,
    ) -> Result<(String, Arc<RwLock<SessionDescriptor>>)> {
        global_session().await.remove_session(session).await
    }

    pub async fn get_session_state(stream_key: &str) -> Option<SessionState> {
        global_session().await.get_state(stream_key).await
    }

    pub async fn update_session_state(stream_key: &str, new_state: SessionState) -> Result<()> {
        global_session()
            .await
            .update_state(stream_key, new_state)
            .await
    }

    pub async fn register_cancel_token(stream_key: &str, token: CancellationToken) -> Result<()> {
        global_tokens().await.register(stream_key, token)
    }

    pub async fn get_cancel_token(stream_key: &str) -> Option<CancellationToken> {
        global_tokens().await.get(stream_key)
    }
}
