use std::error::Error;
use std::fmt;

#[derive(Debug, Clone)]
pub struct SubscribeError {
    queue: &'static str,
    source: &'static str,
    reason: &'static str,
}

impl SubscribeError {
    pub(crate) fn already_subscribed(queue: &'static str, source: &'static str) -> Self {
        Self {
            queue,
            source,
            reason: "receiver already subscribed",
        }
    }
}

impl fmt::Display for SubscribeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Failed to subscribe queue '{}' at '{}': {}",
            self.queue, self.source, self.reason
        )
    }
}

impl Error for SubscribeError {}
