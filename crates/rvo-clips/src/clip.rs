use std::time::Instant;
use rvo_events::EventType;

#[derive(Clone)]
pub struct ClipJob {
    pub event_type: EventType,
    pub event_ts_ns: u64,
    pub start_ts: Instant,
    pub end_ts: Instant,
}
