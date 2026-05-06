use rvo_signals::store::Signal;
use crate::detector::{
    DetectorContext,
    DetectorHealth,
    DetectorNode,
    DetectorResult,
};

pub struct DummyDetector;

impl DetectorNode for DummyDetector {
    fn id(&self) -> &'static str {
        "dummy-detector"
    }

    fn max_fps(&self) -> f64 {
        30.0
    }

    fn execute(&mut self, ctx: &DetectorContext) -> DetectorResult {
        let signal = Signal {
            value: 1,
            ts_ns: ctx.now_ns,
            ttl_ns: 1_000_000_000, // 1 second TTL
        };

        DetectorResult {
            signals: vec![signal],
            health: DetectorHealth::Ok,
        }
    }
}
