//! Discovery, loading and starting of plugin DLLs.
//!
//! The plugin set is immutable once [`load_all`] returns, so dispatch needs no lock and a
//! plugin that re-enters the host from a callback cannot deadlock. Disabling a faulted
//! plugin flips an atomic instead of mutating the list.

// FFI module: loads DLLs and calls their exports.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use dayz_plugin_api::{
    DescribeFn, PluginCallbacks, PluginHandle, StartFn, Status, StopFn, API_VERSION,
    DESCRIBE_EXPORT, START_EXPORT, STOP_EXPORT,
};
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};

use crate::config::Paths;
use crate::state::Phase;

pub(crate) use super::dispatch::{deliver, deliver_event, deliver_message};

/// A loaded, started plugin.
pub(crate) struct Active {
    /// Handle the state registry assigned.
    pub(crate) handle: PluginHandle,
    /// Plugin name, for log messages.
    pub(crate) name: String,
    /// Callback table the plugin filled in.
    pub(crate) callbacks: PluginCallbacks,
    /// Stop export, called at shutdown.
    pub(crate) stop: Option<StopFn>,
    /// Cleared when the plugin faults; no further callbacks are delivered.
    pub(crate) enabled: AtomicBool,
}

// SAFETY: the pointers in `callbacks` are owned by the plugin DLL, which stays loaded for
// the process's lifetime, and the ABI requires every callback to be thread safe.
unsafe impl Sync for Active {}
// SAFETY: see the `Sync` impl.
unsafe impl Send for Active {}

static ACTIVE: OnceLock<Vec<Active>> = OnceLock::new();

/// Started plugins. Empty until [`load_all`] finishes.
pub(crate) fn active() -> &'static [Active] {
    ACTIVE.get().map_or(&[], Vec::as_slice)
}

/// Find a started plugin by handle.
pub(crate) fn find(handle: PluginHandle) -> Option<&'static Active> {
    active().iter().find(|p| p.handle == handle)
}

impl Active {
    /// Whether callbacks may still be delivered to this plugin.
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Stop delivering callbacks after a fault, and record it in the registry.
    pub(crate) fn disable(&self, reason: &str) {
        if self.enabled.swap(false, Ordering::Relaxed) {
            log::error!(
                "[{}] disabled for the rest of this session: {reason}",
                self.name
            );
            super::state().set_enabled(self.handle, false);
        }
    }
}

/// DLL file names in load order: alphabetical, so the order is reproducible.
fn plugin_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        log::info!("no plugin directory at {}", dir.display());
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll")))
        .collect();
    files.sort();
    files
}

struct Exports {
    describe: DescribeFn,
    start: StartFn,
    stop: Option<StopFn>,
}

/// Resolve the three plugin exports.
fn exports(module: HMODULE) -> Option<Exports> {
    let symbol = |name: &str| {
        let c = CString::new(name).ok()?;
        // SAFETY: valid module handle and a NUL terminated name.
        unsafe { GetProcAddress(module, PCSTR(c.as_ptr().cast())) }
    };
    let describe = symbol(DESCRIBE_EXPORT)?;
    let start = symbol(START_EXPORT)?;
    let stop = symbol(STOP_EXPORT);
    // SAFETY: the exports are documented to have these signatures; a DLL that exports them
    // with a different one is a broken plugin we cannot detect from here.
    unsafe {
        Some(Exports {
            describe: core::mem::transmute::<unsafe extern "system" fn() -> isize, DescribeFn>(
                describe,
            ),
            start: core::mem::transmute::<unsafe extern "system" fn() -> isize, StartFn>(start),
            stop: stop
                .map(|s| core::mem::transmute::<unsafe extern "system" fn() -> isize, StopFn>(s)),
        })
    }
}

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Load every plugin in `paths.plugins_dir`, honouring the disable list.
pub(crate) fn load_all(paths: &Paths, disabled: &[String]) {
    let mut started = Vec::new();
    for file in plugin_files(&paths.plugins_dir) {
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if disabled.iter().any(|d| d.eq_ignore_ascii_case(&stem)) {
            log::info!("skipping {stem}: disabled in loader.toml");
            continue;
        }
        match load_one(&file) {
            Ok(active) => started.push(active),
            Err(e) => log::error!("{}: {e}", file.display()),
        }
    }
    if ACTIVE.set(started).is_err() {
        log::error!("plugins loaded twice");
    }
}

/// Load, describe and start one DLL.
fn load_one(file: &Path) -> Result<Active, String> {
    let path = wide(file);
    // SAFETY: `path` is NUL terminated; the altered search path lets a plugin ship its own
    // dependencies next to itself.
    let module =
        unsafe { LoadLibraryExW(PCWSTR(path.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
            .map_err(|e| format!("cannot load: {e}"))?;
    let exports = exports(module).ok_or_else(|| {
        format!("not a plugin: missing {DESCRIBE_EXPORT}, {START_EXPORT} or {STOP_EXPORT}")
    })?;

    // SAFETY: the export was resolved above; calling it is what the ABI is for.
    let info = unsafe { super::guard::call(|| (exports.describe)()) }
        .map_err(|e| format!("{DESCRIBE_EXPORT} faulted: {e}"))?;
    if info.is_null() {
        return Err(format!("{DESCRIBE_EXPORT} returned null"));
    }
    // SAFETY: the ABI requires the returned struct to stay valid while the DLL is loaded.
    let info = unsafe { &*info };
    if info.api_version != API_VERSION {
        return Err(format!(
            "built for API version {}, loader speaks {API_VERSION}",
            info.api_version
        ));
    }
    let name = super::hostapi::text(info.name);
    let version = super::hostapi::text(info.version);
    let file_name = file
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();

    let handle = super::state()
        .add_plugin(&name, &version, &file_name)
        .map_err(|e| format!("rejected name {name:?}: {e:?}"))?;
    log::info!("starting {name} {version} from {file_name}");

    let mut callbacks = PluginCallbacks::empty();
    let host = super::host_api();
    {
        let mut guard = super::state();
        guard.phase = Phase::Starting(handle);
    }
    // SAFETY: `host` lives for the process's lifetime and `callbacks` is writable.
    let status =
        unsafe { super::guard::call(|| (exports.start)(host, handle, &raw mut callbacks)) };
    super::state().phase = Phase::Idle;

    match status {
        Ok(Status::Ok) => {}
        Ok(other) => return Err(format!("{START_EXPORT} returned {other:?}")),
        Err(e) => return Err(format!("{START_EXPORT} faulted: {e}")),
    }
    if callbacks.struct_size < core::mem::size_of::<PluginCallbacks>() {
        log::warn!("[{name}] was built against an older callback table");
    }
    super::state().set_enabled(handle, true);
    Ok(Active {
        handle,
        name,
        callbacks,
        stop: exports.stop,
        enabled: AtomicBool::new(true),
    })
}

/// Call every plugin's stop export. Called at process shutdown.
pub(crate) fn stop_all() {
    for plugin in active() {
        let Some(stop) = plugin.stop else { continue };
        if !plugin.is_enabled() {
            continue;
        }
        let ctx = plugin.callbacks.ctx;
        // SAFETY: `ctx` is the pointer the plugin gave us in `start`.
        if let Err(e) = unsafe { super::guard::call(|| stop(ctx)) } {
            log::error!("[{}] {STOP_EXPORT} faulted: {e}", plugin.name);
        }
    }
}
