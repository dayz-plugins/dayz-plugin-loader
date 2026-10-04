//! Export glue generated into every plugin by `export_plugin!`: builds the callback table,
//! owns the plugin instance and guards each callback against panics.

// Second and last unsafe module of the SDK: this is where loader pointers become references.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;

use dayz_plugin_api::{
    self as api, Bytes, HostApi, LogLevel, PluginCallbacks, PluginHandle, PluginInfo, ReplyFn,
    Status, StopReason, Str, API_VERSION,
};

use crate::dialogs::Shown;
use crate::game::{Chat, ChatVerdict, Event, Rpc};
use crate::host::{bytes_from, str_from, Host, PluginError, PluginRef};
use crate::input::Input;
use crate::logger::HostLogger;
use crate::plugin::{Plugin, PresentInfo, SwapchainInfo};
use crate::ui::Ui;

/// Marker trait proving a type can be exported; blanket-implemented for every [`Plugin`].
pub trait Exports: Plugin {}
impl<P: Plugin> Exports for P {}

/// Storage for the single instance of a plugin type inside its DLL.
pub struct Slot<P: Plugin> {
    state: OnceLock<Box<State<P>>>,
}

struct State<P> {
    host: Host,
    plugin: P,
}

impl<P: Plugin> Slot<P> {
    /// Create an empty slot (used by the macro in a `static`).
    #[must_use]
    pub const fn new() -> Self {
        Slot {
            state: OnceLock::new(),
        }
    }
}

impl<P: Plugin> Default for Slot<P> {
    fn default() -> Self {
        Self::new()
    }
}

/// `PluginInfo` holds raw string pointers, so it is not automatically `Sync`.
struct StaticInfo(PluginInfo);

/// The dependency list in ABI form, leaked so the loader may read it at any time.
///
/// Leaking is the point: the ABI requires the array to outlive the describe call, and a
/// plugin DLL is never unloaded while the game runs.
fn dependencies<P: Plugin>() -> (*const api::Dependency, usize) {
    if P::DEPENDENCIES.is_empty() {
        return (core::ptr::null(), 0);
    }
    let list: &'static [api::Dependency] = Vec::leak(
        P::DEPENDENCIES
            .iter()
            .map(|d| d.to_api())
            .collect::<Vec<_>>(),
    );
    (list.as_ptr(), list.len())
}

// SAFETY: every `Str` inside points at a `&'static str` compiled into the plugin and the
// dependency array is leaked, so everything reachable is immutable and lives for the whole
// process; reading it from any thread is safe.
unsafe impl Sync for StaticInfo {}
// SAFETY: see the `Sync` impl; `OnceLock` additionally requires `Send` of its contents.
unsafe impl Send for StaticInfo {}

static INFO: OnceLock<StaticInfo> = OnceLock::new();

/// Body of the generated `dayz_plugin_describe`.
pub fn describe<P: Plugin>() -> *const PluginInfo {
    let info = INFO.get_or_init(|| {
        let (dependencies, dependency_count) = dependencies::<P>();
        StaticInfo(PluginInfo {
            struct_size: core::mem::size_of::<PluginInfo>(),
            api_version: API_VERSION,
            name: Str::new(P::NAME),
            version: Str::new(P::VERSION),
            description: Str::new(P::DESCRIPTION),
            dependencies,
            dependency_count,
        })
    });
    core::ptr::from_ref(&info.0)
}

/// Body of the generated `dayz_plugin_start`.
///
/// # Safety
/// `host` must point to a table valid for the plugin's lifetime and `callbacks` to writable memory.
pub unsafe fn start<P: Plugin>(
    slot: &'static Slot<P>,
    host: *const HostApi,
    handle: PluginHandle,
    callbacks: *mut PluginCallbacks,
) -> Status {
    if host.is_null() || callbacks.is_null() {
        return Status::InvalidArgument;
    }
    // SAFETY: checked non-null; the loader keeps the table alive for our lifetime.
    let api = unsafe { &*host };
    if api.api_version != API_VERSION || api.struct_size < core::mem::size_of::<HostApi>() {
        return Status::Unsupported;
    }
    let host = Host::new(api, handle);
    HostLogger::install(host);

    let outcome = catch_unwind(AssertUnwindSafe(|| P::start(host)));
    let plugin = match outcome {
        Ok(Ok(plugin)) => plugin,
        Ok(Err(e)) => {
            host.log(LogLevel::Error, &format!("start failed: {e}"));
            return Status::Error;
        }
        Err(_) => {
            host.log(LogLevel::Error, "start panicked");
            return Status::Error;
        }
    };
    let state = Box::new(State { host, plugin });
    if slot.state.set(state).is_err() {
        host.log(LogLevel::Error, "start called twice");
        return Status::WrongPhase;
    }
    let Some(state) = slot.state.get() else {
        return Status::Error;
    };
    // SAFETY: the loader hands us a writable PluginCallbacks of at least its struct_size.
    unsafe {
        callbacks.write(PluginCallbacks {
            struct_size: core::mem::size_of::<PluginCallbacks>(),
            ctx: core::ptr::from_ref::<State<P>>(state).cast_mut().cast(),
            on_swapchain: Some(on_swapchain::<P>),
            on_present: Some(on_present::<P>),
            on_resize: Some(on_resize::<P>),
            on_hotkey: Some(on_hotkey::<P>),
            on_setting_changed: Some(on_setting_changed::<P>),
            on_command: Some(on_command::<P>),
            on_message: Some(on_message::<P>),
            on_event: Some(on_event::<P>),
            on_enable: Some(on_enable::<P>),
            on_disable: Some(on_disable::<P>),
            on_ui: Some(on_ui::<P>),
            on_dialog: Some(on_dialog::<P>),
            on_input: Some(on_input::<P>),
            on_game_event: Some(on_game_event::<P>),
            on_chat: Some(on_chat::<P>),
            on_rpc: Some(on_rpc::<P>),
        });
    }
    Status::Ok
}

