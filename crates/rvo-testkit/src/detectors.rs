use std::thread;
use std::time::Duration;

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use rvo_detector::detector::{
    DetectorContext, DetectorCostHint, DetectorHealth, DetectorMeta, DetectorNode, DetectorResult,
};
use rvo_signals::store::{Signal, SignalType};

pub struct ScriptEntry {
    pub tick: u64,
    pub signal_type: SignalType,
    pub value: u64,
    pub ttl_ns: u64,
}

pub struct ScriptedDetector {
    id: &'static str,
    max_fps: f64,
    script: Vec<ScriptEntry>,
    tick_count: u64,
    output_signals: &'static [SignalType],
}

impl ScriptedDetector {
    pub fn new(id: &'static str, max_fps: f64, script: Vec<ScriptEntry>) -> Self {
        assert!(
            !script.is_empty(),
            "ScriptedDetector requires at least one script entry"
        );

        let mut signal_types = Vec::new();
        for entry in &script {
            if !signal_types.contains(&entry.signal_type) {
                signal_types.push(entry.signal_type);
            }
        }

        Self {
            id,
            max_fps,
            script,
            tick_count: 0,
            output_signals: Box::leak(signal_types.into_boxed_slice()),
        }
    }
}

impl DetectorNode for ScriptedDetector {
    fn meta(&self) -> DetectorMeta {
        DetectorMeta {
            id: self.id,
            max_fps: self.max_fps,
            dependencies: &[],
            output_signals: self.output_signals,
            cost_hint: DetectorCostHint::Low,
            requires_frame: false,
        }
    }

    fn execute(&mut self, ctx: &DetectorContext<'_>) -> DetectorResult {
        self.tick_count += 1;
        let tick = self.tick_count - 1;

        let signals = self
            .script
            .iter()
            .filter(|entry| entry.tick == tick)
            .map(|entry| Signal {
                signal_type: entry.signal_type,
                value: entry.value,
                ts_ns: ctx.now_ns,
                ttl_ns: entry.ttl_ns,
            })
            .collect();

        DetectorResult {
            signals,
            health: DetectorHealth::Ok,
        }
    }
}

pub struct ProbabilisticDetector {
    id: &'static str,
    max_fps: f64,
    signal_type: SignalType,
    output_signals: &'static [SignalType],
    emit_probability: f64,
    value: u64,
    ttl_ns: u64,
    rng: SmallRng,
}

impl ProbabilisticDetector {
    pub fn new(
        id: &'static str,
        max_fps: f64,
        signal_type: SignalType,
        emit_probability: f64,
        value: u64,
        ttl_ns: u64,
        seed: u64,
    ) -> Self {
        assert!(
            (0.0..=1.0).contains(&emit_probability),
            "emit_probability must be in [0.0, 1.0]"
        );

        Self {
            id,
            max_fps,
            signal_type,
            output_signals: Box::leak(vec![signal_type].into_boxed_slice()),
            emit_probability,
            value,
            ttl_ns,
            rng: SmallRng::seed_from_u64(seed),
        }
    }
}

impl DetectorNode for ProbabilisticDetector {
    fn meta(&self) -> DetectorMeta {
        DetectorMeta {
            id: self.id,
            max_fps: self.max_fps,
            dependencies: &[],
            output_signals: self.output_signals,
            cost_hint: DetectorCostHint::Low,
            requires_frame: false,
        }
    }

    fn execute(&mut self, ctx: &DetectorContext<'_>) -> DetectorResult {
        let mut signals = Vec::new();
        if self.rng.gen::<f64>() < self.emit_probability {
            signals.push(Signal {
                signal_type: self.signal_type,
                value: self.value,
                ts_ns: ctx.now_ns,
                ttl_ns: self.ttl_ns,
            });
        }

        DetectorResult {
            signals,
            health: DetectorHealth::Ok,
        }
    }
}

pub struct LatencyDetector {
    inner: Box<dyn DetectorNode>,
    latency: Duration,
    jitter: Option<Duration>,
    rng: SmallRng,
    /// Reported cost classification — independent of inner's cost so callers can
    /// choose whether the scheduler should shed this detector under load.
    cost_hint: DetectorCostHint,
    /// Declared fps limit — determines the per-tick time budget used by the
    /// overrun check. Set this to the fps at which the detector is *expected*
    /// to run, not the fps it can sustain given the artificial latency.
    max_fps: f64,
}

