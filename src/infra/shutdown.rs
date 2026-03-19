use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::info;

#[derive(Clone)]
pub struct ShutdownManager {
    token: CancellationToken,
}

impl ShutdownManager {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    #[allow(dead_code)]
    pub fn cancel(&self) {
        self.token.cancel();
    }

    #[allow(dead_code)]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub async fn wait_for_signal(&self) {
        let ctrl_c = async {
            signal::ctrl_c()
                .await
                .expect("failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("failed to install signal handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {
                info!("Received Ctrl+C signal");
            },
            _ = terminate => {
                info!("Received SIGTERM signal");
            },
            _ = self.token.cancelled() => {
                info!("Shutdown initiated internally");
            }
        }

        if !self.token.is_cancelled() {
            info!("Broadcasting shutdown signal...");
            self.token.cancel();
        }
    }
}

impl Default for ShutdownManager {
    fn default() -> Self {
        Self::new()
    }
}
