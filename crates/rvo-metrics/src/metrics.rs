use hdrhistogram::Histogram;
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// A latency histogram with fixed bounds (1 ns .. 60 s, 3 significant figures).
///
/// Recording uses `saturating_record`, so it never allocates on the hot path
/// (no auto-resize) and never errors (out-of-range values clamp to the max).
///
/// Each histogram in [`Metrics`] is written by a **single** thread — tick-thread
/// histograms by the scheduler, `remote_latency_ns` by the remote workers — so
/// the `Mutex` is effectively uncontended. If a histogram ever gains multiple
/// concurrent writers, switch it to `hdrhistogram::sync::SyncHistogram`.
pub struct LatencyHist(Mutex<Histogram<u64>>);

impl LatencyHist {
    fn new() -> Self {
        LatencyHist(Mutex::new(
            Histogram::new_with_bounds(1, 60_000_000_000, 3).expect("valid histogram bounds"),
        ))
    }

    /// Record a nanosecond latency sample (clamped to the histogram range).
    pub fn record_ns(&self, ns: u64) {
        if let Ok(mut h) = self.0.lock() {
            h.saturating_record(ns.max(1));
        }
    }

    /// (p50, p99, p99.9, count) in nanoseconds.
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        match self.0.lock() {
            Ok(h) => (
                h.value_at_quantile(0.50),
                h.value_at_quantile(0.99),
                h.value_at_quantile(0.999),
                h.len(),
            ),
            Err(_) => (0, 0, 0, 0),
        }
    }
}

impl Default for LatencyHist {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Metrics {
    // Scheduler
    pub scheduler_ticks: AtomicU64,

    // Detectors
    pub detector_execs: AtomicU64,
    pub detector_skips: AtomicU64,
    pub detector_failures: AtomicU64,
    pub detector_exec_ns_total: AtomicU64,

    // Events
    pub events_emitted: AtomicU64,

    // Drop counters — incremented by ClipManager, EventPublisher, and Camera
    // when a bounded queue is full and work is shed.
    pub frame_drops: AtomicU64,
    pub clip_drops: AtomicU64,
    pub event_drops: AtomicU64,

    // Saturation gauges — current depth of each bounded queue (USE method).
    // Set at the producing site; a rising gauge is early warning before drops.
    pub frame_queue_depth: AtomicU64,
    pub event_queue_depth: AtomicU64,
    pub clip_pending_depth: AtomicU64,

    // Latency histograms (nanoseconds). Definitions are deliberately precise so
    // the in-process and remote paths are not conflated:
    //   tick_ns           — duration of one scheduler tick (loop jitter).
    //   detector_exec_ns  — time inside a single detector `execute()` call.
    //   frame_staleness_ns— age of the newest frame when the tick processes it
    //                       (camera→buffer→scheduler latency; in-process path).
    //   remote_latency_ns — frame capture → gRPC reply, measured in the remote
    //                       worker against the source frame (true remote E2E).
    pub tick_ns: LatencyHist,
    pub detector_exec_ns: LatencyHist,
    pub frame_staleness_ns: LatencyHist,
    pub remote_latency_ns: LatencyHist,
}

pub static METRICS: Lazy<Metrics> = Lazy::new(|| Metrics {
    scheduler_ticks: AtomicU64::new(0),
    detector_execs: AtomicU64::new(0),
    detector_skips: AtomicU64::new(0),
    detector_failures: AtomicU64::new(0),
    detector_exec_ns_total: AtomicU64::new(0),
    events_emitted: AtomicU64::new(0),
    frame_drops: AtomicU64::new(0),
    clip_drops: AtomicU64::new(0),
    event_drops: AtomicU64::new(0),
    frame_queue_depth: AtomicU64::new(0),
    event_queue_depth: AtomicU64::new(0),
    clip_pending_depth: AtomicU64::new(0),
    tick_ns: LatencyHist::new(),
    detector_exec_ns: LatencyHist::new(),
    frame_staleness_ns: LatencyHist::new(),
    remote_latency_ns: LatencyHist::new(),
});

pub fn render_prometheus() -> String {
    let mut out = String::with_capacity(2048);

    // Counters.
    out.push_str(&format!(
        "\
rvo_scheduler_ticks {}\n\
rvo_detector_exec_total {}\n\
rvo_detector_skip_total {}\n\
rvo_detector_failure_total {}\n\
rvo_detector_exec_ns_total {}\n\
rvo_events_emitted_total {}\n\
rvo_frame_drops_total {}\n\
rvo_clip_drops_total {}\n\
rvo_event_drops_total {}\n",
        METRICS.scheduler_ticks.load(Ordering::Relaxed),
        METRICS.detector_execs.load(Ordering::Relaxed),
        METRICS.detector_skips.load(Ordering::Relaxed),
        METRICS.detector_failures.load(Ordering::Relaxed),
        METRICS.detector_exec_ns_total.load(Ordering::Relaxed),
        METRICS.events_emitted.load(Ordering::Relaxed),
        METRICS.frame_drops.load(Ordering::Relaxed),
        METRICS.clip_drops.load(Ordering::Relaxed),
        METRICS.event_drops.load(Ordering::Relaxed),
    ));

    // Gauges.
    out.push_str(&format!(
        "\
rvo_frame_queue_depth {}\n\
rvo_event_queue_depth {}\n\
rvo_clip_pending_depth {}\n",
        METRICS.frame_queue_depth.load(Ordering::Relaxed),
        METRICS.event_queue_depth.load(Ordering::Relaxed),
        METRICS.clip_pending_depth.load(Ordering::Relaxed),
    ));

    // Latency histograms as Prometheus summaries (nanoseconds).
    for (name, hist) in [
        ("rvo_tick_ns", &METRICS.tick_ns),
        ("rvo_detector_exec_ns", &METRICS.detector_exec_ns),
        ("rvo_frame_staleness_ns", &METRICS.frame_staleness_ns),
        ("rvo_remote_latency_ns", &METRICS.remote_latency_ns),
    ] {
        let (p50, p99, p999, count) = hist.snapshot();
        out.push_str(&format!(
            "\
{name}{{quantile=\"0.5\"}} {p50}\n\
{name}{{quantile=\"0.99\"}} {p99}\n\
{name}{{quantile=\"0.999\"}} {p999}\n\
{name}_count {count}\n",
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_hist_records_and_reports_percentiles() {
        let h = LatencyHist::new();
        for v in 1..=1000u64 {
            h.record_ns(v * 1_000); // 1µs .. 1ms
        }
        let (p50, p99, p999, count) = h.snapshot();
        assert_eq!(count, 1000);
        assert!(p50 > 0 && p99 >= p50 && p999 >= p99);
    }

    #[test]
    fn out_of_range_saturates_without_panic() {
        let h = LatencyHist::new();
        h.record_ns(u64::MAX); // far beyond the 60s bound — must clamp, not panic
        let (_, _, p999, count) = h.snapshot();
        assert_eq!(count, 1);
        // Clamped into the top bucket (representative value is ~the 60s bound).
        assert!(p999 >= 59_000_000_000);
    }
}
