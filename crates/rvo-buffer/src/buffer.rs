use opencv::core::Mat;
use std::time::Instant;

#[derive(Clone)]
pub struct Frame {
    pub ts: Instant,
    pub id: u64,
    pub image: Mat,
}

pub struct FrameBuffer {
    frames: Vec<Option<Frame>>,
    capacity: usize,
    write_idx: usize,
}

impl FrameBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: vec![None; capacity],
            capacity,
            write_idx: 0,
        }
    }

    /// Push a frame from camera (O(1), overwrite-oldest)
    pub fn push(&mut self, frame: Frame) {
        self.frames[self.write_idx] = Some(frame);
        self.write_idx = (self.write_idx + 1) % self.capacity;
    }

    /// Snapshot slice by time window (cold path only)
    pub fn slice(
        &self,
        start: Instant,
        end: Instant,
    ) -> Vec<Frame> {
        let mut out = Vec::new();

        for slot in &self.frames {
            if let Some(f) = slot {
                if f.ts >= start && f.ts <= end {
                    out.push(f.clone());
                }
            }
        }

        out.sort_by_key(|f| f.ts);
        out
    }

    pub fn newest_instant(&self) -> Option<Instant> {
        let mut newest: Option<&Frame> = None;

        for slot in &self.frames {
            if let Some(f) = slot {
                if newest.map_or(true, |n| f.ts > n.ts) {
                    newest = Some(f);
                }
            }
        }

        newest.map(|f| f.ts)
    }

}


#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn dummy_frame(id: u64, ts: Instant) -> Frame {
        Frame {
            ts,
            id,
            image: opencv::core::Mat::default(),
        }
    }

    #[test]
    fn overwrites_old_frames() {
        let mut buf = FrameBuffer::new(2);
        let t0 = Instant::now();

        buf.push(dummy_frame(1, t0));
        buf.push(dummy_frame(2, t0));
        buf.push(dummy_frame(3, t0));

        let frames = buf.slice(t0 - Duration::from_secs(1), t0 + Duration::from_secs(1));
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().any(|f| f.id == 3));
    }

    #[test]
    fn slice_returns_frames_in_timestamp_order() {
        let mut buf = FrameBuffer::new(3);
        let t0 = Instant::now();

        buf.push(dummy_frame(2, t0 + Duration::from_millis(20)));
        buf.push(dummy_frame(3, t0 + Duration::from_millis(30)));
        buf.push(dummy_frame(1, t0 + Duration::from_millis(10)));

        let frames = buf.slice(t0, t0 + Duration::from_millis(40));
        let ids: Vec<u64> = frames.iter().map(|f| f.id).collect();

        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn newest_instant_is_none_when_empty() {
        let buf = FrameBuffer::new(2);

        assert!(buf.newest_instant().is_none());
    }
}

/* Frame Buffer Tests
What this proves:
1. Bounded memory
2. Overwrite semantics
3. No unbounded growth
*/
