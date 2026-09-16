//! The Settings window opened from the tray menu.
//!
//! Plain Win32 controls rather than anything custom-drawn: this is an ordinary
//! form, and the system already has a themed, DPI-aware, keyboard-navigable
//! implementation of every widget on it.

use anyhow::Result;
use std::path::Path;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, DeleteObject, HFONT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::hotkey;
use crate::settings::{Overrides, Settings};

const CLASS: PCWSTR = w!("RosettaSettingsWindow");

const ID_HOTKEY: usize = 1001;
const ID_THEME: usize = 1002;
const ID_BEAMS: usize = 1003;
const ID_BACKEND: usize = 1004;
const ID_LANG: usize = 1005;
const ID_UPSCALE: usize = 1006;
const ID_PSM: usize = 1007;
const ID_DEBUG: usize = 1008;
const ID_UPDATES: usize = 1009;
const ID_SAVE: usize = 1;
const ID_CANCEL: usize = 2;
const ID_DEFAULTS: usize = 1010;

/// Posted to the owner once settings have been written to disk.
pub const WM_SETTINGS_SAVED: u32 = WM_APP + 3;

const BEAMS: &[usize] = &[1, 2, 4, 6, 8];
const UPSCALES: &[u32] = &[1, 2, 3, 4];
/// (mode, label) for the Tesseract page-segmentation modes worth exposing.
const PSMS: &[(i32, &str)] = &[
    (3, "3 - automatic"),
    (4, "4 - columns"),
    (6, "6 - single block (default)"),
    (11, "11 - sparse text"),
    (12, "12 - sparse with layout"),
    (13, "13 - raw line"),
];
const THEMES: &[(&str, &str)] = &[("dark", "Dark"), ("light", "Light")];
const BACKENDS: &[(&str, &str)] = &[
    ("cpu", "CPU (fastest here)"),
    ("dml", "DirectML"),
    ("cuda", "CUDA"),
];

struct Dlg {
    owner: HWND,
    font: HFONT,
    head_font: HFONT,
    over: Overrides,
    langs: Vec<String>,
    ctl: Vec<(usize, HWND)>,
}

impl Dlg {
    fn get(&self, id: usize) -> HWND {
        self.ctl
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, h)| *h)
            .unwrap_or_default()
    }
}

/// Opens the window, or focuses it if it is already up.
pub fn open(owner: HWND, settings: &Settings, over: &Overrides, tessdata: &Path) -> Result<HWND> {
    unsafe {
        let existing = FindWindowExW(None, None, CLASS, None);
        if let Ok(h) = existing {
            if !h.is_invalid() {
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
                return Ok(h);
            }
        }

        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // COLOR_BTNFACE + 1, the documented way to hand a system colour
            // to a window class as a brush.
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(16isize as *mut _),
            ..Default::default()
        };
        // Re-registering the same class is harmless; the first call wins.
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            CLASS,
            w!("Rosetta - Settings"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            100,
            100,
            Some(owner),
            None,
            Some(instance.into()),
            None,
        )?;

        let dlg = Box::new(Dlg {
            owner,
            font: HFONT::default(),
            head_font: HFONT::default(),
            over: over.clone(),
            langs: available_languages(tessdata),
            ctl: Vec::new(),
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(dlg) as isize);

        build(hwnd, settings)?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        Ok(hwnd)
    }
}

/// Tesseract language codes actually present, so the list cannot offer one that
/// would fail to initialise.
fn available_languages(tessdata: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(tessdata)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("traineddata") {
                p.file_stem().and_then(|s| s.to_str()).map(str::to_string)
            } else {
                None
            }
        })
        .collect();
    out.sort();
    if out.is_empty() {
        out.push("heb".into());
    }
    out
}

unsafe fn dlg_from(hwnd: HWND) -> Option<&'static mut Dlg> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if ptr == 0 {
        None
    } else {
        Some(unsafe { &mut *(ptr as *mut Dlg) })
    }
}

