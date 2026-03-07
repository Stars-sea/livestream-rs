#[cfg(feature = "opentelemetry")]
mod init;

#[cfg(feature = "opentelemetry")]
pub mod metrics;

#[cfg(not(feature = "opentelemetry"))]
#[path = "metrics_noop.rs"]
pub mod metrics;

#[cfg(feature = "opentelemetry")]
pub use init::{init_otlp, otlp_enabled};
