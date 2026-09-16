//! Tray app, selection overlay, and live translation rendering.
//!
//! Two windows: a hidden message window owning the tray icon and hotkeys, and a
//! layered overlay that is fullscreen while you drag a region and then shrinks to
//! sit on the region itself. The overlay is marked `WDA_EXCLUDEFROMCAPTURE`, so
//! the worker re-capturing the same rectangle sees the original text rather than
//! the translations we just painted over it.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, DeleteObject, GetDC, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetCapture, UnregisterHotKey, MOD_NOREPEAT, VK_ESCAPE,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::hotkey;
use super::render::{Renderer, Rgba};
use super::settings_dlg::{self, WM_SETTINGS_SAVED};
use crate::capture::{enable_dpi_awareness, exclude_from_capture, monitors, virtual_screen};
use crate::clipboard;
use crate::geom::Rect;
use crate::settings::Settings;
use crate::worker::{ScanResult, Shared, Status, Worker};

const WM_TRAY: u32 = WM_APP + 1;
const WM_RESULT: u32 = WM_APP + 2;

const HOTKEY_ACTIVATE: i32 = 1;
const HOTKEY_CANCEL: i32 = 2;

const TIMER_LIVE: usize = 1;
const TIMER_SPIN: usize = 2;
/// Spinner refresh; fast enough to read as motion without redrawing wastefully.
const SPIN_TICK_MS: u32 = 45;
const LIVE_TICK_MS: u32 = 90;
/// How often to re-scan an unmoved region, to pick up content changes.
const IDLE_RESCAN: Duration = Duration::from_millis(450);

const MENU_SELECT: usize = 100;
const MENU_EXIT: usize = 101;
const MENU_SETTINGS: usize = 102;

/// Slack around the region for the frame, handles, shadows and the toolbar
/// that hangs below the selection. Too small and the toolbar gets clipped by
/// the window it is drawn into.
const PAD: i32 = 56;

// Toolbar metrics. The status pill and the toolbar sit side by side under the
// selection, so they share one height and the buttons are centred within it.
const PILL_H: i32 = 26;
const BTN_H: i32 = 20;
const BTN_COPY_W: i32 = 54;
const BTN_ICON_W: i32 = 28;
const BTN_GAP: i32 = 3;
const TB_PAD: i32 = 6;
const TB_DROP: i32 = 10;
/// Time a confirmation message replaces the status readout.
const TOAST: Duration = Duration::from_millis(1400);
const EDGE_GRAB: i32 = 10;
const MIN_REGION: i32 = 24;

/// Glassmorphism, with one hard constraint: the translation chip sits directly
/// on top of the Hebrew it replaces, so it must stay opaque. The chrome around
/// it (status pill, hint, dimensions badge) is the part that gets to be frosted.
#[derive(Clone, Copy)]
struct Theme {
    dim: Rgba,
    accent: Rgba,
    accent_soft: Rgba,
    chip_bg: Rgba,
    chip_border: Rgba,
    chip_fg: Rgba,
    glass_bg: Rgba,
    glass_border: Rgba,
    glass_fg: Rgba,
    handle_fill: Rgba,
    shadow: f32,
}

const DARK: Theme = Theme {
    dim: Rgba::hex(0x05060A, 0.50),
    accent: Rgba::hex(0x5B9DFF, 1.0),
    accent_soft: Rgba::hex(0x5B9DFF, 0.30),
    chip_bg: Rgba::hex(0x121521, 0.98),
    chip_border: Rgba::hex(0xFFFFFF, 0.10),
    chip_fg: Rgba::hex(0xF2F5FC, 1.0),
    glass_bg: Rgba::hex(0x121521, 0.72),
    glass_border: Rgba::hex(0xFFFFFF, 0.14),
    glass_fg: Rgba::hex(0xD8DEEC, 1.0),
    handle_fill: Rgba::hex(0xFFFFFF, 1.0),
    shadow: 0.34,
};

const LIGHT: Theme = Theme {
    dim: Rgba::hex(0x0A0C12, 0.34),
    accent: Rgba::hex(0x2F6FE4, 1.0),
    accent_soft: Rgba::hex(0x2F6FE4, 0.26),
    chip_bg: Rgba::hex(0xFBFCFE, 0.99),
    chip_border: Rgba::hex(0x0A0C12, 0.12),
    chip_fg: Rgba::hex(0x121521, 1.0),
    glass_bg: Rgba::hex(0xF7F9FC, 0.78),
    glass_border: Rgba::hex(0x0A0C12, 0.10),
    glass_fg: Rgba::hex(0x2A3040, 1.0),
    handle_fill: Rgba::hex(0xFFFFFF, 1.0),
    shadow: 0.20,
};

fn theme_for(name: &str) -> Theme {
    if name == "light" {
        LIGHT
    } else {
        DARK
    }
}

const RADIUS: f32 = 7.0;
const CHIP_RADIUS: f32 = 5.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Hidden,
    Selecting,
    Live,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Handle {
    None,
    Move,
    N,
    S,
    E,
    W,
    Ne,
    Nw,
    Se,
    Sw,
}

struct Drag {
    handle: Handle,
    start_mouse: POINT,
    start_region: Rect,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Button {
    CopySource,
    CopyTranslation,
    Refresh,
    Close,
}

/// Toolbar geometry, in whatever space `sel` was given in.
struct Toolbar {
    pill: Rect,
    copy_source: Rect,
    copy_translation: Rect,
    refresh: Rect,
    close: Rect,
}

impl Toolbar {
    fn layout(sel: Rect) -> Self {
        let w = TB_PAD * 2 + BTN_COPY_W * 2 + BTN_ICON_W * 2 + BTN_GAP * 3;
        let h = PILL_H;
        // Right-aligned to the selection, but never pushed off its left edge.
        let x = (sel.right() - w).max(sel.x);
        let y = sel.bottom() + TB_DROP;
        let (bx, by) = (x + TB_PAD, y + (PILL_H - BTN_H) / 2);
        let step = |i: i32| bx + i;
        Self {
            pill: Rect::new(x, y, w, h),
            copy_source: Rect::new(step(0), by, BTN_COPY_W, BTN_H),
            copy_translation: Rect::new(step(BTN_COPY_W + BTN_GAP), by, BTN_COPY_W, BTN_H),
            refresh: Rect::new(step((BTN_COPY_W + BTN_GAP) * 2), by, BTN_ICON_W, BTN_H),
            close: Rect::new(
                step((BTN_COPY_W + BTN_GAP) * 2 + BTN_ICON_W + BTN_GAP),
                by,
                BTN_ICON_W,
                BTN_H,
            ),
        }
    }

