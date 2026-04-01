use tokio_util::sync::CancellationToken;

use crate::infra::media::packet::FlvTag;

pub struct WrappedFlvTag {
    pub stream_key: String,
    pub tag: FlvTag,
    pub cancel_token: CancellationToken,
}

impl WrappedFlvTag {
    pub fn new(stream_key: &str, tag: FlvTag, cancel_token: &CancellationToken) -> Self {
        Self {
            stream_key: stream_key.to_string(),
            tag,
            cancel_token: cancel_token.clone(),
        }
    }
}
