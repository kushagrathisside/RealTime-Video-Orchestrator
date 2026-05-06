use rvo_signals::store::SignalStore;
use crate::event::Event;
use crate::EventDefinition;
use rvo_signals::store::Signal;
use crate::event::EventType;



#[derive(Clone, Copy)]
enum State {
    Idle,
    Potential { start_ns: u64 },
    Cooldown { until_ns: u64 },
}

pub struct EventEngine {
    def: EventDefinition,
    state: State,
}

impl EventEngine {
    pub fn new(def: EventDefinition) -> Self {
        Self {
            def,
            state: State::Idle,
        }
    }

    pub fn update(
        &mut self,
        now_ns: u64,
        signals: &SignalStore,
    ) -> Option<Event> {
        // Dummy condition: signal value >= threshold
        let condition = signals
            .get(now_ns)
            .map(|s| s.value >= self.def.signal_threshold)
            .unwrap_or(false);

        match self.state {
            State::Idle => {
                if condition {
                    self.state = State::Potential {
                        start_ns: now_ns,
                    };
                }
            }

            State::Potential { start_ns } => {
                if !condition {
                    self.state = State::Idle;
                } else if now_ns - start_ns >= self.def.duration_ns {
                    let confidence =
                        (now_ns - start_ns) as f64
                            / self.def.duration_ns as f64;

                    let event = Event {
                        event_type: self.def.event_type,
                        ts_ns: now_ns,
                        confidence: confidence.min(1.0),
                    };

                    self.state = State::Cooldown {
                        until_ns: now_ns + self.def.cooldown_ns,
                    };

                    return Some(event);
                }
            }

            State::Cooldown { until_ns } => {
                if now_ns >= until_ns {
                    self.state = State::Idle;
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rvo_signals::store::SignalStore;

    #[test]
    fn event_triggers_after_duration() {
        let def = EventDefinition {
            event_type: EventType::DummyEvent,
            signal_threshold: 1,
            duration_ns: 1_000_000_000, // 1s
            cooldown_ns: 5_000_000_000,
        };

        let mut engine = EventEngine::new(def);
        let mut store = SignalStore::new();

        // Simulate signal present
        store.upsert(Signal {
            //name: "dummy".to_string(),
            value: 1,
            ts_ns: 1,
            ttl_ns:1,
        });


        // Before duration → no event
        assert!(engine.update(500_000_000, &store).is_none());

        // After duration → event
        let evt = engine.update(1_500_000_000, &store);
        assert!(evt.is_some());
    }

    #[test]
    fn cooldown_is_enforced() {
        let def = EventDefinition {
            event_type: EventType::DummyEvent,
            signal_threshold: 1,
            duration_ns: 0,
            cooldown_ns: 1_000_000_000,
        };

        let mut engine = EventEngine::new(def);
        let mut store = SignalStore::new();
        store.upsert(Signal {
            //name: "dummy".to_string(),
            value: 1,
            ts_ns: 1,
            ttl_ns:1,
        });

        let first = engine.update(0, &store);
        assert!(first.is_some());

        // Within cooldown → no event
        let second = engine.update(500_000_000, &store);
        assert!(second.is_none());
    }
}
// Event Engine Tests
/* What this proves:
1. Temporal logic works
2. No dependency on frames
3. Deterministic behavior
*/