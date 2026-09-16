//! Capture -> OCR -> translate, off the UI thread.
//!
//! The UI thread must never block: it is driving a layered window that sits on
//! top of whatever the user is reading. So the whole pipeline lives here, and
//! finished frames are handed back by posting a message to the app window.

use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::capture::ScreenCapture;
use crate::geom::Rect;
use crate::ocr::OcrEngine;
use crate::stabilize::Stabilizer;
use crate::translate::{Config, Marian};

/// One translated line, positioned relative to the scanned region.
#[derive(Debug, Clone)]
pub struct Item {
    pub rect: Rect,
    pub source: String,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub generation: u64,
    pub region: Rect,
    pub items: Vec<Item>,
    pub ocr_time: Duration,
    pub mt_time: Duration,
}

#[derive(Debug, Clone, Default)]
pub enum Status {
    #[default]
    Starting,
    Ready,
    Working,
    Failed(String),
}

pub enum Job {
    Scan { region: Rect, generation: u64, force: bool },
    Quit,
}

/// Shared slot holding the most recent result, read by the UI thread on wake-up.
pub type Shared = Arc<Mutex<Snapshot>>;

#[derive(Default)]
pub struct Snapshot {
    pub status: Status,
    pub latest: Option<ScanResult>,
}

pub struct Worker {
    tx: Sender<Job>,
    join: Option<std::thread::JoinHandle<()>>,
    pub shared: Shared,
}

impl Worker {
    pub fn spawn(
        notify_hwnd: isize,
        notify_msg: u32,
        models: PathBuf,
        vendor: PathBuf,
        cfg: Config,
        lang: String,
    ) -> Self {
        // Depth 1: while a scan runs, only the newest pending request matters.
        let (tx, rx) = bounded::<Job>(1);
        let shared: Shared = Arc::new(Mutex::new(Snapshot::default()));
        let worker_shared = shared.clone();

        let join = std::thread::Builder::new()
            .name("rosetta-pipeline".into())
            .spawn(move || {
                if let Err(e) = run(notify_hwnd, notify_msg, models, vendor, cfg, lang, rx, &worker_shared)
                {
                    worker_shared.lock().status = Status::Failed(format!("{e:#}"));
                    notify(notify_hwnd, notify_msg);
                }
            })
            .expect("spawning pipeline thread");

        Self { tx, join: Some(join), shared }
    }

    /// Non-blocking: if the worker is busy and a request is already queued, the
    /// stale one is dropped in favour of this newer region.
    pub fn request(&self, region: Rect, generation: u64, force: bool) {
        let job = Job::Scan { region, generation, force };
        if self.tx.try_send(job).is_err() {
            // Queue full: replace whatever is waiting.
            let _ = self.tx.try_send(Job::Scan { region, generation, force });
        }
    }

    /// Shut the pipeline down and wait for it. The wait matters: Tesseract's
    /// language data is released by `TessBaseAPI`'s destructor, and exiting the
    /// process while the worker still owns it makes Tesseract print
    /// "WARNING! LEAK!" for every cached dictionary.
    pub fn quit(&mut self) {
        let _ = self.tx.send(Job::Quit);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

fn notify(hwnd: isize, msg: u32) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    notify_hwnd: isize,
    notify_msg: u32,
    models: PathBuf,
    vendor: PathBuf,
    cfg: Config,
    lang: String,
    rx: Receiver<Job>,
    shared: &Shared,
) -> Result<()> {
    let mut capture = ScreenCapture::new()?;
    let mut ocr = OcrEngine::new(&vendor.join("bin"), &vendor.join("tessdata"), &lang)?;
    let mut mt = Marian::load(&models, cfg)?;

    shared.lock().status = Status::Ready;
    notify(notify_hwnd, notify_msg);

    // Translating the same string twice is pure waste while the box is nudged
    // around, and it is the single biggest win in the live loop.
    let mut cache: HashMap<String, String> = HashMap::new();
    let mut last_fingerprint: Option<u64> = None;
    let mut last_items: Vec<Item> = Vec::new();
    let mut last_region = Rect::default();
    let mut stabilizer = Stabilizer::default();

    while let Ok(job) = rx.recv() {
        let (region, generation, force) = match job {
            Job::Quit => break,
            Job::Scan { region, generation, force } => (region, generation, force),
        };
        if region.is_empty() {
            continue;
        }

        // A selection dragged somewhere unrelated starts fresh; otherwise held
        // lines from the previous region would linger over the new one. An
        // explicit refresh also starts clean, since that is what the user is
        // asking for.
        if force || (!last_region.is_empty() && region.iou(&last_region) < 0.05) {
            stabilizer.clear();
        }

        shared.lock().status = Status::Working;

        let frame = match capture.grab(region) {
            Ok(f) => f,
            Err(e) => {
                shared.lock().status = Status::Failed(format!("capture: {e:#}"));
                notify(notify_hwnd, notify_msg);
                continue;
            }
        };

        let fingerprint = frame.fingerprint();
        if !force && last_fingerprint == Some(fingerprint) && last_region == region {
            // Same pixels as last time: re-publish rather than re-run the model.
            let mut s = shared.lock();
            s.status = Status::Ready;
            s.latest = Some(ScanResult {
                generation,
                region,
                items: last_items.clone(),
                ocr_time: Duration::ZERO,
                mt_time: Duration::ZERO,
            });
            drop(s);
            notify(notify_hwnd, notify_msg);
            continue;
        }

        let t0 = Instant::now();
        let lines = match ocr.read_bgra(frame.data, frame.width, frame.height) {
            Ok(l) => l,
            Err(e) => {
                shared.lock().status = Status::Failed(format!("ocr: {e:#}"));
                notify(notify_hwnd, notify_msg);
                continue;
            }
        };
        let ocr_time = t0.elapsed();

        let t1 = Instant::now();
        let mut items = Vec::with_capacity(lines.len());
        for line in lines {
            let text = match cache.get(&line.text) {
                Some(hit) => hit.clone(),
                None => match mt.translate(&line.text) {
                    Ok(t) => {
                        // Unbounded growth would be a slow leak in a tray app.
                        if cache.len() > 2048 {
                            cache.clear();
                        }
                        cache.insert(line.text.clone(), t.clone());
                        t
                    }
                    Err(e) => format!("[{e}]"),
                },
            };
            if text.trim().is_empty() {
                continue;
            }
            items.push(Item { rect: line.rect, source: line.text, text });
        }
        let mt_time = t1.elapsed();

        // A fresh scan disagreeing slightly with the last one must not make
        // translations blink; hold lines across a few missed frames.
        let raw_count = items.len();
        let items = stabilizer.update(region, items);

        if std::env::var("ROSETTA_DEBUG").is_ok() {
            eprintln!(
                "[scan gen={generation} {}x{}@{},{}] ocr {:?} mt {:?} -> {} raw / {} shown",
                region.w, region.h, region.x, region.y, ocr_time, mt_time, raw_count, items.len()
            );
            for it in &items {
                eprintln!("    {:>4},{:<4} {:>3}x{:<3}  {}  =>  {}",
                    it.rect.x, it.rect.y, it.rect.w, it.rect.h, it.source, it.text);
            }
        }

        last_fingerprint = Some(fingerprint);
        last_items = items.clone();
        last_region = region;

        let mut s = shared.lock();
        s.status = Status::Ready;
        s.latest = Some(ScanResult {
            generation,
            region,
            items,
            ocr_time,
            mt_time,
        });
        drop(s);
        notify(notify_hwnd, notify_msg);
    }

    Ok(())
}
