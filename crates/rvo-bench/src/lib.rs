//! Shared utilities for the RVO benchmark harness.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

use rvo_metrics::METRICS;

/// Snapshot of all histogram percentiles and gauge values at a point in time.
#[derive(Debug, Clone)]
pub struct HistSummary {
    pub tick_p50_ns: u64,
    pub tick_p99_ns: u64,
    pub tick_p999_ns: u64,
    pub tick_count: u64,
    pub exec_p50_ns: u64,
    pub exec_p99_ns: u64,
    pub exec_p999_ns: u64,
    pub exec_count: u64,
    pub staleness_p50_ns: u64,
    pub staleness_p99_ns: u64,
    pub staleness_count: u64,
    pub remote_p50_ns: u64,
    pub remote_p99_ns: u64,
    pub remote_p999_ns: u64,
    pub remote_count: u64,
    pub frame_queue_depth: u64,
    pub event_queue_depth: u64,
    pub clip_pending_depth: u64,
}

impl HistSummary {
    pub fn capture() -> Self {
        let (t50, t99, t999, tc) = METRICS.tick_ns.snapshot();
        let (e50, e99, e999, ec) = METRICS.detector_exec_ns.snapshot();
        let (s50, s99, _, sc) = METRICS.frame_staleness_ns.snapshot();
        let (r50, r99, r999, rc) = METRICS.remote_latency_ns.snapshot();
        use std::sync::atomic::Ordering;
        HistSummary {
            tick_p50_ns: t50,
            tick_p99_ns: t99,
            tick_p999_ns: t999,
            tick_count: tc,
            exec_p50_ns: e50,
            exec_p99_ns: e99,
            exec_p999_ns: e999,
            exec_count: ec,
            staleness_p50_ns: s50,
            staleness_p99_ns: s99,
            staleness_count: sc,
            remote_p50_ns: r50,
            remote_p99_ns: r99,
            remote_p999_ns: r999,
            remote_count: rc,
            frame_queue_depth: METRICS.frame_queue_depth.load(Ordering::Relaxed),
            event_queue_depth: METRICS.event_queue_depth.load(Ordering::Relaxed),
            clip_pending_depth: METRICS.clip_pending_depth.load(Ordering::Relaxed),
        }
    }
}

/// Counter snapshot for computing per-interval rates.
#[derive(Debug, Clone, Default)]
pub struct CounterSnapshot {
    pub ticks: u64,
    pub execs: u64,
    pub skips: u64,
    pub failures: u64,
    pub events: u64,
    pub frame_drops: u64,
    pub clip_drops: u64,
}

impl CounterSnapshot {
    pub fn capture() -> Self {
        use std::sync::atomic::Ordering;
        CounterSnapshot {
            ticks: METRICS.scheduler_ticks.load(Ordering::Relaxed),
            execs: METRICS.detector_execs.load(Ordering::Relaxed),
            skips: METRICS.detector_skips.load(Ordering::Relaxed),
            failures: METRICS.detector_failures.load(Ordering::Relaxed),
            events: METRICS.events_emitted.load(Ordering::Relaxed),
            frame_drops: METRICS.frame_drops.load(Ordering::Relaxed),
            clip_drops: METRICS.clip_drops.load(Ordering::Relaxed),
        }
    }

    pub fn delta_since(&self, earlier: &CounterSnapshot) -> CounterSnapshot {
        CounterSnapshot {
            ticks: self.ticks.saturating_sub(earlier.ticks),
            execs: self.execs.saturating_sub(earlier.execs),
            skips: self.skips.saturating_sub(earlier.skips),
            failures: self.failures.saturating_sub(earlier.failures),
            events: self.events.saturating_sub(earlier.events),
            frame_drops: self.frame_drops.saturating_sub(earlier.frame_drops),
            clip_drops: self.clip_drops.saturating_sub(earlier.clip_drops),
        }
    }
}

// ---- CSV writer ------------------------------------------------------------

pub struct CsvWriter {
    pub inner: BufWriter<File>,
}

const TIME_SERIES_HEADER: &str = "elapsed_ms,ticks_delta,execs_delta,skips_delta,events_delta,frame_drops_delta,tick_p50_ns,tick_p99_ns,exec_p50_ns,exec_p99_ns,staleness_p50_ns,staleness_p99_ns,frame_queue_depth";

const SUMMARY_HEADER: &str = "run_id,scenario,detector_sleep_ms,input_fps,actual_camera_fps,duration_secs,tick_p50_ns,tick_p99_ns,tick_p999_ns,tick_count,exec_p50_ns,exec_p99_ns,exec_p999_ns,total_ticks,total_execs,total_skips,total_events,total_frame_drops,effective_fps,frame_loss_rate";

impl CsvWriter {
    pub fn create_time_series(path: &Path) -> std::io::Result<Self> {
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        writeln!(f, "{}", TIME_SERIES_HEADER)?;
        Ok(CsvWriter {
            inner: BufWriter::new(f),
        })
    }

    pub fn create_summary(path: &Path) -> std::io::Result<Self> {
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        writeln!(f, "{}", SUMMARY_HEADER)?;
        Ok(CsvWriter {
            inner: BufWriter::new(f),
        })
    }

    /// Open an existing summary file for appending (no header written).
    pub fn append_summary(path: &Path) -> std::io::Result<Self> {
        let f = OpenOptions::new().append(true).open(path)?;
        Ok(CsvWriter {
            inner: BufWriter::new(f),
        })
    }

    pub fn write_time_series_row(
        &mut self,
        elapsed_ms: u64,
        delta: &CounterSnapshot,
        hist: &HistSummary,
    ) -> std::io::Result<()> {
        writeln!(
            self.inner,
            "{},{},{},{},{},{},{},{},{},{},{},{},{}",
            elapsed_ms,
            delta.ticks,
            delta.execs,
            delta.skips,
            delta.events,
            delta.frame_drops,
            hist.tick_p50_ns,
            hist.tick_p99_ns,
            hist.exec_p50_ns,
            hist.exec_p99_ns,
            hist.staleness_p50_ns,
            hist.staleness_p99_ns,
            hist.frame_queue_depth,
        )
    }

    pub fn write_summary_row(
        &mut self,
        run_id: u64,
        scenario: &str,
        detector_sleep_ms: u64,
        input_fps: f64,
        actual_camera_fps: f64,
        duration_secs: u64,
        hist: &HistSummary,
        final_counters: &CounterSnapshot,
    ) -> std::io::Result<()> {
        let effective_fps = final_counters.ticks as f64 / duration_secs as f64;
        // Use actual measured camera fps so frame_loss_rate is accurate even when
        // thread::sleep granularity limits the camera thread below its configured rate.
        let frame_loss_rate = (actual_camera_fps - effective_fps).max(0.0);
        writeln!(
            self.inner,
            "{},{},{},{:.2},{:.2},{},{},{},{},{},{},{},{},{},{},{},{},{},{:.2},{:.2}",
            run_id,
            scenario,
            detector_sleep_ms,
            input_fps,
            actual_camera_fps,
            duration_secs,
            hist.tick_p50_ns,
            hist.tick_p99_ns,
            hist.tick_p999_ns,
            hist.tick_count,
            hist.exec_p50_ns,
            hist.exec_p99_ns,
            hist.exec_p999_ns,
            final_counters.ticks,
            final_counters.execs,
            final_counters.skips,
            final_counters.events,
            final_counters.frame_drops,
            effective_fps,
            frame_loss_rate,
        )
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
