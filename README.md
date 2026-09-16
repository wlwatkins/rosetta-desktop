# rosetta-desktop

A Windows tray app that translates Hebrew text on screen to English, in place.

Press `Ctrl+Alt+T` (configurable), drag a box over Hebrew text, and the translation is painted
over the original. Drag or resize the box and it re-translates whatever it now
covers. There is no application window — just the tray icon and the overlay,
like Lightshot or Sleekshot.

Everything runs locally. The shipped binary needs no Python and makes no network
calls.

## Running it

```
cargo run --release
```

The tray icon appears; `Ctrl+Alt+T` starts a selection, `Esc` dismisses it,
right-click the tray icon for a menu. On a multi-monitor setup the "drag to
select" prompt is drawn at the centre of every display, not at the centre of the
virtual desktop (which falls on the seam between screens).

Under the selection sits a toolbar:

| button | action |
| --- | --- |
| copy **HE** | copy the recognized Hebrew to the clipboard, in reading order |
| copy **EN** | copy the English translations |
| refresh | force a re-scan, ignoring the unchanged-pixels shortcut |
| close | dismiss the overlay |

The refresh button becomes a spinner while the pipeline is working. Icons are
drawn as vectors, so they stay crisp at any DPI and pull in no icon font.

To use a different shortcut:

```
ROSETTA_HOTKEY="ctrl+shift+t" cargo run --release
```

Accepts any combination of `ctrl`, `alt`, `shift`, `win` plus a letter, a digit,
or `f1`-`f24`. At least one modifier is required. Note that `Ctrl+Shift+T` is
"reopen closed tab" in most browsers, and a global hotkey takes priority over
the focused app.

### Dev commands

The same binary exposes the pipeline for testing without the UI:

```
rosetta-desktop translate "שלום עולם"     # text -> English
rosetta-desktop ocr <image.png>            # OCR only, with line boxes
rosetta-desktop pipeline <image.png>       # OCR -> translate
rosetta-desktop grab <x> <y> <w> <h> [out.png]   # capture a screen region and run both
rosetta-desktop bench                      # int8 vs fp32 x greedy vs beam
```

### Environment knobs

| variable | default | meaning |
| --- | --- | --- |
| `ROSETTA_HOTKEY` | `ctrl+alt+t` | activation shortcut, e.g. `ctrl+shift+t` |
| `ROSETTA_BACKEND` | `cpu` | `cpu`, `dml` (DirectML), `cuda` (needs `--features cuda`) |
| `ROSETTA_BEAMS` | `4` | beam width; `1` is greedy |
| `ROSETTA_MODELS` | `models/` | model directory |
| `ROSETTA_THEME` | `dark` | `dark` or `light` |
| `ROSETTA_UPSCALE` | `2` | how much to upscale before OCR |
| `ROSETTA_PSM` | `6` | Tesseract page-segmentation mode |
| `ROSETTA_LANG` | `heb` | Tesseract language |
| `ROSETTA_DEBUG` | off | log every scan and its translations |
| `ROSETTA_NO_EXCLUDE` | off | let the overlay appear in screenshots (debug only) |

## Build-time setup

Two one-time scripts produce the assets the binary loads. Neither is needed at
runtime, and neither is committed -- both output directories are gitignored.

**1. Translation model.** `Helsinki-NLP/opus-mt-tc-big-he-en` publishes no ONNX
build, so it is converted locally. This is the only place Python appears, and
nothing from it ships:

```
tools/setup_model.sh                # -> models/ (~354MB int8)
tools/setup_model.sh --keep-fp32    # also -> models_fp32/, for `bench`
```

Creates `.venv-export/`, exports to ONNX, quantizes to int8, and builds the
tokenizer JSON. The ~3GB of fp32 intermediates are deleted afterwards unless you
pass `--keep-raw`.

**2. OCR runtime.** Windows' built-in OCR has no Hebrew, so Tesseract is
vendored beside the executable:

```
tools/vendor_tesseract.sh           # -> vendor/ (~133MB)
```

Installs Tesseract if missing, copies its DLLs, fetches `heb`/`eng` from
tessdata_best, then prunes to the DLLs `libtesseract-5.dll` actually imports --
the installer ships the whole pango/cairo/icu stack for its PDF tools, which is
46MB of dead weight here. Tesseract is loaded at runtime via `libloading`, so
the build needs no C++ toolchain.

Run `python tools/prune_vendor.py` on its own for a dry-run report of which
vendored DLLs are reachable.

## How it fits together

```
src/
  capture.rs    BitBlt screen grab + DPI awareness + capture exclusion
  ocr/          Tesseract C API via libloading; grayscale, upscale, auto-invert
  translate/    Marian encoder/decoder over ONNX Runtime, beam search
  stabilize.rs  temporal smoothing of OCR results
  worker.rs     capture -> OCR -> translate, off the UI thread
  ui/           tray, hotkeys, selection overlay, Direct2D rendering
```

Three details carry most of the design:

**The overlay is invisible to screen capture.** It sets
`WDA_EXCLUDEFROMCAPTURE`, so when the worker re-grabs the region the overlay is
sitting on, it gets the original Hebrew rather than the English we just painted
there. Without this the live loop feeds on its own output and collapses within a
frame or two.

**OCR results are smoothed over time.** Tesseract redoes layout analysis every
frame, so two scans of identical content can disagree — a line merges with its
neighbour, or its confidence dips and it vanishes. Drawn directly, that reads as
translations flickering in and out. `stabilize.rs` tracks lines in screen
coordinates and holds a missing one for a few frames before retiring it.

**Translations are cached by source string.** Nudging the box re-OCRs but almost
never re-translates, which is what keeps dragging responsive.

## Model configuration, measured

Benchmarked with `rosetta-desktop bench` on 7 sentences (CPU, this machine):

| config | long sentence | agreement |
| --- | --- | --- |
| int8 + beam4 | 325 ms | — |
| fp32 + beam4 | 787 ms | identical to int8 |
| fp32 + DirectML | 1197 ms | identical |
| int8 + DirectML | 3978 ms | identical |

int8 and fp32 produced byte-identical output on every probe. The quality
difference that *does* exist is greedy vs beam search — greedy gives "postponed
for the next week" where beam4 gives "postponed until next week". So the default
is **int8 + beam4 on CPU**: same output as fp32, roughly 3x faster.

DirectML loses badly because each decode step is tiny (batch 4, one token) and
the KV caches cross PCIe every step; launch overhead dominates. Making the GPU
pay off would need IO-binding to keep the caches device-resident.

## Known limitations

- **Per-line translation.** Each OCR line is translated independently, so a
  paragraph wrapped across several lines loses cross-line context. Sentence
  reassembly would improve quality but complicates positioning.
- **Opaque chips.** The translation panels cannot be frosted like the rest of
  the chrome -- they are covering the Hebrew, and any translucency lets it show
  through.
- Hebrew OCR quality is Tesseract's; dense or low-contrast text degrades.

## Repository layout

Only source and configuration are tracked. Everything heavy is generated:

| path | size | tracked | rebuild with |
| --- | --- | --- | --- |
| `src/`, `tools/` | ~200KB | yes | -- |
| `models/` | 354MB | no | `tools/setup_model.sh` |
| `models_fp32/` | 1.4GB | no | `tools/setup_model.sh --keep-fp32` |
| `vendor/` | 133MB | no | `tools/vendor_tesseract.sh` |
| `.venv-export/` | 794MB | no | `tools/setup_model.sh` |
| `target/` | ~4GB | no | `cargo build --release` |

`models_fp32/` exists only so `bench` can compare precisions; delete it and
`bench` simply skips those rows.
