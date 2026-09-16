use anyhow::{Context, Result};
use std::path::PathBuf;

/// Model artifacts live next to the executable once installed, but during
/// `cargo run` they sit in the repo. Check both so dev and release behave alike.
pub fn models_dir() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("models"));
            // target/debug/rosetta-desktop.exe -> repo root
            if let Some(root) = dir.parent().and_then(|p| p.parent()) {
                candidates.push(root.join("models"));
            }
        }
    }
    candidates.push(PathBuf::from("models"));

    candidates
        .iter()
        .find(|p| p.join("model_meta.json").is_file())
        .cloned()
        .with_context(|| {
            format!(
                "no model_meta.json found; run tools/setup_model.sh. looked in: {}",
                candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// Bundled native dependencies (Tesseract DLLs + traineddata).
pub fn vendor_dir() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("vendor"));
            if let Some(root) = dir.parent().and_then(|p| p.parent()) {
                candidates.push(root.join("vendor"));
            }
        }
    }
    candidates.push(PathBuf::from("vendor"));

    candidates
        .iter()
        .find(|p| p.join("bin").join("libtesseract-5.dll").is_file())
        .cloned()
        .with_context(|| {
            format!(
                "no vendored Tesseract found; looked in: {}",
                candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
            )
        })
}
