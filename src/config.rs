//! ~/.config/unleash/config.toml. Saved to a temp name and renamed, so a crash never leaves
//! a half-written file.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub appearance: Appearance,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    /// "rice" follows the active rice; a rice id keeps that rice's colours
    pub colors: String,
    pub font: String,
    /// window opacity, 0.3..1
    pub opacity: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config { appearance: Appearance::default() }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance { colors: "rice".into(), font: "Google Sans Flex".into(), opacity: 0.94 }
    }
}

pub fn dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("unleash")
}

pub fn path() -> PathBuf {
    dir().join("config.toml")
}

/// Missing or broken file: defaults (a broken file is left alone, not overwritten, until the
/// user changes a setting).
pub fn load() -> Config {
    std::fs::read_to_string(path()).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(c: &Config) -> std::io::Result<()> {
    let text = toml::to_string_pretty(c).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(dir())?;
    let tmp = dir().join(format!(".config.toml.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path())
}

pub fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".cache")).join("unleash")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_and_partial_files() {
        let c: Config = toml::from_str("[appearance]\ncolors = \"gravity-falls\"\n").unwrap();
        assert_eq!(c.appearance.colors, "gravity-falls");
        assert_eq!(c.appearance.font, "Google Sans Flex");
        let back: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }
}