    fn hit(&self, px: i32, py: i32) -> Option<Button> {
        for (r, b) in [
            (self.copy_source, Button::CopySource),
            (self.copy_translation, Button::CopyTranslation),
            (self.refresh, Button::Refresh),
            (self.close, Button::Close),
        ] {
            if r.contains(px, py) {
                return Some(b);
            }
        }
        None
    }

    fn rect_of(&self, b: Button) -> Rect {
        match b {
            Button::CopySource => self.copy_source,
            Button::CopyTranslation => self.copy_translation,
            Button::Refresh => self.refresh,
            Button::Close => self.close,
        }
    }
}

pub struct App {
    app_hwnd: HWND,
    overlay: HWND,
    renderer: Renderer,
    worker: Worker,
    shared: Shared,

    mode: Mode,
    region: Rect,
    anchor: POINT,
    drag: Option<Drag>,

    generation: u64,
    last_requested: Rect,
    last_request_at: Option<Instant>,
    result: Option<ScanResult>,
    hover: Handle,
    tray_added: bool,
    capturing: bool,
    theme: Theme,
    hotkey_label: String,
    hover_btn: Option<Button>,
    pressed_btn: Option<Button>,
    toast: Option<(String, Instant)>,
    spin_phase: u32,
    spinning: bool,
    /// Per-monitor bounds, refreshed each time the picker opens.
    monitors: Vec<Rect>,
    settings: Settings,
    models_dir: PathBuf,
    vendor_dir: PathBuf,
}

/// Name shared with the installer's `AppMutex`, so a setup run can tell that
/// the app is still open and ask for it to be closed before overwriting it.
pub const SINGLE_INSTANCE_MUTEX: &str = "RosettaDesktopSingleInstance";

/// Holds the single-instance mutex for the lifetime of the process.
struct InstanceLock(windows::Win32::Foundation::HANDLE);

impl Drop for InstanceLock {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// `Ok(None)` when another copy already owns the mutex.
fn acquire_single_instance() -> Result<Option<InstanceLock>> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    unsafe {
        let name = HSTRING::from(SINGLE_INSTANCE_MUTEX);
        let handle = CreateMutexW(None, false, &name)?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
            return Ok(None);
        }
        Ok(Some(InstanceLock(handle)))
    }
}

pub fn run(models: PathBuf, vendor: PathBuf, settings: Settings) -> Result<()> {
    // Without this a second launch dies on RegisterHotKey with a confusing
    // "already taken" error, and an installer has no way to know we are open.
    let Some(_instance) = acquire_single_instance()? else {
        println!("Rosetta is already running - look for it in the notification area.");
        return Ok(());
    };

    enable_dpi_awareness();
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
    }

    let instance = unsafe { GetModuleHandleW(None)? };

    // Hidden owner window: tray icon, hotkeys, worker notifications.
    let app_class = w!("RosettaDesktopApp");
    let overlay_class = w!("RosettaDesktopOverlay");
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: app_class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            bail!("RegisterClassW(app) failed");
        }
        let wc2 = WNDCLASSW {
            lpfnWndProc: Some(overlay_proc),
            hInstance: instance.into(),
            lpszClassName: overlay_class,
            hCursor: LoadCursorW(None, IDC_CROSS)?,
            ..Default::default()
        };
        if RegisterClassW(&wc2) == 0 {
            bail!("RegisterClassW(overlay) failed");
        }
    }

    let app_hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            app_class,
            w!("Rosetta"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )?
    };

    let vs = virtual_screen();
    let overlay = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            overlay_class,
            w!("Rosetta Overlay"),
            WS_POPUP,
            vs.x,
            vs.y,
            vs.w,
            vs.h,
            None,
            None,
            Some(instance.into()),
            None,
        )?
    };
    // Keep our own painting out of every capture path, including our own.
    // ROSETTA_NO_EXCLUDE leaves the overlay visible to screenshots, which is the
    // only way to inspect what it actually drew.
    if std::env::var("ROSETTA_NO_EXCLUDE").is_err() {
        if let Err(e) = exclude_from_capture(overlay) {
            eprintln!("[warn] SetWindowDisplayAffinity failed ({e}); live mode may feed on its own output");
        }
    } else {
        eprintln!("[debug] overlay is VISIBLE to capture; live mode will feed on its own output");
    }

    let worker = Worker::spawn(
        app_hwnd.0 as isize,
        WM_RESULT,
        models.clone(),
        vendor.clone(),
        settings.translate_config(),
        settings.lang.clone(),
        settings.upscale,
        settings.psm,
    );
    let shared = worker.shared.clone();

    // Configurable so the default never has to fight an app the user cares
    // about; Ctrl+Shift+T in particular is "reopen closed tab" in most browsers.
    let key = hotkey::parse(&settings.hotkey)
        .with_context(|| format!("hotkey {:?}", settings.hotkey))?;

    let mut app = Box::new(App {
        app_hwnd,
        overlay,
        renderer: Renderer::new()?,
        worker,
        shared,
        mode: Mode::Hidden,
        region: Rect::default(),
        anchor: POINT::default(),
        drag: None,
        generation: 0,
        last_requested: Rect::default(),
        last_request_at: None,
        result: None,
        hover: Handle::None,
        tray_added: false,
        capturing: false,
        theme: theme_for(&settings.theme),
        hotkey_label: key.label.clone(),
        hover_btn: None,
        pressed_btn: None,
        toast: None,
        spin_phase: 0,
        spinning: false,
        monitors: monitors(),
        settings: settings.clone(),
        models_dir: models,
        vendor_dir: vendor,
    });

    unsafe {
        let ptr = app.as_mut() as *mut App as isize;
        SetWindowLongPtrW(app_hwnd, GWLP_USERDATA, ptr);
        SetWindowLongPtrW(overlay, GWLP_USERDATA, ptr);

        RegisterHotKey(Some(app_hwnd), HOTKEY_ACTIVATE, key.mods, key.vk).with_context(|| {
            format!(
                "registering {}: another app already owns it. \
                 Pick a different one with ROSETTA_HOTKEY, e.g. ROSETTA_HOTKEY=ctrl+shift+f9",
                key.label
            )
        })?;
    }

    app.add_tray(app_hwnd)?;
    println!(
        "Rosetta is running in the tray. Press {} to select a region, Esc to dismiss.",
        key.label
    );

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // Teardown happens in `App::drop`, which also covers the error paths above.
    Ok(())
}

