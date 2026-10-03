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
mod host;
mod types;

pub use callbacks::{PluginCallbacks, PresentInfo, SwapchainInfo};
pub use host::{HostApi, ReplyFn};
pub use types::{
    ArgEntry, Bytes, CommandDesc, Dependency, DependencyKind, EnvEntry, HotkeyDesc, LogLevel,
    PluginHandle, PluginInfo, SettingDesc, SettingFlags, SettingKind, Status, Str,
};

/// Version of this ABI. The loader refuses plugins describing a different major version.
pub const API_VERSION: u32 = 1;

/// Name of the export every plugin must provide: `extern "C" fn() -> *const PluginInfo`.
pub const DESCRIBE_EXPORT: &str = "dayz_plugin_describe";
/// Name of the start export: `extern "C" fn(*const HostApi, PluginHandle, *mut PluginCallbacks) -> Status`.
pub const START_EXPORT: &str = "dayz_plugin_start";
/// Name of the stop export: `extern "C" fn(*mut c_void)` receiving the plugin context.
pub const STOP_EXPORT: &str = "dayz_plugin_stop";

/// Signature of [`DESCRIBE_EXPORT`]. The returned struct must stay valid while the DLL is loaded.
pub type DescribeFn = unsafe extern "C" fn() -> *const PluginInfo;
/// Signature of [`START_EXPORT`]. The plugin fills `callbacks` and may keep `host` for its lifetime.
pub type StartFn =
    unsafe extern "C" fn(*const HostApi, PluginHandle, *mut PluginCallbacks) -> Status;
/// Signature of [`STOP_EXPORT`]. Called once with the context the plugin stored in its callbacks.
pub type StopFn = unsafe extern "C" fn(*mut c_void);
