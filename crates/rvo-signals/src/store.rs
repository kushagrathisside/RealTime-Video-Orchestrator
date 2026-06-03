use std::sync::atomic::{AtomicU64, Ordering};

/// Typed signal slots in the signal store.
///
/// Each variant maps 1-to-1 to a fixed slot in `SignalStore`. Add new variants
/// here alongside any new detector that produces them — `COUNT` must stay in
/// sync with the number of variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SignalType {
    /// Synthetic signal emitted by `DummyDetector` for testing.
    Dummy,
    /// Normalised motion intensity: 0 = still, 255 = full-frame motion.
    MotionLevel,
    /// 1 when at least one face is visible in the frame, 0 otherwise.
    FacePresent,
    /// 1 when at least one person is detected in the frame, 0 otherwise.
    PersonDetected,
}

impl SignalType {
    const COUNT: usize = 4;

    /// All signal variants, for iteration (e.g. dashboards/snapshots).
    pub const ALL: [SignalType; Self::COUNT] = [
        SignalType::Dummy,
        SignalType::MotionLevel,
        SignalType::FacePresent,
        SignalType::PersonDetected,
    ];

    /// Stable wire/display name. Matches the strings accepted by config and the
    /// gRPC contract.
    pub fn name(self) -> &'static str {
        match self {
            SignalType::Dummy => "Dummy",
            SignalType::MotionLevel => "MotionLevel",
            SignalType::FacePresent => "FacePresent",
            SignalType::PersonDetected => "PersonDetected",
        }
    }

    /// Parse a signal type from its [`name`](Self::name). Inverse of `name()`.
    pub fn from_name(name: &str) -> Option<SignalType> {
        SignalType::ALL.into_iter().find(|s| s.name() == name)
    }

    fn index(self) -> usize {
        match self {
            SignalType::Dummy => 0,
            SignalType::MotionLevel => 1,
            SignalType::FacePresent => 2,
            SignalType::PersonDetected => 3,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Signal {
    pub signal_type: SignalType,
    pub value: u64,
    pub ts_ns: u64,
    pub ttl_ns: u64,
}

struct SignalSlot {
    version: AtomicU64,
    signal: Signal,
}

impl SignalSlot {
    fn new() -> Self {
        Self {
            version: AtomicU64::new(0),
            signal: Signal {
                signal_type: SignalType::Dummy,
                value: 0,
                ts_ns: 0,
                ttl_ns: 0,
            },
        }
    }
}

pub struct SignalStore {
    slots: Vec<SignalSlot>,
}

impl Default for SignalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalStore {
    pub fn new() -> Self {
        Self {
            slots: (0..SignalType::COUNT).map(|_| SignalSlot::new()).collect(),
        }
    }

    /// Write a signal into its typed slot.
    ///
    /// The version counter follows a seqlock protocol (odd = write in progress,
    /// even = stable). Today `upsert` takes `&mut self`, so all writes are
    /// already serialised by the borrow checker and the version check on the
    /// read side is defensive rather than strictly necessary. It is kept so
    /// the store remains correct if write access is ever relaxed to `&self`
    /// via interior mutability for concurrent detector workers.
    pub fn upsert(&mut self, signal: Signal) {
        let slot = &mut self.slots[signal.signal_type.index()];
        let v = slot.version.load(Ordering::Relaxed);
        slot.version.store(v + 1, Ordering::Release); // write start (odd)
        slot.signal = signal;
        slot.version.store(v + 2, Ordering::Release); // write end   (even)
    }

    pub fn get(&self, signal_type: SignalType, now_ns: u64) -> Option<Signal> {
        let slot = &self.slots[signal_type.index()];
        let v1 = slot.version.load(Ordering::Acquire);
        if !v1.is_multiple_of(2) {
            return None;
        }

        let sig = slot.signal;

        let v2 = slot.version.load(Ordering::Acquire);
        if v1 != v2 {
            return None;
        }

        if sig.ts_ns.saturating_add(sig.ttl_ns) < now_ns {
            None
        } else {
            Some(sig)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gets_fresh_signal_by_type() {
        let mut store = SignalStore::new();

        store.upsert(Signal {
            signal_type: SignalType::Dummy,
            value: 7,
            ts_ns: 1_000,
            ttl_ns: 1_000,
        });

        let signal = store.get(SignalType::Dummy, 1_500).expect("fresh signal");

        assert_eq!(signal.value, 7);
    }

    #[test]
    fn expired_signal_is_absent() {
        let mut store = SignalStore::new();

        store.upsert(Signal {
            signal_type: SignalType::Dummy,
            value: 7,
            ts_ns: 1_000,
            ttl_ns: 100,
        });

        assert!(store.get(SignalType::Dummy, 2_000).is_none());
    }
}