/// Body of the generated `dayz_plugin_stop`.
///
/// # Safety
/// `ctx` must be the pointer written by [`start`].
pub unsafe fn stop<P: Plugin>(slot: &'static Slot<P>, ctx: *mut c_void, reason: StopReason) {
    let Some(state) = slot.state.get() else {
        return;
    };
    if core::ptr::from_ref::<State<P>>(state)
        .cast_mut()
        .cast::<c_void>()
        != ctx
    {
        return;
    }
    guard(state, "stop", |s| {
        s.plugin.stop(&s.host, reason);
    });
}

unsafe extern "C" fn on_enable<P: Plugin>(ctx: *mut c_void) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    guard(s, "on_enable", |s| s.plugin.on_enable(&s.host));
}

unsafe extern "C" fn on_disable<P: Plugin>(ctx: *mut c_void) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    guard(s, "on_disable", |s| s.plugin.on_disable(&s.host));
}

/// Run a callback, turning a panic into a log line instead of unwinding into the loader.
fn guard<P: Plugin, R>(state: &State<P>, what: &str, f: impl FnOnce(&State<P>) -> R) -> Option<R> {
    catch_unwind(AssertUnwindSafe(|| f(state)))
        .map_err(|_| {
            state.host.log(LogLevel::Error, &format!("panic in {what}"));
        })
        .ok()
}

/// # Safety
/// `ctx` must be the `State<P>` written by [`start`].
unsafe fn state<'a, P: Plugin>(ctx: *mut c_void) -> &'a State<P> {
    // SAFETY: the loader only passes back the ctx we gave it, which points into a leaked Box.
    unsafe { &*ctx.cast::<State<P>>().cast_const() }
}

unsafe extern "C" fn on_swapchain<P: Plugin>(ctx: *mut c_void, info: *const api::SwapchainInfo) {
    if info.is_null() {
        return;
    }
    // SAFETY: documented pointer contracts of this callback.
    let (s, i) = unsafe { (state::<P>(ctx), &*info) };
    let info = SwapchainInfo {
        swapchain: i.swapchain,
        device: i.device,
        hwnd: i.hwnd,
        width: i.width,
        height: i.height,
    };
    guard(s, "on_swapchain", |s| {
        s.plugin.on_swapchain(&s.host, &info);
    });
}

unsafe extern "C" fn on_present<P: Plugin>(ctx: *mut c_void, info: *const api::PresentInfo) {
    if info.is_null() {
        return;
    }
    // SAFETY: documented pointer contracts of this callback.
    let (s, i) = unsafe { (state::<P>(ctx), &*info) };
    let info = PresentInfo {
        swapchain: i.swapchain,
        sync_interval: i.sync_interval,
        flags: i.flags,
    };
    guard(s, "on_present", |s| {
        s.plugin.on_present(&s.host, &info);
    });
}

unsafe extern "C" fn on_resize<P: Plugin>(ctx: *mut c_void, width: u32, height: u32) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    guard(s, "on_resize", |s| {
        s.plugin.on_resize(&s.host, width, height);
    });
}

unsafe extern "C" fn on_hotkey<P: Plugin>(ctx: *mut c_void, action: Str) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let action = str_from(action);
    guard(s, "on_hotkey", |s| {
        s.plugin.on_hotkey(&s.host, &action);
    });
}

unsafe extern "C" fn on_setting_changed<P: Plugin>(ctx: *mut c_void, key: Str, value: Str) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let (key, value) = (str_from(key), str_from(value));
    guard(s, "on_setting_changed", |s| {
        s.plugin.on_setting_changed(&s.host, &key, &value);
    });
}

