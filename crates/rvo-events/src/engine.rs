use crate::event::Event;
use crate::EventDefinition;
use rvo_signals::store::SignalStore;

#[derive(Clone, Copy)]
enum State {
    Idle,
    Potential { start_ns: u64 },
    Cooldown { until_ns: u64 },
}

struct EventMachine {
    def: EventDefinition,
    state: State,
}

impl EventMachine {
    fn new(def: EventDefinition) -> Self {
        Self {
            def,
            state: State::Idle,
        }
    }

    fn emit_event(&mut self, now_ns: u64, start_ns: u64) -> Event {
        let elapsed_ns = now_ns.saturating_sub(start_ns);
        let confidence = if self.def.duration_ns == 0 {
            1.0
        } else {
            (elapsed_ns as f64 / self.def.duration_ns as f64).min(1.0)
        };

        self.state = State::Cooldown {
            until_ns: now_ns.saturating_add(self.def.cooldown_ns),
        };

        Event {
            event_type: self.def.event_type.clone(),
            ts_ns: now_ns,
            confidence,
        }
    }

    fn update(&mut self, now_ns: u64, signals: &SignalStore) -> Option<Event> {
        let condition_met = self.def.condition.evaluate(signals, now_ns);

        match self.state {
            State::Idle => {
                if condition_met {
                    if self.def.duration_ns == 0 {
                        return Some(self.emit_event(now_ns, now_ns));
                    } else {
                        self.state = State::Potential { start_ns: now_ns };
                    }
                }
            }

            State::Potential { start_ns } => {
                if !condition_met {
                    self.state = State::Idle;
                } else if now_ns.saturating_sub(start_ns) >= self.def.duration_ns {
                    return Some(self.emit_event(now_ns, start_ns));
                }
            }

            State::Cooldown { until_ns } => {
                if now_ns >= until_ns {
                    if condition_met {
                        if self.def.duration_ns == 0 {
                            return Some(self.emit_event(now_ns, now_ns));
                        } else {
                            self.state = State::Potential { start_ns: now_ns };
                        }
                    } else {
                        self.state = State::Idle;
                    }
                }
            }
        }

        None
    }
}

pub struct EventEngine {
    machines: Vec<EventMachine>,
}

impl EventEngine {
    pub fn new(def: EventDefinition) -> Self {
        Self::new_many(vec![def])
    }

    pub fn new_many(defs: Vec<EventDefinition>) -> Self {
        Self {
            machines: defs.into_iter().map(EventMachine::new).collect(),
        }
    }

    pub fn update(&mut self, now_ns: u64, signals: &SignalStore) -> Vec<Event> {
        self.machines
            .iter_mut()
            .filter_map(|machine| machine.update(now_ns, signals))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::condition::{CompareOp, Condition, SignalPredicate};
    use crate::event::EventType;
    use rvo_signals::store::{Signal, SignalStore, SignalType};

    fn dummy_signal(value: u64, ttl_ns: u64) -> Signal {
        Signal {
            signal_type: SignalType::Dummy,
            value,
            ts_ns: 0,
            ttl_ns,
        }
    }

    #[test]
    fn event_triggers_after_duration() {
        let def = EventDefinition {
            event_type: EventType::DummyEvent,
            condition: Condition::single_gte(SignalType::Dummy, 1),
            duration_ns: 1_000_000_000, // 1s
            cooldown_ns: 5_000_000_000,
        };

        let mut engine = EventEngine::new(def);
        let mut store = SignalStore::new();
        store.upsert(dummy_signal(1, 10_000_000_000)); // TTL 10s

        // Before duration elapses: no event
        assert!(engine.update(500_000_000, &store).is_empty());
        // After duration elapses: event fires
        let events = engine.update(1_500_000_000, &store);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn cooldown_is_enforced() {
        let def = EventDefinition {
            event_type: EventType::DummyEvent,
            condition: Condition::single_gte(SignalType::Dummy, 1),
            duration_ns: 0,
            cooldown_ns: 1_000_000_000, // 1s
        };

        let mut engine = EventEngine::new(def);
        let mut store = SignalStore::new();
        store.upsert(dummy_signal(1, 10_000_000_000));

        let first = engine.update(0, &store);
        assert_eq!(first.len(), 1);

        // Still in cooldown
        assert!(engine.update(500_000_000, &store).is_empty());

        // After cooldown expires: fires again
        let third = engine.update(1_500_000_000, &store);
        assert_eq!(third.len(), 1);
    }

    #[test]
    fn multiple_definitions_update_independently() {
        let defs = vec![
            EventDefinition {
                event_type: EventType::DummyEvent,
                condition: Condition::single_gte(SignalType::Dummy, 1),
                duration_ns: 0,
                cooldown_ns: 1_000_000_000,
            },
            EventDefinition {
                event_type: EventType::DummyEvent,
                condition: Condition::single_gte(SignalType::Dummy, 1),
                duration_ns: 0,
                cooldown_ns: 1_000_000_000,
            },
        ];

        let mut engine = EventEngine::new_many(defs);
        let mut store = SignalStore::new();
        store.upsert(dummy_signal(1, 10_000_000_000));

        // Both machines should fire independently
        assert_eq!(engine.update(0, &store).len(), 2);
    }

    #[test]
    fn all_condition_requires_every_signal() {
        let def = EventDefinition {
            event_type: EventType::DummyEvent,
            condition: Condition::All(vec![
                SignalPredicate {
                    signal_type: SignalType::Dummy,
                    op: CompareOp::Gte,
                    value: 1,
                },
                SignalPredicate {
                    signal_type: SignalType::FacePresent,
                    op: CompareOp::Eq,
                    value: 1,
                },
            ]),
            duration_ns: 0,
            cooldown_ns: 1_000_000_000,
        };

        let mut engine = EventEngine::new(def);
        let mut store = SignalStore::new();

        // Only Dummy is set — All condition fails
        store.upsert(dummy_signal(1, 10_000_000_000));
        assert!(engine.update(0, &store).is_empty());

        // Both signals set — All condition passes
        store.upsert(Signal {
            signal_type: SignalType::FacePresent,
            value: 1,
            ts_ns: 0,
            ttl_ns: 10_000_000_000,
        });
        assert_eq!(engine.update(100, &store).len(), 1);
    }

    #[test]
    fn any_condition_passes_on_one_signal() {
        let def = EventDefinition {
            event_type: EventType::DummyEvent,
            condition: Condition::Any(vec![
                SignalPredicate {
                    signal_type: SignalType::Dummy,
                    op: CompareOp::Gte,
                    value: 1,
                },
                SignalPredicate {
                    signal_type: SignalType::MotionLevel,
                    op: CompareOp::Gte,
                    value: 50,
                },
            ]),
            duration_ns: 0,
            cooldown_ns: 1_000_000_000,
        };

        let mut engine = EventEngine::new(def);
        let mut store = SignalStore::new();

        // Only MotionLevel set above threshold — Any passes
        store.upsert(Signal {
            signal_type: SignalType::MotionLevel,
            value: 100,
            ts_ns: 0,
            ttl_ns: 10_000_000_000,
        });
        assert_eq!(engine.update(0, &store).len(), 1);
    }
}

/*
What this proves:
1. Temporal logic works (duration, cooldown)
2. No dependency on frames
3. Deterministic behavior
4. Condition DSL (All/Any) routes correctly to typed signals
*/
