//! RVO macro load harness — produces per-interval time-series and end-of-run
//! summary CSV files, which feed the figures described in `docs/PLOT_GUIDE.md`.
//!
//! # Usage (always release + on bare-metal Linux, never WSL for p99 numbers)
//!
//!   cargo build -p rvo-bench --bin load_harness --release
//!
//!   # Run all 14 scenarios (clears summary.csv first, 2s pause between runs)
//!   ./target/release/load_harness --all
//!   ./target/release/load_harness --all --duration-secs 60   # longer runs
//!
//!   # Single scenario
//!   ./target/release/load_harness --scenario load_shed
//!   ./target/release/load_harness --scenario blocking_10ms --out-dir /tmp/bench
//!
//! # Scenarios
//!
//! ## HOL-blocking group (demonstrates tick latency tracks detector cost)
//!
//! | Scenario           | Detectors                        | Goal                          |
//! |--------------------|----------------------------------|-------------------------------|
//! | baseline           | none                             | pure scheduler overhead       |
//! | inproc_low         | DummyDetector (~0ms)             | cheap in-process baseline     |
//! | blocking_1ms       | LatencyDetector(1ms, Low, 30fps) | HOL blocking at 1ms           |
//! | blocking_3ms       | LatencyDetector(3ms, Low, 30fps) | HOL blocking at 3ms           |
//! | blocking_10ms      | LatencyDetector(10ms,Low, 30fps) | HOL blocking at 10ms          |
//! | blocking_50ms      | LatencyDetector(50ms,Low, 30fps) | HOL blocking at 50ms          |
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
//!   120fps < 182/s → no drops.   300fps > 182/s → channel saturates in <1s → drops.
//!
//! ## fps reference group (baseline throughput with a fast detector)
//!
//! | Scenario           | Detectors    | Camera fps | Goal                          |
//! |--------------------|--------------|------------|-------------------------------|
//! | fps_30             | DummyDetector| 30 fps     | throughput baseline           |
//! | fps_60             | DummyDetector| 60 fps     | 2× baseline                   |
//! | fps_120            | DummyDetector| 120 fps    | 4× baseline                   |
//! | fps_300            | DummyDetector| 300 fps    | 10× baseline (no drops)       |

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use crossbeam_channel::{bounded, Receiver};
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
    "fps_30",
    "fps_60",
    "fps_120",
    "fps_300",
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
        "blocking_1ms" => vec![latency_detector(1, DetectorCostHint::Low, 30.0)],
        "blocking_3ms" => vec![latency_detector(3, DetectorCostHint::Low, 30.0)],
        "blocking_10ms" => vec![latency_detector(10, DetectorCostHint::Low, 30.0)],
        "blocking_50ms" => vec![latency_detector(50, DetectorCostHint::Low, 30.0)],

        // Load-shedding group: High cost + max_fps=60 → budget=33ms < 50ms runtime
        // → overrun fires on first execution → 500ms backoff → tick p99 near-baseline.
        "load_shed" => vec![
            Box::new(DummyDetector),
            latency_detector(50, DetectorCostHint::High, 60.0),
        ],

        // Overload group: Low cost at 1000fps → detector runs every tick (1ms interval)
        // → each tick costs ~5ms → effective tick rate ≈ 182/s.  At 300/600 fps the
        // camera outpaces the scheduler → bounded channel saturates → frame drops.
        // Low cost keeps the detector from being shed (we want slow ticks, not avoided ones).
        "overload_threshold" => vec![latency_detector(5, DetectorCostHint::Low, 1000.0)],
        "overload_moderate" => vec![latency_detector(5, DetectorCostHint::Low, 1000.0)],
        "overload_severe" => vec![latency_detector(5, DetectorCostHint::Low, 1000.0)],

        // fps reference group: DummyDetector is µs-cost so tick rate stays ~2000/s,
        // well above any fps tested here. No drops are expected — this is the fast-pipeline
        // reference that shows the overload group's drops are caused by the slow detector.
        "fps_30" => vec![Box::new(DummyDetector)],
        "fps_60" => vec![Box::new(DummyDetector)],
        "fps_120" => vec![Box::new(DummyDetector)],
        "fps_300" => vec![Box::new(DummyDetector)],

        other => {
            eprintln!("[harness] unknown scenario '{}', using baseline", other);
            vec![]
        }
    }
}

/// Target synthetic camera fps for scenarios that need a camera feed.
fn camera_fps_for(scenario: &str) -> Option<f64> {
    match scenario {
        "fps_30" => Some(30.0),
        "fps_60" => Some(60.0),
        "fps_120" => Some(120.0),
        "fps_300" => Some(300.0),
        "overload_threshold" => Some(120.0),
        "overload_moderate" => Some(300.0),
        "overload_severe" => Some(600.0),
        _ => None,
    }
}

