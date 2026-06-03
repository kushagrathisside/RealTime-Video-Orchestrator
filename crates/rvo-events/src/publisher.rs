use std::fs::OpenOptions;
use std::io::Write as IoWrite;
use std::sync::atomic::Ordering;
use std::thread;

use crossbeam_channel::{Receiver, Sender, TrySendError};

use rvo_metrics::METRICS;

use crate::Event;

pub struct EventPublisher {
    tx: Sender<Event>,
}

impl EventPublisher {
    pub fn new(tx: Sender<Event>) -> Self {
        Self { tx }
    }

    pub fn publish(&self, event: &Event) {
        match self.tx.try_send(*event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                METRICS.event_drops.fetch_add(1, Ordering::Relaxed);
                eprintln!("[EVENT] Dropped event (queue full)");
            }
            Err(TrySendError::Disconnected(_)) => {
                eprintln!("[EVENT] Publisher channel disconnected");
            }
        }
        METRICS
            .event_queue_depth
            .store(self.tx.len() as u64, Ordering::Relaxed);
    }
}

/// Start the event consumer thread.
///
/// Always logs confirmed events to stdout. When `log_path` is `Some`, also
/// appends each event as a JSON line to that file, flushing after every write
/// so the file can be tailed in realtime.
///
/// Combining both outputs in one thread avoids needing a fan-out mechanism
/// while keeping the number of threads minimal.
pub fn start_event_logger(rx: Receiver<Event>) {
    start_event_consumer(rx, None);
}

/// Start the event consumer with an optional file sink.
pub fn start_event_file_sink(rx: Receiver<Event>, log_path: String) {
    start_event_consumer(rx, Some(log_path));
}

fn start_event_consumer(rx: Receiver<Event>, log_path: Option<String>) {
    thread::spawn(move || {
        let mut file = log_path.as_deref().and_then(|path| {
            match OpenOptions::new().create(true).append(true).open(path) {
                Ok(f) => {
                    println!("[EVENT] Writing events to '{}'", path);
                    Some(f)
                }
                Err(err) => {
                    eprintln!("[EVENT] Failed to open log '{}': {}", path, err);
                    None
                }
            }
        });

        while let Ok(event) = rx.recv() {
            println!(
                "[EVENT] type={:?} ts_ns={} confidence={:.3}",
                event.event_type, event.ts_ns, event.confidence
            );

            if let Some(ref mut f) = file {
                match serde_json::to_string(&event) {
                    Ok(json) => {
                        if let Err(err) = writeln!(f, "{}", json) {
                            eprintln!("[EVENT] File write error: {}", err);
                        } else {
                            let _ = f.flush();
                        }
                    }
                    Err(err) => {
                        eprintln!("[EVENT] Serialize error: {}", err);
                    }
                }
            }
        }
    });
}
