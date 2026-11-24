//! Settings that belong to Drift itself, stored on this Mac.

use std::{env, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Prefs {
    /// Off by default: turning it on sends track names to LRCLIB.
    pub synced_lyrics: bool,
    pub live_island: bool,
    pub live_wallpaper: bool,
    /// The light around the island, and the way it grows and brightens with the voice.
    pub island_glow: bool,
    pub island_pulse: bool,
    /// The galaxy opens up and a wave of light rolls out when a chorus comes.
    pub chorus_burst: bool,
    /// The song's galaxy also fills the Drift window, under the interface.
    pub window_galaxy: bool,
    /// Off by default: plays a song's music video instead of the song and turns it into stars.
    pub clip_particles: bool,
    /// The column beside the galaxy on "Now": the galaxy or clip switch, the queue, lyrics and related music.
    pub now_column: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            synced_lyrics: false,
            live_island: true,
            live_wallpaper: true,
            island_glow: true,
            island_pulse: true,
            chorus_burst: true,
            window_galaxy: true,
            clip_particles: false,
            now_column: true,
        }
    }
}

fn path() -> Option<PathBuf> {
    let home = env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/Drift/settings.json"))
}

impl Prefs {
    pub fn load() -> Self {
        path()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = fs::write(path, bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_keep_live_features_on() {
        let prefs: Prefs = serde_json::from_str(r#"{"synced_lyrics": true}"#).unwrap();
        assert!(prefs.synced_lyrics && prefs.live_island && prefs.live_wallpaper);
        assert!(prefs.island_glow && prefs.island_pulse && prefs.chorus_burst && prefs.window_galaxy);
    }
}
