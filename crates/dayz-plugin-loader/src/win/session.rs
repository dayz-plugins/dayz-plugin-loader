//! `connect`, `disconnect`, `quit` and local chat: the loader's control over the game.
//!
//! `quit` goes through the window, not through the game's own `RequestExit` native, and that
//! is deliberate rather than a shortcut. Closing the window is what the game already answers
//! to, it needs nothing resolved at runtime, and it works in the menu and in a world alike.
//!
//! The other three are `CGame` natives, and the loader has the instance to call them on —
//! [`super::game_events`] catches it from the engine's own hooks; see there for why that is
//! the pointer and not a guess at one.
//!
//! ## They are called on the next frame, not now
//!
//! A `CGame` member is engine code that expects to be running on the engine's own thread.
//! A console line does not: it arrives on whichever thread typed it, which for the external
//! console or the debug plugin's socket is not the game's. Calling `Connect` from there
//! deadlocked a live session — the game kept rendering the frame it was on and never drew
//! another, and the socket thread never returned from the call.
//!
//! So nothing here calls the engine. Each request goes in a queue and [`run_pending`] drains
//! it from the loader's per-frame tick, which runs inside `Present` and therefore on the
//! thread the engine itself calls it on. The cost is that the console answers "asked" before
//! the engine has done it, and the answer is in the log a frame later. That is the right
//! trade: the alternative was a call that works from one thread and hangs the game from
//! another, with nothing in the signature to say which.
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

use std::sync::Mutex;

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

/// Work waiting to be done on the game's own thread.
///
/// A queue rather than a single slot because two lines typed quickly should both happen, and
/// a `Vec` that is almost always empty costs one uncontended lock per frame.
static PENDING: Mutex<Vec<Request>> = Mutex::new(Vec::new());

/// One deferred call into the engine.
enum Request {
    Disconnect,
    Connect { host: String, port: u16 },
    Chat { text: String, colour: String },
}

/// Perform one session operation and return what the console should print.
pub(crate) fn perform(op: &SessionOp) -> Vec<String> {
    match op {
        // `quit` is the exception and stays immediate: it posts a window message rather than
        // calling the engine, and posting is safe from any thread — that is what posting is.
        SessionOp::Quit => quit(),
        SessionOp::Disconnect => vec![enqueue(Request::Disconnect)],
        SessionOp::Connect(address) => vec![enqueue(Request::Connect {
            host: address.host.clone(),
            port: address.port,
        })],
    }
}

/// Put one request in the queue, refusing it now if it could not possibly work.
///
/// Checked here rather than on the game thread so that a console line gets a real answer:
/// "the game object is not known yet" is worth saying to whoever typed it, and a frame later
/// in the log is not the same thing.
fn enqueue(request: Request) -> String {
    let described = match &request {
        Request::Disconnect => "leave the server".to_owned(),
        Request::Connect { host, port } => format!("connect to {host}:{port}"),
        Request::Chat { .. } => "say something".to_owned(),
    };
    if super::game_events::game_instance().is_none() {
        return format!(
            "cannot {described}: the game object is not known yet. The loader catches it from \
             the engine's own chat and remote-call paths, so it arrives once a session has \
             started."
        );
    }
    queue().push(request);
    format!("asked the game to {described}; it happens on the next frame.")
}

/// Borrow the queue, taking a poisoned lock as readable: it holds plain values.
fn queue() -> std::sync::MutexGuard<'static, Vec<Request>> {
    PENDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Do everything that was asked for. Called from the per-frame tick, on the game's thread.
pub(crate) fn run_pending() {
    // Taken rather than drained in place, so nothing holds the lock while engine code runs:
    // `Connect` can take a while, and a console line typed meanwhile should queue, not block.
    let requests = core::mem::take(&mut *queue());
    for request in requests {
        let outcome = match request {
            Request::Disconnect => disconnect(),
            Request::Connect { host, port } => connect(&host, port),
            Request::Chat { text, colour } => match chat_local(&text, &colour) {
                Ok(()) => "said it".to_owned(),
                Err(why) => why,
            },
        };
        log::info!("{outcome}");
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

/// Ask for one line in this client's own chat, on the next frame.
///
/// This is what the host API calls. A plugin may be on the game's thread already — in
/// `on_game_event` it always is — but it may equally be on one of its own, and the ABI says
/// nothing about which. Queuing is the only answer that is right for both.
pub(crate) fn request_chat(text: &str, colour: &str) -> Result<(), String> {
    if super::game_events::game_instance().is_none() {
        return Err("the game object is not known yet".to_owned());
    }
    queue().push(Request::Chat {
        text: text.to_owned(),
        colour: colour.to_owned(),
    });
    Ok(())
}

/// Put one line in this client's own chat, which nobody else sees. On the game's thread.
fn chat_local(text: &str, colour: &str) -> Result<(), String> {
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
