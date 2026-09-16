mod autostart;
mod capture;
mod clipboard;
mod geom;
mod ocr;
mod paths;
mod settings;
mod stabilize;
mod translate;
mod ui;
mod update;
mod worker;

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use translate::{Backend, Config, Marian};

/// Sentences spanning what the overlay will actually see: UI labels, error
/// strings, headlines, and longer prose.
const PROBES: &[&str] = &[
    "שלום עולם",
    "הגדרות",
    "לא ניתן להתחבר לשרת. נסה שוב מאוחר יותר.",
    "ראש הממשלה הודיע היום על תוכנית חדשה לצמצום יוקר המחיה במדינה.",
    "המשתמש חייב להזין סיסמה בת שמונה תווים לפחות, הכוללת אות גדולה ומספר.",
    "הישיבה נדחתה לשבוע הבא בגלל היעדרות של שלושה מחברי הוועדה.",
    "למרות שהגשם לא הפסיק כל הלילה, הם החליטו לצאת לטיול בהרים עם עלות השחר.",
];

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("translate") => cmd_translate(&args[1..]),
        Some("bench") => cmd_bench(),
        Some("ocr") => cmd_ocr(&args[1..]),
        Some("pipeline") => cmd_pipeline(&args[1..]),
        Some("grab") => cmd_grab(&args[1..]),
        Some("settings") => cmd_settings(),
        Some("about") => ui::run_about(),
        Some("check-updates") => cmd_check_updates(),
        None | Some("run") => cmd_run(),
        other => bail!(
            "unknown command {other:?}; try:\n  \
             translate <hebrew text>\n  \
             bench\n  \
             ocr <image.png>\n  \
             pipeline <image.png>"
        ),
    }
}

fn env_cfg() -> Config {
    let mut cfg = Config::default();
    if let Ok(v) = std::env::var("ROSETTA_BACKEND") {
        if let Some(b) = Backend::parse(&v) {
            cfg.backend = b;
        }
    }
    if let Ok(v) = std::env::var("ROSETTA_BEAMS") {
        if let Ok(n) = v.parse::<usize>() {
            cfg.beams = n.max(1);
        }
    }
    cfg
}

fn models_dir() -> Result<PathBuf> {
    match std::env::var("ROSETTA_MODELS") {
        Ok(d) => Ok(PathBuf::from(d)),
        Err(_) => paths::models_dir(),
    }
}

fn load_ocr() -> Result<ocr::OcrEngine> {
    let vendor = paths::vendor_dir()?;
    let mut e = ocr::OcrEngine::new(&vendor.join("bin"), &vendor.join("tessdata"), "heb")?;
    if let Ok(v) = std::env::var("ROSETTA_UPSCALE") {
        if let Ok(n) = v.parse::<u32>() {
            e.upscale = n.max(1);
        }
    }
    if let Ok(v) = std::env::var("ROSETTA_PSM") {
        if let Ok(n) = v.parse::<i32>() {
            e.psm = n;
        }
    }
    Ok(e)
}

fn load_bgra(path: &str) -> Result<(Vec<u8>, u32, u32)> {
    let img = image::open(path).with_context(|| format!("opening {path}"))?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    // Our capture path hands over BGRA, so match that here too.
    let mut bgra = rgba.into_raw();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Ok((bgra, w, h))
}

fn cmd_translate(rest: &[String]) -> Result<()> {
    let dir = models_dir()?;
    let cfg = env_cfg();
    let text = rest.join(" ");

    let t0 = std::time::Instant::now();
    let mut m = Marian::load(&dir, cfg.clone())?;
    let load = t0.elapsed();

    let t1 = std::time::Instant::now();
    let out = m.translate(&text)?;
    println!("[{} | {:?} | beams={}] load {load:?}", dir.display(), m.active_backend(), cfg.beams);
    println!("HE: {text}");
    println!("EN: {out}");
    println!("[translate {:?}]", t1.elapsed());
    Ok(())
}

fn cmd_ocr(rest: &[String]) -> Result<()> {
    let path = rest.first().context("usage: ocr <image.png>")?;
    let (bgra, w, h) = load_bgra(path)?;

    let t0 = std::time::Instant::now();
    let mut engine = load_ocr()?;
    let load = t0.elapsed();

    let t1 = std::time::Instant::now();
    let lines = engine.read_bgra(&bgra, w, h)?;
    let took = t1.elapsed();

    println!("[{path} {w}x{h} | upscale={} psm={}] load {load:?} ocr {took:?}", engine.upscale, engine.psm);
    for l in &lines {
        println!(
            "  ({:>4},{:>4} {:>4}x{:<4}) conf {:>5.1}  {}",
            l.rect.x, l.rect.y, l.rect.w, l.rect.h, l.confidence, l.text
        );
    }
    if lines.is_empty() {
        println!("  (no lines above confidence threshold)");
    }
    Ok(())
}