fn build(hwnd: HWND, s: &Settings) -> Result<()> {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        let px = |v: i32| v * dpi as i32 / 96;

        // The shell's own message font, at this window's DPI, plus a semibold
        // cut of it for the section headers.
        let mut ncm = NONCLIENTMETRICSW {
            cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let got = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut _),
            0,
            dpi,
        )
        .is_ok();
        let (font, head_font) = if got {
            let body = CreateFontIndirectW(&ncm.lfMessageFont);
            let mut head_lf = ncm.lfMessageFont;
            head_lf.lfWeight = 600; // semibold
            (body, CreateFontIndirectW(&head_lf))
        } else {
            (HFONT::default(), HFONT::default())
        };

        let Some(dlg) = dlg_from(hwnd) else { return Ok(()) };
        dlg.font = font;
        dlg.head_font = head_font;

        // Layout grid, in 96-dpi units.
        let width = 504;
        let margin = px(24);
        let label_x = px(40);
        let label_w = px(160);
        let ctl_x = px(208);
        let ctl_w = px(272);
        let row_h = px(32);
        let mut y = px(20);

        let add = |class: PCWSTR, text: &str, style: WINDOW_STYLE, id: usize,
                   x: i32, w: i32, h: i32, yy: i32, f: HFONT| -> HWND {
            let hw = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                &HSTRING::from(text),
                WS_CHILD | WS_VISIBLE | style,
                x,
                yy,
                w,
                h,
                Some(hwnd),
                Some(HMENU(id as *mut _)),
                None,
                None,
            )
            .unwrap_or_default();
            if !f.is_invalid() {
                SendMessageW(hw, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
            }
            hw
        };

        let combo_style = WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL;
        let mut controls: Vec<(usize, HWND)> = Vec::new();

        let section = |title: &str, yy: &mut i32| {
            add(w!("STATIC"), title, WINDOW_STYLE(0), 0, margin, px(300), px(20), *yy, head_font);
            *yy += px(26);
        };
        let label = |text: &str, yy: i32| {
            add(w!("STATIC"), text, WINDOW_STYLE(0), 0, label_x, label_w, px(20), yy + px(4), font);
        };

        // ---- shortcut and appearance ----
        section("Shortcut and appearance", &mut y);

        label("Activation shortcut", y);
        let h = add(w!("EDIT"), &s.hotkey,
            WINDOW_STYLE(ES_AUTOHSCROLL as u32) | WS_TABSTOP | WS_BORDER,
            ID_HOTKEY, ctl_x, ctl_w, px(26), y, font);
        controls.push((ID_HOTKEY, h));
        y += row_h;

        label("Theme", y);
        let h = add(w!("COMBOBOX"), "", combo_style, ID_THEME, ctl_x, ctl_w, px(240), y, font);
        for (i, (key, lbl)) in THEMES.iter().enumerate() {
            combo_add(h, lbl);
            if *key == s.theme { combo_select(h, i); }
        }
        controls.push((ID_THEME, h));
        y += row_h + px(10);

        // ---- translation ----
        section("Translation", &mut y);

        label("Beam width", y);
        let h = add(w!("COMBOBOX"), "", combo_style, ID_BEAMS, ctl_x, ctl_w, px(240), y, font);
        for (i, b) in BEAMS.iter().enumerate() {
            combo_add(h, &format!("{b}{}", match *b {
                1 => "  (greedy, fastest)",
                4 => "  (default, best quality)",
                _ => "",
            }));
            if *b == s.beams { combo_select(h, i); }
        }
        controls.push((ID_BEAMS, h));
        y += row_h;

        label("Inference backend", y);
        let h = add(w!("COMBOBOX"), "", combo_style, ID_BACKEND, ctl_x, ctl_w, px(240), y, font);
        for (i, (key, lbl)) in BACKENDS.iter().enumerate() {
            combo_add(h, lbl);
            if *key == s.backend { combo_select(h, i); }
        }
        controls.push((ID_BACKEND, h));
        y += row_h + px(10);

        // ---- OCR ----
        section("Text recognition", &mut y);

        label("Language", y);
        let h = add(w!("COMBOBOX"), "", combo_style, ID_LANG, ctl_x, ctl_w, px(240), y, font);
        let langs = dlg.langs.clone();
        for (i, l) in langs.iter().enumerate() {
            combo_add(h, l);
            if *l == s.lang { combo_select(h, i); }
        }
        controls.push((ID_LANG, h));
        y += row_h;

        label("Upscale before OCR", y);
        let h = add(w!("COMBOBOX"), "", combo_style, ID_UPSCALE, ctl_x, ctl_w, px(240), y, font);
        for (i, u) in UPSCALES.iter().enumerate() {
            combo_add(h, &format!("{u}x{}", if *u == 2 { "  (default)" } else { "" }));
            if *u == s.upscale { combo_select(h, i); }
        }
        controls.push((ID_UPSCALE, h));
        y += row_h;

        label("Page segmentation", y);
        let h = add(w!("COMBOBOX"), "", combo_style, ID_PSM, ctl_x, ctl_w, px(240), y, font);
        for (i, (mode, lbl)) in PSMS.iter().enumerate() {
            combo_add(h, lbl);
            if *mode == s.psm { combo_select(h, i); }
        }
        controls.push((ID_PSM, h));
        y += row_h + px(10);

        // ---- diagnostics ----
        section("Updates and diagnostics", &mut y);

        let h = add(w!("BUTTON"), "Check for updates at start-up",
            WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
            ID_UPDATES, label_x, px(420), px(24), y, font);
        SendMessageW(h, BM_SETCHECK, Some(WPARAM(if s.check_updates { 1 } else { 0 })), None);
        controls.push((ID_UPDATES, h));
        y += px(26);

        let h = add(w!("BUTTON"), "Log every scan and its translations to the console",
            WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
            ID_DEBUG, label_x, px(420), px(24), y, font);
        SendMessageW(h, BM_SETCHECK, Some(WPARAM(if s.debug_log { 1 } else { 0 })), None);
        controls.push((ID_DEBUG, h));
        y += px(34);

        // ---- notes ----
        let mut note = String::from(
            "Backend, beam width, language, upscale and page segmentation rebuild the\ntranslation pipeline when saved, which takes about a second.",
        );
        if !dlg.over.is_empty() {
            note.push_str(&format!(
                "\n\nEnvironment variables are overriding: {}. Those win until unset.",
                dlg.over.0.join(", ")
            ));
        }
        let note_h = if dlg.over.is_empty() { px(34) } else { px(62) };
        add(w!("STATIC"), &note, WINDOW_STYLE(0), 0, margin, px(456), note_h, y, font);
        y += note_h + px(12);

        // ---- button band ----
        // SS_ETCHEDHORZ is the standard themed dialog separator.
        add(w!("STATIC"), "", WINDOW_STYLE(0x10), 0, 0, px(width), px(1), y, font);
        y += px(14);

        let btn_w = px(100);
        let btn_h = px(30);
        let right = px(width) - margin;
        add(w!("BUTTON"), "Restore defaults", WINDOW_STYLE(0) | WS_TABSTOP,
            ID_DEFAULTS, margin, px(130), btn_h, y, font);
        add(w!("BUTTON"), "Save", WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | WS_TABSTOP,
            ID_SAVE, right - btn_w * 2 - px(8), btn_w, btn_h, y, font);
        add(w!("BUTTON"), "Cancel", WINDOW_STYLE(0) | WS_TABSTOP,
            ID_CANCEL, right - btn_w, btn_w, btn_h, y, font);
        y += btn_h + px(18);

        dlg.ctl = controls;

        let mut rect = windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: px(width),
            bottom: y,
        };
        let _ = AdjustWindowRectEx(
            &mut rect,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            false,
            WS_EX_DLGMODALFRAME,
        );
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOMOVE | SWP_NOZORDER,
        );
        center_on_cursor_monitor(hwnd);
        Ok(())
    }
}

