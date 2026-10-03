//! The input half of the [`HostApi`](dayz_plugin_api::HostApi) table.
//!
//! Separate from `hostapi.rs` for size, and because none of these four touch the loader
//! state: subscriptions live in [`super::plugin_input`] behind their own lock, and sending
//! goes straight to the system.

// FFI module: the functions here are called by plugins through raw pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;

use dayz_plugin_api::{InputAction, InputMask, PluginHandle, Status};

use super::{input_send, plugin_input, plugins};

/// `HostApi::input_listen`.
pub(super) unsafe extern "C" fn input_listen(
    _host: *mut c_void,
    plugin: PluginHandle,
    mask: InputMask,
) -> Status {
    // A subscription without a callback would be a plugin waiting for events that cannot be
    // delivered; saying so is better than silently never calling it.
    let has_callback = plugins::find(plugin).is_some_and(|p| p.callbacks.on_input.is_some());
    if !has_callback && mask != InputMask::NONE {
        return Status::InvalidArgument;
    }
    plugin_input::listen(plugin, mask)
}

/// `HostApi::input_send`.
pub(super) unsafe extern "C" fn input_send(
    _host: *mut c_void,
    _plugin: PluginHandle,
    actions: *const InputAction,
    count: usize,
) -> Status {
    if count == 0 {
        return Status::Ok;
    }
    if actions.is_null() {
        return Status::InvalidArgument;
    }
    // SAFETY: the ABI requires `actions` to describe `count` readable structures for the
    // duration of the call.
    let actions = unsafe { core::slice::from_raw_parts(actions, count) };
    input_send::send(actions)
}

/// `HostApi::input_key_down`.
pub(super) unsafe extern "C" fn input_key_down(
    _host: *mut c_void,
    _plugin: PluginHandle,
    vk: u32,
    out_down: *mut u32,
) -> Status {
    if out_down.is_null() {
        return Status::InvalidArgument;
    }
    let Ok(vk) = u16::try_from(vk) else {
        return Status::InvalidArgument;
    };
    let down = dayz_plugin_core::hotkeys::KeyState::is_down(&super::input::AsyncKeys, vk);
    // SAFETY: checked non-null; the ABI requires it to be writable.
    unsafe { out_down.write(u32::from(down)) };
    Status::Ok
}

/// `HostApi::input_register_hid`.
pub(super) unsafe extern "C" fn input_register_hid(
    _host: *mut c_void,
    _plugin: PluginHandle,
    usage_page: u16,
    usage: u16,
) -> Status {
    input_send::register_hid(super::game_window(), usage_page, usage)
}
