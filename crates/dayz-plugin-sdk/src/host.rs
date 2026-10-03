//! Safe wrapper around the loader's [`HostApi`] table.

// This module is the plugin side of the FFI boundary; every unsafe block documents the
// loader contract it relies on. No other SDK module except `ffi` needs unsafe.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::fmt;

use std::collections::BTreeMap;

use dayz_plugin_api::{
    ArgEntry, Bytes, CommandDesc, EnvEntry, HostApi, HotkeyDesc, LogLevel, PluginHandle,
    SettingDesc, SettingFlags, Status, Str,
};
pub use dayz_plugin_core::cmdline::{Arg, CommandLine};

use crate::input::{Action, Watch};
use crate::settings::{Setting, SettingKind};

/// Error type for plugin code. Carries a loader status or a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    /// A loader call failed with this status.
    Status(Status),
    /// The operation is not implemented.
    Unsupported,
    /// Free text, shown in the console and log.
    Message(String),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::Status(s) => write!(f, "loader returned {s:?}"),
            PluginError::Unsupported => f.write_str("not supported"),
            PluginError::Message(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for PluginError {}

impl From<String> for PluginError {
    fn from(m: String) -> Self {
        PluginError::Message(m)
    }
}

impl From<&str> for PluginError {
    fn from(m: &str) -> Self {
        PluginError::Message(m.to_owned())
    }
}

/// Reply sink handed to the receiving plugin: stores the bytes in the caller's `Option`.
unsafe extern "C" fn collect_reply(ctx: *mut c_void, payload: Bytes) {
    // SAFETY: `ctx` is the `&mut Option<Vec<u8>>` of the `message` call in progress, which
    // the ABI guarantees is alive until that call returns.
    let slot = unsafe { &mut *ctx.cast::<Option<Vec<u8>>>() };
    *slot = Some(bytes_from(payload).to_vec());
}

fn check(status: Status) -> Result<(), PluginError> {
    if status == Status::Ok {
        Ok(())
    } else {
        Err(PluginError::Status(status))
    }
}

/// Handle of another loaded plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PluginRef(pub(crate) PluginHandle);

/// The loader, as seen by one plugin. Cheap to clone; valid for the plugin's lifetime.
#[derive(Clone, Copy)]
pub struct Host {
    api: &'static HostApi,
    handle: PluginHandle,
}

// SAFETY: the loader documents every HostApi function as thread safe, and the table itself
// is immutable for the plugin's lifetime.
unsafe impl Send for Host {}
// SAFETY: see the Send impl.
unsafe impl Sync for Host {}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Host")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

/// Copy a borrowed [`Str`] the loader passed us into an owned `String`.
pub(crate) fn str_from(s: Str) -> String {
    String::from_utf8_lossy(bytes_from(Bytes {
        ptr: s.ptr,
        len: s.len,
    }))
    .into_owned()
}

/// View a host-provided array.
///
/// # Safety
/// `ptr` must be null or point to `len` initialised `T` valid for `'a`.
unsafe fn slice_from<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: guaranteed by this function's contract, which the loader upholds.
    unsafe { core::slice::from_raw_parts(ptr, len) }
}

/// View a borrowed [`Bytes`] the loader passed us.
pub(crate) fn bytes_from<'a>(b: Bytes) -> &'a [u8] {
    if b.ptr.is_null() || b.len == 0 {
        return &[];
    }
    // SAFETY: the loader guarantees ptr/len describe readable memory for the current call;
    // callers only use the slice within that call.
    unsafe { core::slice::from_raw_parts(b.ptr, b.len) }
}

impl Host {
    pub(crate) fn new(api: &'static HostApi, handle: PluginHandle) -> Self {
        Host { api, handle }
    }

