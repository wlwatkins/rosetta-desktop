//! "Start Rosetta when I sign in", backed by the per-user Run key.
//!
//! The installer writes the same value when that box is ticked, so there is one
//! mechanism rather than a Startup shortcut competing with a registry entry --
//! two of those would launch the app twice.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Must match the [Registry] entry in installer\rosetta.iss.
pub const VALUE_NAME: &str = "Rosetta";

/// The command line Windows should run at sign-in.
///
/// Quoted because the default install path sits under "Program Files" on some
/// machines and under a user name that may contain spaces on all of them.
pub fn command_for(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

fn exe_path() -> Result<PathBuf> {
    std::env::current_exe().context("locating the running executable")
}

fn open(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<HKEY> {
    let mut key = HKEY::default();
    unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            None,
            access,
            &mut key,
        )
        .ok()
        .context("opening the Run key")?;
    }
    Ok(key)
}

/// True when the Run value exists and points at this executable.
///
/// A value left behind by a copy that has since been moved should read as off,
/// or the menu would show a tick for something that no longer starts.
pub fn is_enabled() -> bool {
    is_enabled_named(VALUE_NAME)
}

/// Split out so tests can use a value name of their own rather than writing the
/// real one, which would make a test run schedule a test binary at sign-in.
fn is_enabled_named(value: &str) -> bool {
    let Ok(exe) = exe_path() else { return false };
    let Ok(key) = open(KEY_READ) else { return false };

    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let result = unsafe {
        RegQueryValueExW(
            key,
            &HSTRING::from(value),
            None,
            None,
            Some(buffer.as_mut_ptr() as *mut u8),
            Some(&mut size),
        )
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if result.is_err() {
        return false;
    }

    let chars = (size as usize / 2).saturating_sub(1).min(buffer.len());
    let stored = String::from_utf16_lossy(&buffer[..chars]);
    let stored = stored.trim_end_matches('\0');
    stored.eq_ignore_ascii_case(&command_for(&exe))
}

pub fn set(enabled: bool) -> Result<()> {
    set_named(VALUE_NAME, enabled)
}

fn set_named(value: &str, enabled: bool) -> Result<()> {
    let key = open(KEY_WRITE)?;
    let name = HSTRING::from(value);

    let outcome = unsafe {
        if enabled {
            let command = HSTRING::from(command_for(&exe_path()?));
            // The trailing NUL is part of a REG_SZ payload.
            let bytes = std::slice::from_raw_parts(
                command.as_ptr() as *const u8,
                (command.len() + 1) * 2,
            );
            RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes))
        } else {
            let deleted = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
            // Turning off something already off is success, not failure.
            if deleted == ERROR_FILE_NOT_FOUND {
                windows::Win32::Foundation::WIN32_ERROR(0)
            } else {
                deleted
            }
        }
    };

    unsafe {
        let _ = RegCloseKey(key);
    }
    outcome.ok().context("writing the Run key")?;
    Ok(())
}

pub fn toggle() -> Result<bool> {
    let next = !is_enabled();
    set(next)?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_is_quoted() {
        let cmd = command_for(Path::new(r"C:\Program Files\Rosetta\rosetta-desktop.exe"));
        assert_eq!(cmd, r#""C:\Program Files\Rosetta\rosetta-desktop.exe""#);
    }

    #[test]
    fn a_path_with_spaces_stays_one_argument() {
        // Without the quotes Windows would try to run C:\Users\Some.exe.
        let cmd = command_for(Path::new(r"C:\Users\Some One\App\rosetta-desktop.exe"));
        assert!(cmd.starts_with('"') && cmd.ends_with('"'));
        assert_eq!(cmd.matches('"').count(), 2);
    }

    #[test]
    fn reading_the_current_state_does_not_panic() {
        // Whatever the machine's actual state, this must be answerable.
        let _ = is_enabled();
    }

    #[test]
    fn the_run_key_round_trips() {
        // Under a name of its own, so a test run can never leave the real
        // "Rosetta" value pointing at a test binary.
        const NAME: &str = "RosettaAutostartTest";

        assert!(!is_enabled_named(NAME), "the test value should not exist yet");

        set_named(NAME, true).expect("writing the value");
        assert!(is_enabled_named(NAME), "it should read back as enabled");

        set_named(NAME, false).expect("deleting the value");
        assert!(!is_enabled_named(NAME), "and as disabled once removed");

        // Removing something already absent is success, not an error.
        set_named(NAME, false).expect("deleting twice should be harmless");
    }
}
