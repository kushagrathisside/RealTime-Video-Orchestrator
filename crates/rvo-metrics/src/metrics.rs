use std::sync::atomic::{AtomicU64, Ordering};
use once_cell::sync::Lazy;

pub struct Metrics {
    pub scheduler_ticks: AtomicU64,
    pub detector_execs: AtomicU64,
    pub detector_skips: AtomicU64,
    pub detector_failures: AtomicU64,
    pub detector_exec_ns_total: AtomicU64,
    pub events_emitted: AtomicU64,
}

pub static METRICS: Lazy<Metrics> = Lazy::new(|| Metrics {
    scheduler_ticks: AtomicU64::new(0),
    detector_execs: AtomicU64::new(0),
    detector_skips: AtomicU64::new(0),
    detector_failures: AtomicU64::new(0),
    detector_exec_ns_total: AtomicU64::new(0),
    events_emitted: AtomicU64::new(0),
});

pub fn render_prometheus() -> String {
    format!(
        "\
rvo_scheduler_ticks {}\n\
rvo_detector_exec_total {}\n\
rvo_detector_skip_total {}\n\
rvo_detector_failure_total {}\n\
rvo_detector_exec_ns_total {}\n\
rvo_events_emitted_total {}\n",
        METRICS.scheduler_ticks.load(Ordering::Relaxed),
        METRICS.detector_execs.load(Ordering::Relaxed),
        METRICS.detector_skips.load(Ordering::Relaxed),
        METRICS.detector_failures.load(Ordering::Relaxed),
        METRICS.detector_exec_ns_total.load(Ordering::Relaxed),
        METRICS.events_emitted.load(Ordering::Relaxed),
    )
}
