//! RVO macro load harness — produces per-interval time-series and end-of-run
//! summary CSV files consumed by `scripts/plot.py`.
//!
//! # Usage (always release + on bare-metal Linux, never WSL for p99 numbers)
//!
//!   cargo build -p rvo-bench --bin load_harness --release
//!   ./target/release/load_harness --scenario baseline --duration-secs 30
//!   ./target/release/load_harness --scenario blocking_3ms --duration-secs 30
//!
//! Or run all scenarios via `scripts/bench.sh`.
//!
//! # Scenarios
//!
//! | Scenario           | Detectors                  | Goal                          |
//! |--------------------|----------------------------|-------------------------------|
//! | baseline           | none                        | pure scheduler overhead       |
//! | inproc_low         | DummyDetector (µs cost)    | cheap in-process baseline     |
//! | blocking_1ms       | LatencyDetector(1ms)       | HOL blocking at 1ms           |
//! | blocking_3ms       | LatencyDetector(3ms)       | HOL blocking at 3ms (default) |
//! | blocking_10ms      | LatencyDetector(10ms)      | HOL blocking at 10ms          |
//! | blocking_50ms      | LatencyDetector(50ms)      | severe HOL / load-shedding    |
//! | load_shed          | Dummy + LatencyDetector(50ms) | load-shedding in action    |
//! | fps_30             | DummyDetector, camera 30fps | throughput baseline           |
//! | fps_60             | DummyDetector, camera 60fps | saturation onset              |
//! | fps_120            | DummyDetector, camera 120fps| drop-or-process tradeoff      |
//! | fps_300            | DummyDetector, camera 300fps| sustained overload            |

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use crossbeam_channel::bounded;
use rvo_bench::{CounterSnapshot, CsvWriter, HistSummary};
use rvo_buffer::{Frame, FrameBuffer};
use rvo_clips::ClipManager;
use rvo_detector::detector::DetectorNode;
use rvo_detector::DummyDetector;
use rvo_events::{Condition, EventDefinition, EventEngine, EventPublisher, EventType};
use rvo_scheduler::scheduler::Scheduler;
use rvo_signals::store::SignalType;
use rvo_testkit::LatencyDetector;

// ---------- CLI -------------------------------------------------------------

#[derive(Parser)]
#[command(name = "load_harness", about = "RVO macro load harness")]
struct Cli {
    /// Which scenario to run.
    #[arg(long, default_value = "baseline")]
    scenario: String,

    /// How long to run the scenario (seconds). Exclude the first
    /// `--warmup-secs` for percentile measurement.
    #[arg(long, default_value_t = 30)]
    duration_secs: u64,

