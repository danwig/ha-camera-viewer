use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Camera {
    pub name: String,
    #[serde(rename = "entityId")]
    pub entity_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default = "default_ha_url")]
    pub ha_url: String,
    #[serde(default)]
    pub ha_token: String,
    #[serde(default)]
    pub cameras: Vec<Camera>,
    #[serde(default = "default_esp_port")]
    pub esp_port: u16,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_timeout")]
    pub timeout: u32,
    #[serde(default)]
    pub manual_timeout: u32,
    #[serde(default = "default_true")]
    pub always_on_top: bool,
    #[serde(default = "default_true")]
    pub show_timer_bar: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ha_url: default_ha_url(),
            ha_token: String::new(),
            cameras: Vec::new(),
            esp_port: default_esp_port(),
            width: default_width(),
            height: default_height(),
            timeout: default_timeout(),
            manual_timeout: 0,
            always_on_top: true,
            show_timer_bar: true,
        }
    }
}

fn default_ha_url() -> String { "http://homeassistant.local:8123".into() }
fn default_esp_port() -> u16 { 6053 }
fn default_width() -> u32 { 640 }
fn default_height() -> u32 { 480 }
fn default_timeout() -> u32 { 30 }
fn default_true() -> bool { true }

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ha-camera-viewer")
        .join("config.json")
}

pub fn load() -> Config {
    let path = config_path();
    if let Ok(data) = std::fs::read_to_string(&path) {
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        Config::default()
    }
}

pub fn save(config: &Config) -> Result<(), String> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}
