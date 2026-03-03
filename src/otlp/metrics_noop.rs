pub struct OTelMetrics {
    pub rtmp_connections: NoopCounter,
    pub online_streams: NoopCounter,
    pub pull_connections: NoopCounter,
}

pub struct NoopCounter;

pub fn get_metrics() -> &'static OTelMetrics {
    static METRICS: OTelMetrics = OTelMetrics {
        rtmp_connections: NoopCounter,
        online_streams: NoopCounter,
        pull_connections: NoopCounter,
    };

    &METRICS
}

impl OTelMetrics {
    pub fn add_network_bytes_in(&self, _value: u64, _labels: &[()]) {}

    pub fn add_network_bytes_out(&self, _value: u64, _labels: &[()]) {}

    pub fn add_ingest_packets(&self, _value: u64, _labels: &[()]) {}

    pub fn grpc_call(&'static self, _method: &'static str) -> GrpcCallGuard {
        GrpcCallGuard { success: false }
    }
}

pub struct GrpcCallGuard {
    success: bool,
}

impl GrpcCallGuard {
    pub fn success(&mut self) {
        self.success = true;
    }
}

pub struct MetricGuard;

impl MetricGuard {
    pub fn new(_counter: &'static NoopCounter, _labels: Vec<()>) -> Self {
        Self
    }
}
