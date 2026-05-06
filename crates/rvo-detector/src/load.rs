use std::time::{Duration, Instant};
use crate::detector::{
    DetectorNode,
    DetectorContext,
    DetectorResult,
    DetectorHealth,
};


pub struct LoadDetector {
    busy_ns: u64,
}

impl LoadDetector {
    pub fn new(busy_ns: u64) -> Self {
        Self { busy_ns }
    }
}

impl DetectorNode for LoadDetector {
    fn id(&self) -> &'static str {
        "synthetic_load"
    }

    fn max_fps(&self) -> f64 {
        10.0
    }

    fn execute(&mut self, ctx: &DetectorContext) -> DetectorResult {
        let start = Instant::now();

        // Busy-spin (NO sleep — real CPU pressure)
        while start.elapsed().as_nanos() < self.busy_ns as u128 {}

        DetectorResult {
            signals: Vec::new(),
            health: DetectorHealth::Ok,
        }
    }
}
