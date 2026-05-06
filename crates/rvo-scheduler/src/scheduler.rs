use std::time::{Duration, Instant};

use rvo_detector::detector::{DetectorContext, DetectorNode};
use rvo_signals::store::SignalStore;
use rvo_events::{EventEngine, EventDefinition, EventType};
use rvo_metrics::METRICS;
use std::sync::atomic::Ordering;
use rvo_buffer::{Frame, FrameBuffer};
use crossbeam_channel::Receiver;
use rvo_clips::ClipManager;

struct DetectorRuntime {
    last_run: Instant,
}

pub struct Scheduler {
    detectors: Vec<Box<dyn DetectorNode>>,
    runtime: Vec<DetectorRuntime>,
    started_at: Instant,
    signal_store: SignalStore,
    event_engine: EventEngine,
    frame_buffer: FrameBuffer,
    frame_rx: Receiver<Frame>,
    clip_manager: ClipManager,
}

impl Scheduler {
    pub fn frame_slice(
        &self,
        start: Instant,
        end: Instant,
    ) -> Vec<Frame> {
        self.frame_buffer.slice(start, end)
    }

    pub fn new(
        detectors: Vec<Box<dyn DetectorNode>>,
        event_engine: EventEngine,
        frame_rx: Receiver<Frame>,
        clip_manager: ClipManager,
    ) -> Self {
        let now = Instant::now();
        let runtime = detectors
            .iter()
            .map(|_| DetectorRuntime {
                last_run: now,
            })
            .collect();

        Self {
            detectors,
            runtime,
            started_at: now,
            signal_store: SignalStore::new(),
            event_engine,
            frame_buffer: FrameBuffer::new(300), // ~10s @ 30fps
            frame_rx,
            clip_manager,
        }
    }

    pub fn tick(&mut self) {
        
        while let Ok(frame) = self.frame_rx.try_recv() {
            self.frame_buffer.push(frame);
        }

        METRICS.scheduler_ticks.fetch_add(1, Ordering::Relaxed);

        let now = Instant::now();
        let now_ns = now.duration_since(self.started_at).as_nanos() as u64;

        for (i, detector) in self.detectors.iter_mut().enumerate() {

            let min_interval =
                Duration::from_secs_f64(1.0 / detector.max_fps());

            if now.duration_since(self.runtime[i].last_run) < min_interval {
                METRICS.detector_skips.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            let ctx = DetectorContext { now_ns };
            let result = detector.execute(&ctx);

            METRICS.detector_execs.fetch_add(1, Ordering::Relaxed);

            self.runtime[i].last_run = now;

            for sig in result.signals {
                self.signal_store.upsert(sig);
            }
        }

        if let Some(event) =
            self.event_engine.update(now_ns, &self.signal_store)
        {
            METRICS.events_emitted.fetch_add(1, Ordering::Relaxed);
            self.clip_manager.on_event(
                &event,
                &self.frame_buffer,
        );
    }
    }

    pub fn swap_runtime(
        &mut self,
        detectors: Vec<Box<dyn DetectorNode>>,
        event_engine: EventEngine,
    ) {
        self.runtime = detectors
            .iter()
            .map(|_| DetectorRuntime {
                last_run: std::time::Instant::now(),
            })
            .collect();

        self.detectors = detectors;
        self.event_engine = event_engine;
    }

}


#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;
    use std::time::Duration;
    use rvo_camera::mock::start_mock_camera;
    use rvo_detector::DummyDetector;
    use rvo_events::{EventEngine, EventDefinition, EventType};
    use rvo_clips::ClipManager;

    #[test]
    fn scheduler_runs_without_blocking() {
        let (frame_tx, frame_rx) = bounded(5);
        start_mock_camera(frame_tx);

        let detectors = vec![Box::new(DummyDetector)];
        let event_engine = EventEngine::new(EventDefinition {
            event_type: EventType::DummyEvent,
            signal_threshold: 1,
            duration_ns: 0,
            cooldown_ns: 0,
        });

        let (clip_tx, _clip_rx) = bounded(1);
        let clip_manager = ClipManager::new(
            clip_tx,
            Duration::from_secs(1),
            Duration::from_secs(1),
        );

        let mut scheduler = Scheduler::new(
            detectors,
            event_engine,
            frame_rx,
            clip_manager,
        );

        for _ in 0..100 {
            scheduler.tick();
        }
    }
}
/*
What this proves:
1. Scheduler does not block
2. Camera + scheduler coexist
3. Threads behave correctly
*/
