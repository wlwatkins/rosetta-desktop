//! Minimal Unicode clipboard write.

use anyhow::{bail, Result};
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// Put `text` on the clipboard as CF_UNICODETEXT.
///
/// On success the system takes ownership of the allocation, so it must not be
/// freed here — only the failure paths release it.
pub fn set_text(owner: HWND, text: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * std::mem::size_of::<u16>();

    unsafe {
        OpenClipboard(Some(owner))?;
        let result = (|| -> Result<()> {
            EmptyClipboard()?;

            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes)?;
            let ptr = GlobalLock(handle) as *mut u16;
            if ptr.is_null() {
                bail!("GlobalLock failed");
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            // Returns an error once the lock count reaches zero, which is the
            // expected outcome here.
            let _ = GlobalUnlock(handle);

            SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(handle.0)))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}
