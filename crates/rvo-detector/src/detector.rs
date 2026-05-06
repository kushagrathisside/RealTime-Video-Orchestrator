use rvo_signals::store::Signal;

pub struct DetectorContext {
    pub now_ns: u64,
}

pub enum DetectorHealth {
    Ok,
    Failed,
}

pub struct DetectorResult {
    pub signals: Vec<Signal>,
    pub health: DetectorHealth,
}

pub trait DetectorNode: Send {
    fn id(&self) -> &'static str;
    fn max_fps(&self) -> f64;
    fn execute(&mut self, ctx: &DetectorContext) -> DetectorResult;
}
