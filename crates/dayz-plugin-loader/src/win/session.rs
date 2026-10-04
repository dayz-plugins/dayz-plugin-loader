//! `connect`, `disconnect`, `quit` and local chat: the loader's control over the game.
//!
//! `quit` goes through the window, not through the game's own `RequestExit` native, and that
//! is deliberate rather than a shortcut. Closing the window is what the game already answers
//! to, it needs nothing resolved at runtime, and it works in the menu and in a world alike.
//!
//! The other three are `CGame` natives — ordinary member functions, so the loader can call
//! them directly once it has the instance to call them on. It gets that from
//! [`super::game_events`], which catches it from the engine's own remote-call dispatch; see
//! there for why that is the pointer and not a guess at one, and for what it costs.
//!
//! Their signatures come from the shipped scripts, and the one thing worth saying about them
//! is that the `string` parameters arrive as **plain C strings**: `chat.local` passes what it
//! is given straight to the engine's string-holder constructor, which walks it to the NUL. So
//! nothing here has to build a holder, and nothing has to release one.
//!
//! ```text
//! Connect(UIScriptedMenu parent, string ip, int port, string password) -> int
//! DisconnectSession()
//! Chat(string text, string colorClass)
//! ```

// FFI module: calls the game's own member functions.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::ffi::CString;

use dayz_plugin_core::console::SessionOp;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

/// `int Connect(CGame *this, UIScriptedMenu *parent, const char *ip, int port, const char *password)`.
type ConnectFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *const u8, i32, *const u8) -> i32;
/// `void DisconnectSession(CGame *this)`.
type DisconnectFn = unsafe extern "C" fn(*mut c_void);
/// `void Chat(CGame *this, const char *text, const char *colourClass)`.
type ChatFn = unsafe extern "C" fn(*mut c_void, *const u8, *const u8);

/// Colour class used when a caller does not name one. The game's own default for script chat.
const DEFAULT_COLOUR: &str = "ColorImportant";

/// Perform one session operation and return what the console should print.
pub(crate) fn perform(op: &SessionOp) -> Vec<String> {
    match op {
        SessionOp::Quit => quit(),
        SessionOp::Disconnect => vec![disconnect()],
        SessionOp::Connect(address) => vec![connect(&address.host, address.port)],
    }
}

/// Resolve one `CGame` native, with the game object to call it on.
///
/// Both halves have to be there: the address comes from `dayz-data` and the instance from a
/// hook that may not have fired yet, and the two failures need different answers.
fn native(symbol: &str) -> Result<(*mut c_void, usize), String> {
    let Some(game) = super::game_events::game_instance() else {
        return Err(
            "the game object is not known yet; it is caught from the engine's first \
                    remote call, so join a server once this session"
                .to_owned(),
        );
    };
    let Some(resolved) = super::data::resolved() else {
        return Err("no symbol database for this build".to_owned());
    };
    let Some(entry) = resolved.table.symbol(symbol) else {
        return Err(format!("{symbol} is not in the database for this build"));
    };
    let Ok(rva) = usize::try_from(entry.rva) else {
        return Err(format!("{symbol} has an address this process cannot hold"));
    };
    let Some(address) = (resolved.module_base as usize).checked_add(rva) else {
        return Err(format!("{symbol} resolves outside the image"));
    };
    Ok((game, address))
}

/// Leave the current server.
fn disconnect() -> String {
    let (game, address) = match native("session.disconnect") {
        Ok(found) => found,
        Err(why) => return format!("cannot disconnect: {why}"),
    };
    // SAFETY: `session.disconnect` is a `CGame` member taking only `this`, and `game` is the
    // instance the engine itself passed to another of its members. Called on whichever
    // thread typed the line, which is what the game's own UI does too.
    unsafe {
        let call: DisconnectFn = core::mem::transmute(address);
        call(game);
    }
    log::info!("asked the game to leave the server");
    "leaving the server.".to_owned()
}

/// Join a server.
fn connect(host: &str, port: u16) -> String {
    let (game, address) = match native("session.connect") {
        Ok(found) => found,
        Err(why) => return format!("cannot connect: {why}"),
    };
    let (Ok(host_c), Ok(empty)) = (CString::new(host), CString::new("")) else {
        return "that host name cannot be passed to the game".to_owned();
    };
    // SAFETY: `session.connect` is a `CGame` member with the signature above; `game` is the
    // instance the engine passed us, both strings are NUL terminated and outlive the call,
    // and a null parent is what the game passes when no menu owns the attempt.
    let result = unsafe {
        let call: ConnectFn = core::mem::transmute(address);
        call(
            game,
            core::ptr::null_mut(),
            host_c.as_ptr().cast::<u8>(),
            i32::from(port),
            empty.as_ptr().cast::<u8>(),
        )
    };
    log::info!("asked the game to connect to {host}:{port}, which answered {result}");
    format!("connecting to {host}:{port} (the game answered {result}).")
}

/// Put one line in this client's own chat, which nobody else sees.
pub(crate) fn chat_local(text: &str, colour: &str) -> Result<(), String> {
    let (game, address) = native("chat.local")?;
    let colour = if colour.is_empty() {
        DEFAULT_COLOUR
    } else {
        colour
    };
    let (Ok(text_c), Ok(colour_c)) = (CString::new(text), CString::new(colour)) else {
        return Err("a chat line cannot contain a NUL".to_owned());
    };
    // SAFETY: `chat.local` is a `CGame` member taking two C strings, which it copies into
    // engine string holders before returning; both outlive the call.
    unsafe {
        let call: ChatFn = core::mem::transmute(address);
        call(
            game,
            text_c.as_ptr().cast::<u8>(),
            colour_c.as_ptr().cast::<u8>(),
        );
    }
    Ok(())
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
