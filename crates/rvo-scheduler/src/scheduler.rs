use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use rvo_buffer::{Frame, FrameBuffer};
use rvo_clips::ClipManager;
use rvo_detector::detector::{
    DetectorContext, DetectorCostHint, DetectorHealth, DetectorNode, DetectorResult,
};
use rvo_events::{EventEngine, EventPublisher};
use rvo_metrics::METRICS;
use rvo_signals::store::{SignalStore, SignalType};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;

// When a detector overruns its FPS budget by this factor, it is placed in
// backoff. A factor of 2 means: if a 30fps detector takes > 66ms it backs off.
const OVERRUN_FACTOR: f64 = 2.0;

struct DetectorRuntime {
    last_run: Instant,
    disabled: bool,
    /// When set, the detector is skipped until this instant passes.
    backoff_until: Option<Instant>,
}

impl DetectorRuntime {
    fn new(now: Instant) -> Self {
        Self {
            last_run: now,
            disabled: false,
            backoff_until: None,
        }
    }

    fn is_in_backoff(&self, now: Instant) -> bool {
        self.backoff_until.is_some_and(|until| now < until)
    }

    fn apply_backoff(&mut self, cost: DetectorCostHint, now: Instant) {
        let duration = match cost {
            DetectorCostHint::Low => return, // never back off low-cost detectors
            DetectorCostHint::Medium => Duration::from_millis(100),
            DetectorCostHint::High => Duration::from_millis(500),
        };
        self.backoff_until = Some(now + duration);
    }
}

pub struct Scheduler {
    detectors: Vec<Box<dyn DetectorNode>>,
    runtime: Vec<DetectorRuntime>,
    started_at: Instant,
    signal_store: SignalStore,
    event_engine: EventEngine,
    /// Shared with ClipManager so post-roll threads can read frames after the
    /// scheduler has moved on.
    frame_buffer: Arc<Mutex<FrameBuffer>>,
    frame_rx: Receiver<Frame>,
    clip_manager: ClipManager,
    event_publisher: EventPublisher,
}

impl Scheduler {
    pub fn frame_slice(&self, start: Instant, end: Instant) -> Vec<Frame> {
        self.frame_buffer.lock().unwrap().slice(start, end)
    }

    /// Snapshot the current (non-expired) value of every signal type.
    ///
    /// `None` means the slot is empty or its TTL has lapsed. Used by the TUI
    /// and web dashboards to show live signal state.
    pub fn signal_snapshot(&self) -> Vec<(SignalType, Option<u64>)> {
        let now_ns = Instant::now().duration_since(self.started_at).as_nanos() as u64;
        SignalType::ALL
            .iter()
            .map(|&s| (s, self.signal_store.get(s, now_ns).map(|sig| sig.value)))
            .collect()
    }

    /// Add a detector to the running scheduler.
    ///
    /// Unlike `swap_runtime`, this preserves existing detectors and event
    /// state — it just appends one more node. Callers hold the scheduler lock,
    /// so this never races a `tick()` (both serialize on the same mutex). Used
    /// by the web POC to plug in a model node live.
    pub fn add_detector(&mut self, detector: Box<dyn DetectorNode>) {
        self.runtime.push(DetectorRuntime::new(Instant::now()));
        self.detectors.push(detector);
    }

