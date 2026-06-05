//! RVO macro load harness — produces per-interval time-series and end-of-run
//! summary CSV files, which feed the figures described in `docs/PLOT_GUIDE.md`.
//!
//! # Usage (always release + on bare-metal Linux, never WSL for p99 numbers)
//!
//!   cargo build -p rvo-bench --bin load_harness --release
//!
//!   # Run all scenarios (clears summary.csv first, 2s pause between runs)
//!   ./target/release/load_harness --all
//!   ./target/release/load_harness --all --duration-secs 60   # longer runs
//!   ./target/release/load_harness --all --runs 3             # 3 repeats for variance
//!
//!   # Single scenario
//!   ./target/release/load_harness --scenario load_shed
//!   ./target/release/load_harness --scenario blocking_10ms --out-dir /tmp/bench
//!
//! # Scenarios
//!
//! ## HOL-blocking group (demonstrates tick latency tracks detector cost)
//!
//! max_fps=10000 (min_interval=0.1ms) ensures the detector runs on every tick,
//! so tick_p50 directly measures detector latency rather than scheduler overhead.
//!
//! | Scenario           | Detectors                           | tick_p50 | Goal                          |
//! |--------------------|-------------------------------------|----------|-------------------------------|
//! | baseline           | none                                | ~5µs     | pure scheduler overhead       |
//! | inproc_low         | DummyDetector (~0ms)                | ~5µs     | cheap in-process baseline     |
//! | blocking_1ms       | LatencyDetector(1ms, Low, 10000fps) | ~1ms     | HOL blocking at 1ms           |
//! | blocking_3ms       | LatencyDetector(3ms, Low, 10000fps) | ~3ms     | HOL blocking at 3ms           |
//! | blocking_10ms      | LatencyDetector(10ms,Low, 10000fps) | ~10ms    | HOL blocking at 10ms          |
//! | blocking_50ms      | LatencyDetector(50ms,Low, 10000fps) | ~50ms    | drain≈19.8/s < 30fps camera → ring-buffer loss |
//!
//! ## Load-shedding group (demonstrates shedding decouples tick from slow detector)
//!
//! | Scenario           | Detectors                                          | Goal                |
//! |--------------------|----------------------------------------------------|---------------------|
//! | load_shed          | DummyDetector + LatencyDetector(50ms, High, 60fps) | backoff in action   |
//!
//! Why 60fps for the LatencyDetector in load_shed?
//!   min_interval = 1/60 ≈ 16.7ms, budget = 16.7ms × 2 = 33ms.
//!   50ms > 33ms  →  overrun triggers  →  apply_backoff(High)  →  500ms backoff.
//!   Tick p99 stays near-baseline (DummyDetector runs freely between backoff windows).
//!
//! ## Overload group (demonstrates bounded queues shed frames, not latency)
//!
//! | Scenario           | Detectors                           | Camera fps | Goal                 |
//! |--------------------|-------------------------------------|------------|----------------------|
//! | overload_threshold | LatencyDetector(5ms, Low, 1000fps)  |  120 fps   | no drops (reference) |
//! | overload_moderate  | LatencyDetector(5ms, Low, 1000fps)  |  300 fps   | moderate drops       |
//! | overload_severe    | LatencyDetector(5ms, Low, 1000fps)  |  600 fps   | heavy drops          |
//!
//! Why 1000fps for the slow detector and Low cost?
//!   min_interval = 1ms, so the detector runs on every eligible tick.
//!   5ms sleep + 0.5ms inter-tick sleep = 5.5ms/tick → effective tick rate ≈ 182/s.
//!   Low cost = never shed (we want the tick to be genuinely slow, not backed off).
//!   The scheduler batch-drains the frame channel on every tick, so the bounded
//!   channel (cap 64) stays shallow and never saturates. Frame loss happens in the
//!   FrameBuffer ring buffer: frames that arrive faster than ticks are silently
//!   overwritten before a detector ever reads them.
//!   Overload is confirmed by: effective_fps = ticks/duration < camera_fps.
//!
//! ## Throughput ceiling group (fast detector, camera fps sweeps across scheduler tick-rate ceiling)
//!
//! With DummyDetector the scheduler ticks at ~1756/s. Scenarios probe below and
//! above that ceiling to show when the batch-drain ring buffer starts losing frames.
//! frame_loss_rate is computed from actual_camera_fps (measured), not configured fps.
//!
//! | Scenario   | Camera fps | Expected outcome                              |
//! |------------|------------|-----------------------------------------------|
//! | fps_1000   | 1000 fps   | scheduler (1756/s) keeps up → 0 frame loss    |
//! | fps_2000   | 2000 fps   | just over ceiling → moderate ring-buffer loss  |
//! | fps_5000   | 5000 fps   | well over ceiling → heavy ring-buffer loss     |

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use crossbeam_channel::{bounded, Receiver, TrySendError};
use rvo_bench::{CounterSnapshot, CsvWriter, HistSummary};
use rvo_buffer::{Frame, FrameBuffer};
use rvo_clips::ClipManager;
use rvo_detector::detector::{DetectorCostHint, DetectorNode};
use rvo_detector::DummyDetector;
use rvo_events::{Condition, Event, EventDefinition, EventEngine, EventPublisher, EventType};
use rvo_metrics::METRICS;
use rvo_scheduler::scheduler::Scheduler;
use rvo_signals::store::SignalType;
use rvo_testkit::LatencyDetector;

