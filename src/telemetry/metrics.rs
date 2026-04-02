use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Histogram, ObservableGauge, UpDownCounter},
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

pub struct OTelMetrics {
    pub rtmp_sessions: UpDownCounter<i64>,
    pub ingest_streams: UpDownCounter<i64>,
    pub egress_connections: UpDownCounter<i64>,
    pub pipeline_active_streams: UpDownCounter<i64>,
    pub pipeline_packets_total: Counter<u64>,
    pub pipeline_bytes_total: Counter<u64>,
    pub pipeline_errors_total: Counter<u64>,
    pub pipeline_middleware_latency_us: Histogram<u64>,
    pub network_bytes_in: Counter<u64>,
    pub network_bytes_out: Counter<u64>,
    pub ingest_packets_total: Counter<u64>,
    pub grpc_requests_total: Counter<u64>,
    pub grpc_requests_failed: Counter<u64>,
    _network_bytes_in_rate: ObservableGauge<u64>,
    _network_bytes_out_rate: ObservableGauge<u64>,
    _ingest_packets_rate: ObservableGauge<u64>,
    rate_state: Arc<RateState>,
}

#[derive(Default)]
struct RateState {
    bytes_in_window: AtomicU64,
    bytes_out_window: AtomicU64,
    packets_window: AtomicU64,
    bytes_in_per_sec: AtomicU64,
    bytes_out_per_sec: AtomicU64,
    packets_per_sec: AtomicU64,
}

impl RateState {
    fn spawn_sampler(state: Arc<Self>) {
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(1));
                state.bytes_in_per_sec.store(
                    state.bytes_in_window.swap(0, Ordering::Relaxed),
                    Ordering::Relaxed,
                );
                state.bytes_out_per_sec.store(
                    state.bytes_out_window.swap(0, Ordering::Relaxed),
                    Ordering::Relaxed,
                );
                state.packets_per_sec.store(
                    state.packets_window.swap(0, Ordering::Relaxed),
                    Ordering::Relaxed,
                );
            }
        });
    }
}

pub fn get_metrics() -> &'static OTelMetrics {
    static METRICS: OnceLock<OTelMetrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        let meter = global::meter("livestream-rs");
        let rate_state = Arc::new(RateState::default());
        RateState::spawn_sampler(rate_state.clone());

        let network_bytes_in_rate_state = rate_state.clone();
        let network_bytes_out_rate_state = rate_state.clone();
        let ingest_packets_rate_state = rate_state.clone();

        OTelMetrics {
            rtmp_sessions: meter
                .i64_up_down_counter("rtmp_sessions")
                .with_description("Active RTMP sessions")
                .build(),
            ingest_streams: meter
                .i64_up_down_counter("ingest_streams")
                .with_description("Active streaming ingestions")
                .build(),
            egress_connections: meter
                .i64_up_down_counter("egress_connections")
                .with_description("Active egress stream consumers")
                .build(),
            pipeline_active_streams: meter
                .i64_up_down_counter("pipeline_active_streams")
                .with_description("Active stream-scoped pipeline instances")
                .build(),
            pipeline_packets_total: meter
                .u64_counter("pipeline_packets_total")
                .with_description("Total packets observed by pipeline middleware")
                .with_unit("{packet}")
                .build(),
            pipeline_bytes_total: meter
                .u64_counter("pipeline_bytes_total")
                .with_description("Total payload bytes observed by pipeline middleware")
                .with_unit("By")
                .build(),
            pipeline_errors_total: meter
                .u64_counter("pipeline_errors_total")
                .with_description("Total recoverable pipeline processing errors")
                .build(),
            pipeline_middleware_latency_us: meter
                .u64_histogram("pipeline_middleware_latency_us")
                .with_description("Middleware processing latency in microseconds")
                .with_unit("us")
                .build(),
            network_bytes_in: meter
                .u64_counter("network_bytes_in")
                .with_description("Total incoming network bytes")
                .with_unit("By")
                .build(),
            network_bytes_out: meter
                .u64_counter("network_bytes_out")
                .with_description("Total outgoing network bytes")
                .with_unit("By")
                .build(),
            ingest_packets_total: meter
                .u64_counter("ingest_packets_total")
                .with_description("Total number of packets processed by ingest")
                .with_unit("{packet}")
                .build(),
            grpc_requests_total: meter
                .u64_counter("grpc_requests_total")
                .with_description("Total number of gRPC requests received")
                .build(),
            grpc_requests_failed: meter
                .u64_counter("grpc_requests_failed")
                .with_description("Total number of failed gRPC requests")
                .build(),
            _network_bytes_in_rate: meter
                .u64_observable_gauge("network_bytes_in_rate")
                .with_description("Incoming network throughput in bytes per second")
                .with_unit("By/s")
                .with_callback(move |observer| {
                    observer.observe(
                        network_bytes_in_rate_state
                            .bytes_in_per_sec
                            .load(Ordering::Relaxed),
                        &[],
                    );
                })
                .build(),
            _network_bytes_out_rate: meter
                .u64_observable_gauge("network_bytes_out_rate")
                .with_description("Outgoing network throughput in bytes per second")
                .with_unit("By/s")
                .with_callback(move |observer| {
                    observer.observe(
                        network_bytes_out_rate_state
                            .bytes_out_per_sec
                            .load(Ordering::Relaxed),
                        &[],
                    );
                })
                .build(),
            _ingest_packets_rate: meter
                .u64_observable_gauge("ingest_packets_rate")
                .with_description("Ingest packet processing rate in packets per second")
                .with_unit("{packet}/s")
                .with_callback(move |observer| {
                    observer.observe(
                        ingest_packets_rate_state
                            .packets_per_sec
                            .load(Ordering::Relaxed),
                        &[],
                    );
                })
                .build(),
            rate_state,
        }
    })
}

