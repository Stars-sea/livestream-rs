use serde::Deserialize;

fn default_rtmp_forward() -> usize {
    8192
}
fn default_flv_relay() -> usize {
    2048
}
fn default_packet_relay() -> usize {
    2048
}
fn default_control() -> usize {
    1024
}
fn default_event() -> usize {
    4096
}

/// Channel capacity configuration for transport queues.
#[derive(Clone, Debug, Deserialize)]
pub struct QueueConfig {
    #[serde(default = "default_rtmp_forward")]
    pub rtmp_forward: usize,

    #[serde(default = "default_flv_relay")]
    pub flv_relay: usize,

    #[serde(default = "default_packet_relay")]
    pub packet_relay: usize,

    #[serde(default = "default_control")]
    pub control: usize,

    #[serde(default = "default_event")]
    pub event: usize,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            rtmp_forward: default_rtmp_forward(),
            flv_relay: default_flv_relay(),
            packet_relay: default_packet_relay(),
            control: default_control(),
            event: default_event(),
        }
    }
}