fn status_of(result: Option<&Result<(), PluginError>>) -> Status {
    match result {
        Some(Ok(())) => Status::Ok,
        Some(Err(PluginError::Unsupported)) => Status::Unsupported,
        Some(Err(PluginError::Status(s))) => *s,
        Some(Err(PluginError::Message(_))) | None => Status::Error,
    }
}

unsafe extern "C" fn on_command<P: Plugin>(ctx: *mut c_void, name: Str, args: Str) -> Status {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let (name, args) = (str_from(name), str_from(args));
    let result = guard(s, "on_command", |s| {
        s.plugin.on_command(&s.host, &name, &args).inspect_err(|e| {
            if let PluginError::Message(m) = e {
                s.host.console_print(&format!("{name}: {m}"));
            }
        })
    });
    status_of(result.as_ref())
}

unsafe extern "C" fn on_message<P: Plugin>(
    ctx: *mut c_void,
    from: PluginHandle,
    topic: Str,
    payload: Bytes,
    reply: Option<ReplyFn>,
    reply_ctx: *mut c_void,
) -> Status {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let topic = str_from(topic);
    let payload = bytes_from(payload);
    let result = guard(s, "on_message", |s| {
        s.plugin
            .on_message(&s.host, PluginRef(from), &topic, payload)
    });
    match result {
        Some(Ok(Some(bytes))) => {
            if let Some(reply) = reply {
                // SAFETY: the loader guarantees `reply`/`reply_ctx` are valid during this call.
                unsafe { reply(reply_ctx, Bytes::new(&bytes)) };
            }
            Status::Ok
        }
        Some(Ok(None)) => Status::Ok,
        other => status_of(other.map(|r| r.map(|_| ())).as_ref()),
    }
}

unsafe extern "C" fn on_ui<P: Plugin>(ctx: *mut c_void, panel: Str, frame: u64) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let panel = str_from(panel);
    let ui = Ui::new(s.host.api(), s.host.handle(), frame);
    guard(s, "on_ui", |s| {
        s.plugin.on_ui(&s.host, &ui, &panel);
    });
}

unsafe extern "C" fn on_dialog<P: Plugin>(
    ctx: *mut c_void,
    id: u64,
    answer: api::UiAnswer,
    text: Str,
) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let text = str_from(text);
    guard(s, "on_dialog", |s| {
        s.plugin.on_dialog(&s.host, Shown(id), answer, &text);
    });
}

unsafe extern "C" fn on_input<P: Plugin>(
    ctx: *mut c_void,
    event: *const api::InputEvent,
) -> api::InputResponse {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    // SAFETY: as above; the borrow ends with this call, which is what the ABI promises.
    let Some(input) = (unsafe { Input::from_abi(event) }) else {
        return api::InputResponse::PASS;
    };
    let mut verdict = api::InputResponse::PASS;
    guard(s, "on_input", |s| {
        verdict = s.plugin.on_input(&s.host, &input);
    });
    verdict
}

unsafe extern "C" fn on_event<P: Plugin>(
    ctx: *mut c_void,
    from: PluginHandle,
    topic: Str,
    payload: Bytes,
) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let topic = str_from(topic);
    let payload = bytes_from(payload);
    guard(s, "on_event", |s| {
        s.plugin.on_event(&s.host, PluginRef(from), &topic, payload);
    });
}

unsafe extern "C" fn on_game_event<P: Plugin>(ctx: *mut c_void, event: *const api::GameEvent) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    // SAFETY: as above; the borrow ends with this call, which is what the ABI promises.
    let Some(mut described) = (unsafe { Event::from_abi(event) }) else {
        return;
    };
    // The fields are borrowed from the loader but the slice of them has to live somewhere, so
    // it lives here: on the stack of the one call that can see it, dropped before returning.
    let mut fields = Vec::new();
    // SAFETY: as above.
    if let Some(read) = unsafe { Event::read_fields(event, &mut fields) } {
        described.fields = read;
    }
    guard(s, "on_game_event", |s| {
        s.plugin.on_game_event(&s.host, &described);
    });
}

unsafe extern "C" fn on_chat<P: Plugin>(
    ctx: *mut c_void,
    message: *const api::ChatMessage,
) -> api::GameResponse {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    // SAFETY: as above; the borrow ends with this call, which is what the ABI promises.
    let Some(chat) = (unsafe { Chat::from_abi(message) }) else {
        return ChatVerdict::PASS;
    };
    let mut verdict = ChatVerdict::PASS;
    guard(s, "on_chat", |s| {
        verdict = s.plugin.on_chat(&s.host, &chat);
    });
    verdict
}

unsafe extern "C" fn on_rpc<P: Plugin>(ctx: *mut c_void, call: *const api::RemoteCall) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    // SAFETY: as above.
    let Some(call) = (unsafe { Rpc::from_abi(call) }) else {
        return;
    };
    guard(s, "on_rpc", |s| s.plugin.on_rpc(&s.host, &call));
}
