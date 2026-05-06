mod config;
pub use config::*;

use std::fs;

pub fn try_load_config(path: &str) -> Result<RvoConfig, String> {
    let contents = fs::read_to_string(path)
        .map_err(|err| format!("Failed to read config file: {}", err))?;

    let cfg: RvoConfig = serde_yaml::from_str(&contents)
        .map_err(|err| format!("Invalid YAML format: {}", err))?;

    cfg.validate()?;

    Ok(cfg)
}

pub fn load_config(path: &str) -> RvoConfig {
    try_load_config(path).expect("Invalid configuration")
}
