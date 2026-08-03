#[cfg(feature = "opentelemetry")]
mod imp {
    use opentelemetry::{
        global,
        metrics::{Counter, Histogram, UpDownCounter},
    };
    use std::sync::OnceLock;

    pub struct OTelMetrics {
        pub pipeline_active_streams: UpDownCounter<i64>,
        pub pipeline_packets_total: Counter<u64>,
        pub pipeline_bytes_total: Counter<u64>,
        pub pipeline_errors_total: Counter<u64>,
        pub pipeline_middleware_latency_us: Histogram<u64>,
        pub transport_queue_drops_total: Counter<u64>,
        pub transport_listener_lag_total: Counter<u64>,
        pub transport_ttl_expirations_total: Counter<u64>,
        pub transport_connections_active: UpDownCounter<i64>,
        pub transport_connections_rejected_total: Counter<u64>,
        pub storage_minio_uploads_total: Counter<u64>,
        pub storage_minio_upload_latency_ms: Histogram<u64>,
    }

    pub fn get_metrics() -> &'static OTelMetrics {
        static METRICS: OnceLock<OTelMetrics> = OnceLock::new();
        METRICS.get_or_init(|| {
            let meter = global::meter("livestream");

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
                transport_queue_drops_total: meter
                    .u64_counter("transport_queue_drops_total")
                    .with_description("Dropped transport queue messages")
                    .with_unit("{item}")
                    .build(),
                transport_listener_lag_total: meter
                    .u64_counter("transport_listener_lag_total")
                    .with_description("Lagged messages observed by broadcast listeners")
                    .with_unit("{item}")
                    .build(),
                transport_ttl_expirations_total: meter
                    .u64_counter("transport_ttl_expirations_total")
                    .with_description("Session cleanups triggered by precreate TTL expiration")
                    .with_unit("{session}")
                    .build(),
                transport_connections_active: meter
                    .i64_up_down_counter("transport_connections_active")
                    .with_description("Currently active TCP connections")
                    .with_unit("{connection}")
                    .build(),
                transport_connections_rejected_total: meter
                    .u64_counter("transport_connections_rejected_total")
                    .with_description("Connection attempts rejected at capacity")
                    .with_unit("{connection}")
                    .build(),
                storage_minio_uploads_total: meter
                    .u64_counter("storage_minio_uploads_total")
                    .with_description("Total MinIO upload attempts")
                    .with_unit("{upload}")
                    .build(),
                storage_minio_upload_latency_ms: meter
                    .u64_histogram("storage_minio_upload_latency_ms")
                    .with_description("MinIO upload latency in milliseconds")
                    .with_unit("ms")
                    .build(),
            }
        })
    }
}

#[cfg(not(feature = "opentelemetry"))]
mod imp {
    pub struct OTelMetrics;

    pub fn get_metrics() -> &'static OTelMetrics {
        static METRICS: OTelMetrics = OTelMetrics;
        &METRICS
    }
}