pub fn center_on_cursor_monitor(hwnd: HWND) {
    unsafe {
        use windows::Win32::Foundation::RECT;
        let mut me = RECT::default();
        if GetWindowRect(hwnd, &mut me).is_err() {
            return;
        }
        let (w, h) = (me.right - me.left, me.bottom - me.top);
        // Centre on the monitor holding the cursor: the owner is a hidden
        // message window and has no useful position.
        let mut pt = windows::Win32::Foundation::POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = windows::Win32::Graphics::Gdi::MonitorFromPoint(
            pt,
            windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST,
        );
        let mut info = windows::Win32::Graphics::Gdi::MONITORINFO {
            cbSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32,
            ..Default::default()
        };
        if windows::Win32::Graphics::Gdi::GetMonitorInfoW(mon, &mut info).as_bool() {
            let r = info.rcWork;
            let x = r.left + ((r.right - r.left) - w) / 2;
            let y = r.top + ((r.bottom - r.top) - h) / 2;
            let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
        }
    }
}

fn combo_add(h: HWND, text: &str) {
    unsafe {
        let s = HSTRING::from(text);
        SendMessageW(h, CB_ADDSTRING, None, Some(LPARAM(s.as_ptr() as isize)));
    }
}

fn combo_select(h: HWND, index: usize) {
    unsafe {
        SendMessageW(h, CB_SETCURSEL, Some(WPARAM(index)), None);
    }
}

fn combo_index(h: HWND) -> usize {
    unsafe { SendMessageW(h, CB_GETCURSEL, None, None).0.max(0) as usize }
}

