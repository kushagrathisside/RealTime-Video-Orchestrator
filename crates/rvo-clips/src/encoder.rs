use std::thread;
use std::time::Duration;

use crossbeam_channel::Receiver;
use rvo_buffer::Frame;
use crate::clip::ClipJob;

pub fn start_encoder_worker(
    rx: Receiver<(ClipJob, Vec<Frame>)>,
) {
    thread::spawn(move || {
        while let Ok((job, frames)) = rx.recv() {
            // Simulate encoding cost
            println!(
                "[ENCODER] Encoding clip for event {:?}, frames={}",
                job.event_type,
                frames.len()
            );

            thread::sleep(Duration::from_millis(200));

            // Later:
            // - H.264 encoding
            // - file write
            // - metadata write
        }
    });
}
