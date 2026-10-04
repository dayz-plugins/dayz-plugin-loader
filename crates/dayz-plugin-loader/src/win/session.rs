//! `connect`, `disconnect` and `quit`: the console's control over the game session.
//!
//! `quit` goes through the window, not through the game's own `RequestExit` native, and that
//! is deliberate rather than a shortcut. Closing the window is what the game already answers
//! to, it needs nothing resolved at runtime, and it works in the menu and in a world alike.
//!
//! `connect` and `disconnect` cannot be done that way. Both are `CGame` natives — ordinary
//! member functions, so the loader could call them directly — but both need the `CGame`
//! instance as `this`, and that pointer has not been found yet. See `events.md` in the
//! research notes for what is known and what the next step is; until then these two say so
//! rather than pretending.

use dayz_plugin_core::console::SessionOp;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

/// Perform one session operation and return what the console should print.
pub(crate) fn perform(op: &SessionOp) -> Vec<String> {
    match op {
        SessionOp::Quit => quit(),
        SessionOp::Disconnect => vec![unsupported("disconnect", "CGame::DisconnectSession")],
        SessionOp::Connect(address) => vec![
            format!("cannot connect to {address} yet."),
            unsupported("connect", "CGame::Connect"),
        ],
    }
}

/// Ask the game to close, the way its own window button does.
fn quit() -> Vec<String> {
    let hwnd = super::game_window();
    if hwnd.is_null() {
        return vec!["the game window is not known yet; nothing to close".to_owned()];
    }
    // SAFETY: `hwnd` is the window the swapchain descriptor gave us, and `WM_CLOSE` carries
    // no parameters. Posted rather than sent: this runs on whichever thread typed the line,
    // and a synchronous send from outside the window's own thread would block on it.
    #[allow(unsafe_code)]
    let posted = unsafe { PostMessageW(Some(HWND(hwnd)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    match posted {
        Ok(()) => {
            log::info!("asked the game to close");
            vec!["closing the game.".to_owned()]
        }
        Err(e) => {
            log::warn!("could not post WM_CLOSE: {e}");
            vec![format!("could not ask the game to close: {e}")]
        }
    }
}

/// One line saying why a session command cannot act yet, naming what it needs.
fn unsupported(command: &str, native: &str) -> String {
    format!(
        "{command} needs the game object to call {native} on, and the loader cannot resolve \
         it on this build yet. See research/events.md."
    )
}
