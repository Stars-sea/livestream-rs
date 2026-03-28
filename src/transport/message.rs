use crate::transport::SessionState;

// TODO: Add documentation for the relationship between these live_id and the stream keys used in the registry.
// TODO: Consider moving to a contract module?
#[derive(Debug, Clone)]
pub enum ControlMessage {
    PrecreateStream { live_id: String },

    StopStream { live_id: String },
}

pub enum StreamEvent {
    StateChange {
        live_id: String,
        new_state: SessionState,
    },
}
