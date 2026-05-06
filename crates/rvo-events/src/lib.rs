pub mod event;
pub mod engine;

pub use engine::EventEngine;
pub use event::{Event, EventType};

#[derive(Clone)]
pub struct EventDefinition {
    pub event_type: EventType,
    pub signal_threshold: u64,
    pub duration_ns: u64,
    pub cooldown_ns: u64,
}
