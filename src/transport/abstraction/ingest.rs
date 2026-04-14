pub trait IngestPacket<P> {
    fn live_id(&self) -> &str;

    fn packet(&self) -> P;
}

pub trait IngestHandler {}
