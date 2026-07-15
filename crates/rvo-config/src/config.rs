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
                    Some(sig) => {
                        rvo_signals::store::SignalRegistry::register(sig)?;
                    }
                }
            }
        }

        for e in &self.events {
            // Register signal type dynamically if not built-in
            rvo_signals::store::SignalRegistry::register(&e.signal_type)?;

            // duration_ms == 0 is valid: instant trigger, confidence = 1.0.

            if e.cooldown_ms == 0 {
                return Err(format!(
                    "Event '{}' has zero cooldown — this would cause continuous emission",
                    e.event_type
                ));
            }
        }

        // Cross-reference: every event's signal_type must be produced by at least
        // one enabled detector. Without this check, a signal name typo or mismatch
        // between the detector's output_signal and the event's signal_type causes
        // a permanently silent event engine with no error or warning.
        //
        // Built-in detector kinds (dummy, load, jitter) all emit the "Dummy" signal.
        // remote_grpc detectors declare their signal via output_signal.
        let mut produced_signals: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for d in &self.detectors {
            if !d.enabled {
                continue;
            }
            match d.kind.as_str() {
                "dummy" | "load" | "jitter" => {
                    produced_signals.insert("Dummy".to_string());
                }
                "remote_grpc" => {
                    if let Some(sig) = &d.output_signal {
                        produced_signals.insert(sig.clone());
                    }
                }
                _ => {}
            }
        }

        for e in &self.events {
            if !produced_signals.contains(&e.signal_type) {
                return Err(format!(
                    "Event '{}' watches signal '{}' but no enabled detector produces it. \
                     Check that your detector's output_signal matches the event's signal_type.",
                    e.event_type, e.signal_type
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_config(
        detector_kind: &str,
        detector_signal: Option<&str>,
        event_signal: &str,
    ) -> RvoConfig {
        RvoConfig {
            camera: CameraConfig::default(),
            detectors: vec![DetectorConfig {
                kind: detector_kind.to_string(),
                enabled: true,
                busy_ns: if detector_kind == "load" {
                    Some(1_000)
                } else {
                    None
                },
                endpoint: if detector_kind == "remote_grpc" {
                    Some("http://localhost:50051".to_string())
                } else {
                    None
                },
                output_signal: detector_signal.map(|s| s.to_string()),
                timeout_ms: None,
                max_fps: None,
                ttl_ms: None,
            }],
            events: vec![EventConfig {
                event_type: "TestEvent".to_string(),
                signal_type: event_signal.to_string(),
                signal_threshold: 1,
                duration_ms: 100,
                cooldown_ms: 1000,
            }],
            clips_dir: "clips".to_string(),
            event_log: None,
        }
    }

    #[test]
    fn remote_grpc_signal_mismatch_is_rejected() {
        // Detector produces "PersonDetected", event watches "FacePresent" — should fail.
        let cfg = minimal_config("remote_grpc", Some("PersonDetected"), "FacePresent");
        let err = cfg.validate().unwrap_err();
        assert!(
            err.contains("no enabled detector produces it"),
            "Expected mismatch error, got: {err}"
        );
    }

    #[test]
    fn remote_grpc_signal_match_is_accepted() {
        // Detector and event both use "PersonDetected" — should pass.
        let cfg = minimal_config("remote_grpc", Some("PersonDetected"), "PersonDetected");
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn dummy_detector_satisfies_dummy_event() {
        // Built-in dummy detector always produces "Dummy" — should pass.
        let cfg = minimal_config("dummy", None, "Dummy");
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn dummy_detector_does_not_satisfy_non_dummy_event() {
        // dummy detector only produces "Dummy"; watching "PersonDetected" must fail.
        let cfg = minimal_config("dummy", None, "PersonDetected");
        let err = cfg.validate().unwrap_err();
        assert!(
            err.contains("no enabled detector produces it"),
            "Expected mismatch error, got: {err}"
        );
    }

    #[test]
    fn disabled_detector_does_not_satisfy_event() {
        // The matching remote_grpc detector is disabled — event should be rejected.
        let mut cfg = minimal_config("remote_grpc", Some("PersonDetected"), "PersonDetected");
        cfg.detectors[0].enabled = false;
        let err = cfg.validate().unwrap_err();
        assert!(
            err.contains("no enabled detector produces it"),
            "Expected mismatch error for disabled detector, got: {err}"
        );
    }

    #[test]
    fn zero_cooldown_is_rejected() {
        let mut cfg = minimal_config("dummy", None, "Dummy");
        cfg.events[0].cooldown_ms = 0;
        let err = cfg.validate().unwrap_err();
        assert!(
            err.contains("zero cooldown"),
            "Expected cooldown error, got: {err}"
        );
    }
}
