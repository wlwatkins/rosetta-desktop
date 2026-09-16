//! Console attachment for a GUI-subsystem binary.
//!
//! The executable is linked as a Windows (GUI) application so that launching it
//! from the Start menu does not open a terminal window -- and, more to the
//! point, so that closing a terminal cannot kill the tray app.
//!
//! That would normally leave the command-line subcommands mute. So at start-up
//! we try to attach to the console of whatever launched us: run from a shell,
//! output lands in that shell; launched from Explorer or the installer, there is
//! no parent console, the attach fails, and nothing is shown.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AttachConsole, GetConsoleWindow, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS,
    STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

/// True when this process now has a console it can print to.
///
/// Call once, before anything is printed: Rust's standard streams resolve their
/// handles on first use and cache them.
pub fn attach_to_parent() -> bool {
    unsafe {
        // Already have one (a debug build, or someone allocated it for us).
        if !GetConsoleWindow().is_invalid() {
            return true;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return false;
        }

        // A GUI process starts with no standard handles, so attaching is not
        // enough: they have to be pointed at the console device by hand.
        //
        // Only the missing ones, though. A shell that redirected us -- a pipe,
        // a file -- hands over perfectly good handles, and replacing those with
        // CONOUT$ would send the output to the terminal instead of where it was
        // asked to go.
        fill_if_absent(STD_OUTPUT_HANDLE, open_conout);
        fill_if_absent(STD_ERROR_HANDLE, open_conout);
        fill_if_absent(STD_INPUT_HANDLE, open_conin);
        true
    }
}

/// Points a standard handle at the console, but only if it does not already
/// have one.
unsafe fn fill_if_absent(which: STD_HANDLE, open: unsafe fn() -> HANDLE) {
    unsafe {
        let current = GetStdHandle(which).unwrap_or(INVALID_HANDLE_VALUE);
        if !current.is_invalid() && !current.0.is_null() {
            return;
        }
        let fresh = open();
        if fresh != INVALID_HANDLE_VALUE {
            let _ = SetStdHandle(which, fresh);
        }
    }
}

unsafe fn open_conout() -> HANDLE {
    unsafe {
        CreateFileW(
            PCWSTR(windows::core::w!("CONOUT$").as_ptr()),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .unwrap_or(INVALID_HANDLE_VALUE)
    }
}

unsafe fn open_conin() -> HANDLE {
    unsafe {
        CreateFileW(
            PCWSTR(windows::core::w!("CONIN$").as_ptr()),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .unwrap_or(INVALID_HANDLE_VALUE)
    }
}

/// Reports a fatal error the user would otherwise never see.
///
/// With no console there is nowhere for `Error: ...` to go, so a launch that
/// fails -- a taken hotkey, a missing model -- would look like nothing happened.
pub fn report_fatal(has_console: bool, error: &anyhow::Error) {
    if has_console {
        eprintln!("Error: {error:#}");
        return;
    }
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND,
    };
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(format!("{error:#}")),
            &HSTRING::from("Rosetta could not start"),
            MB_ICONERROR | MB_OK | MB_SETFOREGROUND,
        );
    }
}
