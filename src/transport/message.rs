// TODO: Add documentation for the relationship between these live_id and the stream keys used in the registry.
// TODO: Consider moving to a contract module?
#[derive(Debug, Clone)]
pub enum ControlMessage {
    PrecreateStream { live_id: String },

    StopStream { live_id: String },
}
