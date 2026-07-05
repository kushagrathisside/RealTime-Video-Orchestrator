use rvo_camera::CameraSource;
use rvo_config::{try_load_config, RvoConfig};
use rvo_detector::detector::DetectorNode;
use rvo_detector::jitter::JitterDetector;
use rvo_detector::load::LoadDetector;
use rvo_detector::DummyDetector;
use rvo_events::{Condition, EventDefinition, EventEngine, EventType};
use rvo_remote::RemoteGrpcDetector;
use rvo_signals::store::SignalType;

/// Parse a signal-type name into a [`SignalType`]. Shared by detector and event
/// wiring so both reject unknown names identically.
fn parse_signal_type(name: &str) -> Result<SignalType, String> {
    SignalType::from_name(name).ok_or_else(|| format!("Unknown signal_type: {}", name))
}

pub fn build_detectors(cfg: &RvoConfig) -> Result<Vec<Box<dyn DetectorNode>>, String> {
    let mut detectors: Vec<Box<dyn DetectorNode>> = Vec::new();

    for detector in &cfg.detectors {
        if !detector.enabled {
            continue;
        }

        match detector.kind.as_str() {
            "dummy" => detectors.push(Box::new(DummyDetector)),
            "load" => {
                let busy = detector.busy_ns.unwrap_or(1_000_000);
                detectors.push(Box::new(LoadDetector::new(busy)));
            }
            "jitter" => detectors.push(Box::new(JitterDetector)),
            "remote_grpc" => {
                let endpoint = detector
                    .endpoint
                    .clone()
                    .ok_or("Detector 'remote_grpc' requires endpoint")?;
                let signal_name = detector
                    .output_signal
                    .clone()
                    .ok_or("Detector 'remote_grpc' requires output_signal")?;
                let output_signal = parse_signal_type(&signal_name)?;
                let max_fps = detector.max_fps.unwrap_or(15.0);
                let timeout_ms = detector.timeout_ms.unwrap_or(200);
                let ttl_ns = detector.ttl_ms.unwrap_or(1000) * 1_000_000;
                let id = format!("remote:{}", signal_name);

                detectors.push(Box::new(RemoteGrpcDetector::new(
                    id,
                    endpoint,
                    output_signal,
                    max_fps,
                    timeout_ms,
                    ttl_ns,
                )));
            }
            other => return Err(format!("Unknown detector kind: {}", other)),
        }
    }

    Ok(detectors)
}

pub fn build_event_engine(cfg: &RvoConfig) -> Result<EventEngine, String> {
    let mut defs = Vec::new();

    for event in &cfg.events {
        let event_type = EventType::new(event.event_type.clone());

        let signal_type = parse_signal_type(&event.signal_type)?;

        defs.push(EventDefinition {
            event_type,
            condition: Condition::single_gte(signal_type, event.signal_threshold),
            duration_ns: event.duration_ms * 1_000_000,
            cooldown_ns: event.cooldown_ms * 1_000_000,
        });
    }

    Ok(EventEngine::new_many(defs))
}

pub fn build_runtime_config(
    path: &str,
) -> Result<(Vec<Box<dyn DetectorNode>>, EventEngine), String> {
    let cfg = try_load_config(path)?;
    let detectors = build_detectors(&cfg)?;
    let event_engine = build_event_engine(&cfg)?;
    Ok((detectors, event_engine))
}

pub fn build_camera_source(cfg: &RvoConfig) -> CameraSource {
    if let Some(uri) = cfg.camera.source_uri.clone() {
        CameraSource::Uri(uri)
    } else {
        CameraSource::Device(cfg.camera.device_index.unwrap_or(0))
    }
}
