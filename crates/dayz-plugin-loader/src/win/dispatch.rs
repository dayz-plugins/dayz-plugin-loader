//! Delivery of callbacks to plugins. Every call goes through [`super::guard::call`]; a
//! plugin that faults is disabled and skipped from then on.

// FFI module: calls plugin function pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;

use dayz_plugin_api::{self as api, Bytes, PluginHandle, ReplyFn, Status, Str};

use crate::state::Notify;

use super::plugins::{self, Active};

/// Run `body` for one plugin, disabling it if the call faults.
fn to_plugin(plugin: &Active, what: &str, body: impl FnOnce()) {
    if !plugin.is_enabled() {
        return;
    }
    // SAFETY: the caller passes a closure that performs one plugin callback with arguments
    // the ABI requires; the guard contains a fault inside it.
    if let Err(fault) = unsafe { super::guard::call(body) } {
        plugin.disable(&format!("{what} {fault}"));
    }
}

/// Callback delivery was resumed or paused for one plugin.
///
/// Called with the plugin still in whichever state makes the callback honest: enabling tells
/// it after the atomic is set, disabling tells it before the atomic is cleared, so in both
/// cases the plugin is allowed to do its last or first piece of work from inside the call.
pub(crate) fn enabled(plugin: &Active, on: bool) {
    let (cb, what) = if on {
        (plugin.callbacks.on_enable, "on_enable")
    } else {
        (plugin.callbacks.on_disable, "on_disable")
    };
    let Some(cb) = cb else { return };
    let ctx = plugin.callbacks.ctx;
    // SAFETY: `ctx` is the plugin's own context; the callback takes nothing else.
    if let Err(fault) = unsafe { super::guard::call(|| cb(ctx)) } {
        plugin.disable(&format!("{what} {fault}"));
    }
}

/// The game's swapchain was created or recreated.
pub(crate) fn swapchain(info: &api::SwapchainInfo) {
    for &plugin in plugins::active() {
        let Some(cb) = plugin.callbacks.on_swapchain else {
            continue;
        };
        let ctx = plugin.callbacks.ctx;
        // SAFETY: `info` outlives the call; `ctx` is the plugin's own context.
        to_plugin(plugin, "on_swapchain", || unsafe { cb(ctx, info) });
    }
}

/// One `Present` call, before it reaches DXGI.
pub(crate) fn present(info: &api::PresentInfo) {
    for &plugin in plugins::active() {
        let Some(cb) = plugin.callbacks.on_present else {
            continue;
        };
        let ctx = plugin.callbacks.ctx;
        // SAFETY: `info` outlives the call; `ctx` is the plugin's own context.
        to_plugin(plugin, "on_present", || unsafe { cb(ctx, info) });
    }
}

/// A successful `ResizeBuffers`.
pub(crate) fn resize(width: u32, height: u32) {
    for &plugin in plugins::active() {
        let Some(cb) = plugin.callbacks.on_resize else {
            continue;
        };
        let ctx = plugin.callbacks.ctx;
        // SAFETY: `ctx` is the plugin's own context.
        to_plugin(plugin, "on_resize", || unsafe { cb(ctx, width, height) });
    }
}

/// Let a plugin fill one panel body. `token` is only valid for the length of this call.
pub(crate) fn ui(handle: PluginHandle, panel: &str, token: u64) {
    let Some(plugin) = plugins::find(handle) else {
        return;
    };
    let Some(cb) = plugin.callbacks.on_ui else {
        return;
    };
    if !plugin.is_enabled() {
        return;
    }
    let ctx = plugin.callbacks.ctx;
    // SAFETY: `panel` outlives the call; `ctx` is the plugin's own context.
    to_plugin(plugin, "on_ui", || unsafe {
        cb(ctx, Str::new(panel), token);
    });
}

/// Tell a plugin how its dialog ended.
pub(crate) fn dialog(handle: PluginHandle, id: u64, answer: api::UiAnswer, text: &str) {
    let Some(plugin) = plugins::find(handle) else {
        return;
    };
    let Some(cb) = plugin.callbacks.on_dialog else {
        return;
    };
    let ctx = plugin.callbacks.ctx;
    // SAFETY: `text` outlives the call; `ctx` is the plugin's own context.
    to_plugin(plugin, "on_dialog", || unsafe {
        cb(ctx, id, answer, Str::new(text));
    });
}