// ---------- CLI -------------------------------------------------------------

const ALL_SCENARIOS: &[&str] = &[
    "baseline",
    "inproc_low",
    "blocking_1ms",
    "blocking_3ms",
    "blocking_10ms",
    "blocking_50ms",
    "load_shed",
    "overload_threshold",
    "overload_moderate",
    "overload_severe",
    "fps_1000",
    "fps_2000",
    "fps_5000",
];

#[derive(Parser)]
#[command(name = "load_harness", about = "RVO macro load harness")]
struct Cli {
    /// Scenario to run. Ignored when --all is set.
    #[arg(long, default_value = "baseline")]
    scenario: String,

    /// Run all scenarios sequentially (overrides --scenario).
    #[arg(long)]
    all: bool,

    /// Measurement window per scenario in seconds (excludes warm-up).
    /// Total wall time per scenario = warmup_secs + duration_secs.
    #[arg(long, default_value_t = 30)]
    duration_secs: u64,

    /// Warm-up period in seconds. Excluded from all reported metrics.
    #[arg(long, default_value_t = 5)]
    warmup_secs: u64,

    /// How often to sample counter deltas for the time-series (milliseconds).
    #[arg(long, default_value_t = 1000)]
    sample_ms: u64,

    /// How many times to repeat each scenario. Use ≥3 to get mean/stddev from the CSV.
    #[arg(long, default_value_t = 1)]
    runs: u64,

    /// Directory to write CSV files into.
    #[arg(long, default_value = "target/bench_results")]
    out_dir: PathBuf,
}

// ---------- Harness internals -----------------------------------------------

/// A minimal empty frame — sufficient for in-process detectors.
fn solid_frame(id: u64) -> Frame {
    Frame {
        ts: Instant::now(),
        id,
        image: opencv::core::Mat::default(),
    }
}

/// Receivers that must be kept alive for the duration of a scenario run.
///
/// The harness does not consume events or clips — these channels exist only to
/// satisfy the API. Dropping them before the scheduler runs causes every
/// `publish()` / `on_event()` to see `Disconnected` and emit warning lines.
struct _Sinks {
    _event_rx: Receiver<Event>,
    _clip_rx: Receiver<(rvo_clips::clip::ClipJob, Vec<Frame>)>,
}

