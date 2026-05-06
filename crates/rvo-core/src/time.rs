use std::time::{Duration, Instant};

#[derive(Copy, Clone)]
pub struct MonoTime(Instant);

impl MonoTime {
    pub fn now() -> Self {
        MonoTime(Instant::now())
    }

    pub fn elapsed_since(&self, earlier: MonoTime) -> Duration {
        self.0.duration_since(earlier.0)
    }
}
