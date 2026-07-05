use serde::Serialize;
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct EventType(pub Cow<'static, str>);

impl EventType {
    #[allow(non_upper_case_globals)]
    pub const DummyEvent: EventType = EventType(Cow::Borrowed("DummyEvent"));

    pub fn new(s: String) -> Self {
        EventType(Cow::Owned(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub event_type: EventType,
    /// Monotonic nanoseconds since scheduler start.
    pub ts_ns: u64,
    /// Confidence in [0.0, 1.0]: elapsed / duration at the time of emission.
    pub confidence: f64,
}