/// Build the scheduler from a detector list and a shared frame buffer.
/// Returns the scheduler, the frame sender, and the sink receivers.
/// The caller must hold `_Sinks` alive for the entire scenario run.
fn build_scheduler(
    detectors: Vec<Box<dyn DetectorNode>>,
    frame_buffer: Arc<Mutex<FrameBuffer>>,
) -> (Scheduler, crossbeam_channel::Sender<Frame>, _Sinks) {
    let (frame_tx, frame_rx) = bounded(64);
    let (clip_tx, clip_rx) = bounded(8);
    let clip_manager = ClipManager::new(
        clip_tx,
        Duration::from_secs(2),
        Duration::from_secs(1),
        Arc::clone(&frame_buffer),
    );
    let (event_tx, event_rx) = bounded(64);
    let event_publisher = EventPublisher::new(event_tx);
    let event_engine = EventEngine::new(EventDefinition {
        event_type: EventType::DummyEvent,
        condition: Condition::single_gte(SignalType::Dummy, 1),
        duration_ns: 100_000_000, // 100 ms
        cooldown_ns: 500_000_000,
    });
    let scheduler = Scheduler::new(
        detectors,
        event_engine,
        frame_rx,
        clip_manager,
        event_publisher,
        Arc::clone(&frame_buffer),
    );
    let sinks = _Sinks {
        _event_rx: event_rx,
        _clip_rx: clip_rx,
    };
    (scheduler, frame_tx, sinks)
}

/// Build a `LatencyDetector` with explicit cost classification and declared fps.
///
/// `cost_hint` controls whether the scheduler may back this detector off.
/// `max_fps` determines the overrun budget: actual > (1/max_fps × 2) triggers backoff.
fn latency_detector(
    sleep_ms: u64,
    cost_hint: DetectorCostHint,
    max_fps: f64,
) -> Box<dyn DetectorNode> {
    Box::new(LatencyDetector::new(
        Box::new(DummyDetector),
        Duration::from_millis(sleep_ms),
        None,
        42,
        cost_hint,
        max_fps,
    ))
}

/// Build the detector list for a named scenario.
fn detectors_for(scenario: &str) -> Vec<Box<dyn DetectorNode>> {
    match scenario {
        // HOL-blocking group: Low cost so the scheduler never sheds, demonstrating
        // that a slow in-process detector directly delays every tick.
        "baseline" => vec![],
        "inproc_low" => vec![Box::new(DummyDetector)],
        // max_fps=10000 (min_interval=0.1ms) → detector runs on every tick.
        // tick_p50 directly measures the injected sleep, not scheduler overhead.
        "blocking_1ms" => vec![latency_detector(1, DetectorCostHint::Low, 10_000.0)],
        "blocking_3ms" => vec![latency_detector(3, DetectorCostHint::Low, 10_000.0)],
        "blocking_10ms" => vec![latency_detector(10, DetectorCostHint::Low, 10_000.0)],
        "blocking_50ms" => vec![latency_detector(50, DetectorCostHint::Low, 10_000.0)],

        // Load-shedding group: High cost + max_fps=60 → budget=33ms < 50ms runtime
        // → overrun fires on first execution → 500ms backoff → tick p99 near-baseline.
        "load_shed" => vec![
            Box::new(DummyDetector),
            latency_detector(50, DetectorCostHint::High, 60.0),
        ],

        // Overload group: Low cost at 1000fps → detector runs every tick (1ms interval)
        // → each tick costs ~5ms → effective tick rate ≈ 182/s.  At 300/600 fps the
        // camera outpaces the scheduler → FrameBuffer ring-buffer overwrites (not
        // channel saturation — the scheduler batch-drains all pending frames per tick).
        // Low cost keeps the detector from being shed (we want slow ticks, not avoided ones).
        "overload_threshold" => vec![latency_detector(5, DetectorCostHint::Low, 1000.0)],
        "overload_moderate" => vec![latency_detector(5, DetectorCostHint::Low, 1000.0)],
        "overload_severe" => vec![latency_detector(5, DetectorCostHint::Low, 1000.0)],

        // Throughput ceiling group: DummyDetector tick rate ≈1756/s.
        // fps_1000 stays below the ceiling (no loss). fps_2000/5000 exceed it,
        // causing ring-buffer overwrites at (actual_camera_fps - effective_fps) frames/s.
        "fps_1000" => vec![Box::new(DummyDetector)],
        "fps_2000" => vec![Box::new(DummyDetector)],
        "fps_5000" => vec![Box::new(DummyDetector)],

        other => {
            eprintln!("[harness] unknown scenario '{}', using baseline", other);
            vec![]
        }
    }
}