    /// Ids of the currently registered detectors, in execution order.
    pub fn detector_ids(&self) -> Vec<&'static str> {
        self.detectors.iter().map(|d| d.id()).collect()
    }

    pub fn new(
        detectors: Vec<Box<dyn DetectorNode>>,
        event_engine: EventEngine,
        frame_rx: Receiver<Frame>,
        clip_manager: ClipManager,
        event_publisher: EventPublisher,
        frame_buffer: Arc<Mutex<FrameBuffer>>,
    ) -> Self {
        let now = Instant::now();
        let runtime = detectors
            .iter()
            .map(|_| DetectorRuntime::new(now))
            .collect();

        Self {
            detectors,
            runtime,
            started_at: now,
            signal_store: SignalStore::new(),
            event_engine,
            frame_buffer,
            frame_rx,
            clip_manager,
            event_publisher,
        }
    }

    pub fn tick(&mut self) {
        let tick_start = Instant::now();
        // Queue depth before draining = backlog accumulated since the last tick
        // (saturation signal; rises before frames are dropped).
        METRICS
            .frame_queue_depth
            .store(self.frame_rx.len() as u64, Ordering::Relaxed);

        // Drain new frames without holding the lock across the rest of tick.
        {
            let mut buf = self.frame_buffer.lock().unwrap();
            while let Ok(frame) = self.frame_rx.try_recv() {
                buf.push(frame);
            }
        }

        METRICS.scheduler_ticks.fetch_add(1, Ordering::Relaxed);

        let now = Instant::now();
        let now_ns = now.duration_since(self.started_at).as_nanos() as u64;
        let latest_frame = self.frame_buffer.lock().unwrap().newest_frame();
        if let Some(ref f) = latest_frame {
            // In-process path latency: how stale the newest frame is when the
            // tick picks it up (camera → buffer → scheduler).
            let staleness_ns = now
                .saturating_duration_since(f.ts)
                .as_nanos()
                .min(u64::MAX as u128) as u64;
            METRICS.frame_staleness_ns.record_ns(staleness_ns);
        }

        for (i, detector) in self.detectors.iter_mut().enumerate() {
            // --- Gate 1: permanently disabled by Failed health ---
            if self.runtime[i].disabled {
                METRICS.detector_skips.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            // --- Gate 2: FPS cap ---
            let min_interval = Duration::from_secs_f64(1.0 / detector.max_fps());
            if now.duration_since(self.runtime[i].last_run) < min_interval {
                METRICS.detector_skips.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            // --- Gate 3: load-shedding backoff ---
            if self.runtime[i].is_in_backoff(now) {
                METRICS.detector_skips.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            // --- Gate 4: frame requirement ---
            if detector.requires_frame() && latest_frame.is_none() {
                METRICS.detector_skips.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            // --- Gate 5: signal dependency freshness ---
            let dependencies_fresh = detector
                .dependencies()
                .iter()
                .all(|dep| self.signal_store.get(*dep, now_ns).is_some());

            if !dependencies_fresh {
                METRICS.detector_skips.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            // --- Execute ---
            let ctx = DetectorContext {
                now_ns,
                frame: latest_frame.as_ref(),
            };
            let exec_start = Instant::now();
            // A panicking detector (a bad model adapter, an `unwrap`, an OpenCV
            // edge case) must never poison the scheduler mutex and take down the
            // whole pipeline. Catch the unwind and treat it as a failed health
            // result, so the detector is disabled like any other failure.
            let result = catch_unwind(AssertUnwindSafe(|| detector.execute(&ctx))).unwrap_or(
                DetectorResult {
                    signals: Vec::new(),
                    health: DetectorHealth::Failed,
                },
            );
            let elapsed_ns = exec_start.elapsed().as_nanos().min(u64::MAX as u128) as u64;

            METRICS.detector_execs.fetch_add(1, Ordering::Relaxed);
            METRICS
                .detector_exec_ns_total
                .fetch_add(elapsed_ns, Ordering::Relaxed);
            METRICS.detector_exec_ns.record_ns(elapsed_ns);

            self.runtime[i].last_run = now;

            // --- Health: disable on Failed ---
            if result.health == DetectorHealth::Failed {
                self.runtime[i].disabled = true;
                METRICS.detector_failures.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            // --- Load shedding: backoff on cost overrun ---
            let budget_ns = (min_interval.as_nanos() as f64 * OVERRUN_FACTOR) as u64;
            if elapsed_ns > budget_ns {
                self.runtime[i].apply_backoff(detector.cost_hint(), now);
            }

            // --- Store produced signals ---
            for sig in result.signals {
                self.signal_store.upsert(sig);
            }
        }

        // --- Event evaluation ---
        for event in self.event_engine.update(now_ns, &self.signal_store) {
            METRICS.events_emitted.fetch_add(1, Ordering::Relaxed);
            self.event_publisher.publish(&event);
            self.clip_manager.on_event(&event);
        }

        let tick_ns = tick_start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        METRICS.tick_ns.record_ns(tick_ns);
    }

    pub fn swap_runtime(
        &mut self,
        detectors: Vec<Box<dyn DetectorNode>>,
        event_engine: EventEngine,
    ) {
        let now = Instant::now();
        self.runtime = detectors
            .iter()
            .map(|_| DetectorRuntime::new(now))
            .collect();
        self.detectors = detectors;
        self.event_engine = event_engine;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;
    use rvo_clips::ClipManager;
    use rvo_detector::DummyDetector;
    use rvo_events::{Condition, EventDefinition, EventEngine, EventPublisher, EventType};
    use rvo_signals::store::SignalType;
    use rvo_testkit::start_mock_camera;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn scheduler_runs_without_blocking() {
        let (frame_tx, frame_rx) = bounded(5);
        start_mock_camera(frame_tx);

        let detectors = vec![Box::new(DummyDetector) as Box<dyn DetectorNode>];
        let event_engine = EventEngine::new(EventDefinition {
            event_type: EventType::DummyEvent,
            condition: Condition::single_gte(SignalType::Dummy, 1),
            duration_ns: 0,
            cooldown_ns: 1_000_000_000,
        });

        let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(300)));
        let (clip_tx, _clip_rx) = bounded(1);
        let clip_manager = ClipManager::new(
            clip_tx,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Arc::clone(&frame_buffer),
        );
        let (event_tx, _event_rx) = bounded(1);
        let event_publisher = EventPublisher::new(event_tx);

        let mut scheduler = Scheduler::new(
            detectors,
            event_engine,
            frame_rx,
            clip_manager,
            event_publisher,
            frame_buffer,
        );

        for _ in 0..100 {
            scheduler.tick();
        }
    }

    /// A detector that always panics in `execute`.
    struct PanicDetector;

    impl DetectorNode for PanicDetector {
        fn meta(&self) -> rvo_detector::detector::DetectorMeta {
            rvo_detector::detector::DetectorMeta {
                id: "panic",
                max_fps: 1000.0,
                dependencies: &[],
                output_signals: &[],
                cost_hint: DetectorCostHint::Low,
                requires_frame: false,
            }
        }

        fn execute(&mut self, _ctx: &DetectorContext<'_>) -> DetectorResult {
            panic!("detector boom");
        }
    }

    #[test]
    fn panicking_detector_does_not_kill_scheduler() {
        // Silence the panic backtrace this test deliberately triggers.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let (_frame_tx, frame_rx) = bounded(5);

        let detectors = vec![Box::new(PanicDetector) as Box<dyn DetectorNode>];
        let event_engine = EventEngine::new(EventDefinition {
            event_type: EventType::DummyEvent,
            condition: Condition::single_gte(SignalType::Dummy, 1),
            duration_ns: 0,
            cooldown_ns: 1_000_000_000,
        });

        let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(8)));
        let (clip_tx, _clip_rx) = bounded(1);
        let clip_manager = ClipManager::new(
            clip_tx,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Arc::clone(&frame_buffer),
        );
        let (event_tx, _event_rx) = bounded(1);
        let event_publisher = EventPublisher::new(event_tx);

        let mut scheduler = Scheduler::new(
            detectors,
            event_engine,
            frame_rx,
            clip_manager,
            event_publisher,
            frame_buffer,
        );

        // Without panic isolation the first tick would unwind, poison the
        // scheduler, and fail this test. With it, every tick returns normally
        // and the detector is disabled after its first (caught) failure.
        for _ in 0..50 {
            scheduler.tick();
        }

        std::panic::set_hook(prev);
    }
}
/*
What this proves:
1. Scheduler does not block
2. Camera + scheduler coexist
3. Frame buffer is shared safely between scheduler and clip manager
4. Load shedding gates compile and do not panic
*/
