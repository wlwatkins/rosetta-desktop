//! Direct2D/DirectWrite rendering onto a per-pixel-alpha layered window.
//!
//! The overlay must composite over arbitrary desktop content, which rules out
//! plain GDI text (it writes RGB but leaves the alpha channel untouched). A
//! D2D DC render target bound to a 32-bit DIB produces premultiplied BGRA that
//! `UpdateLayeredWindow` can consume directly.

use anyhow::{bail, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
    DWRITE_FACTORY_TYPE_SHARED,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};
use windows_numerics::Vector2;

use crate::geom::Rect;

#[derive(Clone, Copy)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

impl Rgba {
    pub const fn hex(rgb: u32, a: f32) -> Self {
        Self(
            ((rgb >> 16) & 0xff) as f32 / 255.0,
            ((rgb >> 8) & 0xff) as f32 / 255.0,
            (rgb & 0xff) as f32 / 255.0,
            a,
        )
    }
    fn to_d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.0, g: self.1, b: self.2, a: self.3 }
    }
}

pub struct Renderer {
    _d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    rt: ID2D1DCRenderTarget,
    screen_dc: HDC,
    mem_dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    width: i32,
    height: i32,
    drawing: bool,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;

            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = d2d.CreateDCRenderTarget(&props)?;

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            let screen_dc = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen_dc));

            Ok(Self {
                _d2d: d2d,
                dwrite,
                rt,
                screen_dc,
                mem_dc,
                bitmap: HBITMAP::default(),
                old: HGDIOBJ::default(),
                width: 0,
                height: 0,
                drawing: false,
            })
        }
    }

    fn ensure_surface(&mut self, w: i32, h: i32) -> Result<()> {
        if self.width == w && self.height == h && !self.bitmap.is_invalid() {
            return Ok(());
        }
        self.release_surface();

        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
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
        if bitmap.is_invalid() {
            bail!("CreateDIBSection failed for {w}x{h}");
        }
        self.old = unsafe { SelectObject(self.mem_dc, bitmap.into()) };
        self.bitmap = bitmap;
        self.width = w;
        self.height = h;
        Ok(())
    }

    /// Bind the render target to the surface and start a frame.
    pub fn begin(&mut self, w: i32, h: i32) -> Result<()> {
        self.ensure_surface(w, h)?;
        let bind = RECT { left: 0, top: 0, right: w, bottom: h };
        unsafe {
            self.rt.BindDC(self.mem_dc, &bind)?;
            self.rt.BeginDraw();
            self.rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        }
        self.drawing = true;
        Ok(())
    }

    pub fn end(&mut self) -> Result<()> {
        if !self.drawing {
            return Ok(());
        }
        unsafe { self.rt.EndDraw(None, None)? };
        self.drawing = false;
        Ok(())
    }

    /// Push the finished surface to the layered window positioned at `origin`.
    pub fn present(&self, hwnd: HWND, origin: (i32, i32)) -> Result<()> {
        let mut pos = POINT { x: origin.0, y: origin.1 };
        let mut size = SIZE { cx: self.width, cy: self.height };
        let mut src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                Some(self.screen_dc),
                Some(&mut pos),
                Some(&mut size),
                Some(self.mem_dc),
                Some(&mut src),
                windows::Win32::Foundation::COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )?;
        }
        Ok(())
    }

    fn brush(&self, c: Rgba) -> Result<ID2D1SolidColorBrush> {
        Ok(unsafe { self.rt.CreateSolidColorBrush(&c.to_d2d(), None)? })
    }

    pub fn fill_rect(&self, r: Rect, c: Rgba) -> Result<()> {
        let b = self.brush(c)?;
        unsafe { self.rt.FillRectangle(&to_d2d_rect(r), &b) };
        Ok(())
    }

    pub fn fill_round_rect(&self, r: Rect, radius: f32, c: Rgba) -> Result<()> {
        let b = self.brush(c)?;
        let rr = D2D1_ROUNDED_RECT { rect: to_d2d_rect(r), radiusX: radius, radiusY: radius };
        unsafe { self.rt.FillRoundedRectangle(&rr, &b) };
        Ok(())
    }

    pub fn stroke_round_rect(&self, r: Rect, radius: f32, c: Rgba, width: f32) -> Result<()> {
        let b = self.brush(c)?;
        let half = width / 2.0;
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: r.x as f32 + half,
                top: r.y as f32 + half,
                right: r.right() as f32 - half,
                bottom: r.bottom() as f32 - half,
            },
            radiusX: radius,
            radiusY: radius,
        };
        unsafe { self.rt.DrawRoundedRectangle(&rr, &b, width, None) };
        Ok(())
    }

    pub fn fill_ellipse(&self, cx: f32, cy: f32, rx: f32, ry: f32, c: Rgba) -> Result<()> {
        let b = self.brush(c)?;
        let e = windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE {
            point: Vector2 { X: cx, Y: cy },
            radiusX: rx,
            radiusY: ry,
        };
        unsafe { self.rt.FillEllipse(&e, &b) };
        Ok(())
    }

    /// Fake a soft drop shadow by stacking progressively larger, fainter rounded
    /// rects. A DC render target has no shadow effect, and this is cheap enough
    /// to run every frame.
    pub fn drop_shadow(&self, r: Rect, radius: f32, spread: i32, strength: f32) -> Result<()> {
        for i in (1..=spread).rev() {
            let a = strength * (1.0 - i as f32 / (spread + 1) as f32).powf(2.0);
            self.fill_round_rect(
                Rect::new(r.x - i, r.y - i + 1, r.w + i * 2, r.h + i * 2),
                radius + i as f32,
                Rgba(0.0, 0.0, 0.0, a),
            )?;
        }
        Ok(())
    }

    pub fn draw_line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: Rgba, width: f32) -> Result<()> {
        let b = self.brush(c)?;
        unsafe {
            self.rt.DrawLine(
                Vector2 { X: x0, Y: y0 },
                Vector2 { X: x1, Y: y1 },
                &b,
                width,
                None,
            )
        };
        Ok(())
    }

    pub fn text_format(&self, family: &str, size: f32) -> Result<IDWriteTextFormat> {
        let fam: Vec<u16> = family.encode_utf16().chain(std::iter::once(0)).collect();
        let locale: Vec<u16> = "en-us".encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            Ok(self.dwrite.CreateTextFormat(
                PCWSTR(fam.as_ptr()),
                None,
                windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_SEMI_BOLD,
                windows::Win32::Graphics::DirectWrite::DWRITE_FONT_STYLE_NORMAL,
                windows::Win32::Graphics::DirectWrite::DWRITE_FONT_STRETCH_NORMAL,
                size,
                PCWSTR(locale.as_ptr()),
            )?)
        }
    }

    pub fn layout(
        &self,
        text: &str,
        format: &IDWriteTextFormat,
        max_w: f32,
        max_h: f32,
    ) -> Result<(IDWriteTextLayout, f32, f32)> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let layout = unsafe { self.dwrite.CreateTextLayout(&wide, format, max_w, max_h)? };
        let mut m = Default::default();
        unsafe { layout.GetMetrics(&mut m)? };
        Ok((layout, m.width, m.height))
    }

    pub fn draw_layout(&self, layout: &IDWriteTextLayout, x: f32, y: f32, c: Rgba) -> Result<()> {
        let b = self.brush(c)?;
        unsafe {
            self.rt.DrawTextLayout(Vector2 { X: x, Y: y }, layout, &b, D2D1_DRAW_TEXT_OPTIONS_NONE)
        };
        Ok(())
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
        self.width = 0;
        self.height = 0;
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.release_surface();
        unsafe {
            let _ = DeleteDC(self.mem_dc);
            ReleaseDC(None, self.screen_dc);
        }
    }
}

fn to_d2d_rect(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x as f32,
        top: r.y as f32,
        right: r.right() as f32,
        bottom: r.bottom() as f32,
    }
}
