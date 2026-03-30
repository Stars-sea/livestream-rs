use dashmap::DashMap;

use crate::infra::media::context::HlsOutputContext;

pub struct SegmentMiddleware {
    ctxs: DashMap<String, HlsOutputContext>,
}

impl SegmentMiddleware {
    pub fn new() -> Self {
        Self {
            ctxs: DashMap::new(),
        }
    }
}
