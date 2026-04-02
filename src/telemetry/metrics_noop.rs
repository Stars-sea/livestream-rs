pub struct OTelMetrics {
    pub rtmp_sessions: NoopCounter,
    pub ingest_streams: NoopCounter,
    pub egress_connections: NoopCounter,
    pub pipeline_active_streams: NoopCounter,
}

pub struct NoopCounter;

pub fn get_metrics() -> &'static OTelMetrics {
    static METRICS: OTelMetrics = OTelMetrics {
        rtmp_sessions: NoopCounter,
        ingest_streams: NoopCounter,
        egress_connections: NoopCounter,
        pipeline_active_streams: NoopCounter,
    };

    &METRICS
}

impl OTelMetrics {
    pub fn pipeline_stream_started(&self) {}

    pub fn pipeline_stream_ended(&self) {}

    pub fn record_pipeline_packet(&self, _packet_kind: &'static str, _bytes: u64) {}

    pub fn record_pipeline_error(&self, _stage: &'static str) {}

    pub fn record_middleware_latency(&self, _middleware: &'static str, _duration_us: u64) {}

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

pub fn protocol_labels(_protocol: &'static str) -> Vec<()> {
    vec![]
}
