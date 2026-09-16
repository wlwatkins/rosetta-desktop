//! Tesseract bound at runtime through its C API.
//!
//! We load `libtesseract-5.dll` with `libloading` rather than linking it, which
//! keeps the build free of a C++ toolchain and lets the app ship the DLLs beside
//! the executable. `LOAD_WITH_ALTERED_SEARCH_PATH` makes Windows resolve
//! leptonica and the image codecs from the same vendored directory.

use anyhow::{bail, Context, Result};
use libloading::{Library, Symbol};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;

use super::{GrayImage, OcrLine};
use crate::geom::Rect;

const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;

/// TessPageIteratorLevel
const RIL_TEXTLINE: c_int = 2;

/// Assume-a-single-uniform-block; the selection box is usually exactly that.
pub const PSM_SINGLE_BLOCK: c_int = 6;

type FnCreate = unsafe extern "C" fn() -> *mut c_void;
type FnDelete = unsafe extern "C" fn(*mut c_void);
type FnEnd = unsafe extern "C" fn(*mut c_void);
type FnInit3 = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type FnSetVariable = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type FnSetPageSegMode = unsafe extern "C" fn(*mut c_void, c_int);
type FnSetImage =
    unsafe extern "C" fn(*mut c_void, *const u8, c_int, c_int, c_int, c_int);
type FnSetSourceResolution = unsafe extern "C" fn(*mut c_void, c_int);
type FnRecognize = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type FnGetIterator = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type FnIterDelete = unsafe extern "C" fn(*mut c_void);
type FnGetPageIterator = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type FnIterText = unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_char;
type FnIterConfidence = unsafe extern "C" fn(*mut c_void, c_int) -> f32;
type FnIterNext = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type FnIterBoundingBox = unsafe extern "C" fn(
    *mut c_void,
    c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
    *mut c_int,
) -> c_int;
type FnDeleteText = unsafe extern "C" fn(*mut c_char);

struct Api {
    create: FnCreate,
    delete: FnDelete,
    end: FnEnd,
    init3: FnInit3,
    set_variable: FnSetVariable,
    set_page_seg_mode: FnSetPageSegMode,
    set_image: FnSetImage,
    set_source_resolution: FnSetSourceResolution,
    recognize: FnRecognize,
    get_iterator: FnGetIterator,
    iter_delete: FnIterDelete,
    get_page_iterator: FnGetPageIterator,
    iter_text: FnIterText,
    iter_confidence: FnIterConfidence,
    iter_next: FnIterNext,
    iter_bounding_box: FnIterBoundingBox,
    delete_text: FnDeleteText,
}

pub struct Tesseract {
    // Declared before `_lib` so the handle is torn down before the DLL unloads.
    handle: *mut c_void,
    api: Api,
    _lib: Library,
}

// The handle is only ever touched through `&mut self`, and we never share one
// across threads without moving it.
unsafe impl Send for Tesseract {}

impl Tesseract {
    /// `bin_dir` holds libtesseract-5.dll and friends; `tessdata` holds the
    /// `*.traineddata` files.
    pub fn new(bin_dir: &Path, tessdata: &Path, lang: &str) -> Result<Self> {
        let dll = bin_dir.join("libtesseract-5.dll");
        if !dll.is_file() {
            bail!("missing {}", dll.display());
        }
        let data_file = tessdata.join(format!("{lang}.traineddata"));
        if !data_file.is_file() {
            bail!("missing {}", data_file.display());
        }

        let lib = unsafe {
            libloading::os::windows::Library::load_with_flags(
                &dll,
                LOAD_WITH_ALTERED_SEARCH_PATH,
            )
        }
        .with_context(|| format!("loading {}", dll.display()))?;
        let lib: Library = lib.into();

        unsafe {
            macro_rules! sym {
                ($name:literal, $t:ty) => {{
                    let s: Symbol<$t> = lib
                        .get($name)
                        .with_context(|| format!("resolving {}", stringify!($name)))?;
                    *s
                }};
            }

            let api = Api {
                create: sym!(b"TessBaseAPICreate\0", FnCreate),
                delete: sym!(b"TessBaseAPIDelete\0", FnDelete),
                end: sym!(b"TessBaseAPIEnd\0", FnEnd),
                init3: sym!(b"TessBaseAPIInit3\0", FnInit3),
                set_variable: sym!(b"TessBaseAPISetVariable\0", FnSetVariable),
                set_page_seg_mode: sym!(b"TessBaseAPISetPageSegMode\0", FnSetPageSegMode),
                set_image: sym!(b"TessBaseAPISetImage\0", FnSetImage),
                set_source_resolution: sym!(
                    b"TessBaseAPISetSourceResolution\0",
                    FnSetSourceResolution
                ),
                recognize: sym!(b"TessBaseAPIRecognize\0", FnRecognize),
                get_iterator: sym!(b"TessBaseAPIGetIterator\0", FnGetIterator),
                iter_delete: sym!(b"TessResultIteratorDelete\0", FnIterDelete),
                get_page_iterator: sym!(
                    b"TessResultIteratorGetPageIterator\0",
                    FnGetPageIterator
                ),
                iter_text: sym!(b"TessResultIteratorGetUTF8Text\0", FnIterText),
                iter_confidence: sym!(b"TessResultIteratorConfidence\0", FnIterConfidence),
                iter_next: sym!(b"TessPageIteratorNext\0", FnIterNext),
                iter_bounding_box: sym!(b"TessPageIteratorBoundingBox\0", FnIterBoundingBox),
                delete_text: sym!(b"TessDeleteText\0", FnDeleteText),
            };

            let handle = (api.create)();
            if handle.is_null() {
                bail!("TessBaseAPICreate returned null");
            }

            let datapath = CString::new(tessdata.to_string_lossy().as_ref())?;
            let language = CString::new(lang)?;
            if (api.init3)(handle, datapath.as_ptr(), language.as_ptr()) != 0 {
                (api.delete)(handle);
                bail!("TessBaseAPIInit3 failed for lang `{lang}` at {}", tessdata.display());
            }

            Ok(Self { handle, api, _lib: lib })
        }
    }