    pub(crate) fn api(&self) -> &'static HostApi {
        self.api
    }

    pub(crate) fn handle(&self) -> PluginHandle {
        self.handle
    }

    /// This plugin's handle as seen by other plugins.
    #[must_use]
    pub fn me(&self) -> PluginRef {
        PluginRef(self.handle)
    }

    /// Game installation directory.
    #[must_use]
    pub fn game_dir(&self) -> String {
        str_from(self.api.game_dir)
    }

    /// Directory holding plugin config files.
    #[must_use]
    pub fn config_dir(&self) -> String {
        str_from(self.api.config_dir)
    }

    /// The game's command line verbatim, without the executable path.
    #[must_use]
    pub fn command_line(&self) -> String {
        str_from(self.api.command_line)
    }

    /// The game's command line, parsed. Covers `--flag`, `-key=value`, `/key value` and
    /// positionals; see [`CommandLine`] for lookups.
    #[must_use]
    pub fn args(&self) -> CommandLine {
        // SAFETY: the loader documents args/arg_count as a snapshot valid for our lifetime.
        let entries: &[ArgEntry] = unsafe { slice_from(self.api.args, self.api.arg_count) };
        CommandLine::parse(entries.iter().map(|e| str_from(e.raw)))
    }

    /// Every environment variable of the game process, as a snapshot taken when the loader
    /// initialised. Variable names keep their original case.
    #[must_use]
    pub fn env(&self) -> BTreeMap<String, String> {
        // SAFETY: the loader documents env/env_count as a snapshot valid for our lifetime.
        let entries: &[EnvEntry] = unsafe { slice_from(self.api.env, self.api.env_count) };
        entries
            .iter()
            .map(|e| (str_from(e.name), str_from(e.value)))
            .collect()
    }

    /// One environment variable, or `None` when it is not set.
    #[must_use]
    pub fn env_var(&self, name: &str) -> Option<String> {
        // SAFETY: see `env`.
        let entries: &[EnvEntry] = unsafe { slice_from(self.api.env, self.api.env_count) };
        entries
            .iter()
            .find(|e| str_from(e.name) == name)
            .map(|e| str_from(e.value))
    }

    /// Write to the shared loader log. Prefer the `log` macros, which end up here.
    pub fn log(&self, level: LogLevel, msg: &str) {
        // SAFETY: valid table pointer; `msg` outlives the call.
        unsafe { (self.api.log)(self.api.host, self.handle, level, Str::new(msg)) }
    }

    /// Register a setting. Only allowed during `start`.
    ///
    /// # Errors
    /// Invalid key, duplicate key, bad default, or called outside `start`.
    pub fn setting(&self, setting: &Setting) -> Result<(), PluginError> {
        let choices = setting.choices.join("|");
        let mut flags = SettingFlags::NONE.0;
        if setting.restart_required {
            flags |= SettingFlags::RESTART_REQUIRED.0;
        }
        if setting.transient {
            flags |= SettingFlags::TRANSIENT.0;
        }
        if setting.advanced {
            flags |= SettingFlags::ADVANCED.0;
        }
        let desc = SettingDesc {
            struct_size: core::mem::size_of::<SettingDesc>(),
            key: Str::new(&setting.key),
            title: Str::new(&setting.title),
            description: Str::new(&setting.description),
            kind: match setting.kind {
                SettingKind::Bool => dayz_plugin_api::SettingKind::Bool,
                SettingKind::Int => dayz_plugin_api::SettingKind::Int,
                SettingKind::Float => dayz_plugin_api::SettingKind::Float,
                SettingKind::Enum => dayz_plugin_api::SettingKind::Enum,
                SettingKind::String => dayz_plugin_api::SettingKind::String,
            },
            default: Str::new(&setting.default),
            min: setting.min,
            max: setting.max,
            choices: Str::new(&choices),
            flags: SettingFlags(flags),
        };
        // SAFETY: `desc` and every string it borrows live until the call returns.
        check(unsafe { (self.api.setting_register)(self.api.host, self.handle, &raw const desc) })
    }

    /// Read a setting as text. `name` is a key of this plugin or `<plugin>.<key>`.
    ///
    /// # Errors
    /// Unknown setting.
    pub fn get(&self, name: &str) -> Result<String, PluginError> {
        let mut buf = vec![0u8; 256];
        loop {
            let mut len = 0usize;
            // SAFETY: `buf` is writable for `buf.len()` bytes and `len` is a valid out-pointer.
            let status = unsafe {
                (self.api.setting_get)(
                    self.api.host,
                    self.handle,
                    Str::new(name),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &raw mut len,
                )
            };
            match status {
                Status::Ok => {
                    buf.truncate(len);
                    return Ok(String::from_utf8_lossy(&buf).into_owned());
                }
                Status::BufferTooSmall => buf.resize(len, 0),
                other => return Err(PluginError::Status(other)),
            }
        }
    }

    /// Read a setting and parse it.
    ///
    /// # Errors
    /// Unknown setting or a value that does not parse as `T`.
    pub fn get_as<T: core::str::FromStr>(&self, name: &str) -> Result<T, PluginError> {
        let text = self.get(name)?;
        text.parse()
            .map_err(|_| PluginError::Message(format!("{name}: cannot parse {text:?}")))
    }

    /// Write a setting as text. Validated by the loader; fires `on_setting_changed`.
    ///
    /// # Errors
    /// Unknown setting or invalid value.
    pub fn set(&self, name: &str, value: &str) -> Result<(), PluginError> {
        // SAFETY: both strings outlive the call.
        check(unsafe {
            (self.api.setting_set)(self.api.host, self.handle, Str::new(name), Str::new(value))
        })
    }

    /// Register a hotkey action with a default binding such as `f12`. Only during `start`.
    ///
    /// # Errors
    /// Invalid name, duplicate, unparsable default, or called outside `start`.
    pub fn hotkey(
        &self,
        action: &str,
        title: &str,
        default_binding: &str,
    ) -> Result<(), PluginError> {
        let desc = HotkeyDesc {
            struct_size: core::mem::size_of::<HotkeyDesc>(),
            action: Str::new(action),
            title: Str::new(title),
            default_binding: Str::new(default_binding),
        };
        // SAFETY: `desc` and its strings live until the call returns.
        check(unsafe { (self.api.hotkey_register)(self.api.host, self.handle, &raw const desc) })
    }

    /// Register a console command. Only during `start`.
    ///
    /// # Errors
    /// Invalid name, duplicate, or called outside `start`.
    pub fn command(&self, name: &str, help: &str, usage: &str) -> Result<(), PluginError> {
        let desc = CommandDesc {
            struct_size: core::mem::size_of::<CommandDesc>(),
            name: Str::new(name),
            help: Str::new(help),
            usage: Str::new(usage),
        };
        // SAFETY: `desc` and its strings live until the call returns.
        check(unsafe { (self.api.command_register)(self.api.host, self.handle, &raw const desc) })
    }

    /// Print a line to the in-game console.
    pub fn console_print(&self, line: &str) {
        // SAFETY: `line` outlives the call.
        unsafe { (self.api.console_print)(self.api.host, self.handle, Str::new(line)) }
    }

    /// Execute a console line as if typed.
    ///
    /// # Errors
    /// Whatever the console reports (unknown name, bad value, command failure).
    pub fn console_exec(&self, line: &str) -> Result<(), PluginError> {
        // SAFETY: `line` outlives the call.
        check(unsafe { (self.api.console_exec)(self.api.host, self.handle, Str::new(line)) })
    }

    /// Execute a console line and collect everything it printed.
    ///
    /// What a plugin needs when it is answering for the console rather than driving it: a
    /// remote console, an overlay, a test. The status is returned alongside the output
    /// instead of as an error, because a failed command's message is in those lines.
    pub fn console_capture(&self, line: &str) -> (Status, Vec<String>) {
        let mut lines: Vec<String> = Vec::new();
        let ctx = core::ptr::from_mut(&mut lines).cast::<c_void>();
        // SAFETY: `line` outlives the call, and `ctx` points at `lines`, which outlives it
        // too; `collect_line` only ever receives this pointer.
        let status = unsafe {
            (self.api.console_capture)(
                self.api.host,
                self.handle,
                Str::new(line),
                Some(collect_line),
                ctx,
            )
        };
        (status, lines)
    }

    /// Find another started plugin by name.
    #[must_use]
    pub fn find_plugin(&self, name: &str) -> Option<PluginRef> {
        let mut out = PluginHandle::NONE;
        // SAFETY: `out` is a valid out-pointer; `name` outlives the call.
        let status = unsafe { (self.api.plugin_find)(self.api.host, Str::new(name), &raw mut out) };
        (status == Status::Ok).then_some(PluginRef(out))
    }

    /// Send a synchronous message to another plugin and collect its reply, if any.
    ///
    /// # Errors
    /// Target gone, target does not handle messages, or target reported failure.
    pub fn message(
        &self,
        to: PluginRef,
        topic: &str,
        payload: &[u8],
    ) -> Result<Option<Vec<u8>>, PluginError> {
        let mut reply: Option<Vec<u8>> = None;
        // SAFETY: all borrowed data outlives the call; `reply` is only touched through `collect`
        // during the call, as the ABI guarantees.
        let status = unsafe {
            (self.api.plugin_message)(
                self.api.host,
                self.handle,
                to.0,
                Str::new(topic),
                Bytes::new(payload),
                Some(collect_reply),
                core::ptr::addr_of_mut!(reply).cast(),
            )
        };
        check(status)?;
        Ok(reply)
    }

    /// Register a UI panel. Only during `start`.
    ///
    /// The loader draws the window, remembers whether it is open and calls
    /// [`Plugin::on_ui`](crate::Plugin::on_ui) to fill the body. `binding` is a key in the
    /// loader's grammar that toggles the panel, or `""` for none; the loader handles that key
    /// itself, so it never reaches `on_hotkey`.
    ///
    /// # Errors
    /// Called outside `start`, a name that is not `[a-z0-9_]+`, or a duplicate.
    pub fn panel(
        &self,
        name: &str,
        title: &str,
        default_open: bool,
        binding: &str,
    ) -> Result<(), PluginError> {
        let desc = dayz_plugin_api::PanelDesc {
            struct_size: core::mem::size_of::<dayz_plugin_api::PanelDesc>(),
            name: Str::new(name),
            title: Str::new(title),
            default_open,
            default_binding: Str::new(binding),
        };
        // SAFETY: the descriptor and the strings it points at outlive the call.
        check(unsafe { (self.api.panel_register)(self.api.host, self.handle, &raw const desc) })
    }

    /// Open or close one of this plugin's panels.
    ///
    /// # Errors
    /// No panel of that name.
    pub fn set_panel_open(&self, name: &str, open: bool) -> Result<(), PluginError> {
        // SAFETY: `name` outlives the call.
        check(unsafe {
            (self.api.panel_set_open)(self.api.host, self.handle, Str::new(name), open)
        })
    }

    /// Whether one of this plugin's panels is currently open.
    ///
    /// # Errors
    /// No panel of that name.
    pub fn panel_is_open(&self, name: &str) -> Result<bool, PluginError> {
        let mut open = false;
        // SAFETY: `name` outlives the call; `open` is a valid out-pointer.
        check(unsafe {
            (self.api.panel_is_open)(self.api.host, self.handle, Str::new(name), &raw mut open)
        })?;
        Ok(open)
    }

    /// Put a toast or a notice on screen. It takes no input and goes away by itself.
    ///
    /// # Errors
    /// The loader refused it, which today means only a descriptor it is too old to read.
    pub fn show(&self, notice: &crate::Notice) -> Result<crate::Shown, PluginError> {
        let desc = notice.to_api();
        let mut id = 0u64;
        // SAFETY: the descriptor and the strings it borrows outlive the call; `id` is a valid
        // out-pointer.
        check(unsafe {
            (self.api.notice_show)(self.api.host, self.handle, &raw const desc, &raw mut id)
        })?;
        Ok(crate::Shown(id))
    }

    /// Open a modal dialog. The answer arrives in
    /// [`Plugin::on_dialog`](crate::Plugin::on_dialog), never here: the player answers in
    /// their own time, and a frame may not wait for them.
    ///
    /// # Errors
    /// The loader refused the descriptor.
    pub fn ask(&self, dialog: &crate::Dialog) -> Result<crate::Shown, PluginError> {
        let desc = dialog.to_api();
        let mut id = 0u64;
        // SAFETY: the descriptor and the strings it borrows outlive the call; `id` is a valid
        // out-pointer.
        check(unsafe {
            (self.api.dialog_open)(self.api.host, self.handle, &raw const desc, &raw mut id)
        })?;
        Ok(crate::Shown(id))
    }

    /// Take one of this plugin's dialogs, toasts or notices down early.
    ///
    /// A dialog closed this way still reports to `on_dialog`, with
    /// [`UiAnswer::Closed`](crate::api::UiAnswer::Closed), so a plugin has exactly one place
    /// where a dialog ends.
    ///
    /// # Errors
    /// Unknown id, or one belonging to another plugin.
    pub fn close(&self, shown: crate::Shown) -> Result<(), PluginError> {
        // SAFETY: the id is a plain number the loader validates itself.
        check(unsafe { (self.api.ui_close)(self.api.host, self.handle, shown.0) })
    }

    /// Subscribe to a broadcast topic. Only during `start`.
    ///
    /// # Errors
    /// Called outside `start`.
    pub fn subscribe(&self, topic: &str) -> Result<(), PluginError> {
        // SAFETY: `topic` outlives the call.
        check(unsafe { (self.api.event_subscribe)(self.api.host, self.handle, Str::new(topic)) })
    }

    /// Broadcast to every subscriber of `topic`.
    pub fn publish(&self, topic: &str, payload: &[u8]) {
        // SAFETY: borrowed data outlives the call.
        let _ = unsafe {
            (self.api.event_publish)(
                self.api.host,
                self.handle,
                Str::new(topic),
                Bytes::new(payload),
            )
        };
    }

    /// Version of the dayz-data entry matching the running executable, or `None` when this
    /// build is not in the database.
    #[must_use]
    pub fn game_build(&self) -> Option<String> {
        let build = str_from(self.api.data_build);
        (!build.is_empty()).then_some(build)
    }

    /// Base address the game executable is mapped at.
    #[must_use]
    pub fn module_base(&self) -> *mut c_void {
        self.api.module_base
    }

    /// Absolute address of a named symbol, for example `render.frame`.
    ///
    /// Always ask by name; never compile an address into a plugin. A symbol that did not
    /// resolve for the running build is an error here rather than a wrong address.
    ///
    /// # Errors
    /// The symbol is not in the database, or did not resolve for this build.
    pub fn symbol(&self, name: &str) -> Result<*mut c_void, PluginError> {
        let mut out = core::ptr::null_mut();
        // SAFETY: `name` outlives the call and `out` is a valid out-pointer.
        check(unsafe {
            (self.api.symbol_get)(self.api.host, self.handle, Str::new(name), &raw mut out)
        })?;
        Ok(out)
    }

    /// A named struct field offset, for example `framebase.rotation`.
    ///
    /// # Errors
    /// The offset is not in the database for this build.
    pub fn offset(&self, name: &str) -> Result<u64, PluginError> {
        let mut out = 0u64;
        // SAFETY: `name` outlives the call and `out` is a valid out-pointer.
        check(unsafe {
            (self.api.offset_get)(self.api.host, self.handle, Str::new(name), &raw mut out)
        })?;
        Ok(out)
    }

    /// Declare the symbols this plugin cannot work without. Call during `start` and return
    /// the error: the loader then logs the missing name and leaves the plugin unloaded,
    /// which is what makes a game update a clear message instead of a crash.
    ///
    /// # Errors
    /// The first symbol that did not resolve, named in the message.
    pub fn require_symbols<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), PluginError> {
        for name in names {
            // SAFETY: `name` outlives the call.
            let status =
                unsafe { (self.api.symbol_require)(self.api.host, self.handle, Str::new(name)) };
            if status != Status::Ok {
                return Err(PluginError::Message(format!(
                    "{name} is not available in this build"
                )));
            }
        }
        Ok(())
    }

    /// Ask for a specific backbuffer size. Only honoured during `start`.
    ///
    /// # Errors
    /// Called outside `start` or after the swapchain exists.
    pub fn request_backbuffer_size(&self, width: u32, height: u32) -> Result<(), PluginError> {
        // SAFETY: valid table pointer.
        check(unsafe {
            (self.api.request_backbuffer_size)(self.api.host, self.handle, width, height)
        })
    }

    /// Overwrite the bytes at `address`, with the loader keeping the originals.
    ///
    /// `note` is what the `hooks` console command calls this patch. The loader restores the
    /// original bytes when the plugin stops, so a patch can never outlive its owner.
    ///
    /// # Errors
    /// The address is not committed memory, the length is zero or absurd, or something has
    /// already hooked that address.
    ///
    /// # Safety
    /// The caller is patching the game's code: `address` must be the instruction boundary
    /// they mean, and `bytes` must be valid code for it.
    pub unsafe fn patch(
        &self,
        address: *mut c_void,
        bytes: &[u8],
        note: &str,
    ) -> Result<Hook, PluginError> {
        let mut id = 0u64;
        // SAFETY: `bytes` and `note` outlive the call; `id` is a valid out-pointer. The
        // caller's obligations are this function's own safety contract.
        check(unsafe {
            (self.api.hook_patch)(
                self.api.host,
                self.handle,
                address,
                bytes.as_ptr(),
                bytes.len(),
                Str::new(note),
                &raw mut id,
            )
        })?;
        Ok(Hook(id))
    }

    /// Replace one entry of the virtual table `object` points at, returning the hook and the
    /// pointer that was there, which is how the replacement reaches the original.
    ///
    /// # Errors
    /// A null pointer, a slot that is not writable, or a slot already hooked.
    ///
    /// # Safety
    /// `object` must point at an object with a virtual table of at least `index + 1` entries,
    /// and `replacement` must have the signature that entry is called with.
    pub unsafe fn hook_vtable(
        &self,
        object: *mut c_void,
        index: u32,
        replacement: *mut c_void,
        note: &str,
    ) -> Result<(Hook, *mut c_void), PluginError> {
        let (mut original, mut id) = (core::ptr::null_mut(), 0u64);
        // SAFETY: `note` outlives the call and both out-pointers are valid; the pointer
        // contracts are this function's own safety contract.
        check(unsafe {
            (self.api.hook_vtable)(
                self.api.host,
                self.handle,
                object,
                index,
                replacement,
                Str::new(note),
                &raw mut original,
                &raw mut id,
            )
        })?;
        Ok((Hook(id), original))
    }

    /// Detour `target` to `replacement`, returning the hook and a trampoline that calls the
    /// original function.
    ///
    /// # Errors
    /// A null pointer, an address that is not committed code, a prologue the loader cannot
    /// relocate, or an address something already hooked.
    ///
    /// # Safety
    /// `target` must be a function entry point and `replacement` must have its exact
    /// signature and calling convention; nothing can check that for the caller.
    pub unsafe fn detour(
        &self,
        target: *mut c_void,
        replacement: *mut c_void,
        note: &str,
    ) -> Result<(Hook, *mut c_void), PluginError> {
        let (mut trampoline, mut id) = (core::ptr::null_mut(), 0u64);
        // SAFETY: `note` outlives the call and both out-pointers are valid; the signature
        // match is this function's own safety contract.
        check(unsafe {
            (self.api.hook_detour)(
                self.api.host,
                self.handle,
                target,
                replacement,
                Str::new(note),
                &raw mut trampoline,
                &raw mut id,
            )
        })?;
        Ok((Hook(id), trampoline))
    }

    /// Subscribe to input events of these kinds, delivered to
    /// [`Plugin::on_input`](crate::Plugin::on_input).
    ///
    /// Callable at any time, not only during `start`: a plugin that only wants input while it
    /// is doing something asks for it then and passes [`Watch::NONE`] when it is done. The
    /// last call wins.
    ///
    /// # Errors
    /// The plugin has no `on_input` implementation to deliver to.
    pub fn listen_input(&self, kinds: Watch) -> Result<(), PluginError> {
        // SAFETY: valid table pointer.
        check(unsafe { (self.api.input_listen)(self.api.host, self.handle, kinds) })
    }

    /// Send input to the system, in order, as one burst.
    ///
    /// Real system input rather than a message posted to the game's window, because DayZ
    /// reads the mouse through raw input and the keyboard through a polled table and neither
    /// notices a synthesised message. It goes to whichever window has the focus, and comes
    /// back around through `on_input` like anything else — a plugin that both sends and
    /// watches must be ready to see its own input.
    ///
    /// # Errors
    /// An action this loader does not know, or a system that refused the injection.
    pub fn send_input(&self, actions: &[Action]) -> Result<(), PluginError> {
        if actions.is_empty() {
            return Ok(());
        }
        // `Action` is a single-field tuple struct around the ABI structure, so the slice can
        // be handed over as it is rather than copied.
        // SAFETY: a newtype around `InputAction` has that type's layout.
        let actions: &[dayz_plugin_api::InputAction] =
            unsafe { core::slice::from_raw_parts(actions.as_ptr().cast(), actions.len()) };
        // SAFETY: the slice is readable for its length for the duration of the call.
        check(unsafe {
            (self.api.input_send)(self.api.host, self.handle, actions.as_ptr(), actions.len())
        })
    }

    /// Whether a key is physically down now, by Win32 virtual key code.
    #[must_use]
    pub fn key_down(&self, vk: u16) -> bool {
        let mut down = 0u32;
        // SAFETY: `down` is a valid out-pointer.
        let status = unsafe {
            (self.api.input_key_down)(self.api.host, self.handle, u32::from(vk), &raw mut down)
        };
        status == Status::Ok && down != 0
    }

    /// Ask the system for raw input from an HID usage the game never registered: 1/4 for a
    /// joystick, 1/5 for a gamepad, 1/8 for a multi-axis controller.
    ///
    /// Reports then arrive as [`Input::Hid`](crate::Input::Hid) for every plugin watching
    /// [`Watch::HID`]. The game's own mouse and keyboard registration is left alone.
    ///
    /// # Errors
    /// A usage page outside the generic desktop and VR pages, a window that does not exist
    /// yet, or a system that refused the registration.
    pub fn register_hid(&self, usage_page: u16, usage: u16) -> Result<(), PluginError> {
        // SAFETY: valid table pointer.
        check(unsafe {
            (self.api.input_register_hid)(self.api.host, self.handle, usage_page, usage)
        })
    }

    /// Undo one hook now. The loader undoes whatever is left when the plugin stops, so this
    /// is only needed to remove a hook earlier than that.
    ///
    /// # Errors
    /// The hook does not exist or belongs to another plugin.
    pub fn remove_hook(&self, hook: Hook) -> Result<(), PluginError> {
        // SAFETY: valid table pointer.
        check(unsafe { (self.api.hook_remove)(self.api.host, self.handle, hook.0) })
    }
}

/// Sink for [`Host::console_capture`]: pushes each line into the caller's vector.
///
/// # Safety
/// `ctx` must be the `Vec<String>` pointer `console_capture` passed, and `line` must be
/// readable for the duration of the call, which the ABI requires.
unsafe extern "C" fn collect_line(ctx: *mut c_void, line: Str) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: the only caller is the loader, with the pointer we handed it.
    let lines = unsafe { &mut *ctx.cast::<Vec<String>>() };
    lines.push(str_from(line));
}

/// A hook the loader installed for this plugin, and the handle to remove it early.
///
/// Dropping one does nothing: the loader owns the hook and removes it when the plugin stops.
/// That is the point of registering it there rather than patching the game directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hook(u64);
