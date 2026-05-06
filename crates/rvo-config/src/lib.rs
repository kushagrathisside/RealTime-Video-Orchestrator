mod config;
pub use config::*;

use std::fs;

pub fn load_config(path: &str) -> RvoConfig {
    let contents =
        fs::read_to_string(path).expect("Failed to read config file");

    let cfg: RvoConfig =
        serde_yaml::from_str(&contents)
            .expect("Invalid YAML format");

    cfg.validate().expect("Invalid configuration");

    cfg
}
