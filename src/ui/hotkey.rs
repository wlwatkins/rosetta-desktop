//! Parsing for the user-configurable activation hotkey.

use anyhow::{bail, Result};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};

#[derive(Clone)]
pub struct Hotkey {
    pub mods: HOT_KEY_MODIFIERS,
    pub vk: u32,
    /// Normalized for display, e.g. "Ctrl+Shift+T".
    pub label: String,
}

/// Accepts things like `ctrl+alt+t`, `ctrl+shift+f9`, `win+alt+h`.
pub fn parse(spec: &str) -> Result<Hotkey> {
    let mut mods = MOD_NOREPEAT;
    let mut key: Option<(u32, String)> = None;
    let mut parts = Vec::new();

    for raw in spec.split('+') {
        let token = raw.trim().to_ascii_lowercase();
        if token.is_empty() {
            continue;
        }
        match token.as_str() {
            "ctrl" | "control" => {
                mods |= MOD_CONTROL;
                parts.push("Ctrl".to_string());
            }
            "alt" => {
                mods |= MOD_ALT;
                parts.push("Alt".to_string());
            }
            "shift" => {
                mods |= MOD_SHIFT;
                parts.push("Shift".to_string());
            }
            "win" | "super" | "meta" => {
                mods |= MOD_WIN;
                parts.push("Win".to_string());
            }
            other => {
                if key.is_some() {
                    bail!("hotkey `{spec}` names more than one key");
                }
                key = Some(virtual_key(other)?);
            }
        }
    }

    let Some((vk, name)) = key else {
        bail!("hotkey `{spec}` has no key, only modifiers");
    };
    if mods == MOD_NOREPEAT {
        bail!("hotkey `{spec}` needs at least one modifier, or it would swallow the key globally");
    }
    parts.push(name);

    Ok(Hotkey { mods, vk, label: parts.join("+") })
}

fn virtual_key(token: &str) -> Result<(u32, String)> {
    // Function keys.
    if let Some(n) = token.strip_prefix('f') {
        if let Ok(n) = n.parse::<u32>() {
            if (1..=24).contains(&n) {
                // VK_F1 is 0x70 and the rest follow consecutively.
                return Ok((0x6F + n, format!("F{n}")));
            }
        }
    }

    let mut chars = token.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        bail!("unrecognized key `{token}` in hotkey");
    };
    if c.is_ascii_alphabetic() {
        let up = c.to_ascii_uppercase();
        return Ok((up as u32, up.to_string()));
    }
    if c.is_ascii_digit() {
        return Ok((c as u32, c.to_string()));
    }
    bail!("unrecognized key `{token}` in hotkey")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_specs() {
        assert_eq!(parse("ctrl+alt+t").unwrap().label, "Ctrl+Alt+T");
        assert_eq!(parse("Ctrl+Shift+T").unwrap().label, "Ctrl+Shift+T");
        assert_eq!(parse("ctrl+shift+f9").unwrap().vk, 0x78);
        assert_eq!(parse("win+alt+h").unwrap().label, "Win+Alt+H");
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse("t").is_err(), "bare key must be rejected");
        assert!(parse("ctrl+alt").is_err(), "modifiers only must be rejected");
        assert!(parse("ctrl+alt+tt").is_err());
    }
}