// ---------- Validation -------------------------------------------------------

/// Check that the scenario's intended mechanism actually fired.
///
/// Exits 1 with a diagnostic message when the intended condition never occurs.
/// This prevents silent false-positive benchmark results where the numbers look
/// plausible but the mechanism under test was never exercised.
fn validate_scenario(scenario: &str, hist: &HistSummary, counters: &CounterSnapshot) {
    match scenario {
        "load_shed" => {
            // With effective backoff the tick loop runs at ~2kHz (dominated by
            // fast DummyDetector ticks between 500ms backoff windows). In a 30s
            // measurement window we expect >> 5000 ticks. If the scheduler is
            // instead running at the slow detector's pace (~20/s) — as happens
            // when backoff never fires — total ticks stays around 600.
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

        // overload_threshold is the reference scenario: 120fps < ~182/s drain rate,
        // so zero drops is the CORRECT result. This arm must appear before the
        // starts_with("overload_") catch-all — Rust matches top-to-bottom and
        // "overload_threshold".starts_with("overload_") is true.
        "overload_threshold" => {
            if counters.frame_drops > 0 {
                eprintln!(
                    "\n[BENCH VALIDATION WARN] overload_threshold: {} unexpected frame drops. \
                     This reference scenario should not drop frames (120fps < ~182/s drain). \
                     Check camera fps and effective tick rate.",
                    counters.frame_drops
                );
            } else {
                println!(
                    "[BENCH VALIDATION OK] overload_threshold: 0 frame drops \
                     (120fps below drain capacity of ~182/s, as expected)"
                );
            }
        }

        s if s.starts_with("overload_") => {
            // The camera fps exceeds the effective tick rate (~182/s), so the bounded
            // frame channel must saturate and drop frames. Zero drops means the
            // drain rate was actually >= the feed rate — the slow detector is not
            // running on every tick or the camera fps is too low.
            if counters.frame_drops == 0 {
                eprintln!(
                    "\n[BENCH VALIDATION FAIL] {}: zero frame drops. \
                     Queue never saturated. Effective tick rate >= camera fps. \
                     Check that the slow detector runs on every tick \
                     (max_fps=1000, min_interval=1ms) and that camera fps exceeds 182/s.",
                    s
                );
                std::process::exit(1);
            }
            println!(
                "[BENCH VALIDATION OK] {}: {} frame drops in run \
                 (bounded queue saturated as expected)",
                s, counters.frame_drops
            );
        }

        _ => {}
    }
}

// ---------- run -------------------------------------------------------------

fn run(cli: &Cli) -> std::io::Result<()> {
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
    let camera_fps = camera_fps_for(scenario).unwrap_or(30.0);
    let tx = frame_tx;
    let interval = Duration::from_secs_f64(1.0 / camera_fps);
    thread::spawn(move || {
        let mut id = 0u64;
        loop {
            let _ = tx.try_send(solid_frame(id));
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
        scenario,
        detector_sleep_ms,
        input_fps,
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
    validate_scenario(scenario, &final_hist, &final_counters);

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
    for (i, scenario) in scenarios.iter().enumerate() {
        if total > 1 {
            println!(
                "\n══════════════════════════════════════════════\n \
                 Scenario {}/{}: {}\n\
                 ══════════════════════════════════════════════",
                i + 1,
                total,
                scenario
            );
        }
        // Build a per-scenario Cli with the scenario name overridden.
        let per = Cli {
            scenario: scenario.to_string(),
            all: false,
            duration_secs: cli.duration_secs,
            warmup_secs: cli.warmup_secs,
            sample_ms: cli.sample_ms,
            out_dir: cli.out_dir.clone(),
        };
        // summary.csv must be cleared before the first scenario so we don't
        // append to a stale file from a previous run.
        if i == 0 {
            let sum_path = per.out_dir.join("summary.csv");
            if sum_path.exists() {
                if let Err(e) = std::fs::remove_file(&sum_path) {
                    eprintln!("[harness] warning: could not remove stale summary: {}", e);
                }
            }
        }
        if let Err(err) = run(&per) {
            eprintln!("[harness] error in {}: {}", scenario, err);
            std::process::exit(1);
        }
        // Brief pause between scenarios so the OS scheduler settles.
        if i + 1 < total {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }

    if total > 1 {
        println!(
            "\n[harness] all {} scenarios done. Results in {}/",
            total,
            cli.out_dir.display()
        );
        println!("[harness] see docs/PLOT_GUIDE.md to generate figures.");
    }
}
