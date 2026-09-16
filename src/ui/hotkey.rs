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
        assert_eq!(parse("win+alt+h").unwrap().label, "Win+Alt+H");
    }

    #[test]
    fn is_case_and_space_insensitive() {
        assert_eq!(parse("  CTRL + ALT + t ").unwrap().label, "Ctrl+Alt+T");
    }

    #[test]
    fn accepts_the_modifier_aliases() {
        for spec in ["win+t", "super+t", "meta+t"] {
            assert_eq!(parse(spec).unwrap().label, "Win+T", "{spec}");
        }
        assert_eq!(parse("control+t").unwrap().label, "Ctrl+T");
    }

    #[test]
    fn function_keys_map_to_the_right_codes() {
        // VK_F1 is 0x70 and the rest follow consecutively.
        assert_eq!(parse("ctrl+f1").unwrap().vk, 0x70);
        assert_eq!(parse("ctrl+shift+f9").unwrap().vk, 0x78);
        assert_eq!(parse("ctrl+f24").unwrap().vk, 0x87);
        assert_eq!(parse("ctrl+f12").unwrap().label, "Ctrl+F12");
    }

    #[test]
    fn letters_and_digits_use_their_ascii_codes() {
        assert_eq!(parse("ctrl+a").unwrap().vk, 0x41);
        assert_eq!(parse("ctrl+z").unwrap().vk, 0x5A);
        assert_eq!(parse("ctrl+0").unwrap().vk, 0x30);
        assert_eq!(parse("ctrl+9").unwrap().vk, 0x39);
    }

    #[test]
    fn modifiers_combine() {
        let all = parse("ctrl+alt+shift+win+k").unwrap();
        assert!(all.mods.contains(MOD_CONTROL));
        assert!(all.mods.contains(MOD_ALT));
        assert!(all.mods.contains(MOD_SHIFT));
        assert!(all.mods.contains(MOD_WIN));
        // NOREPEAT is always on: holding the key must not re-fire.
        assert!(all.mods.contains(MOD_NOREPEAT));
        assert_eq!(all.label, "Ctrl+Alt+Shift+Win+K");
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse("t").is_err(), "a bare key would be swallowed globally");
        assert!(parse("ctrl+alt").is_err(), "modifiers alone are not a shortcut");
        assert!(parse("ctrl+alt+tt").is_err(), "two-letter keys are not a thing");
        assert!(parse("").is_err());
        assert!(parse("ctrl+").is_err());
    }

    #[test]
    fn rejects_two_keys() {
        assert!(parse("ctrl+a+b").is_err());
    }

    #[test]
    fn rejects_out_of_range_function_keys() {
        assert!(parse("ctrl+f0").is_err());
        assert!(parse("ctrl+f25").is_err());
    }

    #[test]
    fn rejects_punctuation_keys() {
        // Not supported, and silently mapping them would surprise.
        assert!(parse("ctrl+alt++").is_err());
        assert!(parse("ctrl+alt+/").is_err());
    }

    #[test]
    fn every_default_in_the_docs_parses() {
        // The settings window and README both offer these.
        for spec in ["ctrl+alt+t", "ctrl+shift+t", "ctrl+shift+f9", "win+alt+h"] {
            assert!(parse(spec).is_ok(), "{spec} should be accepted");
        }
    }
}
