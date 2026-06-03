//! Micro-benchmarks for core RVO hot-path operations.
//!
//! These establish per-operation service times that feed the capacity model:
//!   capacity ≈ Σ(detector_fps × service_time) must fit in one tick budget.
//!
//! Run with:
//!   cargo bench -p rvo-bench --bench micro --release
//!
//! HTML reports land in target/criterion/. Never run in debug — timings are
//! meaningless without optimisation.

use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use crossbeam_channel::bounded;
use opencv::core::Mat;

use rvo_buffer::{Frame, FrameBuffer};
use rvo_clips::ClipManager;
use rvo_detector::detector::{
    DetectorContext, DetectorCostHint, DetectorHealth, DetectorMeta, DetectorNode, DetectorResult,
};
use rvo_events::{Condition, EventDefinition, EventEngine, EventPublisher, EventType};
use rvo_scheduler::scheduler::Scheduler;
use rvo_signals::store::{Signal, SignalStore, SignalType};
use std::sync::{Arc, Mutex};

// ---------- helpers ---------------------------------------------------------

fn test_frame() -> Frame {
    Frame {
        ts: Instant::now(),
        id: 0,
        image: Mat::default(),
    }
}

fn fresh_signal(signal_type: SignalType, value: u64) -> Signal {
    Signal {
        signal_type,
        value,
        ts_ns: 0,
        ttl_ns: u64::MAX,
    }
}

// ---------- SignalStore ------------------------------------------------------

fn bench_signal_store(c: &mut Criterion) {
    let mut g = c.benchmark_group("signal_store");

    g.bench_function("upsert", |b| {
        let mut store = SignalStore::new();
        let sig = fresh_signal(SignalType::Dummy, 1);
        b.iter(|| store.upsert(black_box(sig)));
    });

    g.bench_function("get_hit", |b| {
        let mut store = SignalStore::new();
        store.upsert(fresh_signal(SignalType::Dummy, 1));
        b.iter(|| black_box(store.get(SignalType::Dummy, black_box(100))));
    });

    g.bench_function("get_miss_expired", |b| {
        let mut store = SignalStore::new();
        store.upsert(Signal {
            signal_type: SignalType::Dummy,
            value: 1,
            ts_ns: 0,
            ttl_ns: 1,
        });
        b.iter(|| black_box(store.get(SignalType::Dummy, black_box(1_000_000_000))));
    });

    g.finish();
}

// ---------- FrameBuffer ------------------------------------------------------

fn bench_frame_buffer(c: &mut Criterion) {
    let mut g = c.benchmark_group("frame_buffer");

    g.bench_function("push_300", |b| {
        let mut buf = FrameBuffer::new(300);
        b.iter(|| buf.push(test_frame()));
    });

    g.bench_function("slice_window_10s", |b| {
        let mut buf = FrameBuffer::new(300);
        let t0 = Instant::now();
        for i in 0..300u64 {
            buf.push(Frame {
                ts: t0 + Duration::from_millis(i * 33),
                id: i,
                image: Mat::default(),
            });
        }
        b.iter(|| {
            let end = t0 + Duration::from_secs(10);
            black_box(buf.slice(t0, end))
        });
    });

    g.finish();
}

// ---------- EventEngine ------------------------------------------------------

fn bench_event_engine(c: &mut Criterion) {
    let mut g = c.benchmark_group("event_engine");

    g.bench_function("update_no_fire", |b| {
        let mut engine = EventEngine::new(EventDefinition {
            event_type: EventType::DummyEvent,
            condition: Condition::single_gte(SignalType::Dummy, 1),
            duration_ns: 5_000_000_000,
            cooldown_ns: 5_000_000_000,
        });
        let mut store = SignalStore::new();
        store.upsert(fresh_signal(SignalType::Dummy, 1));
        let mut ns: u64 = 0;
        b.iter(|| {
            ns += 1_000_000;
            black_box(engine.update(ns, &store))
        });
    });

    g.bench_function("update_fires", |b| {
        // duration_ns=0 → instant trigger every tick
        let mut engine = EventEngine::new(EventDefinition {
            event_type: EventType::DummyEvent,
            condition: Condition::single_gte(SignalType::Dummy, 1),
            duration_ns: 0,
            cooldown_ns: 1,
        });
        let mut store = SignalStore::new();
        store.upsert(fresh_signal(SignalType::Dummy, 1));
        let mut ns: u64 = 0;
        b.iter(|| {
            ns += 2;
            black_box(engine.update(ns, &store))
        });
    });

    g.finish();
}

// ---------- Scheduler::tick -------------------------------------------------

/// Minimal no-op detector — measures scheduler overhead with zero detector cost.
struct NullDetector;

impl DetectorNode for NullDetector {
    fn meta(&self) -> DetectorMeta {
        DetectorMeta {
            id: "null",
            max_fps: 1000.0,
            dependencies: &[],
            output_signals: &[],
            cost_hint: DetectorCostHint::Low,
            requires_frame: false,
        }
    }
    fn execute(&mut self, _ctx: &DetectorContext<'_>) -> DetectorResult {
        DetectorResult {
            signals: Vec::new(),
            health: DetectorHealth::Ok,
        }
    }
}

fn build_scheduler(detectors: Vec<Box<dyn DetectorNode>>) -> Scheduler {
    let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(300)));
    let (_, frame_rx) = bounded(16);
    let (clip_tx, _clip_rx) = bounded(8);
    let clip_manager = ClipManager::new(
        clip_tx,
        Duration::from_secs(2),
        Duration::from_secs(1),
        Arc::clone(&frame_buffer),
    );
    let (event_tx, _event_rx) = bounded(64);
    let event_publisher = EventPublisher::new(event_tx);
    let event_engine = EventEngine::new(EventDefinition {
        event_type: EventType::DummyEvent,
        condition: Condition::single_gte(SignalType::Dummy, 1),
        duration_ns: 5_000_000_000,
        cooldown_ns: 5_000_000_000,
    });
    Scheduler::new(
        detectors,
        event_engine,
        frame_rx,
        clip_manager,
        event_publisher,
        frame_buffer,
    )
}

fn bench_scheduler_tick(c: &mut Criterion) {
    let mut g = c.benchmark_group("scheduler_tick");

    g.bench_function("no_detectors", |b| {
        let mut sched = build_scheduler(vec![]);
        b.iter(|| sched.tick());
    });

    for n in [1usize, 4, 8] {
        g.bench_with_input(BenchmarkId::new("null_detectors", n), &n, |b, &n| {
            let detectors = (0..n)
                .map(|_| Box::new(NullDetector) as Box<dyn DetectorNode>)
                .collect();
            let mut sched = build_scheduler(detectors);
            b.iter(|| sched.tick());
        });
    }

    g.finish();
}

// ---------- criterion main --------------------------------------------------

criterion_group!(
    benches,
    bench_signal_store,
    bench_frame_buffer,
    bench_event_engine,
    bench_scheduler_tick,
);
criterion_main!(benches);