fn cmd_pipeline(rest: &[String]) -> Result<()> {
    let path = rest.first().context("usage: pipeline <image.png>")?;
    let (bgra, w, h) = load_bgra(path)?;

    let mut engine = load_ocr()?;
    let mut m = Marian::load(&models_dir()?, env_cfg())?;

    let t0 = std::time::Instant::now();
    let lines = engine.read_bgra(&bgra, w, h)?;
    let t_ocr = t0.elapsed();

    let t1 = std::time::Instant::now();
    let mut out = Vec::new();
    for l in &lines {
        out.push((l, m.translate(&l.text)?));
    }
    let t_mt = t1.elapsed();

    println!("[{path} {w}x{h}] ocr {t_ocr:?}, translate {t_mt:?}, {} lines", lines.len());
    for (l, en) in &out {
        println!("\n  box ({},{} {}x{}) conf {:.1}", l.rect.x, l.rect.y, l.rect.w, l.rect.h, l.confidence);
        println!("  HE  {}", l.text);
        println!("  EN  {en}");
    }
    Ok(())
}

/// Ask GitHub whether there is a newer release, and say so.
fn cmd_check_updates() -> Result<()> {
    let current = update::current_version();
    println!("running {current}, checking {} ...", update::repo());
    match update::check(current)? {
        Some(r) => {
            println!("update available: {} ({})", r.version, r.tag);
            println!("  {}", r.page_url);
            match (&r.installer_name, &r.installer_url) {
                (Some(n), Some(u)) => println!("  installer: {n}
  {u}"),
                _ => println!("  (no installer attached to that release)"),
            }
        }
        None => println!("up to date"),
    }
    Ok(())
}

/// Open just the settings window.
fn cmd_settings() -> Result<()> {
    ui::run_settings(&paths::vendor_dir()?)?;
    println!("Settings saved to {}", settings::Settings::path()?.display());
    println!("A running copy of Rosetta picks them up when you press Save; otherwise they apply at next start.");
    Ok(())
}

/// Launch the tray app.
fn cmd_run() -> Result<()> {
    let (settings, _) = settings::Settings::load();
    ui::run(models_dir()?, paths::vendor_dir()?, settings)
}

/// Capture a screen region and run the whole pipeline over it, optionally
/// saving the captured pixels for inspection.
fn cmd_grab(rest: &[String]) -> Result<()> {
    if rest.len() < 4 {
        bail!("usage: grab <x> <y> <w> <h> [out.png]");
    }
    let n = |i: usize| -> Result<i32> { Ok(rest[i].parse::<i32>()?) };
    let rect = geom::Rect::new(n(0)?, n(1)?, n(2)?, n(3)?);

    capture::enable_dpi_awareness();
    println!("virtual screen: {:?}", capture::virtual_screen());

    let mut cap = capture::ScreenCapture::new()?;
    let t0 = std::time::Instant::now();
    let frame = cap.grab(rect)?;
    let t_cap = t0.elapsed();
    println!("captured {:?} in {t_cap:?} (fingerprint {:x})", rect, frame.fingerprint());

    if let Some(out) = rest.get(4) {
        let mut rgba = frame.data.to_vec();
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        image::RgbaImage::from_raw(frame.width, frame.height, rgba)
            .context("building image")?
            .save(out)?;
        println!("wrote {out}");
    }

    let mut engine = load_ocr()?;
    let lines = engine.read_bgra(frame.data, frame.width, frame.height)?;
    println!("{} line(s)", lines.len());
    if lines.is_empty() {
        return Ok(());
    }

    let mut m = Marian::load(&models_dir()?, env_cfg())?;
    for l in &lines {
        println!("\n  box ({},{} {}x{}) conf {:.1}", l.rect.x, l.rect.y, l.rect.w, l.rect.h, l.confidence);
        println!("  HE  {}", l.text);
        println!("  EN  {}", m.translate(&l.text)?);
    }
    Ok(())
}

/// Compares the two axes that actually matter for quality: weight precision
/// (int8 vs fp32) and decoding strategy (greedy vs beam).
fn cmd_bench() -> Result<()> {
    let backend = env_cfg().backend;
    let variants: Vec<(&str, PathBuf, usize)> = vec![
        ("int8 greedy", PathBuf::from("models"), 1),
        ("int8 beam4 ", PathBuf::from("models"), 4),
        ("fp32 greedy", PathBuf::from("models_fp32"), 1),
        ("fp32 beam4 ", PathBuf::from("models_fp32"), 4),
    ];

    let mut models = Vec::new();
    for (label, dir, beams) in &variants {
        if !dir.join("model_meta.json").is_file() {
            println!("skipping {label}: {} not present", dir.display());
            continue;
        }
        let cfg = Config { backend, beams: *beams, ..Config::default() };
        let t = std::time::Instant::now();
        let m = Marian::load(dir, cfg)?;
        println!("loaded {label} ({:?}) in {:?}", m.active_backend(), t.elapsed());
        models.push((*label, m, std::time::Duration::ZERO));
    }

    for probe in PROBES {
        println!("\nHE          {probe}");
        let mut outs: Vec<String> = Vec::new();
        for (label, m, total) in models.iter_mut() {
            let s = std::time::Instant::now();
            let r = m.translate(probe)?;
            let d = s.elapsed();
            *total += d;
            println!("{label} {r}   [{d:?}]");
            outs.push(r);
        }
        let agree = outs.windows(2).all(|w| w[0] == w[1]);
        println!("all agree:  {}", if agree { "yes" } else { "NO" });
    }

    println!("\n--- totals over {} sentences ---", PROBES.len());
    for (label, _, total) in &models {
        println!("{label} {total:?}");
    }
    Ok(())
}
