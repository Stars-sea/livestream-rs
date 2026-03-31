use crate::infra::media::packet::FlvTag;

pub struct WrappedFlvTag {
    pub stream_key: String,
    pub tag: FlvTag,
}

impl WrappedFlvTag {
    pub fn new(stream_key: &str, tag: FlvTag) -> Self {
        Self {
            stream_key: stream_key.to_string(),
            tag,
        }
    }
}
