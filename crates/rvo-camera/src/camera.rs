use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use opencv::{prelude::*, videoio};

use rvo_buffer::Frame;
use rvo_metrics::METRICS;

/// How to open the camera source.
pub enum CameraSource {
    /// Local device by OS index (0 = default webcam).
    Device(i32),
    /// Any URI that OpenCV's VideoCapture accepts: RTSP streams, file paths,
    /// HTTP MJPEG feeds, GStreamer pipelines, etc.
    Uri(String),
}

pub struct CameraConfig {
    pub source: CameraSource,
}

impl CameraConfig {
    /// Convenience constructor for the common local-webcam case.
    pub fn device(index: i32) -> Self {
        Self {
            source: CameraSource::Device(index),
        }
    }

    /// Convenience constructor for RTSP and other URI sources.
    pub fn uri(url: impl Into<String>) -> Self {
        Self {
            source: CameraSource::Uri(url.into()),
        }
    }
}

/// Probe local camera device indices `0..max` and return those that open.
///
/// Best-effort helper for the CLI's `--list-cameras`: it shows the user which
/// device indices are usable as a `--camera-device` argument. Opening a device
/// can be slow, so callers should keep `max` small (e.g. 10).
pub fn list_cameras(max: i32) -> Vec<i32> {
    let mut found = Vec::new();
    for idx in 0..max {
        if let Ok(cam) = videoio::VideoCapture::new(idx, videoio::CAP_ANY) {
            if cam.is_opened().unwrap_or(false) {
                found.push(idx);
            }
        }
    }
    found
}

pub fn start_camera(cfg: CameraConfig, tx: Sender<Frame>) {
    thread::spawn(move || {
        let mut cam = match &cfg.source {
            CameraSource::Device(idx) => match videoio::VideoCapture::new(*idx, videoio::CAP_ANY) {
                Ok(c) => c,
                Err(err) => {
                    eprintln!("[CAMERA] Failed to open device {}: {}", idx, err);
                    return;
                }
            },
            CameraSource::Uri(uri) => {
                match videoio::VideoCapture::from_file(uri.as_str(), videoio::CAP_ANY) {
                    Ok(c) => c,
                    Err(err) => {
                        eprintln!("[CAMERA] Failed to open URI '{}': {}", uri, err);
                        return;
                    }
                }
            }
        };

        cam.set(videoio::CAP_PROP_FPS, 30.0).ok();

        let mut frame_id: u64 = 0;
        let mut consecutive_failures: u64 = 0;

        loop {
            let mut img = Mat::default();

            if !cam.read(&mut img).unwrap_or(false) {
                consecutive_failures += 1;

                // Log on first failure and every 300 afterward (~10 s at 30 fps)
                if consecutive_failures == 1 || consecutive_failures.is_multiple_of(300) {
                    eprintln!(
                        "[CAMERA] Read failed (consecutive={})",
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

            // Non-blocking send. Drop and count if the scheduler is behind.
            if tx.try_send(frame).is_err() {
                METRICS.frame_drops.fetch_add(1, Ordering::Relaxed);
            }

            frame_id += 1;
        }
    });
}