/// Target synthetic camera fps for scenarios that need a camera feed.
fn camera_fps_for(scenario: &str) -> Option<f64> {
    match scenario {
        "fps_1000" => Some(1000.0),
        "fps_2000" => Some(2000.0),
        "fps_5000" => Some(5000.0),
        "overload_threshold" => Some(120.0),
        "overload_moderate" => Some(300.0),
        "overload_severe" => Some(600.0),
        _ => None,
    }
}

// ---------- Validation -------------------------------------------------------

/// Check that the scenario's intended mechanism actually fired.
///
/// For scenarios where the mechanism under test must produce a specific outcome
/// (load_shed, overload_moderate/severe), exits 1 with a diagnostic. For reference
/// scenarios, prints OK or WARN without aborting so a full --all run still completes.
///
/// `duration_secs` is the measurement window (excludes warm-up) and is used to
/// compute the effective tick rate for overload validation.
fn validate_scenario(
    scenario: &str,
    hist: &HistSummary,
    counters: &CounterSnapshot,
    duration_secs: u64,
    actual_camera_fps: f64,
) {
    match scenario {
        // ---- HOL-blocking group ------------------------------------------------
        // Tick rate >> camera 30 fps for all sub-scenarios except blocking_50ms.
        // Frame drops are always 0 for 1/3/10ms variants; the 50ms variant is a
        // known exception where the drain rate falls below the camera rate.
        "baseline" | "inproc_low" => {
            let tick_p99_ms = hist.tick_p99_ns as f64 / 1e6;
            if counters.frame_drops > 0 {
                eprintln!(
                    "\n[BENCH VALIDATION WARN] {}: {} unexpected frame drops \
                     (tick rate ~2000/s >> camera 30fps — drops should not occur). \
                     tick_p99={:.2}ms. Check system load or scheduler regression.",
                    scenario, counters.frame_drops, tick_p99_ms
                );
            } else {
                println!(
                    "[BENCH VALIDATION OK] {}: 0 frame drops, tick_p99={:.2}ms",
                    scenario, tick_p99_ms
                );
            }
        }

        "blocking_1ms" | "blocking_3ms" | "blocking_10ms" => {
            let expected_ms = match scenario {
                "blocking_1ms" => 1.0_f64,
                "blocking_3ms" => 3.0_f64,
                _ => 10.0_f64,
            };
            let tick_p50_ms = hist.tick_p50_ns as f64 / 1e6;
            let tick_p99_ms = hist.tick_p99_ns as f64 / 1e6;
            if counters.frame_drops > 0 {
                eprintln!(
                    "\n[BENCH VALIDATION WARN] {}: {} unexpected frame drops. tick_p50={:.2}ms",
                    scenario, counters.frame_drops, tick_p50_ms
                );
            } else if tick_p50_ms < expected_ms * 0.5 || tick_p50_ms > expected_ms * 2.5 {
                // tick_p50 should track the detector sleep. A large deviation means
                // the detector isn't running every tick (check max_fps) or system jitter.
                eprintln!(
                    "\n[BENCH VALIDATION WARN] {}: tick_p50={:.2}ms expected ~{:.0}ms \
                     (detector may not be running every tick). tick_p99={:.2}ms",
                    scenario, tick_p50_ms, expected_ms, tick_p99_ms
                );
            } else {
                println!(
                    "[BENCH VALIDATION OK] {}: tick_p50={:.2}ms ≈ {}ms injected sleep, \
                     0 frame drops",
                    scenario, tick_p50_ms, expected_ms as u64
                );
            }
        }

        // blocking_50ms: effective tick rate ≈ 1/(50ms+0.5ms) ≈ 19.8/s, below the
        // camera's 30fps. The batch-drain scheduler keeps the channel shallow (it
        // drains all pending frames on each tick), but ~10 frames/s are silently
        // overwritten in the FrameBuffer ring buffer. frame_drops stays 0 because
        // the channel never fills. The scenario measures HOL tick latency, not drops.
        "blocking_50ms" => {
            let tick_p99_ms = hist.tick_p99_ns as f64 / 1e6;
            let effective_fps = counters.ticks as f64 / duration_secs as f64;
            println!(
                "[BENCH VALIDATION NOTE] blocking_50ms: effective {:.1}/s < camera 30fps \
                 (~{:.0} frames/s lost in ring buffer — expected for 50ms HOL scenario). \
                 tick_p99={:.2}ms",
                effective_fps,
                30.0_f64 - effective_fps,
                tick_p99_ms
            );
        }

        // ---- Load-shedding group -----------------------------------------------
        // With effective backoff the tick loop runs at ~2kHz (dominated by fast
        // DummyDetector ticks between 500ms backoff windows). In a 30s measurement
        // window we expect >> 5000 ticks. If the scheduler is instead running at
        // the slow detector's pace (~20/s) — as happens when backoff never fires —
        // total ticks stays around 600.
        "load_shed" => {
            let tick_p99_ms = hist.tick_p99_ns as f64 / 1e6;
            if counters.ticks < 5_000 {
                eprintln!(
                    "\n[BENCH VALIDATION FAIL] load_shed: {} ticks recorded \
                     (expected >> 5000). tick_p99={:.2}ms. \
                     Load-shedding did not activate — check cost_hint and overrun budget.",
                    counters.ticks, tick_p99_ms
                );
                std::process::exit(1);
            }
            println!(
                "[BENCH VALIDATION OK] load_shed: {} ticks, tick_p99={:.2}ms \
                 (backoff active, fast detector running freely)",
                counters.ticks, tick_p99_ms
            );
        }

        // ---- Overload group ----------------------------------------------------
        // The scheduler batch-drains the frame channel on every tick (drains ALL
        // pending frames via while-try_recv). This means the bounded channel never
        // saturates, and frame_drops (channel-level) always stays 0. Frame loss
        // instead occurs silently in the FrameBuffer ring buffer when frames are
        // overwritten before a detector reads them.
        //
        // The correct overload signal is therefore the EFFECTIVE TICK RATE:
        //   effective_fps = ticks / duration_secs
        //
        // If effective_fps < camera_fps, the scheduler cannot keep up and frames
        // are lost in the ring buffer. If effective_fps >= camera_fps, every frame
        // that arrived had a corresponding tick that could read it.
        //
        // overload_threshold is the REFERENCE: 120fps << ~182/s effective rate →
        // scheduler keeps up → no frame loss. This arm must come before the
        // starts_with("overload_") wildcard — Rust matches top-to-bottom.
        "overload_threshold" => {
            let effective_fps = counters.ticks as f64 / duration_secs as f64;
            let camera_fps = camera_fps_for(scenario).unwrap_or(120.0);
            if effective_fps < camera_fps {
                eprintln!(
                    "\n[BENCH VALIDATION WARN] overload_threshold: effective tick rate \
                     {:.1}/s is below camera {:.0}fps — scheduler cannot keep up. \
                     Expected: ~182/s >> 120fps. Check detector latency and system load.",
                    effective_fps, camera_fps
                );
            } else {
                println!(
                    "[BENCH VALIDATION OK] overload_threshold: effective {:.1}/s >= \
                     camera {:.0}fps (scheduler keeps up, no frame loss, as expected)",
                    effective_fps, camera_fps
                );
            }
        }

        s if s.starts_with("overload_") => {
            // Camera fps exceeds the effective tick rate (~182/s) → scheduler cannot
            // read every frame → frames are overwritten in the FrameBuffer. Validated
            // by checking that effective_fps (ticks/duration) < camera_fps.
            let effective_fps = counters.ticks as f64 / duration_secs as f64;
            let camera_fps = camera_fps_for(scenario).unwrap_or(300.0);
            if effective_fps >= camera_fps {
                eprintln!(
                    "\n[BENCH VALIDATION FAIL] {}: effective tick rate {:.1}/s >= \
                     camera {:.0}fps — scheduler is keeping up, no frame loss. \
                     Expected: ~182/s << {:.0}fps. Check that the slow detector \
                     runs on every tick (max_fps=1000, min_interval=1ms).",
                    s, effective_fps, camera_fps, camera_fps
                );
                std::process::exit(1);
            }
            let loss_rate = camera_fps - effective_fps;
            println!(
                "[BENCH VALIDATION OK] {}: effective {:.1}/s < camera {:.0}fps \
                 (~{:.0} frames/s lost in ring buffer, as expected)",
                s, effective_fps, camera_fps, loss_rate
            );
        }

        // ---- Throughput ceiling group ------------------------------------------
        // Scheduler tick rate ≈1756/s. fps_1000 should see no frame loss.
        // fps_2000/5000 exceed the ceiling → ring-buffer overwrites.
        // frame_loss_rate = actual_camera_fps - effective_fps (both measured).
        s if s.starts_with("fps_") => {
            let effective_fps = counters.ticks as f64 / duration_secs as f64;
            let frame_loss = (actual_camera_fps - effective_fps).max(0.0);
            if s == "fps_1000" {
                if effective_fps < actual_camera_fps {
                    eprintln!(
                        "\n[BENCH VALIDATION WARN] {}: scheduler ({:.0}/s) below camera \
                         ({:.0}/s) — expected no frame loss at 1000fps. Check system load.",
                        s, effective_fps, actual_camera_fps
                    );
                } else {
                    println!(
                        "[BENCH VALIDATION OK] {}: effective {:.0}/s >= actual camera {:.0}/s \
                         → 0 frame loss (below scheduler ceiling, as expected)",
                        s, effective_fps, actual_camera_fps
                    );
                }
            } else if effective_fps >= actual_camera_fps {
                eprintln!(
                    "\n[BENCH VALIDATION WARN] {}: scheduler ({:.0}/s) >= camera ({:.0}/s) — \
                     expected frame loss above ceiling. Actual camera fps may be lower than \
                     configured (sleep granularity).",
                    s, effective_fps, actual_camera_fps
                );
            } else {
                println!(
                    "[BENCH VALIDATION OK] {}: effective {:.0}/s < camera {:.0}/s \
                     → {:.0} frames/s lost in ring buffer (above scheduler ceiling, as expected)",
                    s, effective_fps, actual_camera_fps, frame_loss
                );
            }
        }

        _ => {}
    }
}

