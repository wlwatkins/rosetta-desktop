//! The About window, opened by clicking the version in the tray menu.

use anyhow::Result;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, DeleteObject, HFONT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::update;

const CLASS: PCWSTR = w!("RosettaAboutWindow");
const ID_CLOSE: usize = 2;
const ID_UPDATES: usize = 1101;
const ID_REPO: usize = 1102;

/// Posted to the owner when the About window's update button is pressed.
pub const WM_ABOUT_CHECK_UPDATES: u32 = WM_APP + 4;

struct About {
    owner: HWND,
    font: HFONT,
    title_font: HFONT,
}

pub fn open(owner: HWND) -> Result<HWND> {
    unsafe {
        if let Ok(existing) = FindWindowExW(None, None, CLASS, None) {
            if !existing.is_invalid() {
                let _ = ShowWindow(existing, SW_RESTORE);
                let _ = SetForegroundWindow(existing);
                return Ok(existing);
            }
        }

        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(16isize as *mut _),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            CLASS,
            w!("About Rosetta"),
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

        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            Box::into_raw(Box::new(About {
                owner,
                font: HFONT::default(),
                title_font: HFONT::default(),
            })) as isize,
        );

        build(hwnd)?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        Ok(hwnd)
    }
}

unsafe fn about_from(hwnd: HWND) -> Option<&'static mut About> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if ptr == 0 {
        None
    } else {
        Some(unsafe { &mut *(ptr as *mut About) })
    }
}

fn build(hwnd: HWND) -> Result<()> {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        let px = |v: i32| v * dpi as i32 / 96;

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
        let (font, title_font) = if got {
            let body = CreateFontIndirectW(&ncm.lfMessageFont);
            let mut big = ncm.lfMessageFont;
            big.lfHeight = (big.lfHeight as f32 * 1.7) as i32;
            big.lfWeight = 600;
            (body, CreateFontIndirectW(&big))
        } else {
            (HFONT::default(), HFONT::default())
        };

        let Some(about) = about_from(hwnd) else { return Ok(()) };
        about.font = font;
        about.title_font = title_font;

        let add = |class: PCWSTR, text: &str, style: WINDOW_STYLE, id: usize,
                   x: i32, y: i32, w: i32, h: i32, f: HFONT| {
            let ctl = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                &HSTRING::from(text),
                WS_CHILD | WS_VISIBLE | style,
                x,
                y,
                w,
                h,
                Some(hwnd),
                Some(HMENU(id as *mut _)),
                None,
                None,
            )
            .unwrap_or_default();
            if !f.is_invalid() {
                SendMessageW(ctl, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
            }
        };

        let width = 440;
        let margin = px(26);
        let mut y = px(24);

        add(w!("STATIC"), "Rosetta", WINDOW_STYLE(0), 0, margin, y, px(300), px(40), title_font);
        y += px(42);

        add(
            w!("STATIC"),
            &format!("Version {}", update::current_version()),
            WINDOW_STYLE(0),
            0,
            margin,
            y,
            px(300),
            px(20),
            font,
        );
        y += px(28);

        add(
            w!("STATIC"),
            "Reads Hebrew anywhere on screen and replaces it with English, in\nplace. Screen capture, OCR and translation all run on this machine.",
            WINDOW_STYLE(0),
            0,
            margin,
            y,
            px(width - 52),
            px(40),
            font,
        );
        y += px(52);

        add(w!("STATIC"), "", WINDOW_STYLE(0x10), 0, 0, y, px(width), px(1), font);
        y += px(16);

        add(
            w!("STATIC"),
            "Free software under the GPL-3.0. Bundles the Helsinki-NLP\nopus-mt-tc-big-he-en model (CC-BY-4.0) and Tesseract with\ntessdata_best (Apache-2.0).",
            WINDOW_STYLE(0),
            0,
            margin,
            y,
            px(width - 52),
            px(58),
            font,
        );
        y += px(66);

        let btn_h = px(30);
        add(w!("BUTTON"), "Check for updates", WINDOW_STYLE(0) | WS_TABSTOP, ID_UPDATES, margin, y, px(140), btn_h, font);
        add(w!("BUTTON"), "Project page", WINDOW_STYLE(0) | WS_TABSTOP, ID_REPO, margin + px(148), y, px(110), btn_h, font);
        add(
            w!("BUTTON"),
            "Close",
            WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | WS_TABSTOP,
            ID_CLOSE,
            px(width) - margin - px(96),
            y,
            px(96),
            btn_h,
            font,
        );
        y += btn_h + px(22);

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
        super::settings_dlg::center_on_cursor_monitor(hwnd);
        Ok(())
    }
}

extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = (wp.0 & 0xffff) as usize;
                let Some(about) = about_from(hwnd) else { return LRESULT(0) };
                match id {
                    ID_CLOSE => {
                        let _ = DestroyWindow(hwnd);
                    }
                    ID_UPDATES => {
                        let _ = PostMessageW(
                            Some(about.owner),
                            WM_ABOUT_CHECK_UPDATES,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                    ID_REPO => {
                        let url = HSTRING::from(format!("https://github.com/{}", update::repo()));
                        ShellExecuteW(
                            Some(hwnd),
                            w!("open"),
                            &url,
                            PCWSTR::null(),
                            PCWSTR::null(),
                            SW_SHOWNORMAL,
                        );
                    }
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
                    let about = Box::from_raw(ptr as *mut About);
                    for f in [about.font, about.title_font] {
                        if !f.is_invalid() {
                            let _ = DeleteObject(f.into());
                        }
                    }
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Opens the About window on its own, outside the tray app.
pub fn run_standalone() -> Result<()> {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let hwnd = open(HWND::default())?;
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
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
