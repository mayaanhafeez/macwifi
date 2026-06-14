use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    pub theme: Option<String>,
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
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("macwifi").join("config.toml"))
}