/// A hotkey fired. `action` is the qualified `<plugin>.<action>` name.
pub(crate) fn hotkey(action: &str) {
    let Some((name, bare)) = action.split_once('.') else {
        return;
    };
    let Some(plugin) = plugins::find_by_name(name) else {
        return;
    };
    let Some(cb) = plugin.callbacks.on_hotkey else {
        return;
    };
    let ctx = plugin.callbacks.ctx;
    // SAFETY: `bare` outlives the call; `ctx` is the plugin's own context.
    to_plugin(plugin, "on_hotkey", || unsafe { cb(ctx, Str::new(bare)) });
}

/// Setting change notifications produced by the state.
pub(crate) fn deliver(notifications: Vec<Notify>) {
    for notify in notifications {
        let Notify::SettingChanged {
            plugin: handle,
            key,
            value,
        } = notify;
        let Some(plugin) = plugins::find(handle) else {
            continue;
        };
        let Some(cb) = plugin.callbacks.on_setting_changed else {
            continue;
        };
        let ctx = plugin.callbacks.ctx;
        // SAFETY: both strings outlive the call; `ctx` is the plugin's own context.
        to_plugin(plugin, "on_setting_changed", || unsafe {
            cb(ctx, Str::new(&key), Str::new(&value));
        });
    }
}

/// Run a console command on its owning plugin.
pub(crate) fn command(handle: PluginHandle, name: &str, args: &str) -> Status {
    let Some(plugin) = plugins::find(handle) else {
        return Status::NotFound;
    };
    let Some(cb) = plugin.callbacks.on_command else {
        return Status::Unsupported;
    };
    if !plugin.is_enabled() {
        return Status::NotFound;
    }
    let ctx = plugin.callbacks.ctx;
    // SAFETY: both strings outlive the call; `ctx` is the plugin's own context.
    let result = unsafe { super::guard::call(|| cb(ctx, Str::new(name), Str::new(args))) };
    match result {
        Ok(status) => status,
        Err(fault) => {
            plugin.disable(&format!("on_command {fault}"));
            Status::Error
        }
    }
}

/// Direct message from one plugin to another.
pub(crate) fn deliver_message(
    from: PluginHandle,
    to: PluginHandle,
    topic: &str,
    payload: &[u8],
    reply: Option<ReplyFn>,
    reply_ctx: *mut c_void,
) -> Status {
    let Some(plugin) = plugins::find(to) else {
        return Status::NotFound;
    };
    let Some(cb) = plugin.callbacks.on_message else {
        return Status::Unsupported;
    };
    if !plugin.is_enabled() {
        return Status::NotFound;
    }
    let ctx = plugin.callbacks.ctx;
    // SAFETY: topic, payload and the reply sink all outlive the call; `ctx` is the
    // receiver's own context.
    let result = unsafe {
        super::guard::call(|| {
            cb(
                ctx,
                from,
                Str::new(topic),
                Bytes::new(payload),
                reply,
                reply_ctx,
            )
        })
    };
    match result {
        Ok(status) => status,
        Err(fault) => {
            plugin.disable(&format!("on_message {fault}"));
            Status::Error
        }
    }
}

/// Broadcast to the subscribers the state selected.
pub(crate) fn deliver_event(
    from: PluginHandle,
    targets: &[PluginHandle],
    topic: &str,
    payload: &[u8],
) {
    for handle in targets {
        let Some(plugin) = plugins::find(*handle) else {
            continue;
        };
        let Some(cb) = plugin.callbacks.on_event else {
            continue;
        };
        let ctx = plugin.callbacks.ctx;
        // SAFETY: topic and payload outlive the call; `ctx` is the receiver's own context.
        to_plugin(plugin, "on_event", || unsafe {
            cb(ctx, from, Str::new(topic), Bytes::new(payload));
        });
    }
}
