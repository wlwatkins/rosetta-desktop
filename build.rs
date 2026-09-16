//! Embeds the Windows resources: the application manifest, the icon and the
//! version block.
//!
//! The manifest is the important one. Without it Windows loads comctl32 v5 and
//! every standard control in the settings window renders in the pre-XP style.
//! It also declares per-monitor DPI awareness early enough to apply before the
//! first window exists.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/rosetta.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // MSVC only: the GNU toolchain would need windres instead.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let manifest = root.join("app.manifest");
    if manifest.is_file() {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    }

    // The icon and version block need a resource compiler. It is part of the
    // Windows SDK, which is present wherever MSVC is, but skipping it only
    // costs a generic icon -- never a failed build.
    let icon = root.join("assets").join("rosetta.ico");
    if !icon.is_file() {
        println!("cargo:warning=assets/rosetta.ico is missing; run tools/make_icon.py");
        return;
    }
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found; building without an icon");
        return;
    };

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut parts: Vec<u16> = version
        .split(['.', '-'])
        .filter_map(|p| p.parse().ok())
        .collect();
    parts.resize(4, 0);
    let comma = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);

    let rc_source = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEOS 0x4
FILETYPE 0x1
{{
  BLOCK "StringFileInfo"
  {{
    BLOCK "040904B0"
    {{
      VALUE "FileDescription", "Rosetta - Hebrew to English screen translation"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "rosetta-desktop"
      VALUE "OriginalFilename", "rosetta-desktop.exe"
      VALUE "ProductName", "Rosetta"
      VALUE "ProductVersion", "{version}"
    }}
  }}
  BLOCK "VarFileInfo"
  {{
    VALUE "Translation", 0x409, 1200
  }}
}}
"#,
        icon = icon.display().to_string().replace('\\', "\\\\"),
    );

    let rc_path = out.join("rosetta.rc");
    let res_path = out.join("rosetta.res");
    if std::fs::write(&rc_path, rc_source).is_err() {
        println!("cargo:warning=could not write {}", rc_path.display());
        return;
    }

    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-arg-bins={}", res_path.display());
        }
        _ => println!("cargo:warning=rc.exe failed; building without an icon"),
    }
}

/// Newest `rc.exe` from the installed Windows SDKs.
fn find_rc() -> Option<PathBuf> {
    let base = std::env::var("ProgramFiles(x86)")
        .or_else(|_| std::env::var("ProgramFiles"))
        .ok()?;
    let bin = Path::new(&base).join("Windows Kits").join("10").join("bin");

    let mut found: Vec<PathBuf> = std::fs::read_dir(&bin)
        .ok()?
        .flatten()
        .map(|e| e.path().join("x64").join("rc.exe"))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found.pop()
}
