//! Configuration loading and re-exports.
//!
//! Types are defined in `livestream_core::config`. This module
//! handles the static singleton and re-exports the types
//! for backward-compatible `crate::config::*` access.

use std::sync::OnceLock;

// Re-export types used directly by binary code.
pub use livestream_core::config::{AppConfig, MinioConfig};

pub fn load_config() -> &'static AppConfig {
    static SETTINGS: OnceLock<AppConfig> = OnceLock::new();

    SETTINGS.get_or_init(|| AppConfig::new().expect("Failed to load application settings"))
}