impl OTelMetrics {
    pub fn pipeline_stream_started(&self) {
        self.pipeline_active_streams.add(1, &[]);
    }

    pub fn pipeline_stream_ended(&self) {
        self.pipeline_active_streams.add(-1, &[]);
    }

    pub fn record_pipeline_packet(&self, packet_kind: &'static str, bytes: u64) {
        let labels = [KeyValue::new("packet.kind", packet_kind)];
        self.pipeline_packets_total.add(1, &labels);
        self.pipeline_bytes_total.add(bytes, &labels);
    }

    pub fn record_pipeline_error(&self, stage: &'static str) {
        self.pipeline_errors_total
            .add(1, &[KeyValue::new("pipeline.stage", stage)]);
    }

    pub fn record_middleware_latency(&self, middleware: &'static str, duration_us: u64) {
        self.pipeline_middleware_latency_us
            .record(duration_us, &[KeyValue::new("middleware", middleware)]);
    }

    pub fn add_network_bytes_in(&self, value: u64, labels: &[KeyValue]) {
        self.network_bytes_in.add(value, labels);
        self.rate_state
            .bytes_in_window
            .fetch_add(value, Ordering::Relaxed);
    }

    pub fn add_network_bytes_out(&self, value: u64, labels: &[KeyValue]) {
        self.network_bytes_out.add(value, labels);
        self.rate_state
            .bytes_out_window
            .fetch_add(value, Ordering::Relaxed);
    }

    pub fn add_ingest_packets(&self, value: u64, labels: &[KeyValue]) {
        self.ingest_packets_total.add(value, labels);
        self.rate_state
            .packets_window
            .fetch_add(value, Ordering::Relaxed);
    }

    pub fn grpc_call(&'static self, method: &'static str) -> GrpcCallGuard {
        GrpcCallGuard::new(self, method)
    }

    pub fn grpc_failed(&self, method: &'static str) {
        self.grpc_requests_failed
            .add(1, &[KeyValue::new("method", method)]);
    }
}

pub fn protocol_labels(protocol: &'static str) -> Vec<KeyValue> {
    vec![KeyValue::new("stream.protocol", protocol)]
}

pub struct GrpcCallGuard {
    metrics: &'static OTelMetrics,
    method: &'static str,
    success: bool,
}

impl GrpcCallGuard {
    fn new(metrics: &'static OTelMetrics, method: &'static str) -> Self {
        metrics
            .grpc_requests_total
            .add(1, &[KeyValue::new("method", method)]);
        Self {
            metrics,
            method,
            success: false,
        }
    }

    pub fn success(&mut self) {
        self.success = true;
    }
}

impl Drop for GrpcCallGuard {
    fn drop(&mut self) {
        if !self.success {
            self.metrics.grpc_failed(self.method);
        }
    }
}

pub struct MetricGuard {
    counter: &'static UpDownCounter<i64>,
    labels: Vec<KeyValue>,
}

impl MetricGuard {
    pub fn new(counter: &'static UpDownCounter<i64>, labels: Vec<KeyValue>) -> Self {
        counter.add(1, &labels);
        Self { counter, labels }
    }
}

impl Drop for MetricGuard {
    fn drop(&mut self) {
        self.counter.add(-1, &self.labels);
    }
}
