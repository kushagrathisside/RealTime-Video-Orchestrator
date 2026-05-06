#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    DummyEvent,
}

#[derive(Debug)]
pub struct Event {
    pub event_type: EventType,
    pub ts_ns: u64,
    pub confidence: f64,
}
