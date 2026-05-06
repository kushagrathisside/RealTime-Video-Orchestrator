use std::thread;
use std::time::{Duration, Instant};

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
        let mut cam = match videoio::VideoCapture::new(
            cfg.device_index,
            videoio::CAP_ANY,
        ) {
            Ok(cam) => cam,
            Err(err) => {
                eprintln!(
                    "[CAMERA] Failed to open device {}: {}",
                    cfg.device_index,
                    err
                );
                return;
            }
        };

        cam.set(videoio::CAP_PROP_FPS, 30.0).ok();

        let mut frame_id: u64 = 0;
        let mut consecutive_failures: u64 = 0;

        loop {
            let mut img = Mat::default();

            if !cam.read(&mut img).unwrap_or(false) {
                consecutive_failures += 1;

                if consecutive_failures == 1 || consecutive_failures % 300 == 0 {
                    eprintln!(
                        "[CAMERA] Read failed on device {} (consecutive={})",
                        cfg.device_index,
                        consecutive_failures
                    );
                }

                thread::sleep(Duration::from_millis(10));
                continue;
            }

            consecutive_failures = 0;

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
