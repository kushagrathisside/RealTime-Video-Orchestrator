use rand::{thread_rng, Rng};
use crate::detector::{
    DetectorNode,
    DetectorContext,
    DetectorResult,
    DetectorHealth,
};


pub struct JitterDetector;

impl DetectorNode for JitterDetector {
    fn id(&self) -> &'static str {
        "jitter"
    }

    fn max_fps(&self) -> f64 {
        30.0
    }

    fn execute(&mut self, ctx: &DetectorContext) -> DetectorResult {
        let jitter = thread_rng().gen_range(0..2_000_000); // up to 2ms
        let start = std::time::Instant::now();

        while start.elapsed().as_nanos() < jitter {}

        DetectorResult {
            signals: vec![],
            health: DetectorHealth::Ok,
        }
    }
}
