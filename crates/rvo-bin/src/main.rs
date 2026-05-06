use std::{thread, time::Duration};
use std::sync::{Arc, Mutex};

use crossbeam_channel::bounded;
use signal_hook::consts::SIGHUP;
use signal_hook::iterator::Signals;

use rvo_scheduler::scheduler::Scheduler;
use rvo_metrics::start_metrics_server;

use rvo_detector::detector::{DetectorNode, DummyDetector};
use rvo_detector::load::LoadDetector;
use rvo_detector::jitter::JitterDetector;

use rvo_config::{load_config, RvoConfig};
use rvo_events::{EventEngine, EventDefinition, EventType};

use rvo_camera::{start_camera, CameraConfig};
use rvo_clips::{ClipManager, start_encoder_worker};

/// ---------------- detector factory ----------------
fn build_detectors(cfg: &RvoConfig) -> Vec<Box<dyn DetectorNode>> {
    let mut detectors: Vec<Box<dyn DetectorNode>> = Vec::new();

    for d in &cfg.detectors {
        if !d.enabled {
            continue;
        }

        match d.kind.as_str() {
            "dummy" => detectors.push(Box::new(DummyDetector)),

            "load" => {
                let busy = d.busy_ns.unwrap_or(1_000_000);
                detectors.push(Box::new(LoadDetector::new(busy)));
            }

            "jitter" => detectors.push(Box::new(JitterDetector)),

            other => panic!("Unknown detector kind: {}", other),
        }
    }

    detectors
}

/// ---------------- event engine factory ----------------
fn build_event_engine(cfg: &RvoConfig) -> EventEngine {
    let e = &cfg.events[0]; // single-event for now

    let event_type = match e.event_type.as_str() {
        "DummyEvent" => EventType::DummyEvent,
        other => panic!("Unknown event type: {}", other),
    };

    EventEngine::new(EventDefinition {
        event_type,
        signal_threshold: e.signal_threshold,
        duration_ns: e.duration_ms * 1_000_000,
        cooldown_ns: e.cooldown_ms * 1_000_000,
    })
}

fn main() {
    // ---------------- metrics ----------------
    start_metrics_server(9090);

    // ---------------- initial config ----------------
    let cfg = load_config("config/rvo.yaml");
    let detectors = build_detectors(&cfg);
    let event_engine = build_event_engine(&cfg);

    // ---------------- camera ----------------
    let (frame_tx, frame_rx) = bounded(5);
    start_camera(CameraConfig { device_index: 0 }, frame_tx);

    // ---------------- clips ----------------
    let (clip_tx, clip_rx) = bounded(8);
    start_encoder_worker(clip_rx);

    let clip_manager = ClipManager::new(
        clip_tx,
        Duration::from_secs(3),
        Duration::from_secs(2),
    );

    // ---------------- scheduler ----------------
    let scheduler = Arc::new(Mutex::new(
        Scheduler::new(detectors, event_engine, frame_rx, clip_manager),
    ));

    println!("[RVO] Started (camera + clips)");

    // ---------------- reload thread (Section 1.4) ----------------
    {
        let scheduler = Arc::clone(&scheduler);

        thread::spawn(move || {
            let mut signals = Signals::new([SIGHUP]).expect("signals");

            for _ in signals.forever() {
                println!("[RVO] SIGHUP received, reloading config");

                let cfg = load_config("config/rvo.yaml");
                let detectors = build_detectors(&cfg);
                let event_engine = build_event_engine(&cfg);

                let mut sched = scheduler.lock().unwrap();
                sched.swap_runtime(detectors, event_engine);

                println!("[RVO] Reload complete");
            }
        });
    }

    // ---------------- main loop ----------------
    loop {
        scheduler.lock().unwrap().tick();
        thread::sleep(Duration::from_millis(1));
    }
}
