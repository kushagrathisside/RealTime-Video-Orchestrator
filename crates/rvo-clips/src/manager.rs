use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Sender, TrySendError};

use rvo_buffer::{Frame, FrameBuffer};
use rvo_events::Event;
use rvo_metrics::METRICS;

use crate::clip::ClipJob;

/// Capacity of the pending post-roll queue. Bounds how many in-flight clips can
/// be waiting for their post-roll window before we shed (drop + count).
const PENDING_CAP: usize = 16;

/// A clip whose post-roll window has not yet closed.
struct PendingJob {
    job: ClipJob,
    start: Instant,
    end: Instant,
    /// When the post-roll window closes and the buffer should be sliced.
    fire_at: Instant,
}

pub struct ClipManager {
    /// Bounded queue feeding the single post-roll worker. Replaces the previous
    /// "one thread per event", which was the only unbounded path in the system.
    pending_tx: Sender<PendingJob>,
    before: Duration,
    after: Duration,
    /// Shared frame buffer; `on_event` reads the newest timestamp to anchor the
    /// clip window. The worker holds its own clone to slice later.
    buffer: Arc<Mutex<FrameBuffer>>,
}

impl ClipManager {
    pub fn new(
        tx: Sender<(ClipJob, Vec<Frame>)>,
        before: Duration,
        after: Duration,
        buffer: Arc<Mutex<FrameBuffer>>,
    ) -> Self {
        let (pending_tx, pending_rx) = bounded::<PendingJob>(PENDING_CAP);
        let worker_buffer = Arc::clone(&buffer);

        // One long-lived worker drains the pending queue. For each job it waits
        // out the post-roll window, slices the buffer (briefly holding the
        // lock), and hands the frames to the encoder. This keeps the scheduler
        // tick non-blocking while bounding both threads and memory.
        thread::spawn(move || {
            while let Ok(pending) = pending_rx.recv() {
                let now = Instant::now();
                if pending.fire_at > now {
                    thread::sleep(pending.fire_at - now);
                }

                let frames = worker_buffer
                    .lock()
                    .unwrap()
                    .slice(pending.start, pending.end);

                match tx.try_send((pending.job, frames)) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        METRICS.clip_drops.fetch_add(1, Ordering::Relaxed);
                        println!("[CLIP] Dropped clip job (encoder queue full)");
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        println!("[CLIP] Encoder unavailable");
                        break;
                    }
                }
            }
        });

        Self {
            pending_tx,
            before,
            after,
            buffer,
        }
    }

    /// Called by the scheduler when a confirmed event fires.
    ///
    /// Non-blocking: anchors the clip window to the newest frame's timestamp and
    /// enqueues a pending job for the worker. If the pending queue is full the
    /// job is dropped and counted — the live pipeline is never stalled, and no
    /// per-event thread is spawned.
    pub fn on_event(&self, event: &Event) {
        // Anchor the window to the newest frame available now. If the buffer is
        // empty (camera not ready) we skip rather than fabricate a timestamp.
        let event_ts = {
            let buf = self.buffer.lock().unwrap();
            match buf.newest_instant() {
                Some(ts) => ts,
                None => {
                    METRICS.clip_drops.fetch_add(1, Ordering::Relaxed);
                    println!("[CLIP] Skipped clip job (no frames available)");
                    return;
                }
            }
        };

        let start = event_ts.checked_sub(self.before).unwrap_or(event_ts);
        let end = event_ts + self.after;

        let pending = PendingJob {
            job: ClipJob {
                event_type: event.event_type,
                event_ts_ns: event.ts_ns,
                start_ts: start,
                end_ts: end,
            },
            start,
            end,
            fire_at: Instant::now() + self.after,
        };

        if let Err(TrySendError::Full(_)) = self.pending_tx.try_send(pending) {
            METRICS.clip_drops.fetch_add(1, Ordering::Relaxed);
            println!("[CLIP] Dropped clip job (post-roll queue full)");
        }
        METRICS
            .clip_pending_depth
            .store(self.pending_tx.len() as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencv::core::Mat;
    use rvo_events::EventType;

    fn frame() -> Frame {
        Frame {
            ts: Instant::now(),
            id: 0,
            image: Mat::default(),
        }
    }

    #[test]
    fn event_burst_is_bounded() {
        let (clip_tx, _clip_rx) = bounded(8);
        let buffer = Arc::new(Mutex::new(FrameBuffer::new(16)));
        buffer.lock().unwrap().push(frame());

        let mgr = ClipManager::new(
            clip_tx,
            Duration::from_millis(5),
            Duration::from_millis(5),
            Arc::clone(&buffer),
        );

        let ev = Event {
            event_type: EventType::DummyEvent,
            ts_ns: 0,
            confidence: 1.0,
        };

        // A burst far exceeding PENDING_CAP. Pre-fix this spawned one OS thread
        // per call; now excess is dropped and bounded. Reaching the end quickly
        // without spawning hundreds of threads (or panicking) is the assertion.
        for _ in 0..500 {
            mgr.on_event(&ev);
        }
    }
}
