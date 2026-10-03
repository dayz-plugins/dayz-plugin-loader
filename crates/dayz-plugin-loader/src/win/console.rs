//! The `--console` window: a real Win32 console that carries the loader log, the in-game
//! console output and whatever the game writes to its own standard streams.

// FFI module: console allocation and stream redirection.
#![allow(unsafe_code)]

use std::io::Write;
use std::sync::OnceLock;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AllocConsole, GetConsoleWindow, SetConsoleCtrlHandler, SetConsoleOutputCP, SetConsoleTitleW,
    SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

/// UTF-8 code page, so log lines with non-ASCII text are not mangled.
const CP_UTF8: u32 = 65001;

static ACTIVE: OnceLock<bool> = OnceLock::new();

/// Whether a console window is attached to this process.
pub(crate) fn is_active() -> bool {
    *ACTIVE.get().unwrap_or(&false)
}

/// Attach a console and point the process's standard streams at it.
///
/// The game is a GUI subsystem binary, so it has no console and its `stdout`/`stderr`
/// writes go nowhere. Allocating one and rebinding the standard handles makes both the
/// loader's output and the game's own appear in the same window.
pub(crate) fn attach() -> bool {
    *ACTIVE.get_or_init(|| {
        // SAFETY: AllocConsole has no preconditions; it fails if a console already exists.
        let allocated = unsafe { AllocConsole() }.is_ok();
        // SAFETY: GetConsoleWindow has no preconditions.
        if !allocated && unsafe { GetConsoleWindow() }.is_invalid() {
            return false;
        }
        // SAFETY: both take plain values; failures are cosmetic and ignored deliberately.
        unsafe {
            let _ = SetConsoleOutputCP(CP_UTF8);
            let _ = SetConsoleTitleW(w!("DayZ plugin loader"));
            // Ctrl+C in this window would kill the game; swallow the signals instead.
            let _ = SetConsoleCtrlHandler(None, true);
        }
        bind_streams();
        true
    })
}

/// Point the process standard handles at the console device.
fn bind_streams() {
    for (handle, name, write) in [
        (STD_OUTPUT_HANDLE, w!("CONOUT$"), true),
        (STD_ERROR_HANDLE, w!("CONOUT$"), true),
        (STD_INPUT_HANDLE, w!("CONIN$"), false),
    ] {
        let access = if write {
            GENERIC_WRITE.0
        } else {
            GENERIC_READ.0
        };
        // SAFETY: `name` is a static NUL terminated wide string; the remaining arguments are
        // the documented values for opening the console device.
        let file = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        match file {
            // SAFETY: `h` is a handle we just opened and intentionally leak: the standard
            // handle must stay valid for the process's lifetime.
            Ok(h) => unsafe {
                if let Err(e) = SetStdHandle(handle, h) {
                    let _ = writeln!(std::io::stderr(), "could not rebind {name:?}: {e}");
                }
            },
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "could not open console device: {e}");
            }
        }
    }
}

/// Write one line to the console window, if there is one.
pub(crate) fn print(line: &str) {
    if !is_active() {
        return;
    }
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