// ---------- run -------------------------------------------------------------

fn run(cli: &Cli, run_id: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(&cli.out_dir)?;
    let stem = format!("{}_{}", cli.scenario, cli.duration_secs);
    let ts_path = cli.out_dir.join(format!("{}_timeseries.csv", stem));
    let sum_path = cli.out_dir.join("summary.csv");

    let mut ts_csv = CsvWriter::create_time_series(Path::new(&ts_path))?;
    // Summary appends so multiple invocations accumulate in one file.
    let mut sum_csv = if sum_path.exists() {
        CsvWriter::append_summary(Path::new(&sum_path))?
    } else {
        CsvWriter::create_summary(Path::new(&sum_path))?
    };

    let scenario = &cli.scenario;
    println!(
        "[harness] scenario={} duration={}s warmup={}s sample={}ms",
        scenario, cli.duration_secs, cli.warmup_secs, cli.sample_ms
    );

    // Benchmark isolation: each scenario must start with empty counters and
    // histograms. Without this, scenario N inherits all samples from scenario
    // N-1, making cumulative tick counts and histograms meaningless.
    METRICS.reset();

    // Build the pipeline.
    let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(300)));
    let detectors = detectors_for(scenario);
    let (mut scheduler, frame_tx, _sinks) = build_scheduler(detectors, Arc::clone(&frame_buffer));

    // Synthetic camera thread — sends at target fps with try_send (drops on full).
    // frames_sent counts successful sends; used to compute actual_camera_fps at the end.
    let camera_fps = camera_fps_for(scenario).unwrap_or(30.0);
    let tx = frame_tx;
    let interval = Duration::from_secs_f64(1.0 / camera_fps);
    let frames_sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let frames_sent_cam = Arc::clone(&frames_sent);
    thread::spawn(move || {
        let mut id = 0u64;
        loop {
            match tx.try_send(solid_frame(id)) {
                Ok(()) => {
                    frames_sent_cam.fetch_add(1, Ordering::Relaxed);
                }
                // Channel full: scheduler is genuinely behind — count as a drop.
                Err(TrySendError::Full(_)) => {
                    METRICS.frame_drops.fetch_add(1, Ordering::Relaxed);
                }
                // Scheduler dropped its receiver (scenario ended). Exit so dead
                // threads from previous scenarios do not pollute the next run's
                // frame_drops counter via spurious Disconnected increments.
                Err(TrySendError::Disconnected(_)) => break,
            }
            id += 1;
            thread::sleep(interval);
        }
    });

    let start = Instant::now();
    let warmup = Duration::from_secs(cli.warmup_secs);
    let total = Duration::from_secs(cli.duration_secs);
    let sample_interval = Duration::from_millis(cli.sample_ms);

    let mut last_sample = start;
    let mut last_counters = CounterSnapshot::capture();
    let mut in_warmup = true;
    let mut frames_at_warmup_end: u64 = 0;

    println!("[harness] warming up for {}s ...", cli.warmup_secs);

    loop {
        scheduler.tick();
        // 500µs ceiling keeps CPU load sane. For overload scenarios the slow
        // detector (5ms) dominates this, reducing tick rate to ~182/s.
        thread::sleep(Duration::from_micros(500));

        let elapsed = start.elapsed();
        if in_warmup && elapsed >= warmup {
            in_warmup = false;
            // Reset metrics so histograms and counters cover only the measurement
            // window, not warm-up. Without this, reported percentiles include
            // warm-up samples and total_ticks includes warm-up ticks.
            METRICS.reset();
            frames_at_warmup_end = frames_sent.load(Ordering::Relaxed);
            last_sample = Instant::now();
            last_counters = CounterSnapshot::capture();
            println!("[harness] warm-up done, measuring ...");
        }

        // Break after warmup + measurement window, so --duration-secs is the
        // actual measurement window, not the total including warm-up.
        if elapsed >= warmup + total {
            break;
        }

        if !in_warmup && last_sample.elapsed() >= sample_interval {
            let now_counters = CounterSnapshot::capture();
            let delta = now_counters.delta_since(&last_counters);
            let hist = HistSummary::capture();
            // Normalize to measurement start (0 at warmup end) so Figure 2
            // time-series plots have a 0-based X-axis in the paper.
            let elapsed_ms = elapsed.saturating_sub(warmup).as_millis() as u64;
            ts_csv.write_time_series_row(elapsed_ms, &delta, &hist)?;
            last_counters = now_counters;
            last_sample = Instant::now();

            println!(
                "[harness] t={:.1}s  tick_p99={:.2}ms  skips/s={}  frame_drops/s={}",
                elapsed.as_secs_f64(),
                hist.tick_p99_ns as f64 / 1e6,
                delta.skips,
                delta.frame_drops,
            );
        }
    }

    ts_csv.flush()?;

    // End-of-run summary.
    let final_hist = HistSummary::capture();
    let final_counters = CounterSnapshot::capture();

    // Compute actual camera fps from frames sent during the measurement window only.
    // This is more accurate than the configured fps for scenarios where thread::sleep
    // granularity limits the actual send rate (high-fps scenarios).
    let actual_camera_fps = {
        let frames_in_window = frames_sent
            .load(Ordering::Relaxed)
            .saturating_sub(frames_at_warmup_end);
        frames_in_window as f64 / cli.duration_secs as f64
    };

    let detector_sleep_ms: u64 = match scenario.as_str() {
        "blocking_1ms" => 1,
        "blocking_3ms" => 3,
        "blocking_10ms" => 10,
        "blocking_50ms" => 50,
        "load_shed" => 50,
        s if s.starts_with("overload_") => 5,
        _ => 0,
    };
    let input_fps = camera_fps_for(scenario).unwrap_or(30.0);

    sum_csv.write_summary_row(
        run_id,
        scenario,
        detector_sleep_ms,
        input_fps,
        actual_camera_fps,
        cli.duration_secs,
        &final_hist,
        &final_counters,
    )?;
    sum_csv.flush()?;

    println!(
        "[harness] DONE  tick_p50={:.2}ms  tick_p99={:.2}ms  tick_p999={:.2}ms  \
         ticks={}  frame_drops={}",
        final_hist.tick_p50_ns as f64 / 1e6,
        final_hist.tick_p99_ns as f64 / 1e6,
        final_hist.tick_p999_ns as f64 / 1e6,
        final_counters.ticks,
        final_counters.frame_drops,
    );
    println!("[harness] time-series → {}", ts_path.display());
    println!("[harness] summary     → {}", sum_path.display());

    // Self-validation: fail loudly if the intended mechanism did not fire.
    validate_scenario(
        scenario,
        &final_hist,
        &final_counters,
        cli.duration_secs,
        actual_camera_fps,
    );

    Ok(())
}

