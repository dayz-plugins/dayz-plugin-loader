//! Plain value types shared by the host and callback tables.

/// Borrowed UTF-8 string. Valid for the duration of the call it is passed to.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Str {
    /// Pointer to the first byte; may be null only when `len` is 0.
    pub ptr: *const u8,
    /// Length in bytes.
    pub len: usize,
}

impl Str {
    /// The empty string.
    pub const EMPTY: Str = Str {
        ptr: core::ptr::null(),
        len: 0,
    };

    /// Borrow a Rust string for one call.
    #[must_use]
    pub const fn new(s: &str) -> Self {
        Str {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }
}

/// Borrowed byte payload for messages and events.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Bytes {
    /// Pointer to the first byte; may be null only when `len` is 0.
    pub ptr: *const u8,
    /// Length in bytes.
    pub len: usize,
}

impl Bytes {
    /// The empty payload.
    pub const EMPTY: Bytes = Bytes {
        ptr: core::ptr::null(),
        len: 0,
    };

    /// Borrow a byte slice for one call.
    #[must_use]
    pub const fn new(b: &[u8]) -> Self {
        Bytes {
            ptr: b.as_ptr(),
            len: b.len(),
        }
    }
}

/// Opaque identifier the loader assigns to each loaded plugin.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PluginHandle(pub u32);

impl PluginHandle {
    /// Handle that never refers to a plugin.
    pub const NONE: PluginHandle = PluginHandle(0);
}

/// Result of every boundary call.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Status {
    /// Success.
    Ok = 0,
    /// Generic failure; details are in the log.
    Error = 1,
    /// The callee does not implement this operation.
    Unsupported = 2,
    /// The named plugin, setting, command or hotkey does not exist.
    NotFound = 3,
    /// An argument was malformed (bad UTF-8, out of range, wrong kind).
    InvalidArgument = 4,
    /// The name is already registered by this or another plugin.
    AlreadyExists = 5,
    /// The call is only allowed during a different phase (for example in `start`).
    WrongPhase = 6,
    /// The output buffer is too small; the required size was written to the length out-param.
    BufferTooSmall = 7,
}

/// Log severities, matching the `log` crate ordering.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LogLevel {
    /// Unrecoverable problem.
    Error = 1,
    /// Recoverable problem.
    Warn = 2,
    /// Lifecycle information.
    Info = 3,
    /// Diagnostic detail.
    Debug = 4,
    /// Very verbose tracing.
    Trace = 5,
}

/// Why a plugin's stop export is being called.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StopReason {
    /// The game is shutting down. Everything is about to disappear anyway, so do the least
    /// that is correct: a long teardown here delays the process exit for no benefit.
    Exit = 0,
    /// The plugin was stopped on request (`plugin stop` in the console) while the game keeps
    /// running. Remove hooks, stop threads and release resources: whatever is left behind
    /// stays behind for the rest of the session.
    Unload = 1,
    /// The plugin's `start` failed partway and the loader is undoing it.
    StartFailed = 2,
}

/// What a [`Dependency`] names.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DependencyKind {
    /// Another plugin, by its [`PluginInfo::name`]. Decides load order.
    Plugin = 0,
    /// A DLL that must be findable on the loader's search path, for example `openxr_loader.dll`.
    Library = 1,
    /// A file that must exist, relative to the game directory or absolute.
    File = 2,
    /// A `dayz-data` symbol or offset that must have resolved for this build.
    Symbol = 3,
}

/// One requirement the loader checks before starting a plugin.
///
/// A plugin whose mandatory dependencies are not met is never started, and the reason is
/// logged once instead of surfacing later as a crash inside the plugin.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Dependency {
    /// `size_of::<Dependency>()`.
    pub struct_size: usize,
    /// What `name` refers to.
    pub kind: DependencyKind,
    /// Plugin name, library file name, file path or symbol name.
    pub name: Str,
    /// Version requirement for [`DependencyKind::Plugin`], for example `>=1.2` or
    /// `>=1.2, <2`. Empty accepts any version, and other kinds ignore it.
    pub version: Str,
    /// Non-zero to carry on without it; the dependency then only affects load order.
    pub optional: u32,
}

