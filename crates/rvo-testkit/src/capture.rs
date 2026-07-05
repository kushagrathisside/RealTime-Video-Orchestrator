use std::sync::atomic::Ordering;

use crossbeam_channel::Receiver;

use rvo_events::{Event, EventType};
use rvo_metrics::METRICS;

pub struct EventCapture {
    rx: Receiver<Event>,
    collected: Vec<Event>,
}

impl EventCapture {
    pub fn new(rx: Receiver<Event>) -> Self {
        Self {
            rx,
            collected: Vec::new(),
        }
    }

    pub fn drain(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.collected.push(event);
        }
    }

    pub fn count(&mut self) -> usize {
        self.drain();
        self.collected.len()
    }

    pub fn assert_count(&mut self, n: usize) {
        let actual = self.count();
        assert_eq!(
            actual, n,
            "expected exactly {} events, collected {}",
            n, actual
        );
    }

    pub fn assert_has_event(&mut self, event_type: EventType) {
        self.drain();
        assert!(
            self.collected
                .iter()
                .any(|event| event.event_type == event_type),
            "expected at least one {:?} event, collected {:?}",
            event_type,
            self.collected
                .iter()
                .map(|event| event.event_type.clone())
                .collect::<Vec<_>>()
        );
    }

    pub fn assert_empty(&mut self) {
        let actual = self.count();
        assert_eq!(actual, 0, "expected no events, collected {}", actual);
    }

    pub fn events(&self) -> &[Event] {
        &self.collected
    }

    pub fn clear(&mut self) {
        self.collected.clear();
    }
}

#[derive(Clone, Debug, Default)]
pub struct MetricsSnapshot {
    pub scheduler_ticks: u64,
    pub detector_execs: u64,
    pub detector_skips: u64,
    pub detector_failures: u64,
    pub detector_exec_ns_total: u64,
    pub events_emitted: u64,
    pub frame_drops: u64,
    pub clip_drops: u64,
    pub event_drops: u64,
}

impl MetricsSnapshot {
    pub fn capture() -> Self {
        Self {
            scheduler_ticks: METRICS.scheduler_ticks.load(Ordering::Relaxed),
            detector_execs: METRICS.detector_execs.load(Ordering::Relaxed),
            detector_skips: METRICS.detector_skips.load(Ordering::Relaxed),
            detector_failures: METRICS.detector_failures.load(Ordering::Relaxed),
            detector_exec_ns_total: METRICS.detector_exec_ns_total.load(Ordering::Relaxed),
            events_emitted: METRICS.events_emitted.load(Ordering::Relaxed),
            frame_drops: METRICS.frame_drops.load(Ordering::Relaxed),
            clip_drops: METRICS.clip_drops.load(Ordering::Relaxed),
            event_drops: METRICS.event_drops.load(Ordering::Relaxed),
        }
    }

    pub fn delta_since(&self, earlier: &MetricsSnapshot) -> MetricsSnapshot {
        MetricsSnapshot {
            scheduler_ticks: self.scheduler_ticks.saturating_sub(earlier.scheduler_ticks),
            detector_execs: self.detector_execs.saturating_sub(earlier.detector_execs),
            detector_skips: self.detector_skips.saturating_sub(earlier.detector_skips),
            detector_failures: self
                .detector_failures
                .saturating_sub(earlier.detector_failures),
            detector_exec_ns_total: self
                .detector_exec_ns_total
                .saturating_sub(earlier.detector_exec_ns_total),
            events_emitted: self.events_emitted.saturating_sub(earlier.events_emitted),
            frame_drops: self.frame_drops.saturating_sub(earlier.frame_drops),
            clip_drops: self.clip_drops.saturating_sub(earlier.clip_drops),
            event_drops: self.event_drops.saturating_sub(earlier.event_drops),
        }
    }
}
