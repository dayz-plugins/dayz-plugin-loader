//! Function table the loader hands to every plugin.

use core::ffi::c_void;

use crate::types::{
    ArgEntry, Bytes, CommandDesc, EnvEntry, HotkeyDesc, LogLevel, PluginHandle, SettingDesc,
    Status, Str,
};

/// Callback a message receiver uses to answer synchronously.
pub type ReplyFn = unsafe extern "C" fn(reply_ctx: *mut c_void, payload: Bytes);

/// Services the loader offers. Every function takes `host` as its first argument.
///
/// All functions are thread safe. Registration functions return [`Status::WrongPhase`]
/// outside the plugin's `start` call.
#[repr(C)]
pub struct HostApi {
    /// `size_of::<HostApi>()` as compiled into the loader.
    pub struct_size: usize,
    /// Equals [`crate::API_VERSION`].
    pub api_version: u32,
    /// Opaque loader state, pass back unchanged.
    pub host: *mut c_void,
    /// Game installation directory. Valid for the plugin's lifetime.
    pub game_dir: Str,
    /// Directory holding plugin config files. Valid for the plugin's lifetime.
    pub config_dir: Str,

    /// The game's command line, verbatim and without the executable path.
    pub command_line: Str,
    /// Parsed command line entries in order. Valid for the plugin's lifetime.
    pub args: *const ArgEntry,
    /// Number of entries in [`HostApi::args`].
    pub arg_count: usize,
    /// Every environment variable of the game process, sorted by name. Valid for the
    /// plugin's lifetime: it is a snapshot taken when the loader initialised.
    pub env: *const EnvEntry,
    /// Number of entries in [`HostApi::env`].
    pub env_count: usize,

    /// Base address the game executable is mapped at. Symbol addresses are already absolute;
    /// this is for a plugin that wants to turn one back into an image-relative address.
    pub module_base: *mut c_void,
    /// Version of the matched entry in the dayz-data database, or empty when the running
    /// executable is not a known build.
    pub data_build: Str,

    /// Write a line to the shared loader log, prefixed with the plugin name.
    pub log:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, level: LogLevel, msg: Str),

    /// Register a persisted setting / console variable.
    pub setting_register: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        desc: *const SettingDesc,
    ) -> Status,
    /// Read a setting of any plugin as text. `name` is `key` for own settings or
    /// `<plugin>.<key>` for another plugin's. Writes the value into `buf` and its length
    /// into `out_len`; returns [`Status::BufferTooSmall`] with the needed length otherwise.
    pub setting_get: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        name: Str,
        buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> Status,
    /// Write a setting as text; validated against the descriptor, persisted unless transient,
    /// then the owning plugin's `on_setting_changed` fires.
    pub setting_set: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        name: Str,
        value: Str,
    ) -> Status,

    /// Register a hotkey action. The binding comes from the loader's hotkey config, falling
    /// back to the descriptor default. Hotkeys fire only while the game window has focus.
    pub hotkey_register: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        desc: *const HotkeyDesc,
    ) -> Status,

    /// Register a console command dispatched to `on_command`.
    pub command_register: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        desc: *const CommandDesc,
    ) -> Status,
    /// Print a line to the in-game console (and the log at debug level).
    pub console_print: unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, line: Str),
    /// Execute a console line as if typed by the user.
    pub console_exec:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, line: Str) -> Status,

    /// Look up another started plugin by name.
    pub plugin_find:
        unsafe extern "C" fn(host: *mut c_void, name: Str, out: *mut PluginHandle) -> Status,
    /// Send a synchronous message to one plugin. The receiver may answer through `reply`
    /// before returning; `reply` is never called after this function returns.
    pub plugin_message: unsafe extern "C" fn(
        host: *mut c_void,
        from: PluginHandle,
        to: PluginHandle,
        topic: Str,
        payload: Bytes,
        reply: Option<ReplyFn>,
        reply_ctx: *mut c_void,
    ) -> Status,
    /// Subscribe to a broadcast topic; delivered through `on_event`.
    pub event_subscribe:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, topic: Str) -> Status,
    /// Broadcast to every subscriber of `topic` except the sender. Delivery is synchronous.
    pub event_publish: unsafe extern "C" fn(
        host: *mut c_void,
        from: PluginHandle,
        topic: Str,
        payload: Bytes,
    ) -> Status,

    /// Ask the loader to create the game's swapchain with this backbuffer size instead of the
    /// game's. Only honoured during `start`, before the swapchain exists.
    pub request_backbuffer_size: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        width: u32,
        height: u32,
    ) -> Status,

    /// Absolute address of a named symbol from the dayz-data database, for example
    /// `render.frame`. Returns [`Status::NotFound`] when the symbol did not resolve for this
    /// build, which is the case a plugin must handle instead of hardcoding an address.
    pub symbol_get: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        name: Str,
        out: *mut *mut c_void,
    ) -> Status,
    /// A named struct field offset from the database, for example `framebase.rotation`.
    pub offset_get: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        name: Str,
        out: *mut u64,
    ) -> Status,
    /// Declare that the plugin cannot work without this symbol. Only valid during `start`.
    /// Returns [`Status::NotFound`] for a symbol that did not resolve; the loader logs which
    /// one, so a game update produces a named missing symbol rather than a crash.
    pub symbol_require:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, name: Str) -> Status,
}
