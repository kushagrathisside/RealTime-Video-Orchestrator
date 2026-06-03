use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct RvoConfig {
    #[serde(default)]
    pub camera: CameraConfig,
    pub detectors: Vec<DetectorConfig>,
    pub events: Vec<EventConfig>,
    /// Output directory for clip evidence. Created on first clip if absent.
    #[serde(default = "default_clips_dir")]
    pub clips_dir: String,
    /// Optional path for JSON-lines event output. Not written if absent.
    pub event_log: Option<String>,
}

/// Camera source configuration. Supply either `device_index` (local webcam)
/// or `source_uri` (RTSP stream, file path, or any OpenCV-compatible URI).
/// If both are supplied, `source_uri` takes precedence.
#[derive(Debug, Deserialize, Default)]
pub struct CameraConfig {
    pub device_index: Option<i32>,
    pub source_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DetectorConfig {
    pub kind: String,

    #[serde(default = "default_enabled")]
    pub enabled: bool,

    pub busy_ns: Option<u64>,

    /// gRPC target for `kind: remote_grpc`, e.g. "http://localhost:50051".
    pub endpoint: Option<String>,

    /// Which signal a `remote_grpc` service produces (must be a known
    /// `SignalType` name).
    pub output_signal: Option<String>,

    /// Per-RPC timeout for `remote_grpc`. Defaults to 200ms.
    pub timeout_ms: Option<u64>,

    /// Dispatch-rate cap for `remote_grpc`. Defaults to 15.0.
    pub max_fps: Option<f64>,

    /// Freshness window for signals emitted by `remote_grpc`. Defaults to 1000ms.
    pub ttl_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct EventConfig {
    pub event_type: String,

    /// Which signal slot to watch. Defaults to "Dummy" so existing configs
    /// keep working without change. Ignored if a `condition` block is added
    /// in the future.
    #[serde(default = "default_signal_type")]
    pub signal_type: String,

    pub signal_threshold: u64,

    #[serde(default = "default_duration_ms")]
    pub duration_ms: u64,

    #[serde(default = "default_cooldown_ms")]
    pub cooldown_ms: u64,
}

/* ---------- defaults ---------- */

fn default_enabled() -> bool {
    true
}

fn default_signal_type() -> String {
    "Dummy".to_string()
}

fn default_duration_ms() -> u64 {
    2000
}

fn default_cooldown_ms() -> u64 {
    5000
}

fn default_clips_dir() -> String {
    "clips".to_string()
}

/// Known signal type strings. Must stay in sync with `SignalType` in
/// `rvo-signals`.
const KNOWN_SIGNAL_TYPES: &[&str] = &["Dummy", "MotionLevel", "FacePresent", "PersonDetected"];

const KNOWN_EVENT_TYPES: &[&str] = &["DummyEvent"];

impl RvoConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.detectors.is_empty() {
            return Err("At least one detector must be defined".into());
        }

        if self.events.is_empty() {
            return Err("At least one event must be defined".into());
        }

        for d in &self.detectors {
            match d.kind.as_str() {
                "dummy" | "load" | "jitter" | "remote_grpc" => {}
                other => return Err(format!("Unknown detector kind: {}", other)),
            }

            if d.kind == "load" && d.busy_ns.is_none() {
                return Err("Detector 'load' requires busy_ns".into());
            }

            if d.kind == "remote_grpc" {
                if d.endpoint.is_none() {
                    return Err("Detector 'remote_grpc' requires endpoint".into());
                }
                match &d.output_signal {
                    None => {
                        return Err("Detector 'remote_grpc' requires output_signal".into());
                    }
                    Some(sig) if !KNOWN_SIGNAL_TYPES.contains(&sig.as_str()) => {
                        return Err(format!(
                            "Detector 'remote_grpc' has unknown output_signal '{}'. Known: {:?}",
                            sig, KNOWN_SIGNAL_TYPES
                        ));
                    }
                    Some(_) => {}
                }
            }
        }

        for e in &self.events {
            if !KNOWN_EVENT_TYPES.contains(&e.event_type.as_str()) {
                return Err(format!("Unknown event type: {}", e.event_type));
            }

            if !KNOWN_SIGNAL_TYPES.contains(&e.signal_type.as_str()) {
                return Err(format!(
                    "Unknown signal_type '{}' in event '{}'. Known: {:?}",
                    e.signal_type, e.event_type, KNOWN_SIGNAL_TYPES
                ));
            }

            // duration_ms == 0 is valid: instant trigger, confidence = 1.0.

            if e.cooldown_ms == 0 {
                return Err(format!(
                    "Event '{}' has zero cooldown — this would cause continuous emission",
                    e.event_type
                ));
            }
        }

        Ok(())
    }
}