    pub fn set_variable(&mut self, key: &str, value: &str) -> Result<()> {
        let k = CString::new(key)?;
        let v = CString::new(value)?;
        let ok = unsafe { (self.api.set_variable)(self.handle, k.as_ptr(), v.as_ptr()) };
        if ok == 0 {
            bail!("SetVariable({key}={value}) rejected");
        }
        Ok(())
    }

    /// Recognize an 8-bit grayscale image, returning one entry per text line.
    pub fn recognize(&mut self, img: &GrayImage, psm: c_int) -> Result<Vec<OcrLine>> {
        if img.width == 0 || img.height == 0 {
            return Ok(Vec::new());
        }

        unsafe {
            (self.api.set_page_seg_mode)(self.handle, psm);
            (self.api.set_image)(
                self.handle,
                img.data.as_ptr(),
                img.width as c_int,
                img.height as c_int,
                1,
                img.width as c_int,
            );
            // Screen pixels carry no DPI metadata; telling Tesseract a plausible
            // value stops it warning and improves its layout heuristics.
            (self.api.set_source_resolution)(self.handle, 96 * img.scale.max(1) as c_int);

            if (self.api.recognize)(self.handle, std::ptr::null_mut()) != 0 {
                bail!("TessBaseAPIRecognize failed");
            }

            let iter = (self.api.get_iterator)(self.handle);
            if iter.is_null() {
                return Ok(Vec::new());
            }
            let page = (self.api.get_page_iterator)(iter);
            if page.is_null() {
                (self.api.iter_delete)(iter);
                return Ok(Vec::new());
            }

            let mut lines = Vec::new();
            loop {
                let text_ptr = (self.api.iter_text)(iter, RIL_TEXTLINE);
                if !text_ptr.is_null() {
                    let text = CStr::from_ptr(text_ptr).to_string_lossy().trim().to_string();
                    let conf = (self.api.iter_confidence)(iter, RIL_TEXTLINE);
                    (self.api.delete_text)(text_ptr);

                    let (mut l, mut t, mut r, mut b) = (0, 0, 0, 0);
                    let got = (self.api.iter_bounding_box)(
                        page,
                        RIL_TEXTLINE,
                        &mut l,
                        &mut t,
                        &mut r,
                        &mut b,
                    );
                    if got != 0 && !text.is_empty() {
                        // Box coordinates are in upscaled image space; map back down.
                        let s = img.scale.max(1) as i32;
                        lines.push(OcrLine {
                            text,
                            confidence: conf,
                            rect: Rect::new(l / s, t / s, (r - l) / s, (b - t) / s),
                        });
                    }
                }
                if (self.api.iter_next)(page, RIL_TEXTLINE) == 0 {
                    break;
                }
            }

            (self.api.iter_delete)(iter);
            Ok(lines)
        }
    }
}

impl Drop for Tesseract {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                // End() releases the language data back to Tesseract's static
                // ObjectCache. Skipping it leaves the cache holding references at
                // process teardown, which it reports as "WARNING! LEAK!".
                (self.api.end)(self.handle);
                (self.api.delete)(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}