/// Static description of a plugin, returned by the describe export.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PluginInfo {
    /// `size_of::<PluginInfo>()` as compiled into the plugin.
    pub struct_size: usize,
    /// Must equal [`crate::API_VERSION`].
    pub api_version: u32,
    /// Unique short name, `[a-z0-9_-]+`. Used as the settings namespace and config file name.
    pub name: Str,
    /// Human readable version.
    pub version: Str,
    /// One-line description.
    pub description: Str,
    /// Requirements, or null when `dependency_count` is 0. Must stay valid while loaded.
    pub dependencies: *const Dependency,
    /// Number of entries in `dependencies`.
    pub dependency_count: usize,
}

/// Value type of a setting.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SettingKind {
    /// `true` / `false`.
    Bool = 0,
    /// Integer within `[min, max]`.
    Int = 1,
    /// Floating point within `[min, max]`.
    Float = 2,
    /// One of the `|`-separated `choices`.
    Enum = 3,
    /// Free text.
    String = 4,
}

/// Bit flags for [`SettingDesc::flags`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingFlags(pub u32);

impl SettingFlags {
    /// No flags.
    pub const NONE: SettingFlags = SettingFlags(0);
    /// Changing the value only takes effect after the game restarts.
    pub const RESTART_REQUIRED: SettingFlags = SettingFlags(1);
    /// Runtime variable only: never written to the plugin's config file.
    pub const TRANSIENT: SettingFlags = SettingFlags(2);
}

/// A setting (also a console variable) a plugin registers during start.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SettingDesc {
    /// `size_of::<SettingDesc>()`.
    pub struct_size: usize,
    /// Key within the plugin namespace, `[a-z0-9_.]+`. Console name is `<plugin>.<key>`.
    pub key: Str,
    /// Short label for UI.
    pub title: Str,
    /// Longer help text.
    pub description: Str,
    /// Value type.
    pub kind: SettingKind,
    /// Default value encoded as text (`true`, `42`, `0.5`, `choice`, `text`).
    pub default: Str,
    /// Lower bound for numeric kinds; ignored otherwise.
    pub min: f64,
    /// Upper bound for numeric kinds; ignored otherwise.
    pub max: f64,
    /// `|`-separated choices for [`SettingKind::Enum`]; ignored otherwise.
    pub choices: Str,
    /// See [`SettingFlags`].
    pub flags: SettingFlags,
}

/// A hotkey action a plugin registers during start.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HotkeyDesc {
    /// `size_of::<HotkeyDesc>()`.
    pub struct_size: usize,
    /// Action name within the plugin namespace, `[a-z0-9_]+`.
    pub action: Str,
    /// Short label for UI.
    pub title: Str,
    /// Default binding in loader key grammar, for example `f12` or `ctrl+shift+r`.
    pub default_binding: Str,
}

/// A console command a plugin registers during start.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CommandDesc {
    /// `size_of::<CommandDesc>()`.
    pub struct_size: usize,
    /// Command name within the plugin namespace, `[a-z0-9_]+`. Console name is `<plugin>.<name>`.
    pub name: Str,
    /// One-line help.
    pub help: Str,
    /// Argument syntax shown by `help`.
    pub usage: Str,
}

/// One command line entry as the loader parsed it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ArgEntry {
    /// Normalised name without the leading `-`, `--` or `/`, lowercase. Empty for positionals.
    pub name: Str,
    /// Value, or an empty string when the entry is a bare flag. See [`ArgEntry::has_value`].
    pub value: Str,
    /// The entry exactly as it appeared on the command line.
    pub raw: Str,
    /// Whether the entry carried a value at all (an empty `--key=` still counts).
    pub has_value: u32,
}

/// One environment variable of the game process.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EnvEntry {
    /// Variable name, exactly as the process has it.
    pub name: Str,
    /// Variable value.
    pub value: Str,
}
