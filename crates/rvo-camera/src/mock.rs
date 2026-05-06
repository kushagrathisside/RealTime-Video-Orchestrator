use std::thread;
use std::time::{Duration, Instant};
use crossbeam_channel::Sender;
use rvo_buffer::Frame;

pub fn start_mock_camera(tx: Sender<Frame>) {
    thread::spawn(move || {
        let mut id = 0;
        loop {
            let frame = Frame {
                ts: Instant::now(),
                id,
                image: opencv::core::Mat::default(),
            };
            let _ = tx.try_send(frame);
            id += 1;
            thread::sleep(Duration::from_millis(30));
        }
    });
}

