//! Screen-region capture via BitBlt.
//!
//! The overlay window sets `WDA_EXCLUDEFROMCAPTURE`, so grabbing the region the
//! overlay is sitting on top of returns the *original* pixels rather than our own
//! translations. That is what makes the live loop stable instead of feeding on
//! its own output.

use anyhow::{bail, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ, SRCCOPY,
};

use crate::geom::Rect;

/// A reusable capture target. Recreating the DIB on every frame shows up in the
/// live loop, so the buffer is kept until the region size changes.
pub struct ScreenCapture {
    screen_dc: HDC,
    mem_dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl ScreenCapture {
    pub fn new() -> Result<Self> {
        let screen_dc = unsafe { GetDC(None) };
        if screen_dc.is_invalid() {
            bail!("GetDC(NULL) failed");
        }
        let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
        if mem_dc.is_invalid() {
            unsafe { ReleaseDC(None, screen_dc) };
            bail!("CreateCompatibleDC failed");
        }
        Ok(Self {
            screen_dc,
            mem_dc,
            bitmap: HBITMAP::default(),
            old: HGDIOBJ::default(),
            bits: std::ptr::null_mut(),
            width: 0,
            height: 0,
        })
    }

    fn ensure_surface(&mut self, w: i32, h: i32) -> Result<()> {
        if self.width == w && self.height == h && !self.bits.is_null() {
            return Ok(());
        }
        self.release_surface();

        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Negative height gives a top-down buffer, matching how we index rows.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bitmap = unsafe {
            CreateDIBSection(Some(self.screen_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?
        };
        if bitmap.is_invalid() || bits.is_null() {
            bail!("CreateDIBSection failed for {w}x{h}");
        }

        let old = unsafe { SelectObject(self.mem_dc, bitmap.into()) };
        self.bitmap = bitmap;
        self.old = old;
        self.bits = bits as *mut u8;
        self.width = w;
        self.height = h;
        Ok(())
    }

    /// Copy `rect` off the screen as top-down BGRA.
    pub fn grab(&mut self, rect: Rect) -> Result<CapturedFrame<'_>> {
        if rect.is_empty() {
            bail!("cannot capture an empty rect");
        }
        self.ensure_surface(rect.w, rect.h)?;

        unsafe {
            BitBlt(
                self.mem_dc,
                0,
                0,
                rect.w,
                rect.h,
                Some(self.screen_dc),
                rect.x,
                rect.y,
                // CAPTUREBLT pulls in layered windows, which most modern UI uses.
                SRCCOPY | CAPTUREBLT,
            )?;
        }

        let len = (rect.w as usize) * (rect.h as usize) * 4;
        let data = unsafe { std::slice::from_raw_parts(self.bits, len) };
        Ok(CapturedFrame { data, width: rect.w as u32, height: rect.h as u32 })
    }

    fn release_surface(&mut self) {
        unsafe {
            if !self.old.is_invalid() {
                SelectObject(self.mem_dc, self.old);
                self.old = HGDIOBJ::default();
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
                self.bitmap = HBITMAP::default();
            }
        }
        self.bits = std::ptr::null_mut();
        self.width = 0;
        self.height = 0;
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.release_surface();
        unsafe {
            let _ = DeleteDC(self.mem_dc);
            ReleaseDC(None, self.screen_dc);
        }
    }
}

// The DCs are owned by this struct and only used from the thread that owns it.
unsafe impl Send for ScreenCapture {}

pub struct CapturedFrame<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
}

impl CapturedFrame<'_> {
    /// Cheap content hash, used to skip OCR when the region has not changed.
    /// Sampling beats hashing every byte at 10fps and is plenty to catch edits.
    pub fn fingerprint(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let step = (self.data.len() / 4096).max(4);
        let mut i = 0;
        while i < self.data.len() {
            h ^= self.data[i] as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
            i += step;
        }
        h ^= (self.width as u64) << 32 | self.height as u64;
        h
    }
}

/// Bounds of the whole virtual desktop, which may start at negative coordinates.
pub fn virtual_screen() -> Rect {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        Rect::new(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// Opt into per-monitor DPI awareness so every coordinate we handle is a real
/// physical pixel; without this Windows silently scales our captures and boxes.
pub fn enable_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Marks a window invisible to screen capture, including our own BitBlt.
pub fn exclude_from_capture(hwnd: HWND) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };
    unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)? };
    Ok(())
}

/// Bounds of each physical monitor, in virtual-desktop coordinates.
///
/// The virtual screen's centre falls on the seam between monitors, so anything
/// meant to be seen (a prompt, a hint) has to be placed per monitor instead.
pub fn monitors() -> Vec<Rect> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{LPARAM, RECT, TRUE};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HMONITOR, MONITORINFO,
    };

    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _dc: HDC,
        _clip: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let out = unsafe { &mut *(data.0 as *mut Vec<Rect>) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            let r = info.rcMonitor;
            out.push(Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top));
        }
        TRUE
    }

    let mut out: Vec<Rect> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut out as *mut Vec<Rect> as isize),
        );
    }
    if out.is_empty() {
        vec![virtual_screen()]
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(data: &[u8], w: u32, h: u32) -> CapturedFrame<'_> {
        CapturedFrame { data, width: w, height: h }
    }

    #[test]
    fn the_same_pixels_hash_the_same() {
        let a = vec![7u8; 4096];
        let b = vec![7u8; 4096];
        assert_eq!(frame(&a, 32, 32).fingerprint(), frame(&b, 32, 32).fingerprint());
    }

    #[test]
    fn changed_pixels_change_the_hash() {
        // This is what makes the live loop skip re-running the model.
        let a = vec![7u8; 4096];
        let mut b = a.clone();
        b[0] = 8;
        assert_ne!(frame(&a, 32, 32).fingerprint(), frame(&b, 32, 32).fingerprint());
    }

    #[test]
    fn dimensions_are_part_of_the_hash() {
        // Same bytes, different shape: resizing the box must force a re-scan.
        let data = vec![7u8; 4096];
        assert_ne!(frame(&data, 32, 32).fingerprint(), frame(&data, 16, 64).fingerprint());
    }

    #[test]
    fn a_tiny_frame_does_not_panic() {
        let data = vec![1u8, 2, 3, 4];
        let _ = frame(&data, 1, 1).fingerprint();
    }

    #[test]
    fn virtual_screen_is_not_degenerate() {
        // A smoke test: it calls into Win32, so it mostly proves the bindings
        // and the sign handling of a left-hand monitor are right.
        let vs = virtual_screen();
        assert!(vs.w > 0 && vs.h > 0, "got {vs:?}");
    }

    #[test]
    fn every_monitor_is_inside_the_virtual_screen() {
        let vs = virtual_screen();
        let mons = monitors();
        assert!(!mons.is_empty());
        for m in &mons {
            assert!(!m.is_empty(), "{m:?}");
            assert!(m.intersects(&vs), "{m:?} is outside {vs:?}");
        }
    }
}
