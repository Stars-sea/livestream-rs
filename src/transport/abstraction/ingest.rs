pub struct IngestPacket<P> {
    live_id: String,
    packet: P,
}

impl<P> IngestPacket<P> {
    pub fn new(live_id: impl Into<String>, packet: P) -> Self {
        Self {
            live_id: live_id.into(),
            packet,
        }
    }

    pub fn live_id(&self) -> &str {
        &self.live_id
    }

    #[allow(unused)]
    pub fn packet(&self) -> &P {
        &self.packet
    }

    pub fn into_packet(self) -> P {
        self.packet
    }
}
