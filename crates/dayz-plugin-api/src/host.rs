//! Function table the loader hands to every plugin.

use core::ffi::c_void;

use crate::game::{GameClass, GameMask};
use crate::input::{InputAction, InputMask};
use crate::types::{
    ArgEntry, Bytes, CommandDesc, DialogDesc, EnvEntry, HotkeyDesc, LogLevel, NoticeDesc,
    PanelDesc, PluginHandle, SettingDesc, Status, Str, UiValue, UiWidget,
};

/// Callback a message receiver uses to answer synchronously.
pub type ReplyFn = unsafe extern "C" fn(reply_ctx: *mut c_void, payload: Bytes);

/// Callback receiving one line of console output, in the order it was printed.
pub type LineFn = unsafe extern "C" fn(line_ctx: *mut c_void, line: Str);

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

    /// Overwrite `len` bytes at `address`, remembering what was there.
    ///
    /// The loader keeps the original bytes and restores them when the plugin stops, so a
    /// patch cannot outlive the plugin that made it. `note` is what the console and the log
    /// call this patch. Writes the hook id to `out_id`.
    pub hook_patch: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        address: *mut c_void,
        bytes: *const u8,
        len: usize,
        note: Str,
        out_id: *mut u64,
    ) -> Status,
    /// Replace entry `index` of the virtual table `object` points at.
    ///
    /// Writes the replaced pointer to `out_original`, which is what the plugin calls to reach
    /// the original implementation, and the hook id to `out_id`. The loader puts the original
    /// pointer back when the plugin stops.
    pub hook_vtable: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        object: *mut c_void,
        index: u32,
        replacement: *mut c_void,
        note: Str,
        out_original: *mut *mut c_void,
        out_id: *mut u64,
    ) -> Status,
    /// Detour `target` to `replacement`, in place, for a function nothing else dispatches to.
    ///
    /// The relocated prologue is written to `out_trampoline`: calling that reaches the
    /// original function. The loader removes the detour when the plugin stops.
    ///
    /// Returns [`Status::AlreadyExists`] when any plugin already detoured that address, and
    /// [`Status::Unsupported`] when the prologue cannot be relocated.
    pub hook_detour: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        target: *mut c_void,
        replacement: *mut c_void,
        note: Str,
        out_trampoline: *mut *mut c_void,
        out_id: *mut u64,
    ) -> Status,
    /// Undo one hook by id, before the plugin stops. Hooks left behind are undone for it.
    ///
    /// Returns [`Status::NotFound`] for an unknown id or one belonging to another plugin.
    pub hook_remove:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, id: u64) -> Status,

    /// Register a UI panel. Only during `start`.
    ///
    /// The loader draws the window and remembers whether it is open; the plugin fills the
    /// body in [`PluginCallbacks::on_ui`](crate::PluginCallbacks::on_ui).
    pub panel_register: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        desc: *const PanelDesc,
    ) -> Status,
    /// Open or close one of the plugin's own panels.
    pub panel_set_open: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        name: Str,
        open: bool,
    ) -> Status,
    /// Whether one of the plugin's own panels is currently open.
    pub panel_is_open: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        name: Str,
        out: *mut bool,
    ) -> Status,

    /// Add a widget to the panel body being filled.
    ///
    /// `frame` is the token from `on_ui` and is rejected with [`Status::WrongPhase`] outside
    /// that call, including from another thread; nothing is dereferenced in that case.
    /// `kind` selects the widget, `text` is its label, and `value` carries the widget's
    /// value in and out where it has one. Unused fields are ignored.
    pub ui_widget: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        frame: u64,
        kind: UiWidget,
        text: Str,
        value: *mut UiValue,
    ) -> Status,

    /// Show a toast or a notice. It takes no input and goes away by itself.
    ///
    /// Writes an id to `out_id`, which [`HostApi::ui_close`] takes; `out_id` may be null for
    /// a message nobody will want to take back.
    pub notice_show: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        desc: *const NoticeDesc,
        out_id: *mut u64,
    ) -> Status,

    /// Open a modal dialog and return its id through `out_id`.
    ///
    /// The answer arrives later, in `on_dialog`, because the player answers in their own
    /// time and nothing in the loader may block a frame waiting for them. A plugin that
    /// stops with a dialog still open has it closed for it, with
    /// [`UiAnswer::Closed`](crate::UiAnswer::Closed).
    pub dialog_open: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        desc: *const DialogDesc,
        out_id: *mut u64,
    ) -> Status,

    /// Take down one of this plugin's own dialogs, toasts or notices early.
    ///
    /// A dialog closed this way answers with [`UiAnswer::Closed`](crate::UiAnswer::Closed).
    pub ui_close: unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, id: u64) -> Status,

    /// Run a console line and receive what it printed, line by line, before returning.
    ///
    /// [`HostApi::console_exec`] runs a line but its output only reaches the console window
    /// and the log, which is no use to a plugin that is answering someone else's question —
    /// a remote console, an overlay, a test. `sink` is called once per line while the call
    /// is on the stack and must not be kept.
    pub console_capture: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        line: Str,
        sink: Option<LineFn>,
        line_ctx: *mut c_void,
    ) -> Status,

    /// Subscribe to input events of the kinds in `mask`, delivered to `on_input`.
    ///
    /// Callable at any time, not only during `start`: a plugin that only wants input while it
    /// is doing something asks for it then and passes [`InputMask::NONE`] afterwards. The
    /// last call wins; a plugin without an `on_input` callback is refused.
    pub input_listen:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, mask: InputMask) -> Status,

    /// Send input to the system, as though a device had produced it.
    ///
    /// The whole array goes in one burst, in order, so a chord arrives as a chord. This is
    /// real system input — it reaches whichever window has the focus, which is normally the
    /// game — and it comes back around through `on_input` like anything else.
    pub input_send: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        actions: *const InputAction,
        count: usize,
    ) -> Status,

    /// Whether a key is physically down now, by Win32 virtual key code.
    pub input_key_down: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        vk: u32,
        out_down: *mut u32,
    ) -> Status,

    /// Ask the system for raw input from an HID usage the game never registered.
    ///
    /// `usage_page` and `usage` are the HID pair — 1/4 joystick, 1/5 gamepad, 1/8 multi-axis
    /// controller. Reports then arrive as [`InputKind::Hid`](crate::InputKind::Hid) events
    /// for every plugin subscribed to that kind. Registering is cumulative and the loader
    /// keeps the game's own mouse and keyboard registration untouched.
    pub input_register_hid: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        usage_page: u16,
        usage: u16,
    ) -> Status,

    /// Subscribe to the game's own streams: its events, its chat, its remote calls.
    ///
    /// Delivered to `on_game_event`, `on_chat` and `on_rpc` respectively. Callable at any
    /// time, the last call wins, and [`GameMask::NONE`] unsubscribes. A plugin asking for a
    /// stream it has no callback for is refused.
    ///
    /// The loader installs its hooks on the engine the first time anything subscribes, so a
    /// mask that asks for nothing costs nothing. [`Status::Unsupported`] means the addresses
    /// for this build are not in `dayz-data`, and [`Status::NotFound`] that they are but the
    /// hooks would not install — in both cases the game is untouched.
    pub game_listen:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, mask: GameMask) -> Status,

    /// Every event class the loader knows the shape of, whether or not it has been raised.
    ///
    /// Two calls: `out` null writes the count to `out_count` and returns [`Status::Ok`], then
    /// a second call with room for that many fills them in. A buffer that is too small is
    /// filled as far as it goes, `out_count` still receives the true total, and the status is
    /// [`Status::InvalidArgument`] so the shortfall cannot pass unnoticed.
    ///
    /// The strings in the entries belong to the loader's symbol database and last as long as
    /// the process, so a plugin may keep them. An empty catalogue is not an error: it means
    /// this build's database has no event section, and the loader will still report events by
    /// name as they happen.
    pub game_catalogue: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        out: *mut GameClass,
        capacity: usize,
        out_count: *mut usize,
    ) -> Status,

    /// Put one line in this client's own chat, which no other player and no server sees.
    ///
    /// The engine draws it and forgets it: there is no history to delete it from and nothing
    /// is sent anywhere, so this is the cheapest place to put something a player should read
    /// in passing. `colour` names one of the game's own colour classes — `ColorImportant`,
    /// `ColorFriendly`, `ColorEnemy` — and empty takes the game's default.
    ///
    /// [`Status::NotFound`] means the game object is not known yet. The loader catches it
    /// from the engine rather than hunting for it, so it arrives once the engine has
    /// dispatched a remote call; before that there is nothing to call this on.
    pub chat_local: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        text: Str,
        colour: Str,
    ) -> Status,
}