impl LatencyDetector {
    /// `cost_hint`: whether the scheduler may back this detector off under load.
    /// `max_fps`: the declared fps budget; if actual runtime exceeds
    ///   `(1/max_fps) × OVERRUN_FACTOR`, the scheduler triggers backoff (only
    ///   when `cost_hint` is `Medium` or `High`).
    pub fn new(
        inner: Box<dyn DetectorNode>,
        latency: Duration,
        jitter: Option<Duration>,
        seed: u64,
        cost_hint: DetectorCostHint,
        max_fps: f64,
    ) -> Self {
        Self {
            inner,
            latency,
            jitter,
            rng: SmallRng::seed_from_u64(seed),
            cost_hint,
            max_fps,
        }
    }
}

impl DetectorNode for LatencyDetector {
    fn meta(&self) -> DetectorMeta {
        let inner = self.inner.meta();
        DetectorMeta {
            id: inner.id,
            max_fps: self.max_fps,
            dependencies: inner.dependencies,
            output_signals: inner.output_signals,
            cost_hint: self.cost_hint,
            requires_frame: inner.requires_frame,
        }
    }

    fn execute(&mut self, ctx: &DetectorContext<'_>) -> DetectorResult {
        let extra_jitter = self
            .jitter
            .map(|jitter| {
                if jitter.is_zero() {
                    Duration::ZERO
                } else {
                    Duration::from_nanos(self.rng.gen_range(0..jitter.as_nanos() as u64))
                }
            })
            .unwrap_or(Duration::ZERO);

        thread::sleep(self.latency + extra_jitter);
        self.inner.execute(ctx)
    }
}

pub struct FailingDetector {
    inner: Box<dyn DetectorNode>,
    ok_count: u64,
    exec_count: u64,
}

impl FailingDetector {
    pub fn new(inner: Box<dyn DetectorNode>, ok_count: u64) -> Self {
        Self {
            inner,
            ok_count,
            exec_count: 0,
        }
    }
}

impl DetectorNode for FailingDetector {
    fn meta(&self) -> DetectorMeta {
        self.inner.meta()
    }

    fn execute(&mut self, ctx: &DetectorContext<'_>) -> DetectorResult {
        self.exec_count += 1;

        if self.exec_count <= self.ok_count {
            self.inner.execute(ctx)
        } else if self.exec_count == self.ok_count + 1 {
            DetectorResult {
                signals: Vec::new(),
                health: DetectorHealth::Failed,
            }
        } else {
            unreachable!("scheduler should disable a detector after Failed health");
        }
    }
}

pub struct ChainedDetector {
    id: &'static str,
    max_fps: f64,
    depends_on: &'static [SignalType],
    emits: &'static [SignalType],
    emit_value: u64,
    ttl_ns: u64,
}

impl ChainedDetector {
    pub fn new(
        id: &'static str,
        max_fps: f64,
        depends_on: SignalType,
        emits: SignalType,
        emit_value: u64,
        ttl_ns: u64,
    ) -> Self {
        Self {
            id,
            max_fps,
            depends_on: Box::leak(vec![depends_on].into_boxed_slice()),
            emits: Box::leak(vec![emits].into_boxed_slice()),
            emit_value,
            ttl_ns,
        }
    }
}

impl DetectorNode for ChainedDetector {
    fn meta(&self) -> DetectorMeta {
        DetectorMeta {
            id: self.id,
            max_fps: self.max_fps,
            dependencies: self.depends_on,
            output_signals: self.emits,
            cost_hint: DetectorCostHint::Low,
            requires_frame: false,
        }
    }

    fn execute(&mut self, ctx: &DetectorContext<'_>) -> DetectorResult {
        DetectorResult {
            signals: vec![Signal {
                signal_type: self.emits[0],
                value: self.emit_value,
                ts_ns: ctx.now_ns,
                ttl_ns: self.ttl_ns,
            }],
            health: DetectorHealth::Ok,
        }
    }
}
