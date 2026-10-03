//! Construction of the [`HostApi`] table and the implementations behind its function
//! pointers. Every entry converts to safe types, works on the locked state, and performs
//! plugin calls only after the lock is released.

// FFI module: the functions here are called by plugins through raw pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;

use dayz_plugin_api::{
    ArgEntry, Bytes, CommandDesc, EnvEntry, HostApi, HotkeyDesc, LineFn, LogLevel, PanelDesc,
    PluginHandle, ReplyFn, SettingDesc, SettingFlags, SettingKind, Status, Str, UiValue, UiWidget,
    API_VERSION,
};
use dayz_plugin_core::settings;

use crate::process::Process;
use crate::state::CommandInfo;

use super::{console, data, plugin_hooks, plugins, state};

/// Owned backing storage for every string the table hands out. Leaked once at startup so
/// the pointers stay valid for as long as any plugin can hold them.
struct Storage {
    game_dir: String,
    config_dir: String,
    command_line: String,
    data_build: String,
    args: Vec<ArgEntry>,
    env: Vec<EnvEntry>,
    // Keeps the strings the `Str`s above point into alive.
    _owned: Vec<String>,
}

/// Build the host table for a game directory and process snapshot.
pub(crate) fn build(game_dir: &str, config_dir: &str, process: &Process) -> &'static HostApi {
    let mut owned = Vec::new();
    let mut keep = |s: &str| -> Str {
        owned.push(s.to_owned());
        // The string is owned by `owned`, which the leaked `Storage` keeps alive forever.
        let last = owned.last().map_or("", String::as_str);
        Str::new(last)
    };
    let args: Vec<ArgEntry> = process
        .command_line
        .args()
        .iter()
        .map(|a| ArgEntry {
            name: keep(&a.name),
            value: keep(a.value.as_deref().unwrap_or("")),
            raw: keep(&a.raw),
            has_value: u32::from(a.value.is_some()),
        })
        .collect();
    let env: Vec<EnvEntry> = process
        .env
        .iter()
        .map(|(k, v)| EnvEntry {
            name: keep(k),
            value: keep(v),
        })
        .collect();
    let (module_base, data_build) = data::resolved()
        .map_or((core::ptr::null_mut(), String::new()), |r| {
            (r.module_base, r.build.clone())
        });
    let storage = Box::leak(Box::new(Storage {
        game_dir: game_dir.to_owned(),
        config_dir: config_dir.to_owned(),
        command_line: process.raw_command_line.clone(),
        data_build,
        args,
        env,
        _owned: owned,
    }));
    Box::leak(Box::new(HostApi {
        struct_size: core::mem::size_of::<HostApi>(),
        api_version: API_VERSION,
        host: core::ptr::null_mut(),
        game_dir: Str::new(&storage.game_dir),
        config_dir: Str::new(&storage.config_dir),
        command_line: Str::new(&storage.command_line),
        module_base,
        data_build: Str::new(&storage.data_build),
        args: storage.args.as_ptr(),
        arg_count: storage.args.len(),
        env: storage.env.as_ptr(),
        env_count: storage.env.len(),
        log: host_log,
        setting_register,
        setting_get,
        setting_set,
        hotkey_register,
        command_register,
        console_print,
        console_exec,
        plugin_find,
        plugin_message,
        event_subscribe,
        event_publish,
        request_backbuffer_size,
        symbol_get,
        offset_get,
        symbol_require,
        hook_patch,
        hook_vtable,
        hook_detour,
        hook_remove,
        panel_register,
        panel_set_open,
        panel_is_open,
        ui_widget,
        console_capture,
    }))
}

