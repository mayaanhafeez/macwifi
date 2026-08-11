use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::speedtest::SpeedtestProvider;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    pub theme: Option<String>,
    #[serde(default)]
    pub speedtest: SpeedtestConfig,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SpeedtestConfig {
    #[serde(default)]
    pub provider: SpeedtestProvider,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub custom_command: Option<PathBuf>,
    #[serde(default)]
    pub custom_args: Vec<String>,
}

impl Default for SpeedtestConfig {
    fn default() -> Self {
        Self {
            provider: SpeedtestProvider::Apple,
            timeout_seconds: None,
            custom_command: None,
            custom_args: Vec::new(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let Some(path) = config_path() else {
            return Ok(Self::default());
        };
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn save_theme(name: &str) -> Result<()> {
        let Some(path) = config_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut cfg = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            toml::from_str::<Config>(&text).unwrap_or_default()
        } else {
            Config::default()
        };
        cfg.theme = Some(name.to_string());
        std::fs::write(&path, toml::to_string(&cfg)?)?;
        Ok(())
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("macwifi").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speedtest_timeout_is_optional_and_accepts_existing_values() {
        let defaults: Config = toml::from_str("theme = 'default'").unwrap();
        assert_eq!(defaults.speedtest.timeout_seconds, None);

        let configured: Config =
            toml::from_str("[speedtest]\nprovider = 'netflix'\ntimeout_seconds = 600\n").unwrap();
        assert_eq!(configured.speedtest.timeout_seconds, Some(600));

        let partial: Config = toml::from_str("[speedtest]\ntimeout_seconds = 90\n").unwrap();
        assert!(matches!(
            partial.speedtest.provider,
            SpeedtestProvider::Apple
        ));
        assert_eq!(partial.speedtest.timeout_seconds, Some(90));
    }
}
