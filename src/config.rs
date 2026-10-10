//! Settings kept between runs, in `~/.config/notebook/config.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The workspace folder open last.
    pub folder: Option<PathBuf>,
    /// The chosen skin's name; Default when unset.
    pub skin: Option<String>,
    /// Use the desktop's title bar instead of Patina's frame.
    pub system_title_bar: bool,
}

fn dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("notebook"))
}

impl Config {
    pub fn load() -> Self {
        let Some(dir) = dir() else {
            return Self::default();
        };
        let mut config: Config = std::fs::read_to_string(dir.join("config.toml"))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        // The GTK Notebook kept the last folder here; pick it up once.
        if config.folder.is_none() {
            config.folder = std::fs::read_to_string(dir.join("last-folder"))
                .ok()
                .map(|s| PathBuf::from(s.trim()))
                .filter(|p| p.is_dir());
        }
        config
    }

    pub fn save(&self) {
        let Some(dir) = dir() else { return };
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(text) = toml::to_string(self) {
            let _ = std::fs::write(dir.join("config.toml"), text);
        }
    }
}
