use std::time::Duration;

use crossbeam_channel::{Sender, TrySendError};

use rvo_buffer::{Frame, FrameBuffer};
use rvo_events::Event;

use crate::clip::ClipJob;

pub struct ClipManager {
    tx: Sender<(ClipJob, Vec<Frame>)>,
    before: Duration,
    after: Duration,
}

impl ClipManager {
    pub fn new(
        tx: Sender<(ClipJob, Vec<Frame>)>,
        before: Duration,
        after: Duration,
    ) -> Self {
        Self { tx, before, after }
    }

    pub fn on_event(
        &self,
        event: &Event,
        buffer: &FrameBuffer,
    ) {
        let Some(event_ts) = buffer.newest_instant() else {
            println!("[CLIP] Skipped clip job (no frames available)");
            return;
        };

        let start = event_ts.checked_sub(self.before).unwrap_or(event_ts);
        let end = event_ts.checked_add(self.after).unwrap_or(event_ts);
        let frames = buffer.slice(start, end);
        let job = ClipJob {
            event_type: event.event_type,
            event_ts_ns: event.ts_ns,
            start_ts: start,
            end_ts: end,
        };

        // Drop-on-overflow (critical)
        match self.tx.try_send((job, frames)) {
            Ok(_) => {}
            Err(TrySendError::Full(_)) => {
                println!("[CLIP] Dropped clip job (queue full)");
            }
            Err(TrySendError::Disconnected(_)) => {
                println!("[CLIP] Encoder unavailable");
            }
        }
    }
}
