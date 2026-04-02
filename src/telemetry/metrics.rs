#[cfg(feature = "opentelemetry")]
mod imp {
    use opentelemetry::{
        KeyValue, global,
        metrics::{Counter, Histogram, UpDownCounter},
    };
    use std::sync::OnceLock;

    pub struct OTelMetrics {
        pub pipeline_active_streams: UpDownCounter<i64>,
        pub pipeline_packets_total: Counter<u64>,
        pub pipeline_bytes_total: Counter<u64>,
        pub pipeline_errors_total: Counter<u64>,
        pub pipeline_middleware_latency_us: Histogram<u64>,
    }

    pub fn get_metrics() -> &'static OTelMetrics {
        static METRICS: OnceLock<OTelMetrics> = OnceLock::new();
        METRICS.get_or_init(|| {
            let meter = global::meter("livestream-rs");

            OTelMetrics {
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
    }
}

#[cfg(not(feature = "opentelemetry"))]
mod imp {
    pub struct OTelMetrics;

    pub fn get_metrics() -> &'static OTelMetrics {
        static METRICS: OTelMetrics = OTelMetrics;
        &METRICS
    }

    impl OTelMetrics {
        pub fn pipeline_stream_started(&self) {}

        pub fn pipeline_stream_ended(&self) {}

        pub fn record_pipeline_packet(&self, _packet_kind: &'static str, _bytes: u64) {}

        pub fn record_pipeline_error(&self, _stage: &'static str) {}

        pub fn record_middleware_latency(&self, _middleware: &'static str, _duration_us: u64) {}
    }
}

pub use imp::get_metrics;
