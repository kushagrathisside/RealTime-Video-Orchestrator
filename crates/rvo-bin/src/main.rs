use std::sync::{Arc, Mutex};
use std::{thread, time::Duration};

use clap::Parser;
use crossbeam_channel::bounded;

use rvo_bin::runtime::{
    build_camera_source, build_detectors, build_event_engine, build_runtime_config,
};
use rvo_metrics::start_metrics_server;
use rvo_scheduler::scheduler::Scheduler;

use rvo_buffer::FrameBuffer;
use rvo_config::{try_load_config, DetectorConfig};
use rvo_events::{start_event_file_sink, start_event_logger, EventPublisher};

use rvo_camera::{list_cameras, start_camera, CameraConfig};
use rvo_clips::{start_encoder_worker, ClipManager};

/// RealTime Vision Orchestrator — camera → detectors → events → clips.
///
/// Flags override the YAML config. Events are always sourced from the config
/// file; CLI flags augment the camera and add remote gRPC detectors.
#[derive(Parser, Debug)]
#[command(name = "rvo", version, about)]
struct Cli {
    /// Path to the YAML config (overrides the RVO_CONFIG env var).
    #[arg(long, value_name = "PATH")]
    config: Option<String>,

    /// Local camera device index (overrides the config camera).
    #[arg(long, value_name = "N")]
    camera_device: Option<i32>,

    /// Camera URI — rtsp://…, a file path, an MJPEG URL (overrides the config camera).
    #[arg(long, value_name = "URI", conflicts_with = "camera_device")]
    camera_uri: Option<String>,

    /// Add a remote gRPC detector as ENDPOINT=SIGNAL (repeatable), e.g.
    /// --detector http://localhost:50051=PersonDetected
    #[arg(long = "detector", value_name = "ENDPOINT=SIGNAL")]
    detectors: Vec<String>,

    /// Output directory for clip evidence (overrides the config).
    #[arg(long, value_name = "DIR")]
    clips_dir: Option<String>,

    /// Port for the Prometheus metrics/health server.
    #[arg(long, default_value_t = 9090, value_name = "PORT")]
    metrics_port: u16,

    /// Probe local camera device indices 0..10, print which open, and exit.
    #[arg(long)]
    list_cameras: bool,
}

/// Parse `ENDPOINT=SIGNAL` into a `remote_grpc` DetectorConfig.
fn parse_remote_detector(spec: &str) -> Result<DetectorConfig, String> {
    let (endpoint, signal) = spec
        .split_once('=')
        .ok_or_else(|| format!("--detector must be ENDPOINT=SIGNAL, got '{}'", spec))?;
    if endpoint.is_empty() || signal.is_empty() {
        return Err(format!(
            "--detector must be ENDPOINT=SIGNAL, got '{}'",
            spec
        ));
    }
    Ok(DetectorConfig {
        kind: "remote_grpc".to_string(),
        enabled: true,
        busy_ns: None,
        endpoint: Some(endpoint.to_string()),
        output_signal: Some(signal.to_string()),
        timeout_ms: None,
        max_fps: None,
        ttl_ms: None,
    })
}

#[cfg(unix)]
fn reload_scheduler(scheduler: &Arc<Mutex<Scheduler>>, path: &str) -> Result<(), String> {
    let (detectors, event_engine) = build_runtime_config(path)?;
    let mut sched = scheduler
        .lock()
        .map_err(|_| "Scheduler lock poisoned".to_string())?;
    sched.swap_runtime(detectors, event_engine);
    Ok(())
}

#[cfg(unix)]
fn spawn_reload_thread(scheduler: Arc<Mutex<Scheduler>>, config_path: String) {
    use signal_hook::consts::SIGHUP;
    use signal_hook::iterator::Signals;

    thread::spawn(move || {
        let mut signals = Signals::new([SIGHUP]).expect("signals");

        for _ in signals.forever() {
            println!("[RVO] SIGHUP — reloading config from {}", config_path);

            match reload_scheduler(&scheduler, &config_path) {
                Ok(()) => println!("[RVO] Reload complete"),
                Err(err) => eprintln!("[RVO] Reload failed: {}", err),
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_reload_thread(_scheduler: Arc<Mutex<Scheduler>>, _config_path: String) {
    println!("[RVO] SIGHUP config reload disabled on this platform");
}

fn main() {
    let cli = Cli::parse();

    // ---------------- list cameras and exit ----------------
    if cli.list_cameras {
        println!("[RVO] Probing camera device indices 0..10 …");
        let found = list_cameras(10);
        if found.is_empty() {
            println!("[RVO] No camera devices opened. Try a --camera-uri source instead.");
        } else {
            for idx in &found {
                println!("  device {idx}  (use: --camera-device {idx})");
            }
        }
        return;
    }

    // ---------------- config path ----------------
    let config_path = cli
        .config
        .clone()
        .or_else(|| std::env::var("RVO_CONFIG").ok())
        .unwrap_or_else(|| "config/rvo.yaml".to_string());

    // ---------------- metrics ----------------
    start_metrics_server(cli.metrics_port);

    // ---------------- initial config (+ CLI overrides) ----------------
    let mut cfg = try_load_config(&config_path).expect("initial config");

    if let Some(idx) = cli.camera_device {
        cfg.camera.device_index = Some(idx);
        cfg.camera.source_uri = None;
    }
    if let Some(uri) = cli.camera_uri.clone() {
        cfg.camera.source_uri = Some(uri);
    }
    if let Some(dir) = cli.clips_dir.clone() {
        cfg.clips_dir = dir;
    }
    for spec in &cli.detectors {
        cfg.detectors
            .push(parse_remote_detector(spec).expect("parse --detector"));
    }

    let detectors = build_detectors(&cfg).expect("build detectors");
    let event_engine = build_event_engine(&cfg).expect("build event engine");

    // ---------------- frame buffer ----------------
    let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(300))); // ~10s @ 30fps

    // ---------------- camera ----------------
    let (frame_tx, frame_rx) = bounded(5);
    start_camera(
        CameraConfig {
            source: build_camera_source(&cfg),
        },
        frame_tx,
    );

    // ---------------- clips ----------------
    let (clip_tx, clip_rx) = bounded(8);
    start_encoder_worker(clip_rx, cfg.clips_dir.clone());

    let clip_manager = ClipManager::new(
        clip_tx,
        Duration::from_secs(3),
        Duration::from_secs(2),
        Arc::clone(&frame_buffer),
    );

    // ---------------- events ----------------
    let (event_tx, event_rx) = bounded(64);

    // Single consumer thread handles stdout logging and optional file sink.
    match cfg.event_log {
        Some(log_path) => start_event_file_sink(event_rx, log_path),
        None => start_event_logger(event_rx),
    }

    let event_publisher = EventPublisher::new(event_tx);

    // ---------------- scheduler ----------------
    let scheduler = Arc::new(Mutex::new(Scheduler::new(
        detectors,
        event_engine,
        frame_rx,
        clip_manager,
        event_publisher,
        frame_buffer,
    )));

    println!(
        "[RVO] Started — config={} clips={} metrics=http://127.0.0.1:{}",
        config_path, cfg.clips_dir, cli.metrics_port
    );

    spawn_reload_thread(Arc::clone(&scheduler), config_path);

    // ---------------- main loop ----------------
    loop {
        scheduler.lock().unwrap().tick();
        thread::sleep(Duration::from_millis(1));
    }
}
