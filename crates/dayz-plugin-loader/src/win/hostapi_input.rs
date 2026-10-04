//! The input half of the [`HostApi`](dayz_plugin_api::HostApi) table, and the subscription
//! to the game's own streams, which works the same way.
//!
//! Separate from `hostapi.rs` for size, and because none of these touch the loader state:
//! subscriptions live in [`super::plugin_input`] and [`super::game_events`] behind their own
//! locks, and sending goes straight to the system.

// FFI module: the functions here are called by plugins through raw pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;

use dayz_plugin_api::{GameClass, GameMask, InputAction, InputMask, PluginHandle, Status, Str};

use super::{event_fields, game_events, hostapi, input_send, plugin_input, plugins, session};

/// `HostApi::input_listen`.
pub(super) unsafe extern "C" fn input_listen(
    _host: *mut c_void,
    plugin: PluginHandle,
    mask: InputMask,
) -> Status {
    // A subscription without a callback would be a plugin waiting for events that cannot be
    // delivered; saying so is better than silently never calling it.
    //
    // Only when it can be told, though. A plugin that subscribes from its own `start` — the
    // ordinary place to do it — is not in the active list yet, because plugins are published
    // once the whole batch has started, and its callback table is still being filled in by
    // the call this is running inside. "Not found" there is not a missing callback, and
    // refusing it would mean the one subscription that cannot be checked is the one that
    // fails. Delivery skips a plugin with no callback regardless, so the unchecked case is
    // only a lost error message.
    let unusable = plugins::find(plugin).is_some_and(|p| p.callbacks.on_input.is_none());
    if unusable && mask != InputMask::NONE {
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

/// `HostApi::game_listen`.
pub(super) unsafe extern "C" fn game_listen(
    _host: *mut c_void,
    plugin: PluginHandle,
    mask: GameMask,
) -> Status {
    // Asking for a stream with no callback to deliver it to is refused for the same reason as
    // it is for input, and checked only when it can be — see [`input_listen`] for why a
    // plugin subscribing from its own `start` is not findable yet.
    let missing = plugins::find(plugin).is_some_and(|active| {
        [
            (GameMask::EVENTS, active.callbacks.on_game_event.is_none()),
            (GameMask::CHAT, active.callbacks.on_chat.is_none()),
            (GameMask::RPC, active.callbacks.on_rpc.is_none()),
        ]
        .into_iter()
        .any(|(stream, absent)| mask.covers(stream) && absent)
    });
    if missing {
        return Status::InvalidArgument;
    }
    game_events::listen(plugin, mask)
}

/// `HostApi::game_catalogue`.
pub(super) unsafe extern "C" fn game_catalogue(
    _host: *mut c_void,
    _plugin: PluginHandle,
    out: *mut GameClass,
    capacity: usize,
    out_count: *mut usize,
) -> Status {
    if out_count.is_null() {
        return Status::InvalidArgument;
    }
    let slots: &mut [GameClass] = if out.is_null() || capacity == 0 {
        &mut []
    } else {
        // SAFETY: the ABI requires `out` to describe `capacity` writable structures for the
        // duration of the call.
        unsafe { core::slice::from_raw_parts_mut(out, capacity) }
    };
    let total = event_fields::catalogue(slots);
    // SAFETY: checked non-null; the ABI requires it to be writable.
    unsafe { out_count.write(total) };
    // A buffer that could not hold everything is a short read the caller has to notice: it
    // gets the true total back, so it can ask again with room.
    if total > slots.len() && !slots.is_empty() {
        return Status::InvalidArgument;
    }
    Status::Ok
}

/// `HostApi::chat_local`.
pub(super) unsafe extern "C" fn chat_local(
    _host: *mut c_void,
    _plugin: PluginHandle,
    text: Str,
    colour: Str,
) -> Status {
    let (text, colour) = (hostapi::text(text), hostapi::text(colour));
    match session::chat_local(&text, &colour) {
        Ok(()) => Status::Ok,
        Err(why) => {
            log::debug!("a plugin could not put a line in chat: {why}");
            Status::NotFound
        }
    }
}