// Internal helper used by the exported metric macros below: it expands the
// recording statements when the `opentelemetry` feature is enabled and
// discards them otherwise, so each metric macro body is written exactly once
// instead of being duplicated as a no-op variant. It is exported (rather than
// kept module-local) so the exported macros can reach it via `$crate::` when
// they expand in other crates.
#[macro_export]
#[cfg(feature = "opentelemetry")]
macro_rules! metric_record {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[macro_export]
#[cfg(not(feature = "opentelemetry"))]
macro_rules! metric_record {
    ($($tt:tt)*) => {};
}

#[macro_export]
macro_rules! metric_pipeline_stream_started {
    () => {
        $crate::metric_record!({
            $crate::metrics::get_metrics()
                .pipeline_active_streams
                .add(1, &[]);
        })
    };
}

#[macro_export]
macro_rules! metric_pipeline_stream_ended {
    () => {
        $crate::metric_record!({
            $crate::metrics::get_metrics()
                .pipeline_active_streams
                .add(-1, &[]);
        })
    };
}

#[macro_export]
macro_rules! metric_pipeline_packet {
    ($packet_kind:expr, $bytes:expr) => {
        $crate::metric_record!({
            let labels = [$crate::KeyValue::new("packet.kind", $packet_kind)];
            let metrics = $crate::metrics::get_metrics();
            metrics.pipeline_packets_total.add(1, &labels);
            metrics.pipeline_bytes_total.add(($bytes) as u64, &labels);
        })
    };
}

#[macro_export]
macro_rules! metric_pipeline_error {
    ($stage:expr) => {
        $crate::metric_record!({
            $crate::metrics::get_metrics()
                .pipeline_errors_total
                .add(1, &[$crate::KeyValue::new("pipeline.stage", $stage)]);
        })
    };
}

#[macro_export]
macro_rules! metric_middleware_latency_us {
    ($middleware:expr, $duration_us:expr) => {
        $crate::metric_record!({
            $crate::metrics::get_metrics()
                .pipeline_middleware_latency_us
                .record(
                    ($duration_us) as u64,
                    &[$crate::KeyValue::new("middleware", $middleware)],
                );
        })
    };
}

#[macro_export]
macro_rules! metric_queue_drop {
    ($queue:expr, $reason:expr) => {
        $crate::metric_record!({
            $crate::metrics::get_metrics()
                .transport_queue_drops_total
                .add(
                    1,
                    &[
                        $crate::KeyValue::new("queue", $queue),
                        $crate::KeyValue::new("reason", $reason),
                    ],
                );
        })
    };
}

#[macro_export]
macro_rules! metric_listener_lag {
    ($listener:expr, $skipped:expr) => {
        $crate::metric_record!({
            $crate::metrics::get_metrics()
                .transport_listener_lag_total
                .add(
                    (($skipped) as u64).max(1),
                    &[$crate::KeyValue::new("listener", $listener)],
                );
        })
    };
}

#[macro_export]
macro_rules! metric_ttl_expiration {
    ($protocol:expr, $reason:expr) => {
        $crate::metric_record!({
            $crate::metrics::get_metrics()
                .transport_ttl_expirations_total
                .add(
                    1,
                    &[
                        $crate::KeyValue::new("protocol", $protocol),
                        $crate::KeyValue::new("reason", $reason),
                    ],
                );
        })
    };
}

#[macro_export]
macro_rules! metric_connection_accepted {
    ($protocol:expr) => {
        $crate::metric_record!({
            $crate::metrics::get_metrics()
                .transport_connections_active
                .add(1, &[$crate::KeyValue::new("protocol", $protocol)]);
        })
    };
}

#[macro_export]
macro_rules! metric_connection_closed {
    ($protocol:expr) => {
        $crate::metric_record!({
            $crate::metrics::get_metrics()
                .transport_connections_active
                .add(-1, &[$crate::KeyValue::new("protocol", $protocol)]);
        })
    };
}

#[macro_export]
macro_rules! metric_connection_rejected {
    ($protocol:expr) => {
        $crate::metric_record!({
            $crate::metrics::get_metrics()
                .transport_connections_rejected_total
                .add(1, &[$crate::KeyValue::new("protocol", $protocol)]);
        })
    };
}

#[macro_export]
macro_rules! metric_minio_upload_total {
    ($bucket:expr, $status:expr) => {
        $crate::metric_record!({
            let bucket = $bucket;
            let status = $status;
            $crate::metrics::get_metrics()
                .storage_minio_uploads_total
                .add(
                    1,
                    &[
                        $crate::KeyValue::new("storage.bucket.name", bucket.to_string()),
                        $crate::KeyValue::new("upload.status", status),
                    ],
                );
        })
    };
}

#[macro_export]
macro_rules! metric_minio_upload_latency_ms {
    ($bucket:expr, $status:expr, $duration_ms:expr) => {
        $crate::metric_record!({
            let bucket = $bucket;
            let status = $status;
            $crate::metrics::get_metrics()
                .storage_minio_upload_latency_ms
                .record(
                    ($duration_ms) as u64,
                    &[
                        $crate::KeyValue::new("storage.bucket.name", bucket.to_string()),
                        $crate::KeyValue::new("upload.status", status),
                    ],
                );
        })
    };
}

pub use imp::get_metrics;