fn edit_text(h: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(h);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

fn collect(dlg: &Dlg) -> Settings {
    let mut s = Settings::default();
    s.hotkey = edit_text(dlg.get(ID_HOTKEY)).trim().to_string();
    s.theme = THEMES[combo_index(dlg.get(ID_THEME)).min(THEMES.len() - 1)].0.into();
    s.beams = BEAMS[combo_index(dlg.get(ID_BEAMS)).min(BEAMS.len() - 1)];
    s.backend = BACKENDS[combo_index(dlg.get(ID_BACKEND)).min(BACKENDS.len() - 1)].0.into();
    s.lang = dlg
        .langs
        .get(combo_index(dlg.get(ID_LANG)))
        .cloned()
        .unwrap_or_else(|| "heb".into());
    s.upscale = UPSCALES[combo_index(dlg.get(ID_UPSCALE)).min(UPSCALES.len() - 1)];
    s.psm = PSMS[combo_index(dlg.get(ID_PSM)).min(PSMS.len() - 1)].0;
    s.debug_log =
        unsafe { SendMessageW(dlg.get(ID_DEBUG), BM_GETCHECK, None, None).0 == 1 };
    s.check_updates =
        unsafe { SendMessageW(dlg.get(ID_UPDATES), BM_GETCHECK, None, None).0 == 1 };
    s
}

fn apply_to_controls(dlg: &Dlg, s: &Settings) {
    unsafe {
        let _ = SetWindowTextW(dlg.get(ID_HOTKEY), &HSTRING::from(s.hotkey.as_str()));
        combo_select(dlg.get(ID_THEME), THEMES.iter().position(|(k, _)| *k == s.theme).unwrap_or(0));
        combo_select(dlg.get(ID_BEAMS), BEAMS.iter().position(|b| *b == s.beams).unwrap_or(2));
        combo_select(dlg.get(ID_BACKEND), BACKENDS.iter().position(|(k, _)| *k == s.backend).unwrap_or(0));
        combo_select(dlg.get(ID_LANG), dlg.langs.iter().position(|l| *l == s.lang).unwrap_or(0));
        combo_select(dlg.get(ID_UPSCALE), UPSCALES.iter().position(|u| *u == s.upscale).unwrap_or(1));
        combo_select(dlg.get(ID_PSM), PSMS.iter().position(|(m, _)| *m == s.psm).unwrap_or(2));
        SendMessageW(dlg.get(ID_DEBUG), BM_SETCHECK, Some(WPARAM(if s.debug_log { 1 } else { 0 })), None);
        SendMessageW(dlg.get(ID_UPDATES), BM_SETCHECK, Some(WPARAM(if s.check_updates { 1 } else { 0 })), None);
    }
}

extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = (wp.0 & 0xffff) as usize;
                let Some(dlg) = dlg_from(hwnd) else { return LRESULT(0) };
                match id {
                    ID_SAVE => {
                        let s = collect(dlg);
                        if let Err(e) = hotkey::parse(&s.hotkey) {
                            let text = HSTRING::from(format!("{e}"));
                            MessageBoxW(Some(hwnd), &text, w!("Invalid shortcut"), MB_ICONWARNING | MB_OK);
                            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(dlg.get(ID_HOTKEY)));
                            return LRESULT(0);
                        }
                        if let Err(e) = s.save() {
                            let text = HSTRING::from(format!("{e:#}"));
                            MessageBoxW(Some(hwnd), &text, w!("Could not save settings"), MB_ICONERROR | MB_OK);
                            return LRESULT(0);
                        }
                        let _ = PostMessageW(Some(dlg.owner), WM_SETTINGS_SAVED, WPARAM(0), LPARAM(0));
                        let _ = DestroyWindow(hwnd);
                    }
                    ID_CANCEL => {
                        let _ = DestroyWindow(hwnd);
                    }
                    ID_DEFAULTS => apply_to_controls(dlg, &Settings::default()),
                    _ => {}
                }
                LRESULT(0)
            }
            // Without this the labels inherit whatever DefWindowProc hands
            // back, which is not the dialog face colour and shows as stray
            // colours over the window background.
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                use windows::Win32::Graphics::Gdi::{
                    GetSysColor, GetSysColorBrush, SetBkMode, SetTextColor, COLOR_BTNFACE,
                    COLOR_BTNTEXT, HDC, TRANSPARENT,
                };
                let dc = HDC(wp.0 as *mut _);
                SetTextColor(dc, windows::Win32::Foundation::COLORREF(GetSysColor(COLOR_BTNTEXT)));
                SetBkMode(dc, TRANSPARENT);
                LRESULT(GetSysColorBrush(COLOR_BTNFACE).0 as isize)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if ptr != 0 {
                    let dlg = Box::from_raw(ptr as *mut Dlg);
                    if !dlg.font.is_invalid() {
                        let _ = DeleteObject(dlg.font.into());
                    }
                    if !dlg.head_font.is_invalid() {
                        let _ = DeleteObject(dlg.head_font.into());
                    }
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Opens the settings window on its own, without the tray app running.
///
/// Useful when a saved shortcut turns out to be taken by something else and the
/// app will not start, and as a way to check the form in isolation.
pub fn run_standalone(vendor: &Path) -> Result<()> {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let (settings, over) = Settings::load();
    let hwnd = open(HWND::default(), &settings, &over, &vendor.join("tessdata"))?;

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            // The window is not a real dialog, so tab and Enter need help.
            if IsDialogMessageW(hwnd, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if !IsWindow(Some(hwnd)).as_bool() {
                break;
            }
        }
    }
    Ok(())
}
