//! Persisted user settings.
//!
//! Precedence is defaults < settings.json < environment. The environment still
//! wins because it is what the dev commands and scripts use, but anything set
//! that way is reported so the settings window can say why a field it is
//! showing will not take effect.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::translate::{Backend, Config};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub hotkey: String,
    /// "dark" or "light".
    pub theme: String,
    /// Beam width; 1 is greedy.
    pub beams: usize,
    /// "cpu", "dml" or "cuda".
    pub backend: String,
    /// Tesseract language code, e.g. "heb".
    pub lang: String,
    /// How much to upscale a region before OCR.
    pub upscale: u32,
    /// Tesseract page-segmentation mode.
    pub psm: i32,
    /// Log every scan and its translations to the console.
    pub debug_log: bool,
    /// Ask GitHub for a newer release at start-up. The only network access the
    /// app ever makes, so it is a setting rather than a given.
    pub check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: "ctrl+alt+t".into(),
            theme: "dark".into(),
            beams: 4,
            backend: "cpu".into(),
            lang: "heb".into(),
            upscale: 2,
            psm: 6,
            debug_log: false,
            check_updates: true,
        }
    }
}

/// Which fields the environment is currently overriding.
#[derive(Debug, Clone, Default)]
pub struct Overrides(pub Vec<&'static str>);

impl Overrides {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Settings {
    pub fn path() -> Result<PathBuf> {
        let dir = dirs::config_dir().context("no config directory for this user")?;
        Ok(dir.join("Rosetta").join("settings.json"))
    }

    /// Reads the file if present, then layers the environment on top.
    pub fn load() -> (Self, Overrides) {
        let mut settings = Self::from_file().unwrap_or_default();
        let over = settings.apply_overrides(|key| std::env::var(key).ok());
        (settings, over)
    }

    /// The environment layer, taking its lookup as an argument so it can be
    /// exercised without mutating the process environment (which is global, and
    /// would race with tests running in parallel).
    pub fn apply_overrides(&mut self, get: impl Fn(&str) -> Option<String>) -> Overrides {
        let mut over = Overrides::default();

        if let Some(v) = get("ROSETTA_HOTKEY") {
            self.hotkey = v;
            over.0.push("hotkey");
        }
        if let Some(v) = get("ROSETTA_THEME") {
            self.theme = v;
            over.0.push("theme");
        }
        if let Some(n) = get("ROSETTA_BEAMS").and_then(|v| v.parse::<usize>().ok()) {
            self.beams = n.max(1);
            over.0.push("beams");
        }
        if let Some(v) = get("ROSETTA_BACKEND") {
            self.backend = v;
            over.0.push("backend");
        }
        if let Some(v) = get("ROSETTA_LANG") {
            self.lang = v;
            over.0.push("lang");
        }
        if let Some(n) = get("ROSETTA_UPSCALE").and_then(|v| v.parse::<u32>().ok()) {
            self.upscale = n.max(1);
            over.0.push("upscale");
        }
        if let Some(n) = get("ROSETTA_PSM").and_then(|v| v.parse::<i32>().ok()) {
            self.psm = n;
            over.0.push("psm");
        }
        if get("ROSETTA_DEBUG").is_some() {
            self.debug_log = true;
            over.0.push("debug_log");
        }

        self.clamp();
        over
    }

    fn from_file() -> Option<Self> {
        let path = Self::path().ok()?;
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    fn clamp(&mut self) {
        self.beams = self.beams.clamp(1, 12);
        self.upscale = self.upscale.clamp(1, 4);
        if !matches!(self.psm, 3 | 4 | 6 | 11 | 12 | 13) {
            self.psm = 6;
        }
        if self.theme != "light" {
            self.theme = "dark".into();
        }
        if !matches!(self.backend.as_str(), "cpu" | "dml" | "cuda") {
            self.backend = "cpu".into();
        }
        if self.lang.trim().is_empty() {
            self.lang = "heb".into();
        }
    }

    pub fn translate_config(&self) -> Config {
        Config {
            backend: Backend::parse(&self.backend).unwrap_or(Backend::Cpu),
            beams: self.beams.max(1),
            ..Config::default()
        }
    }

    /// True when a change between these two needs the pipeline rebuilt rather
    /// than just a repaint.
    pub fn pipeline_differs(&self, other: &Self) -> bool {
        self.beams != other.beams
            || self.backend != other.backend
            || self.lang != other.lang
            || self.upscale != other.upscale
            || self.psm != other.psm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        let s = Settings::default();
        assert_eq!(s.hotkey, "ctrl+alt+t");
        assert_eq!(s.theme, "dark");
        assert_eq!(s.beams, 4);
        assert_eq!(s.backend, "cpu");
        assert_eq!(s.lang, "heb");
        assert_eq!(s.upscale, 2);
        assert_eq!(s.psm, 6);
        assert!(!s.debug_log);
    }

    #[test]
    fn json_round_trips() {
        let s = Settings { beams: 8, theme: "light".into(), ..Settings::default() };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn a_partial_file_keeps_the_defaults_for_the_rest() {
        // serde(default) is what lets an old settings.json survive a new field.
        let s: Settings = serde_json::from_str(r#"{"beams": 1}"#).unwrap();
        assert_eq!(s.beams, 1);
        assert_eq!(s.theme, "dark");
        assert_eq!(s.hotkey, "ctrl+alt+t");
    }

    #[test]
    fn an_unknown_field_is_ignored_rather_than_fatal() {
        let s: Settings = serde_json::from_str(r#"{"beams": 2, "from_the_future": true}"#).unwrap();
        assert_eq!(s.beams, 2);
    }

    #[test]
    fn clamp_rejects_nonsense_from_a_hand_edited_file() {
        let mut s = Settings {
            beams: 999,
            upscale: 0,
            psm: 42,
            theme: "chartreuse".into(),
            backend: "quantum".into(),
            lang: "   ".into(),
            ..Settings::default()
        };
        s.clamp();
        assert_eq!(s.beams, 12);
        assert_eq!(s.upscale, 1);
        assert_eq!(s.psm, 6, "an unsupported mode falls back to single block");
        assert_eq!(s.theme, "dark");
        assert_eq!(s.backend, "cpu");
        assert_eq!(s.lang, "heb");
    }

    #[test]
    fn env_overrides_win_and_are_reported() {
        let mut s = Settings::default();
        let over = s.apply_overrides(env(&[
            ("ROSETTA_THEME", "light"),
            ("ROSETTA_BEAMS", "1"),
            ("ROSETTA_DEBUG", "1"),
        ]));
        assert_eq!(s.theme, "light");
        assert_eq!(s.beams, 1);
        assert!(s.debug_log);
        assert_eq!(over.0, vec!["theme", "beams", "debug_log"]);
    }

    #[test]
    fn no_env_means_no_overrides() {
        let mut s = Settings::default();
        let over = s.apply_overrides(env(&[]));
        assert!(over.is_empty());
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn an_unparseable_numeric_override_is_ignored() {
        let mut s = Settings::default();
        let over = s.apply_overrides(env(&[("ROSETTA_BEAMS", "lots")]));
        assert_eq!(s.beams, 4, "the default must survive a bad value");
        assert!(over.is_empty());
    }

    #[test]
    fn overrides_are_clamped_too() {
        let mut s = Settings::default();
        s.apply_overrides(env(&[("ROSETTA_BACKEND", "nonsense"), ("ROSETTA_PSM", "99")]));
        assert_eq!(s.backend, "cpu");
        assert_eq!(s.psm, 6);
    }

    #[test]
    fn debug_flag_is_set_by_presence_not_value() {
        let mut s = Settings::default();
        s.apply_overrides(env(&[("ROSETTA_DEBUG", "0")]));
        assert!(s.debug_log, "ROSETTA_DEBUG=0 still counts as set, like the shell idiom");
    }

    #[test]
    fn translate_config_maps_backend_and_beams() {
        let s = Settings { backend: "dml".into(), beams: 6, ..Settings::default() };
        let cfg = s.translate_config();
        assert_eq!(cfg.backend, Backend::DirectMl);
        assert_eq!(cfg.beams, 6);

        let cfg = Settings { backend: "cuda".into(), ..Settings::default() }.translate_config();
        assert_eq!(cfg.backend, Backend::Cuda);
    }

    #[test]
    fn pipeline_differs_only_for_pipeline_fields() {
        let base = Settings::default();

        // Cosmetic changes must not tear down the worker.
        for cosmetic in [
            Settings { theme: "light".into(), ..base.clone() },
            Settings { hotkey: "ctrl+shift+t".into(), ..base.clone() },
            Settings { debug_log: true, ..base.clone() },
        ] {
            assert!(!base.pipeline_differs(&cosmetic), "{cosmetic:?} should not restart it");
        }

        // These each need the model or OCR engine rebuilt.
        for heavy in [
            Settings { beams: 1, ..base.clone() },
            Settings { backend: "dml".into(), ..base.clone() },
            Settings { lang: "eng".into(), ..base.clone() },
            Settings { upscale: 3, ..base.clone() },
            Settings { psm: 11, ..base.clone() },
        ] {
            assert!(base.pipeline_differs(&heavy), "{heavy:?} should restart it");
        }
    }
}