/// Borrow a `Str` a plugin passed us. Invalid UTF-8 becomes the replacement character
/// rather than an error, so a C plugin's mistake cannot abort a call.
pub(super) fn text(s: Str) -> String {
    if s.ptr.is_null() || s.len == 0 {
        return String::new();
    }
    // SAFETY: the ABI requires ptr/len to describe readable memory for this call.
    let bytes = unsafe { core::slice::from_raw_parts(s.ptr, s.len) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn payload(b: Bytes) -> Vec<u8> {
    if b.ptr.is_null() || b.len == 0 {
        return Vec::new();
    }
    // SAFETY: the ABI requires ptr/len to describe readable memory for this call.
    unsafe { core::slice::from_raw_parts(b.ptr, b.len) }.to_vec()
}

unsafe extern "C" fn host_log(_host: *mut c_void, plugin: PluginHandle, level: LogLevel, msg: Str) {
    let name = state()
        .plugin(plugin)
        .map_or_else(|| "?".to_owned(), |p| p.name.clone());
    let message = text(msg);
    match level {
        LogLevel::Error => log::error!("[{name}] {message}"),
        LogLevel::Warn => log::warn!("[{name}] {message}"),
        LogLevel::Debug => log::debug!("[{name}] {message}"),
        LogLevel::Trace => log::trace!("[{name}] {message}"),
        // `Info`, plus any level a future ABI adds that this loader cannot interpret.
        _ => log::info!("[{name}] {message}"),
    }
}

unsafe extern "C" fn setting_register(
    _host: *mut c_void,
    plugin: PluginHandle,
    desc: *const SettingDesc,
) -> Status {
    if desc.is_null() {
        return Status::InvalidArgument;
    }
    // SAFETY: checked non-null; the ABI requires it to point at a valid descriptor.
    let d = unsafe { &*desc };
    if d.struct_size < core::mem::size_of::<SettingDesc>() {
        return Status::Unsupported;
    }
    let desc = settings::Desc {
        key: text(d.key),
        title: text(d.title),
        description: text(d.description),
        kind: match d.kind {
            SettingKind::Bool => settings::Kind::Bool,
            SettingKind::Int => settings::Kind::Int,
            SettingKind::Float => settings::Kind::Float,
            SettingKind::Enum => settings::Kind::Enum,
            SettingKind::String => settings::Kind::String,
            // A kind this loader does not know cannot be validated; refuse the setting.
            _ => return Status::Unsupported,
        },
        default: text(d.default),
        min: d.min,
        max: d.max,
        choices: text(d.choices)
            .split('|')
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
            .collect(),
        restart_required: d.flags.0 & SettingFlags::RESTART_REQUIRED.0 != 0,
        transient: d.flags.0 & SettingFlags::TRANSIENT.0 != 0,
    };
    state()
        .register_setting(plugin, desc)
        .map_or_else(|e| e, |()| Status::Ok)
}

unsafe extern "C" fn setting_get(
    _host: *mut c_void,
    plugin: PluginHandle,
    name: Str,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> Status {
    if out_len.is_null() {
        return Status::InvalidArgument;
    }
    let value = match state().get_setting(Some(plugin), &text(name)) {
        Ok(v) => v,
        Err(e) => return e,
    };
    // SAFETY: checked non-null; the ABI requires a writable `usize`.
    unsafe { out_len.write(value.len()) };
    if value.len() > cap || buf.is_null() {
        return Status::BufferTooSmall;
    }
    // SAFETY: `buf` is writable for `cap` bytes and `value` fits, per the check above.
    unsafe { core::ptr::copy_nonoverlapping(value.as_ptr(), buf, value.len()) };
    Status::Ok
}

unsafe extern "C" fn setting_set(
    _host: *mut c_void,
    plugin: PluginHandle,
    name: Str,
    value: Str,
) -> Status {
    let (name, value) = (text(name), text(value));
    let outcome = state().set_setting(Some(plugin), &name, &value);
    match outcome {
        Ok(notify) => {
            plugins::deliver(notify.into_iter().collect());
            Status::Ok
        }
        Err((status, message)) => {
            log::warn!("set {name} = {value:?}: {message}");
            status
        }
    }
}

unsafe extern "C" fn hotkey_register(
    _host: *mut c_void,
    plugin: PluginHandle,
    desc: *const HotkeyDesc,
) -> Status {
    if desc.is_null() {
        return Status::InvalidArgument;
    }
    // SAFETY: checked non-null; the ABI requires a valid descriptor.
    let d = unsafe { &*desc };
    if d.struct_size < core::mem::size_of::<HotkeyDesc>() {
        return Status::Unsupported;
    }
    state()
        .register_hotkey(
            plugin,
            &text(d.action),
            &text(d.title),
            &text(d.default_binding),
        )
        .map_or_else(|e| e, |_| Status::Ok)
}

unsafe extern "C" fn command_register(
    _host: *mut c_void,
    plugin: PluginHandle,
    desc: *const CommandDesc,
) -> Status {
    if desc.is_null() {
        return Status::InvalidArgument;
    }
    // SAFETY: checked non-null; the ABI requires a valid descriptor.
    let d = unsafe { &*desc };
    if d.struct_size < core::mem::size_of::<CommandDesc>() {
        return Status::Unsupported;
    }
    let info = CommandInfo {
        help: text(d.help),
        usage: text(d.usage),
    };
    state()
        .register_command(plugin, &text(d.name), info)
        .map_or_else(|e| e, |()| Status::Ok)
}

unsafe extern "C" fn console_print(_host: *mut c_void, plugin: PluginHandle, line: Str) {
    let mut guard = state();
    let name = guard
        .plugin(plugin)
        .map_or_else(|| "?".to_owned(), |p| p.name.clone());
    let line = format!("[{name}] {}", text(line));
    console::print(&line);
    guard.console_print(line);
}

unsafe extern "C" fn console_exec(_host: *mut c_void, plugin: PluginHandle, line: Str) -> Status {
    super::run_console_line(Some(plugin), &text(line))
}

unsafe extern "C" fn plugin_find(_host: *mut c_void, name: Str, out: *mut PluginHandle) -> Status {
    if out.is_null() {
        return Status::InvalidArgument;
    }
    let guard = state();
    let Some(handle) = guard.find_plugin(&text(name)) else {
        return Status::NotFound;
    };
    if !guard.plugin(handle).is_some_and(|p| p.enabled) {
        return Status::NotFound;
    }
    // SAFETY: checked non-null; the ABI requires a writable handle.
    unsafe { out.write(handle) };
    Status::Ok
}

unsafe extern "C" fn plugin_message(
    _host: *mut c_void,
    from: PluginHandle,
    to: PluginHandle,
    topic: Str,
    data: Bytes,
    reply: Option<ReplyFn>,
    reply_ctx: *mut c_void,
) -> Status {
    plugins::deliver_message(from, to, &text(topic), &payload(data), reply, reply_ctx)
}

unsafe extern "C" fn event_subscribe(
    _host: *mut c_void,
    plugin: PluginHandle,
    topic: Str,
) -> Status {
    state()
        .subscribe(plugin, &text(topic))
        .map_or_else(|e| e, |()| Status::Ok)
}

unsafe extern "C" fn event_publish(
    _host: *mut c_void,
    from: PluginHandle,
    topic: Str,
    data: Bytes,
) -> Status {
    let topic = text(topic);
    let data = payload(data);
    let targets = state().subscribers(from, &topic);
    plugins::deliver_event(from, &targets, &topic, &data);
    Status::Ok
}

unsafe extern "C" fn request_backbuffer_size(
    _host: *mut c_void,
    plugin: PluginHandle,
    width: u32,
    height: u32,
) -> Status {
    state()
        .request_backbuffer(plugin, width, height)
        .map_or_else(|e| e, |()| Status::Ok)
}

unsafe extern "C" fn symbol_get(
    _host: *mut c_void,
    _plugin: PluginHandle,
    name: Str,
    out: *mut *mut c_void,
) -> Status {
    if out.is_null() {
        return Status::InvalidArgument;
    }
    let name = text(name);
    let Some(resolved) = data::resolved() else {
        return Status::NotFound;
    };
    let Some(symbol) = resolved.table.symbol(&name) else {
        return Status::NotFound;
    };
    let Ok(rva) = usize::try_from(symbol.rva) else {
        return Status::Error;
    };
    // SAFETY: the symbol resolved inside the mapped image, so base + rva is within it. The
    // pointer is only handed out; the loader never dereferences it.
    let address = unsafe { resolved.module_base.cast::<u8>().add(rva) };
    // SAFETY: checked non-null; the ABI requires a writable pointer.
    unsafe { out.write(address.cast()) };
    Status::Ok
}

unsafe extern "C" fn offset_get(
    _host: *mut c_void,
    _plugin: PluginHandle,
    name: Str,
    out: *mut u64,
) -> Status {
    if out.is_null() {
        return Status::InvalidArgument;
    }
    let name = text(name);
    let Some(resolved) = data::resolved() else {
        return Status::NotFound;
    };
    let Some(value) = resolved.table.offset(&name) else {
        return Status::NotFound;
    };
    // SAFETY: checked non-null; the ABI requires a writable `u64`.
    unsafe { out.write(value) };
    Status::Ok
}

unsafe extern "C" fn symbol_require(_host: *mut c_void, plugin: PluginHandle, name: Str) -> Status {
    let name = text(name);
    let plugin_name = state()
        .plugin(plugin)
        .map_or_else(|| "?".to_owned(), |p| p.name.clone());
    let available = data::resolved().is_some_and(|r| r.table.symbol(&name).is_some());
    if available {
        log::debug!("[{plugin_name}] requires {name}: available");
        return Status::Ok;
    }
    log::error!("[{plugin_name}] requires {name}, which did not resolve for this build");
    Status::NotFound
}

unsafe extern "C" fn hook_patch(
    _host: *mut c_void,
    plugin: PluginHandle,
    address: *mut c_void,
    bytes: *const u8,
    len: usize,
    note: Str,
    out_id: *mut u64,
) -> Status {
    if bytes.is_null() || out_id.is_null() {
        return Status::InvalidArgument;
    }
    // SAFETY: the ABI requires `bytes`/`len` to describe readable memory for this call.
    let patch = unsafe { core::slice::from_raw_parts(bytes, len) };
    match plugin_hooks::patch(plugin, address as usize, patch, &text(note)) {
        Ok(id) => {
            // SAFETY: checked non-null; the ABI requires a writable `u64`.
            unsafe { out_id.write(id) };
            Status::Ok
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn hook_vtable(
    _host: *mut c_void,
    plugin: PluginHandle,
    object: *mut c_void,
    index: u32,
    replacement: *mut c_void,
    note: Str,
    out_original: *mut *mut c_void,
    out_id: *mut u64,
) -> Status {
    if out_original.is_null() || out_id.is_null() {
        return Status::InvalidArgument;
    }
    match plugin_hooks::vtable(plugin, object, index, replacement, &text(note)) {
        Ok((id, original)) => {
            // SAFETY: both checked non-null; the ABI requires writable out-params.
            unsafe {
                out_original.write(original);
                out_id.write(id);
            }
            Status::Ok
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn hook_detour(
    _host: *mut c_void,
    plugin: PluginHandle,
    target: *mut c_void,
    replacement: *mut c_void,
    note: Str,
    out_trampoline: *mut *mut c_void,
    out_id: *mut u64,
) -> Status {
    if out_trampoline.is_null() || out_id.is_null() {
        return Status::InvalidArgument;
    }
    match plugin_hooks::detour(plugin, target, replacement, &text(note)) {
        Ok((id, trampoline)) => {
            // SAFETY: both checked non-null; the ABI requires writable out-params.
            unsafe {
                out_trampoline.write(trampoline);
                out_id.write(id);
            }
            Status::Ok
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn hook_remove(_host: *mut c_void, plugin: PluginHandle, id: u64) -> Status {
    plugin_hooks::remove(plugin, id)
}

unsafe extern "C" fn console_capture(
    _host: *mut c_void,
    plugin: PluginHandle,
    line: Str,
    sink: Option<LineFn>,
    line_ctx: *mut c_void,
) -> Status {
    let (status, printed) = super::run_console_line_capture(Some(plugin), &text(line));
    if let Some(sink) = sink {
        for line in &printed {
            // SAFETY: the ABI requires `sink` to accept a borrowed line for the duration of
            // the call, which is exactly how long `line` lives here.
            unsafe { sink(line_ctx, Str::new(line)) };
        }
    }
    status
}

unsafe extern "C" fn panel_register(
    _host: *mut c_void,
    plugin: PluginHandle,
    desc: *const PanelDesc,
) -> Status {
    if desc.is_null() {
        return Status::InvalidArgument;
    }
    // SAFETY: checked non-null; the ABI requires a valid descriptor.
    let d = unsafe { &*desc };
    if d.struct_size < core::mem::size_of::<PanelDesc>() {
        return Status::Unsupported;
    }
    let (name, title, binding) = (text(d.name), text(d.title), text(d.default_binding));
    let mut guard = state();
    if let Err(e) = guard.register_panel(plugin, &name, &title, d.default_open) {
        return e;
    }
    // A panel's key toggles the panel, which the loader does itself; the plugin never sees
    // the action. Registering it here means it is listed and rebindable like any other.
    if !binding.is_empty() {
        if let Err(e) = guard.register_hotkey(plugin, &name, &title, &binding) {
            log::warn!("panel {name}: no hotkey ({e:?})");
        }
    }
    drop(guard);
    super::ui::refresh();
    Status::Ok
}

unsafe extern "C" fn panel_set_open(
    _host: *mut c_void,
    plugin: PluginHandle,
    name: Str,
    open: bool,
) -> Status {
    let found = state().set_panel_open(plugin, &text(name), open);
    if !found {
        return Status::NotFound;
    }
    super::ui::refresh();
    Status::Ok
}

unsafe extern "C" fn panel_is_open(
    _host: *mut c_void,
    plugin: PluginHandle,
    name: Str,
    out: *mut bool,
) -> Status {
    if out.is_null() {
        return Status::InvalidArgument;
    }
    let Some(open) = state().panel_open(plugin, &text(name)) else {
        return Status::NotFound;
    };
    // SAFETY: checked non-null; the ABI requires a writable out-param.
    unsafe { out.write(open) };
    Status::Ok
}

unsafe extern "C" fn ui_widget(
    _host: *mut c_void,
    plugin: PluginHandle,
    frame: u64,
    kind: UiWidget,
    text_arg: Str,
    value: *mut UiValue,
) -> Status {
    // SAFETY: the ABI requires `value` to be either null or a writable `UiValue` for the
    // duration of the call; a struct older than this loader's is refused rather than read.
    let value = unsafe { value.as_mut() };
    if value
        .as_ref()
        .is_some_and(|v| v.struct_size < core::mem::size_of::<UiValue>())
    {
        return Status::Unsupported;
    }
    super::ui::widget(plugin, frame, kind, &text(text_arg), value)
}