impl Drop for App {
    /// `run` can bail after the worker exists -- a taken hotkey is the common
    /// case -- and leaving the thread alive means the process exits while the
    /// worker still owns Tesseract, which then reports its dictionaries as
    /// leaked. Tearing down here covers every exit path.
    fn drop(&mut self) {
        self.remove_tray(self.app_hwnd);
        unsafe {
            let _ = UnregisterHotKey(Some(self.app_hwnd), HOTKEY_ACTIVATE);
            let _ = UnregisterHotKey(Some(self.app_hwnd), HOTKEY_CANCEL);
        }
        self.worker.quit();
    }
}

unsafe fn app_from(hwnd: HWND) -> Option<&'static mut App> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if ptr == 0 {
        None
    } else {
        Some(unsafe { &mut *(ptr as *mut App) })
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_HOTKEY => {
                if let Some(app) = app_from(hwnd) {
                    match wp.0 as i32 {
                        HOTKEY_ACTIVATE => app.begin_selection(hwnd),
                        HOTKEY_CANCEL => app.dismiss(hwnd),
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_RESULT => {
                if let Some(app) = app_from(hwnd) {
                    app.take_result();
                }
                LRESULT(0)
            }
            WM_SETTINGS_SAVED => {
                if let Some(app) = app_from(hwnd) {
                    app.reload_settings();
                }
                LRESULT(0)
            }
            WM_TIMER => {
                if let Some(app) = app_from(hwnd) {
                    match wp.0 {
                        TIMER_LIVE => app.tick(),
                        TIMER_SPIN => app.spin(),
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_TRAY => {
                let event = (lp.0 & 0xffff) as u32;
                if let Some(app) = app_from(hwnd) {
                    match event {
                        // Double-click, not single: a stray click on the tray
                        // should not dim the screen.
                        WM_LBUTTONDBLCLK => app.begin_selection(hwnd),
                        WM_RBUTTONUP => app.show_menu(hwnd),
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                if let Some(app) = app_from(hwnd) {
                    match (wp.0 & 0xffff) as usize {
                        MENU_SELECT => app.begin_selection(hwnd),
                        MENU_SETTINGS => app.open_settings(),
                        MENU_EXIT => {
                            app.dismiss(hwnd);
                            PostQuitMessage(0);
                        }
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let Some(app) = app_from(hwnd) else {
            return DefWindowProcW(hwnd, msg, wp, lp);
        };
        match msg {
            WM_LBUTTONDOWN => {
                app.on_mouse_down(lp);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                app.on_mouse_move(lp);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                app.on_mouse_up();
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                app.dismiss(hwnd);
                LRESULT(0)
            }
            WM_SETCURSOR => {
                if app.apply_cursor() {
                    LRESULT(1)
                } else {
                    DefWindowProcW(hwnd, msg, wp, lp)
                }
            }
            // Never take focus from whatever the user is reading.
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

impl App {
    // ---------- tray ----------

    fn tray_data(&self, hwnd: HWND) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            ..Default::default()
        };
        nid.hIcon = make_icon().unwrap_or_default();
        let tip = format!("Rosetta - Hebrew to English ({})", self.hotkey_label);
        for (i, c) in tip.encode_utf16().enumerate().take(nid.szTip.len() - 1) {
            nid.szTip[i] = c;
        }
        nid
    }

    fn add_tray(&mut self, hwnd: HWND) -> Result<()> {
        let nid = self.tray_data(hwnd);
        unsafe { Shell_NotifyIconW(NIM_ADD, &nid).ok().context("Shell_NotifyIcon(NIM_ADD)")? };
        self.tray_added = true;
        Ok(())
    }

    fn remove_tray(&mut self, hwnd: HWND) {
        if self.tray_added {
            let nid = self.tray_data(hwnd);
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            }
            self.tray_added = false;
        }
    }

    fn show_menu(&mut self, hwnd: HWND) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let label: Vec<u16> = format!("Select region\t{}", self.hotkey_label)
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let _ = AppendMenuW(menu, MF_STRING, MENU_SELECT, PCWSTR(label.as_ptr()));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Settings..."));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit"));

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            // Required so the menu dismisses when the user clicks elsewhere.
            let _ = SetForegroundWindow(hwnd);
            let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None);
            let _ = DestroyMenu(menu);
        }
    }

    fn open_settings(&mut self) {
        let (current, over) = Settings::load();
        let tessdata = self.vendor_dir.join("tessdata");
        if let Err(e) = settings_dlg::open(self.app_hwnd, &current, &over, &tessdata) {
            eprintln!("[warn] could not open settings: {e:#}");
        }
    }

    /// Re-read the file the settings window just wrote and apply what changed.
    fn reload_settings(&mut self) {
        let (next, _) = Settings::load();
        let previous = std::mem::replace(&mut self.settings, next.clone());

        self.theme = theme_for(&next.theme);

        // The worker reads this per scan, so toggling it needs no restart.
        if next.debug_log {
            std::env::set_var("ROSETTA_DEBUG", "1");
        } else {
            std::env::remove_var("ROSETTA_DEBUG");
        }

        if next.hotkey != previous.hotkey {
            match hotkey::parse(&next.hotkey) {
                Ok(key) => unsafe {
                    let _ = UnregisterHotKey(Some(self.app_hwnd), HOTKEY_ACTIVATE);
                    if RegisterHotKey(Some(self.app_hwnd), HOTKEY_ACTIVATE, key.mods, key.vk).is_ok() {
                        self.hotkey_label = key.label;
                    } else {
                        eprintln!("[warn] {} is taken; keeping the previous shortcut", key.label);
                        if let Ok(old) = hotkey::parse(&previous.hotkey) {
                            let _ = RegisterHotKey(Some(self.app_hwnd), HOTKEY_ACTIVATE, old.mods, old.vk);
                        }
                    }
                },
                Err(e) => eprintln!("[warn] {e:#}"),
            }
            self.remove_tray(self.app_hwnd);
            let _ = self.add_tray(self.app_hwnd);
        }

        // Model and OCR settings live inside the worker, so they need it rebuilt.
        if next.pipeline_differs(&previous) {
            self.worker.quit();
            self.worker = Worker::spawn(
                self.app_hwnd.0 as isize,
                WM_RESULT,
                self.models_dir.clone(),
                self.vendor_dir.clone(),
                next.translate_config(),
                next.lang.clone(),
                next.upscale,
                next.psm,
            );
            self.shared = self.worker.shared.clone();
            self.result = None;
            self.last_requested = Rect::default();
            self.last_request_at = None;
            if self.mode == Mode::Live {
                self.request_scan(true, true);
            }
        }

        let _ = self.redraw();
    }

    // ---------- mode transitions ----------

    fn begin_selection(&mut self, app_hwnd: HWND) {
        // Displays can be plugged in or rearranged between activations.
        self.monitors = monitors();
        self.mode = Mode::Selecting;
        self.region = Rect::default();
        self.drag = None;
        self.result = None;
        self.last_requested = Rect::default();
        self.last_request_at = None;

        unsafe {
            let _ = RegisterHotKey(Some(app_hwnd), HOTKEY_CANCEL, MOD_NOREPEAT, VK_ESCAPE.0 as u32);
            let _ = ShowWindow(self.overlay, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(
                self.overlay,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            let _ = SetTimer(Some(app_hwnd), TIMER_LIVE, LIVE_TICK_MS, None);
        }
        let _ = self.redraw();
    }

    fn dismiss(&mut self, _hwnd: HWND) {
        self.mode = Mode::Hidden;
        self.drag = None;
        self.result = None;
        self.hover_btn = None;
        self.pressed_btn = None;
        self.toast = None;
        unsafe {
            let _ = ShowWindow(self.overlay, SW_HIDE);
            // Always the owner window: it is the one holding the hotkey and timers.
            let _ = UnregisterHotKey(Some(self.app_hwnd), HOTKEY_CANCEL);
            let _ = KillTimer(Some(self.app_hwnd), TIMER_LIVE);
            let _ = KillTimer(Some(self.app_hwnd), TIMER_SPIN);
        }
        self.spinning = false;
    }

    // ---------- input ----------

    fn window_origin(&self) -> (i32, i32) {
        match self.mode {
            Mode::Selecting => {
                let vs = virtual_screen();
                (vs.x, vs.y)
            }
            _ => (self.region.x - PAD, self.region.y - PAD),
        }
    }

    /// Convert a window-local point from lParam into virtual-screen coordinates.
    fn to_screen(&self, lp: LPARAM) -> POINT {
        let x = (lp.0 & 0xffff) as i16 as i32;
        let y = ((lp.0 >> 16) & 0xffff) as i16 as i32;
        let (ox, oy) = self.window_origin();
        POINT { x: x + ox, y: y + oy }
    }

    fn on_mouse_down(&mut self, lp: LPARAM) {
        let pt = self.to_screen(lp);
        match self.mode {
            Mode::Selecting => {
                self.anchor = pt;
                self.region = Rect::new(pt.x, pt.y, 0, 0);
                self.capturing = true;
                unsafe { SetCapture(self.overlay) };
            }
            Mode::Live => {
                // The toolbar sits inside the resize grab margin, so it has to
                // win the hit test before the edge handles do.
                if let Some(b) = Toolbar::layout(self.region).hit(pt.x, pt.y) {
                    self.pressed_btn = Some(b);
                    let _ = self.redraw();
                    return;
                }
                let handle = self.hit_test(pt);
                if handle != Handle::None {
                    self.drag =
                        Some(Drag { handle, start_mouse: pt, start_region: self.region });
                    self.capturing = true;
                    unsafe { SetCapture(self.overlay) };
                }
            }
            Mode::Hidden => {}
        }
    }

    fn on_mouse_move(&mut self, lp: LPARAM) {
        let pt = self.to_screen(lp);
        match self.mode {
            Mode::Selecting => {
                if self.capturing {
                    self.region = Rect::from_points(self.anchor.x, self.anchor.y, pt.x, pt.y);
                    let _ = self.redraw();
                }
            }
            Mode::Live => {
                if let Some(d) = &self.drag {
                    let dx = pt.x - d.start_mouse.x;
                    let dy = pt.y - d.start_mouse.y;
                    self.region = resize(d.start_region, d.handle, dx, dy);
                    let _ = self.redraw();
                } else {
                    let over = Toolbar::layout(self.region).hit(pt.x, pt.y);
                    let h = if over.is_some() { Handle::None } else { self.hit_test(pt) };
                    if over != self.hover_btn || h != self.hover {
                        self.hover_btn = over;
                        self.hover = h;
                        let _ = self.redraw();
                    }
                }
            }
            Mode::Hidden => {}
        }
    }

    fn on_mouse_up(&mut self) {
        // A press that started on a button fires only if it ends there too.
        if let Some(pressed) = self.pressed_btn.take() {
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            if Toolbar::layout(self.region).hit(pt.x, pt.y) == Some(pressed) {
                self.activate(pressed);
            } else {
                let _ = self.redraw();
            }
            return;
        }
        if !self.capturing {
            return;
        }
        self.capturing = false;
        unsafe {
            let _ = ReleaseCapture();
        }
        match self.mode {
            Mode::Selecting => {
                if self.region.w < MIN_REGION || self.region.h < MIN_REGION {
                    // Treat a stray click as "keep picking" rather than a 2px region.
                    self.region = Rect::default();
                    let _ = self.redraw();
                    return;
                }
                self.mode = Mode::Live;
                self.request_scan(true, false);
                let _ = self.redraw();
            }
            Mode::Live => {
                self.drag = None;
                self.request_scan(true, false);
                let _ = self.redraw();
            }
            Mode::Hidden => {}
        }
    }

    fn apply_cursor(&self) -> bool {
        let cursor = match self.mode {
            Mode::Selecting => IDC_CROSS,
            Mode::Live if self.hover_btn.is_some() => IDC_HAND,
            Mode::Live => match self.hover {
                Handle::None => IDC_ARROW,
                Handle::Move => IDC_SIZEALL,
                Handle::N | Handle::S => IDC_SIZENS,
                Handle::E | Handle::W => IDC_SIZEWE,
                Handle::Ne | Handle::Sw => IDC_SIZENESW,
                Handle::Nw | Handle::Se => IDC_SIZENWSE,
            },
            Mode::Hidden => return false,
        };
        unsafe {
            if let Ok(c) = LoadCursorW(None, cursor) {
                SetCursor(Some(c));
                return true;
            }
        }
        false
    }

    fn hit_test(&self, pt: POINT) -> Handle {
        let r = self.region;
        let near = |a: i32, b: i32| (a - b).abs() <= EDGE_GRAB;
        let within_x = pt.x >= r.x - EDGE_GRAB && pt.x <= r.right() + EDGE_GRAB;
        let within_y = pt.y >= r.y - EDGE_GRAB && pt.y <= r.bottom() + EDGE_GRAB;
        if !within_x || !within_y {
            return Handle::None;
        }
        let (l, t, rt, b) = (near(pt.x, r.x), near(pt.y, r.y), near(pt.x, r.right()), near(pt.y, r.bottom()));
        match (l, t, rt, b) {
            (true, true, _, _) => Handle::Nw,
            (_, true, true, _) => Handle::Ne,
            (true, _, _, true) => Handle::Sw,
            (_, _, true, true) => Handle::Se,
            (true, ..) => Handle::W,
            (_, true, ..) => Handle::N,
            (_, _, true, _) => Handle::E,
            (_, _, _, true) => Handle::S,
            _ if r.contains(pt.x, pt.y) => Handle::Move,
            _ => Handle::None,
        }
    }

    // ---------- pipeline ----------

    fn tick(&mut self) {
        if self.mode != Mode::Live {
            return;
        }
        let moved = self.region != self.last_requested;
        let stale = self
            .last_request_at
            .map(|t| t.elapsed() >= IDLE_RESCAN)
            .unwrap_or(true);
        if moved || stale {
            self.request_scan(false, false);
        }
        if self.toast.as_ref().is_some_and(|(_, at)| at.elapsed() > TOAST) {
            self.toast = None;
            let _ = self.redraw();
        }
    }

    /// `bypass_dedupe` skips our local "same region, asked recently" guard;
    /// `force_worker` additionally makes the worker ignore its pixel fingerprint
    /// and redo OCR and translation from scratch.
    fn request_scan(&mut self, bypass_dedupe: bool, force_worker: bool) {
        if self.region.is_empty() {
            return;
        }
        if !bypass_dedupe && self.region == self.last_requested {
            if let Some(t) = self.last_request_at {
                if t.elapsed() < IDLE_RESCAN {
                    return;
                }
            }
        }
        self.generation += 1;
        self.last_requested = self.region;
        self.last_request_at = Some(Instant::now());
        self.worker.request(self.region, self.generation, force_worker);
        self.start_spinner();
    }

    fn start_spinner(&mut self) {
        if !self.spinning {
            self.spinning = true;
            unsafe {
                let _ = SetTimer(Some(self.app_hwnd), TIMER_SPIN, SPIN_TICK_MS, None);
            }
        }
    }

    fn stop_spinner(&mut self) {
        if self.spinning {
            self.spinning = false;
            unsafe {
                let _ = KillTimer(Some(self.app_hwnd), TIMER_SPIN);
            }
        }
    }

    fn spin(&mut self) {
        self.spin_phase = self.spin_phase.wrapping_add(1);
        if self.mode != Mode::Hidden {
            let _ = self.redraw();
        }
    }

    /// Run a toolbar action.
    fn activate(&mut self, button: Button) {
        match button {
            Button::CopySource => self.copy(false),
            Button::CopyTranslation => self.copy(true),
            Button::Refresh => {
                self.toast = Some(("refreshing\u{2026}".to_string(), Instant::now()));
                self.request_scan(true, true);
            }
            Button::Close => {
                self.dismiss(self.app_hwnd);
                return;
            }
        }
        let _ = self.redraw();
    }

    fn copy(&mut self, translated: bool) {
        let Some(res) = &self.result else {
            self.toast = Some(("nothing to copy".to_string(), Instant::now()));
            return;
        };
        // Reading order, not the order Tesseract happened to emit.
        let mut items: Vec<_> = res.items.iter().collect();
        items.sort_by_key(|i| (i.rect.y, i.rect.x));
        let text: String = items
            .iter()
            .map(|i| if translated { i.text.as_str() } else { i.source.as_str() })
            .collect::<Vec<_>>()
            .join("\n");

        if text.trim().is_empty() {
            self.toast = Some(("nothing to copy".to_string(), Instant::now()));
            return;
        }
        let n = items.len();
        let what = if translated { "English" } else { "Hebrew" };
        self.toast = Some(match clipboard::set_text(self.app_hwnd, &text) {
            Ok(()) => (
                format!("copied {n} line{} ({what})", if n == 1 { "" } else { "s" }),
                Instant::now(),
            ),
            Err(e) => (format!("copy failed: {e}"), Instant::now()),
        });
    }

    fn take_result(&mut self) {
        let snapshot = {
            let s = self.shared.lock();
            (s.status.clone(), s.latest.clone())
        };
        if let Some(r) = snapshot.1 {
            // Drop answers for a region we have already moved away from.
            if r.generation >= self.generation.saturating_sub(1) {
                self.result = Some(r);
            }
        }
        // The pipeline has gone idle, so the refresh button stops spinning.
        if !matches!(snapshot.0, Status::Working) {
            self.stop_spinner();
        }
        if self.mode != Mode::Hidden {
            let _ = self.redraw();
        }
    }

    // ---------- painting ----------

    fn redraw(&mut self) -> Result<()> {
        if self.mode == Mode::Hidden {
            return Ok(());
        }
        let win = match self.mode {
            Mode::Selecting => virtual_screen(),
            _ => self.region.inflate(PAD),
        };
        if win.is_empty() {
            return Ok(());
        }

        self.renderer.begin(win.w, win.h)?;
        let res = self.paint(win);
        self.renderer.end()?;
        res?;
        self.renderer.present(self.overlay, (win.x, win.y))?;
        Ok(())
    }

    fn paint(&self, win: Rect) -> Result<()> {
        // Everything below is in window-local coordinates.
        let local = |r: Rect| Rect::new(r.x - win.x, r.y - win.y, r.w, r.h);
        match self.mode {
            Mode::Selecting => self.paint_selecting(win, &local),
            Mode::Live => self.paint_live(win, &local),
            Mode::Hidden => Ok(()),
        }
    }

    fn paint_selecting(&self, win: Rect, local: &dyn Fn(Rect) -> Rect) -> Result<()> {
        let t = self.theme;
        let sel = local(self.region);

        if self.region.is_empty() {
            self.renderer.fill_rect(Rect::new(0, 0, win.w, win.h), t.dim)?;
            // One hint per monitor: the virtual desktop's centre lands on the
            // seam between displays, where nobody is looking.
            for m in &self.monitors {
                let m = local(*m);
                self.hint(
                    "Drag to select a region",
                    "Esc to cancel",
                    m.x + m.w / 2,
                    m.y + m.h / 2,
                )?;
            }
            return Ok(());
        }

        // Dim everything except the selection, as four bands around it, so the
        // region itself stays at full brightness.
        let (w, h) = (win.w, win.h);
        self.renderer.fill_rect(Rect::new(0, 0, w, sel.y), t.dim)?;
        self.renderer.fill_rect(Rect::new(0, sel.bottom(), w, h - sel.bottom()), t.dim)?;
        self.renderer.fill_rect(Rect::new(0, sel.y, sel.x, sel.h), t.dim)?;
        self.renderer.fill_rect(Rect::new(sel.right(), sel.y, w - sel.right(), sel.h), t.dim)?;

        self.frame(sel, false)?;
        let dims = format!("{} \u{00d7} {}", self.region.w, self.region.h);
        self.glass_pill(&dims, sel.x, (sel.y - 30).max(4), 11.5, true)?;
        Ok(())
    }

    fn paint_live(&self, _win: Rect, local: &dyn Fn(Rect) -> Rect) -> Result<()> {
        let sel = local(self.region);
        self.frame(sel, true)?;

        if let Some(res) = &self.result {
            // The result may describe a region we have since nudged; offset by the
            // difference so chips track the text instead of jumping.
            let dx = res.region.x - self.region.x;
            let dy = res.region.y - self.region.y;
            for item in &res.items {
                let box_local = Rect::new(
                    sel.x + item.rect.x + dx,
                    sel.y + item.rect.y + dy,
                    item.rect.w,
                    item.rect.h,
                );
                self.draw_translation(&item.text, box_local)?;
            }
        }

        let tb = Toolbar::layout(sel);
        self.paint_toolbar(&tb)?;
        self.status_chip(sel, tb.pill.x - sel.x - 8)?;
        Ok(())
    }

    /// Selection outline: a soft accent halo, a crisp edge, and rounded grips.
    fn frame(&self, sel: Rect, grips: bool) -> Result<()> {
        let t = self.theme;
        self.renderer.stroke_round_rect(sel.inflate(2), RADIUS + 2.0, t.accent_soft, 3.0)?;
        self.renderer.stroke_round_rect(sel, RADIUS, t.accent, 1.5)?;

        if grips {
            for (cx, cy) in [
                (sel.x, sel.y),
                (sel.right(), sel.y),
                (sel.x, sel.bottom()),
                (sel.right(), sel.bottom()),
            ] {
                let (fx, fy) = (cx as f32, cy as f32);
                self.renderer.fill_ellipse(fx, fy, 5.5, 5.5, Rgba(0.0, 0.0, 0.0, t.shadow * 0.6))?;
                self.renderer.fill_ellipse(fx, fy, 4.5, 4.5, t.handle_fill)?;
                self.renderer.fill_ellipse(fx, fy, 2.2, 2.2, t.accent)?;
            }
        }
        Ok(())
    }

    /// Paint one translated line as a panel sized to the text it replaces,
    /// shrinking the font until the English fits the original line's width.
    fn draw_translation(&self, text: &str, box_local: Rect) -> Result<()> {
        let t = self.theme;
        let pad_x = 7.0;
        let pad_y = 3.0;
        let avail_w = (box_local.w as f32 - pad_x * 2.0).max(40.0);

        let mut size = (box_local.h as f32 * 0.74).clamp(9.0, 30.0);
        let mut chosen = None;
        for _ in 0..7 {
            let fmt = self.renderer.text_format("Segoe UI", size)?;
            let (layout, tw, th) = self.renderer.layout(text, &fmt, avail_w, 4000.0)?;
            // One line that fits, or we have shrunk as far as is readable.
            if th <= box_local.h as f32 * 1.35 || size <= 9.5 {
                chosen = Some((layout, tw, th));
                break;
            }
            size *= 0.87;
        }
        let Some((layout, tw, th)) = chosen else { return Ok(()) };

        let chip_w = (tw + pad_x * 2.0).ceil() as i32;
        let chip_h = (th + pad_y * 2.0).ceil() as i32;
        // The chip must be at least as wide as the line it replaces, or the tail
        // of the original Hebrew stays visible next to the translation.
        let chip = Rect::new(
            box_local.x,
            box_local.y + (box_local.h - chip_h) / 2,
            chip_w.max(box_local.w),
            chip_h.max(box_local.h),
        );

        self.renderer.drop_shadow(chip, CHIP_RADIUS, 4, t.shadow)?;
        self.renderer.fill_round_rect(chip, CHIP_RADIUS, t.chip_bg)?;
        self.renderer.stroke_round_rect(chip, CHIP_RADIUS, t.chip_border, 1.0)?;
        self.renderer.draw_layout(
            &layout,
            chip.x as f32 + pad_x,
            chip.y as f32 + (chip.h as f32 - th) / 2.0,
            t.chip_fg,
        )?;
        Ok(())
    }

    fn status_chip(&self, sel: Rect, max_w: i32) -> Result<()> {
        let status = self.shared.lock().status.clone();
        if let Some((toast, _)) = &self.toast {
            return self.glass_pill_clamped(toast, sel.x, sel.bottom() + TB_DROP, 11.0, true, max_w);
        }
        let text = match (&status, &self.result) {
            (Status::Starting, _) => "loading model\u{2026}".to_string(),
            (Status::Failed(e), _) => format!("error: {e}"),
            (_, Some(r)) if r.items.is_empty() => "no Hebrew text found".to_string(),
            (_, Some(r)) => format!(
                "{} line{}   \u{00b7}   ocr {}ms   \u{00b7}   mt {}ms",
                r.items.len(),
                if r.items.len() == 1 { "" } else { "s" },
                r.ocr_time.as_millis(),
                r.mt_time.as_millis()
            ),
            (Status::Working, None) => "reading\u{2026}".to_string(),
            _ => "ready".to_string(),
        };
        let dot = !matches!(status, Status::Failed(_));
        self.glass_pill_clamped(&text, sel.x, sel.bottom() + TB_DROP, 11.0, dot, max_w)
    }

    /// Status text competes with the toolbar for the strip under the selection.
    /// When it will not fit, fall back to a shorter form, then give up entirely
    /// rather than draw one pill on top of another.
    fn glass_pill_clamped(
        &self,
        text: &str,
        x: i32,
        y: i32,
        size: f32,
        dot: bool,
        max_w: i32,
    ) -> Result<()> {
        if max_w < 60 {
            return Ok(());
        }
        for candidate in [text, text.split("   \u{00b7}   ").next().unwrap_or(text)] {
            let fmt = self.renderer.text_format("Segoe UI", size)?;
            let (_, tw, _) = self.renderer.layout(candidate, &fmt, 1400.0, 200.0)?;
            let width = tw.ceil() as i32 + 22 + if dot { 16 } else { 0 };
            if width <= max_w {
                return self.glass_pill(candidate, x, y, size, dot);
            }
        }
        Ok(())
    }

    fn paint_toolbar(&self, tb: &Toolbar) -> Result<()> {
        let t = self.theme;
        self.renderer.drop_shadow(tb.pill, RADIUS, 5, t.shadow)?;
        self.renderer.fill_round_rect(tb.pill, RADIUS, t.glass_bg)?;
        self.renderer.stroke_round_rect(tb.pill, RADIUS, t.glass_border, 1.0)?;

        let busy = self.spinning;
        for b in [
            Button::CopySource,
            Button::CopyTranslation,
            Button::Refresh,
            Button::Close,
        ] {
            let r = tb.rect_of(b);
            let hot = self.hover_btn == Some(b);
            let down = self.pressed_btn == Some(b);
            if hot || down {
                let a = if down { 0.20 } else { 0.11 };
                self.renderer
                    .fill_round_rect(r, 5.0, Rgba(1.0, 1.0, 1.0, a))?;
            }
            let fg = if hot || down { t.chip_fg } else { t.glass_fg };
            match b {
                Button::CopySource => self.icon_copy(r, "HE", fg)?,
                Button::CopyTranslation => self.icon_copy(r, "EN", fg)?,
                Button::Refresh => {
                    if busy {
                        self.icon_spinner(r)?;
                    } else {
                        self.icon_refresh(r, fg)?;
                    }
                }
                Button::Close => self.icon_close(r, fg)?,
            }
        }
        Ok(())
    }

    /// Two stacked sheets. The back one is drawn as an open L rather than a full
    /// rectangle, so it reads as "behind" without needing to occlude anything --
    /// the pill under it is translucent, so an opaque fill would show as a patch.
    fn icon_copy(&self, r: Rect, label: &str, fg: Rgba) -> Result<()> {
        let gx = r.x as f32 + 7.0;
        let gy = r.y as f32 + (r.h as f32 - 15.0) / 2.0;

        self.renderer.draw_line(gx + 3.5, gy + 0.5, gx + 11.5, gy + 0.5, fg, 1.3)?;
        self.renderer.draw_line(gx + 11.5, gy + 0.5, gx + 11.5, gy + 9.0, fg, 1.3)?;
        self.renderer
            .stroke_round_rect(Rect::new(gx as i32, gy as i32 + 4, 10, 11), 2.0, fg, 1.3)?;

        let fmt = self.renderer.text_format("Segoe UI", 9.5)?;
        let (layout, tw, th) = self.renderer.layout(label, &fmt, 60.0, 40.0)?;
        self.renderer.draw_layout(
            &layout,
            r.right() as f32 - 8.0 - tw,
            r.y as f32 + (r.h as f32 - th) / 2.0,
            fg,
        )?;
        Ok(())
    }

    /// Circular arrow, drawn as a polyline with a chevron head.
    fn icon_refresh(&self, r: Rect, fg: Rgba) -> Result<()> {
        let cx = r.x as f32 + r.w as f32 / 2.0;
        let cy = r.y as f32 + r.h as f32 / 2.0;
        let rad = 6.0;
        let (a0, a1) = (-0.35_f32, 4.9_f32);

        let steps = 18;
        let mut prev = (cx + rad * a0.cos(), cy + rad * a0.sin());
        for i in 1..=steps {
            let a = a0 + (a1 - a0) * (i as f32 / steps as f32);
            let pt = (cx + rad * a.cos(), cy + rad * a.sin());
            self.renderer.draw_line(prev.0, prev.1, pt.0, pt.1, fg, 1.4)?;
            prev = pt;
        }
        // Arrowhead at the open end of the arc.
        let tip = (cx + rad * a0.cos(), cy + rad * a0.sin());
        self.renderer.draw_line(tip.0, tip.1, tip.0 - 3.6, tip.1 - 1.6, fg, 1.4)?;
        self.renderer.draw_line(tip.0, tip.1, tip.0 + 1.0, tip.1 - 4.0, fg, 1.4)?;
        Ok(())
    }

    fn icon_close(&self, r: Rect, fg: Rgba) -> Result<()> {
        let cx = r.x as f32 + r.w as f32 / 2.0;
        let cy = r.y as f32 + r.h as f32 / 2.0;
        let d = 4.5;
        self.renderer.draw_line(cx - d, cy - d, cx + d, cy + d, fg, 1.5)?;
        self.renderer.draw_line(cx + d, cy - d, cx - d, cy + d, fg, 1.5)?;
        Ok(())
    }

    /// Eight dots around a circle with a travelling highlight.
    fn icon_spinner(&self, r: Rect) -> Result<()> {
        let t = self.theme;
        let cx = r.x as f32 + r.w as f32 / 2.0;
        let cy = r.y as f32 + r.h as f32 / 2.0;
        const N: u32 = 8;
        let head = (self.spin_phase / 2) % N;
        for i in 0..N {
            let a = (i as f32) * std::f32::consts::TAU / N as f32 - std::f32::consts::FRAC_PI_2;
            let behind = (i + N - head) % N;
            let fade = 1.0 - (behind as f32 / N as f32);
            let alpha = 0.18 + 0.82 * fade * fade;
            self.renderer.fill_ellipse(
                cx + 6.5 * a.cos(),
                cy + 6.5 * a.sin(),
                1.7,
                1.7,
                Rgba(t.accent.0, t.accent.1, t.accent.2, alpha),
            )?;
        }
        Ok(())
    }

    /// Frosted pill used for all chrome: dimensions badge, status, hints.
    fn glass_pill(&self, text: &str, x: i32, y: i32, size: f32, dot: bool) -> Result<()> {
        let t = self.theme;
        let fmt = self.renderer.text_format("Segoe UI", size)?;
        let (layout, tw, th) = self.renderer.layout(text, &fmt, 1400.0, 200.0)?;

        let lead = if dot { 16 } else { 0 };
        let r = Rect::new(x, y, tw.ceil() as i32 + 22 + lead, PILL_H);

        self.renderer.drop_shadow(r, RADIUS, 5, t.shadow)?;
        self.renderer.fill_round_rect(r, RADIUS, t.glass_bg)?;
        self.renderer.stroke_round_rect(r, RADIUS, t.glass_border, 1.0)?;
        if dot {
            self.renderer.fill_ellipse(
                r.x as f32 + 13.0,
                r.y as f32 + r.h as f32 / 2.0,
                3.0,
                3.0,
                t.accent,
            )?;
        }
        self.renderer.draw_layout(
            &layout,
            r.x as f32 + 11.0 + lead as f32,
            r.y as f32 + (PILL_H as f32 - th) / 2.0,
            t.glass_fg,
        )?;
        Ok(())
    }

    /// Centred two-line hint shown before a region has been dragged.
    fn hint(&self, title: &str, sub: &str, cx: i32, cy: i32) -> Result<()> {
        let t = self.theme;
        let f1 = self.renderer.text_format("Segoe UI", 15.0)?;
        let f2 = self.renderer.text_format("Segoe UI", 11.5)?;
        let (l1, w1, h1) = self.renderer.layout(title, &f1, 900.0, 200.0)?;
        let (l2, w2, h2) = self.renderer.layout(sub, &f2, 900.0, 200.0)?;

        let inner_w = w1.max(w2).ceil() as i32;
        let r = Rect::new(
            cx - inner_w / 2 - 22,
            cy - (h1 + h2) as i32 / 2 - 18,
            inner_w + 44,
            (h1 + h2).ceil() as i32 + 40,
        );
        self.renderer.drop_shadow(r, RADIUS + 3.0, 6, t.shadow)?;
        self.renderer.fill_round_rect(r, RADIUS + 3.0, t.glass_bg)?;
        self.renderer.stroke_round_rect(r, RADIUS + 3.0, t.glass_border, 1.0)?;

        self.renderer
            .draw_layout(&l1, (cx - w1 as i32 / 2) as f32, r.y as f32 + 15.0, t.chip_fg)?;
        self.renderer.draw_layout(
            &l2,
            (cx - w2 as i32 / 2) as f32,
            r.y as f32 + 17.0 + h1,
            t.glass_fg,
        )?;
        Ok(())
    }
}

fn resize(start: Rect, handle: Handle, dx: i32, dy: i32) -> Rect {
    let mut r = start;
    match handle {
        Handle::Move => {
            r.x += dx;
            r.y += dy;
        }
        Handle::N => {
            r.y += dy;
            r.h -= dy;
        }
        Handle::S => r.h += dy,
        Handle::W => {
            r.x += dx;
            r.w -= dx;
        }
        Handle::E => r.w += dx,
        Handle::Nw => {
            r.x += dx;
            r.w -= dx;
            r.y += dy;
            r.h -= dy;
        }
        Handle::Ne => {
            r.w += dx;
            r.y += dy;
            r.h -= dy;
        }
        Handle::Sw => {
            r.x += dx;
            r.w -= dx;
            r.h += dy;
        }
        Handle::Se => {
            r.w += dx;
            r.h += dy;
        }
        Handle::None => {}
    }
    if r.w < MIN_REGION {
        r.w = MIN_REGION;
    }
    if r.h < MIN_REGION {
        r.h = MIN_REGION;
    }
    r
}

/// Build the tray icon in code so the build needs no .rc/.ico pipeline: a rounded
/// accent tile with three "text lines", the middle one highlighted.
fn make_icon() -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    const N: i32 = 32;
    let mut px = vec![0u8; (N * N * 4) as usize];

    let put = |px: &mut Vec<u8>, x: i32, y: i32, c: (u8, u8, u8, u8)| {
        if x < 0 || y < 0 || x >= N || y >= N {
            return;
        }
        let i = ((y * N + x) * 4) as usize;
        // Premultiplied BGRA, which is what CreateIconIndirect expects for 32bpp.
        let a = c.3 as u32;
        px[i] = (c.2 as u32 * a / 255) as u8;
        px[i + 1] = (c.1 as u32 * a / 255) as u8;
        px[i + 2] = (c.0 as u32 * a / 255) as u8;
        px[i + 3] = c.3;
    };

    // Rounded tile.
    let radius = 7;
    for y in 0..N {
        for x in 0..N {
            let inset = 1;
            let (lx, ly) = (x - inset, y - inset);
            let (w, h) = (N - inset * 2, N - inset * 2);
            if lx < 0 || ly < 0 || lx >= w || ly >= h {
                continue;
            }
            let cx = lx.min(w - 1 - lx);
            let cy = ly.min(h - 1 - ly);
            if cx < radius && cy < radius {
                let dx = (radius - cx) as f32;
                let dy = (radius - cy) as f32;
                if (dx * dx + dy * dy).sqrt() > radius as f32 + 0.5 {
                    continue;
                }
            }
            put(&mut px, x, y, (0x2B, 0x53, 0x9C, 0xFF));
        }
    }
    // Three text lines; the middle is the "translated" one.
    for (ty, x0, x1, col) in [
        (10, 7, 25, (0xC8u8, 0xD6u8, 0xF2u8, 0xFFu8)),
        (15, 7, 21, (0x4C, 0x8D, 0xFF, 0xFF)),
        (20, 7, 23, (0xC8, 0xD6, 0xF2, 0xFF)),
    ] {
        for y in ty..ty + 3 {
            for x in x0..x1 {
                put(&mut px, x, y, col);
            }
        }
    }

    unsafe {
        let screen = GetDC(None);
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: N,
            biHeight: -N,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color: HBITMAP =
            CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        ReleaseDC(None, screen);
        if bits.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len());

        // A monochrome mask is still required even for an alpha icon.
        let mask = windows::Win32::Graphics::Gdi::CreateBitmap(N, N, 1, 1, None);

        let ii = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&ii).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
