use super::state::SessionState;

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
