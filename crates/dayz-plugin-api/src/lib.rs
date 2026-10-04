//! Stable C ABI between the DayZ plugin loader (`dxgi.dll`) and its plugins.
//!
//! Everything in this crate is `#[repr(C)]`, dependency free and must stay source and
//! binary compatible within one [`API_VERSION`]. Plugins written in other languages mirror
//! these definitions; Rust plugins should use the `dayz-plugin-sdk` crate instead of
//! touching this crate directly.
//!
//! Conventions:
//! - Strings are UTF-8 slices ([`Str`]) that are valid for the duration of the call only,
//!   unless a field documents otherwise. Nobody frees them.
//! - Every struct that may grow starts with `struct_size` so both sides can detect fields
//!   the other side does not know about.
//! - Every call returns a [`Status`]; nothing panics or throws across the boundary.

#![no_std]

use core::ffi::c_void;

mod callbacks;
mod game;
mod host;
mod input;
mod types;

pub use callbacks::{PluginCallbacks, PresentInfo, SwapchainInfo};
pub use game::{ChatMessage, GameEvent, GameMask, GameResponse, RemoteCall};
pub use host::{HostApi, LineFn, ReplyFn};
pub use input::{
    InputAction, InputActionFlags, InputActionKind, InputEvent, InputKind, InputMask,
    InputModifiers, InputResponse,
};
pub use types::{
    ArgEntry, Bytes, CommandDesc, Dependency, DependencyKind, DialogDesc, EnvEntry, HotkeyDesc,
    LogLevel, NoticeDesc, PanelDesc, PluginHandle, PluginInfo, SettingDesc, SettingFlags,
    SettingKind, Status, StopReason, Str, UiAnswer, UiDialog, UiLevel, UiNotice, UiValue, UiWidget,
};

/// Version of this ABI. The loader refuses plugins describing a different major version.
///
/// 2 added the dependency list to [`PluginInfo`], the enable and disable callbacks, and the
/// [`StopReason`] argument to the stop export. 3 added the hook registry and
/// [`HostApi::console_capture`]. 4 added the UI panels: [`PanelDesc`],
/// [`HostApi::panel_register`], [`HostApi::ui_widget`] and the `on_ui` callback. 5 added the
/// toasts, notices and dialogs: [`HostApi::notice_show`], [`HostApi::dialog_open`],
/// [`HostApi::ui_close`] and the `on_dialog` callback. 6 added the input stream:
/// [`HostApi::input_listen`], [`HostApi::input_send`], [`HostApi::input_key_down`],
/// [`HostApi::input_register_hid`] and the `on_input` callback. 7 added the game's own
/// streams: [`HostApi::game_listen`], [`GameEvent`], [`ChatMessage`], [`RemoteCall`] and the
/// `on_game_event`, `on_chat` and `on_rpc` callbacks.
pub const API_VERSION: u32 = 7;

/// Name of the export every plugin must provide: `extern "C" fn() -> *const PluginInfo`.
pub const DESCRIBE_EXPORT: &str = "dayz_plugin_describe";
/// Name of the start export: `extern "C" fn(*const HostApi, PluginHandle, *mut PluginCallbacks) -> Status`.
pub const START_EXPORT: &str = "dayz_plugin_start";
/// Name of the stop export: `extern "C" fn(*mut c_void, StopReason)` receiving the plugin
/// context and why it is being stopped.
pub const STOP_EXPORT: &str = "dayz_plugin_stop";

/// Signature of [`DESCRIBE_EXPORT`]. The returned struct must stay valid while the DLL is loaded.
pub type DescribeFn = unsafe extern "C" fn() -> *const PluginInfo;
/// Signature of [`START_EXPORT`]. The plugin fills `callbacks` and may keep `host` for its lifetime.
pub type StartFn =
    unsafe extern "C" fn(*const HostApi, PluginHandle, *mut PluginCallbacks) -> Status;
/// Signature of [`STOP_EXPORT`]. Called once with the context the plugin stored in its
/// callbacks, and the reason the loader is stopping it.
pub type StopFn = unsafe extern "C" fn(*mut c_void, StopReason);