fn main() {
    let cli = Cli::parse();

    let scenarios: Vec<&str> = if cli.all {
        ALL_SCENARIOS.to_vec()
    } else {
        vec![cli.scenario.as_str()]
    };

    let total = scenarios.len();
    let runs = cli.runs.max(1);

    // Clear summary.csv once before the very first scenario of run 1 so stale
    // data from a previous invocation does not accumulate.
    {
        let sum_path = cli.out_dir.join("summary.csv");
        if sum_path.exists() {
            if let Err(e) = std::fs::remove_file(&sum_path) {
                eprintln!("[harness] warning: could not remove stale summary: {}", e);
            }
        }
    }

    for run_id in 1..=runs {
        if runs > 1 {
            println!("\n══════════════════════════════════════════════");
            println!(" Run {}/{}", run_id, runs);
            println!("══════════════════════════════════════════════");
        }
        for (i, scenario) in scenarios.iter().enumerate() {
            if total > 1 {
                println!(
                    "\n══════════════════════════════════════════════\n \
                     Scenario {}/{}: {}{}\n\
                     ══════════════════════════════════════════════",
                    i + 1,
                    total,
                    scenario,
                    if runs > 1 {
                        format!("  [run {}/{}]", run_id, runs)
                    } else {
                        String::new()
                    }
                );
            }
            // Build a per-scenario Cli with the scenario name overridden.
            let per = Cli {
                scenario: scenario.to_string(),
                all: false,
                runs: 1,
                duration_secs: cli.duration_secs,
                warmup_secs: cli.warmup_secs,
                sample_ms: cli.sample_ms,
                out_dir: cli.out_dir.clone(),
            };
            if let Err(err) = run(&per, run_id) {
                eprintln!("[harness] error in {}: {}", scenario, err);
                std::process::exit(1);
            }
            // Brief pause between scenarios so the OS scheduler settles.
            if i + 1 < total || run_id < runs {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }

    if total > 1 || runs > 1 {
        println!(
            "\n[harness] {} scenarios × {} run(s) done. Results in {}/",
            total,
            runs,
            cli.out_dir.display()
        );
        println!("[harness] see docs/PLOT_GUIDE.md to generate figures.");
    }
}
