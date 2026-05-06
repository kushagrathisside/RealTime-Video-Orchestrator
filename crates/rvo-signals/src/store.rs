use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy)]
pub enum SignalType {
    Dummy,
}

#[derive(Clone, Copy)]
pub struct Signal {
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
                value: 0,
                ts_ns: 0,
                ttl_ns: 0,
            },
        }
    }
}

pub struct SignalStore {
    slot: SignalSlot,
}

impl SignalStore {
    pub fn new() -> Self {
        Self {
            slot: SignalSlot::new(),
        }
    }

    pub fn upsert(&mut self, signal: Signal) {
        let v = self.slot.version.load(Ordering::Relaxed);
        self.slot.version.store(v + 1, Ordering::Release); // write start
        self.slot.signal = signal;
        self.slot.version.store(v + 2, Ordering::Release); // write end
    }

    pub fn get(&self, now_ns: u64) -> Option<Signal> {
        let v1 = self.slot.version.load(Ordering::Acquire);
        if v1 % 2 != 0 {
            return None;
        }

        let sig = self.slot.signal;

        let v2 = self.slot.version.load(Ordering::Acquire);
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
