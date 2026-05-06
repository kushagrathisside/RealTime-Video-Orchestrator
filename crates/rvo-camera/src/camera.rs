use std::thread;
use std::time::Instant;

use crossbeam_channel::Sender;
use opencv::{prelude::*, videoio};

use rvo_buffer::Frame;

pub struct CameraConfig {
    pub device_index: i32,
}

pub fn start_camera(
    cfg: CameraConfig,
    tx: Sender<Frame>,
) {
    thread::spawn(move || {
        let mut cam = videoio::VideoCapture::new(
            cfg.device_index,
            videoio::CAP_ANY,
        ).expect("Failed to open camera");

        cam.set(videoio::CAP_PROP_FPS, 30.0).ok();

        let mut frame_id: u64 = 0;

        loop {
            let mut img = Mat::default();

            if !cam.read(&mut img).unwrap_or(false) {
                continue; // drop on failure
            }

            let frame = Frame {
                ts: Instant::now(),
                id: frame_id,
                image: img,
            };

            // Non-blocking send (drop if full)
            let _ = tx.try_send(frame);

            frame_id += 1;
        }
    });
}
