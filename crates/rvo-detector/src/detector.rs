use rvo_buffer::Frame;
use rvo_signals::store::{Signal, SignalType};

pub struct DetectorContext<'a> {
    pub now_ns: u64,
    pub frame: Option<&'a Frame>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectorHealth {
    Ok,
    Failed,
}

pub struct DetectorResult {
    pub signals: Vec<Signal>,
    pub health: DetectorHealth,
}

#[derive(Clone, Copy)]
pub enum DetectorCostHint {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy)]
pub struct DetectorMeta {
    pub id: &'static str,
    pub max_fps: f64,
    pub dependencies: &'static [SignalType],
    pub output_signals: &'static [SignalType],
    pub cost_hint: DetectorCostHint,
    pub requires_frame: bool,
}

pub trait DetectorNode: Send {
    fn meta(&self) -> DetectorMeta;

    fn id(&self) -> &'static str {
        self.meta().id
    }

    fn max_fps(&self) -> f64 {
        self.meta().max_fps
    }

    fn dependencies(&self) -> &'static [SignalType] {
        self.meta().dependencies
    }

    fn output_signals(&self) -> &'static [SignalType] {
        self.meta().output_signals
    }

    fn cost_hint(&self) -> DetectorCostHint {
        self.meta().cost_hint
    }

    fn requires_frame(&self) -> bool {
        self.meta().requires_frame
    }

    fn execute(&mut self, ctx: &DetectorContext<'_>) -> DetectorResult;
}
