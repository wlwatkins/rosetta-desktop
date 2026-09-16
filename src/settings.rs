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
        let mut over = Overrides::default();

        if let Ok(v) = std::env::var("ROSETTA_HOTKEY") {
            settings.hotkey = v;
            over.0.push("hotkey");
        }
        if let Ok(v) = std::env::var("ROSETTA_THEME") {
            settings.theme = v;
            over.0.push("theme");
        }
        if let Ok(v) = std::env::var("ROSETTA_BEAMS") {
            if let Ok(n) = v.parse::<usize>() {
                settings.beams = n.max(1);
                over.0.push("beams");
            }
        }
        if let Ok(v) = std::env::var("ROSETTA_BACKEND") {
            settings.backend = v;
            over.0.push("backend");
        }
        if let Ok(v) = std::env::var("ROSETTA_LANG") {
            settings.lang = v;
            over.0.push("lang");
        }
        if let Ok(v) = std::env::var("ROSETTA_UPSCALE") {
            if let Ok(n) = v.parse::<u32>() {
                settings.upscale = n.max(1);
                over.0.push("upscale");
            }
        }
        if let Ok(v) = std::env::var("ROSETTA_PSM") {
            if let Ok(n) = v.parse::<i32>() {
                settings.psm = n;
                over.0.push("psm");
            }
        }
        if std::env::var("ROSETTA_DEBUG").is_ok() {
            settings.debug_log = true;
            over.0.push("debug_log");
        }

        settings.clamp();
        (settings, over)
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
