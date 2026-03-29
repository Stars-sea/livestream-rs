use dashmap::DashMap;

use crate::media::format::HlsOutputContext;

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