    /// Warm-up period excluded from reported metrics (seconds).
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

/// A minimal empty frame (Mat::default) — sufficient for in-process detectors.
/// Remote detectors that JPEG-encode would need a real Mat, but those are
/// run via the demo services separately, not through this harness.
fn solid_frame(id: u64) -> Frame {
    Frame {
        ts: Instant::now(),
        id,
        image: opencv::core::Mat::default(),
    }
}

/// Build the scheduler from a detector list and a shared frame buffer.
fn build_scheduler(
    detectors: Vec<Box<dyn DetectorNode>>,
    frame_buffer: Arc<Mutex<FrameBuffer>>,
) -> (Scheduler, crossbeam_channel::Sender<Frame>) {
    let (frame_tx, frame_rx) = bounded(64);
    let (clip_tx, _clip_rx) = bounded(8);
    let clip_manager = ClipManager::new(
        clip_tx,
        Duration::from_secs(2),
        Duration::from_secs(1),
        Arc::clone(&frame_buffer),
    );
    let (event_tx, _event_rx) = bounded(64);
    let event_publisher = EventPublisher::new(event_tx);
    // Use a short-confirmation event that exercises the state machine.
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
    (scheduler, frame_tx)
}

/// Build a `LatencyDetector` wrapping a `DummyDetector` with a fixed sleep.
fn latency_detector(sleep_ms: u64) -> Box<dyn DetectorNode> {
    use std::time::Duration;
    Box::new(LatencyDetector::new(
        Box::new(DummyDetector),
        Duration::from_millis(sleep_ms),
        None,
        42,
    ))
}

/// Build the detector list for a named scenario.
fn detectors_for(scenario: &str) -> Vec<Box<dyn DetectorNode>> {
    match scenario {
        "baseline" => vec![],
        "inproc_low" => vec![Box::new(DummyDetector)],
        "blocking_1ms" => vec![latency_detector(1)],
        "blocking_3ms" => vec![latency_detector(3)],
        "blocking_10ms" => vec![latency_detector(10)],
        "blocking_50ms" => vec![latency_detector(50)],
        "load_shed" => vec![Box::new(DummyDetector), latency_detector(50)],
        "fps_30" | "fps_60" | "fps_120" | "fps_300" => vec![Box::new(DummyDetector)],
        other => {
            eprintln!("[harness] unknown scenario '{}', using baseline", other);
            vec![]
        }
    }
}

/// Target camera fps for scenarios that drive a synthetic camera.
fn camera_fps_for(scenario: &str) -> Option<f64> {
    match scenario {
        "fps_30" => Some(30.0),
        "fps_60" => Some(60.0),
        "fps_120" => Some(120.0),
        "fps_300" => Some(300.0),
        _ => None,
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

    // Build the pipeline.
    let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(300)));
    let detectors = detectors_for(scenario);
    let (mut scheduler, frame_tx) = build_scheduler(detectors, Arc::clone(&frame_buffer));

    // Optional synthetic camera thread injecting frames at target fps.
    if let Some(fps) = camera_fps_for(scenario) {
        let tx = frame_tx.clone();
        let interval = Duration::from_secs_f64(1.0 / fps);
        thread::spawn(move || {
            let mut id = 0u64;
            loop {
                let _ = tx.try_send(solid_frame(id));
                id += 1;
                thread::sleep(interval);
            }
        });
    } else {
        // Scenarios that don't need camera frames — just inject a steady
        // trickle so frame-requiring detectors don't always skip.
        let tx = frame_tx;
        thread::spawn(move || {
            let mut id = 0u64;
            loop {
                let _ = tx.try_send(solid_frame(id));
                id += 1;
                thread::sleep(Duration::from_millis(33)); // ~30fps
            }
        });
    }

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
        thread::sleep(Duration::from_micros(500)); // ~2 kHz ceiling (don't pin the CPU)

        let elapsed = start.elapsed();
        if in_warmup && elapsed >= warmup {
            in_warmup = false;
            last_sample = Instant::now();
            last_counters = CounterSnapshot::capture();
            println!("[harness] warm-up done, measuring ...");
        }

        if elapsed >= total {
            break;
        }

        // Sample at the requested interval (after warm-up).
        if !in_warmup && last_sample.elapsed() >= sample_interval {
            let now_counters = CounterSnapshot::capture();
            let delta = now_counters.delta_since(&last_counters);
            let hist = HistSummary::capture();
            let elapsed_ms = start.elapsed().as_millis() as u64;
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
    let detector_sleep_ms = match scenario.as_str() {
        "blocking_1ms" => 1,
        "blocking_3ms" => 3,
        "blocking_10ms" => 10,
        "blocking_50ms" => 50,
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
        "[harness] DONE  tick_p50={:.2}ms  tick_p99={:.2}ms  tick_p999={:.2}ms  ticks={}  frame_drops={}",
        final_hist.tick_p50_ns as f64 / 1e6,
        final_hist.tick_p99_ns as f64 / 1e6,
        final_hist.tick_p999_ns as f64 / 1e6,
        final_counters.ticks,
        final_counters.frame_drops,
    );
    println!("[harness] time-series → {}", ts_path.display());
    println!("[harness] summary     → {}", sum_path.display());

    Ok(())
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(&cli) {
        eprintln!("[harness] error: {}", err);
        std::process::exit(1);
    }
}
