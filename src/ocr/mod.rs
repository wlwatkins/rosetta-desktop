mod tesseract;

pub use tesseract::{Tesseract, PSM_SINGLE_BLOCK};

use anyhow::Result;
use std::path::Path;

use crate::geom::Rect;

/// One recognized line, with its box in *captured-region* coordinates.
#[derive(Debug, Clone)]
pub struct OcrLine {
    pub text: String,
    /// Tesseract's mean confidence, 0-100.
    pub confidence: f32,
    pub rect: Rect,
}

/// 8-bit grayscale, the form Tesseract is happiest with.
pub struct GrayImage {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// How much the source was upscaled to build this buffer.
    pub scale: u32,
}

impl GrayImage {
    /// Convert a top-down BGRA capture to grayscale, upscaling by `scale` and
    /// inverting light-on-dark text so Tesseract sees its preferred polarity.
    pub fn from_bgra(src: &[u8], width: u32, height: u32, scale: u32) -> Self {
        let scale = scale.max(1);
        let (w, h) = (width as usize, height as usize);

        // Rec. 601 luma, matching what the eye weights and what Tesseract expects.
        let mut gray = vec![0u8; w * h];
        let mut sum: u64 = 0;
        for i in 0..w * h {
            let b = src[i * 4] as u32;
            let g = src[i * 4 + 1] as u32;
            let r = src[i * 4 + 2] as u32;
            let y = ((r * 299 + g * 587 + b * 114) / 1000) as u8;
            gray[i] = y;
            sum += y as u64;
        }

        // Dark-mode UI is light text on a dark field; Tesseract 5 is markedly worse
        // on that, so flip it when the region reads as predominantly dark.
        let mean = (sum / (w * h).max(1) as u64) as u8;
        if mean < 110 {
            for px in gray.iter_mut() {
                *px = 255 - *px;
            }
        }

        if scale == 1 {
            return Self { data: gray, width, height, scale };
        }

        let (nw, nh) = (w * scale as usize, h * scale as usize);
        let mut out = vec![0u8; nw * nh];
        let sx = w as f32 / nw as f32;
        let sy = h as f32 / nh as f32;
        for y in 0..nh {
            let fy = ((y as f32 + 0.5) * sy - 0.5).max(0.0);
            let y0 = fy.floor() as usize;
            let y1 = (y0 + 1).min(h - 1);
            let wy = fy - y0 as f32;
            for x in 0..nw {
                let fx = ((x as f32 + 0.5) * sx - 0.5).max(0.0);
                let x0 = fx.floor() as usize;
                let x1 = (x0 + 1).min(w - 1);
                let wx = fx - x0 as f32;

                let p00 = gray[y0 * w + x0] as f32;
                let p01 = gray[y0 * w + x1] as f32;
                let p10 = gray[y1 * w + x0] as f32;
                let p11 = gray[y1 * w + x1] as f32;
                let top = p00 + (p01 - p00) * wx;
                let bot = p10 + (p11 - p10) * wx;
                out[y * nw + x] = (top + (bot - top) * wy).round().clamp(0.0, 255.0) as u8;
            }
        }

        Self { data: out, width: nw as u32, height: nh as u32, scale }
    }
}

pub struct OcrEngine {
    tess: Tesseract,
    /// Screen text is usually 12-18px; Tesseract wants a taller x-height.
    pub upscale: u32,
    pub psm: i32,
    pub min_confidence: f32,
}

impl OcrEngine {
    pub fn new(bin_dir: &Path, tessdata: &Path, lang: &str) -> Result<Self> {
        let mut tess = Tesseract::new(bin_dir, tessdata, lang)?;
        // The overlay only ever needs plain text; suppressing these keeps the
        // iterator cheap.
        let _ = tess.set_variable("tessedit_create_hocr", "0");
        let _ = tess.set_variable("tessedit_create_tsv", "0");
        // Real UI text dips well below 50 on small or antialiased glyphs; the
        // stabilizer is a better defence against junk than a high floor here.
        Ok(Self { tess, upscale: 2, psm: PSM_SINGLE_BLOCK, min_confidence: 32.0 })
    }

    pub fn read_bgra(&mut self, bgra: &[u8], width: u32, height: u32) -> Result<Vec<OcrLine>> {
        let img = GrayImage::from_bgra(bgra, width, height, self.upscale);
        self.read(&img)
    }

    pub fn read(&mut self, img: &GrayImage) -> Result<Vec<OcrLine>> {
        let mut lines = self.tess.recognize(img, self.psm)?;
        lines.retain(|l| l.confidence >= self.min_confidence && !l.text.trim().is_empty());
        Ok(lines)
    }
}
