use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct RvoConfig {
    pub detectors: Vec<DetectorConfig>,
    pub events: Vec<EventConfig>,
}

#[derive(Debug, Deserialize)]
pub struct DetectorConfig {
    pub kind: String,

    #[serde(default = "default_enabled")]
    pub enabled: bool,

    pub busy_ns: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct EventConfig {
    pub event_type: String,

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

fn default_duration_ms() -> u64 {
    2000
}

fn default_cooldown_ms() -> u64 {
    5000
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
                "dummy" | "load" | "jitter" => {}
                other => {
                    return Err(format!(
                        "Unknown detector kind: {}",
                        other
                    ));
                }
            }

            if d.kind == "load" && d.busy_ns.is_none() {
                return Err(
                    "Detector 'load' requires busy_ns".into(),
                );
            }
        }

        for e in &self.events {
            match e.event_type.as_str() {
                "DummyEvent" => {}
                other => {
                    return Err(format!(
                        "Unknown event type: {}",
                        other
                    ));
                }
            }

            if e.duration_ms == 0 {
                return Err(format!(
                    "Event {} has zero duration",
                    e.event_type
                ));
            }

            if e.cooldown_ms == 0 {
                return Err(format!(
                    "Event {} has zero cooldown",
                    e.event_type
                ));
            }
        }

        Ok(())
    }
}
