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
    /// "" = the one in ~/.config/gtk-4.0/settings.ini (the portal may report another, e.g. a KDE
    /// theme with blue folders), else this icon theme's name
    pub icon_theme: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { appearance: Appearance::default() }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance { colors: "rice".into(), font: "Google Sans Flex".into(), opacity: 0.94, icon_theme: String::new() }
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

/// `gtk-icon-theme-name` from settings.ini (gtk-4.0, then gtk-3.0).
pub fn gtk_ini_icon_theme() -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    ["gtk-4.0", "gtk-3.0"].iter().find_map(|v| ini_value(&std::fs::read_to_string(base.join(v).join("settings.ini")).ok()?, "gtk-icon-theme-name"))
}

fn ini_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string()).filter(|v| !v.is_empty())
    })
}

/// The icon theme to use: the setting, else settings.ini, else whatever GTK picked.
pub fn icon_theme(c: &Config) -> Option<String> {
    let t = c.appearance.icon_theme.trim();
    if t.is_empty() { gtk_ini_icon_theme() } else { Some(t.to_string()) }
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
    #[test]
    fn reads_icon_theme_from_settings_ini() {
        let ini = "[Settings]\ngtk-font-name=X\ngtk-icon-theme-name = Papirus-Catppuccin-Mauve\n";
        assert_eq!(ini_value(ini, "gtk-icon-theme-name").as_deref(), Some("Papirus-Catppuccin-Mauve"));
        assert_eq!(ini_value("gtk-icon-theme-name=", "gtk-icon-theme-name"), None);
    }
}
