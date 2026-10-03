//! Discovery, loading and starting of plugin DLLs.
//!
//! Loading happens in two passes: every DLL is loaded and asked to describe itself, then the
//! declared dependencies decide who starts and in what order. A plugin therefore learns about
//! a missing dependency as a log line before it runs any code, not as a crash later.
//!
//! The started set is append only and published through an atomic pointer, so dispatch on the
//! render thread needs no lock and a plugin that re-enters the host from a callback cannot
//! deadlock. Disabling a faulted plugin flips an atomic; nothing is ever removed, because the
//! handles plugins hold are indices into this list.

// FFI module: loads DLLs and calls their exports.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Mutex;

use dayz_plugin_api::{
    DescribeFn, PluginCallbacks, PluginHandle, StartFn, Status, StopFn, API_VERSION,
    DESCRIBE_EXPORT, START_EXPORT, STOP_EXPORT,
};
use dayz_plugin_core::deps::{self, Candidate};
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};

use crate::config::Paths;
use crate::state::Phase;

use super::deps as external;

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

/// The published list. Null until the first plugin starts.
static ACTIVE: AtomicPtr<Vec<&'static Active>> = AtomicPtr::new(core::ptr::null_mut());
/// Serialises publishing. Readers never take it.
static PUBLISH: Mutex<()> = Mutex::new(());

/// Started plugins. Empty until the first one starts.
pub(crate) fn active() -> &'static [&'static Active] {
    let ptr = ACTIVE.load(Ordering::Acquire);
    if ptr.is_null() {
        return &[];
    }
    // SAFETY: a non-null pointer here came from `Box::into_raw` on a leaked `Vec` that is
    // never freed or mutated afterwards, so the slice is valid for the process's lifetime.
    unsafe { &*ptr }.as_slice()
}

/// Publish `plugins` in addition to whatever is already running.
///
/// The old vector is leaked rather than freed: a reader on the render thread may be walking
/// it at this exact moment, and one leaked vector of references per load is nothing against
/// the cost of making dispatch take a lock.
fn publish(plugins: Vec<Active>) {
    let _guard = PUBLISH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut combined: Vec<&'static Active> = active().to_vec();
    combined.extend(plugins.into_iter().map(|p| &*Box::leak(Box::new(p))));
    let boxed = Box::into_raw(Box::new(combined));
    ACTIVE.store(boxed, Ordering::Release);
}

/// Find a started plugin by handle.
pub(crate) fn find(handle: PluginHandle) -> Option<&'static Active> {
    active().iter().copied().find(|p| p.handle == handle)
}

/// Find a started plugin by name.
pub(crate) fn find_by_name(name: &str) -> Option<&'static Active> {
    active().iter().copied().find(|p| p.name == name)
}

impl Active {
    /// Whether callbacks may still be delivered to this plugin.
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Resume callback delivery. Returns whether this changed anything.
    pub(crate) fn enable(&self) -> bool {
        let changed = !self.enabled.swap(true, Ordering::Relaxed);
        if changed {
            super::state().set_enabled(self.handle, true);
        }
        changed
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

    /// Stop delivering callbacks on request, without the error log a fault produces.
    pub(crate) fn suspend(&self) -> bool {
        let changed = self.enabled.swap(false, Ordering::Relaxed);
        if changed {
            super::state().set_enabled(self.handle, false);
        }
        changed
    }

    /// Call the stop export, if the plugin has one.
    pub(crate) fn call_stop(&self) {
        let Some(stop) = self.stop else { return };
        let ctx = self.callbacks.ctx;
        // SAFETY: `ctx` is the pointer the plugin gave us in `start`.
        if let Err(e) = unsafe { super::guard::call(|| stop(ctx)) } {
            log::error!("[{}] {STOP_EXPORT} faulted: {e}", self.name);
        }
    }
}

/// DLL file names in discovery order: alphabetical, so the order is reproducible.
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

/// A DLL that loaded and described itself, before any dependency decision.
pub(super) struct Described {
    /// File it came from.
    pub(super) file: PathBuf,
    /// Name from the describe export.
    pub(super) name: String,
    /// Version from the describe export.
    pub(super) version: String,
    /// What it declared it needs.
    pub(super) declared: Vec<external::Declared>,
    exports: Exports,
}

/// First pass: load a DLL and read its description. Nothing is started yet.
pub(super) fn describe_one(file: &Path) -> Result<Described, String> {
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
    Ok(Described {
        file: file.to_path_buf(),
        name: super::hostapi::text(info.name),
        version: super::hostapi::text(info.version),
        declared: external::declared(info),
        exports,
    })
}

/// Second pass: register and start one described plugin.
pub(super) fn start_one(described: &Described) -> Result<Active, String> {
    let Described {
        file,
        name,
        version,
        declared,
        exports,
    } = described;
    let file_name = file
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();

    let handle = super::state()
        .add_plugin(name, version, &file_name)
        .map_err(|e| format!("rejected name {name:?}: {e:?}"))?;
    log::info!("starting {name} {version} from {file_name}");
    for dependency in declared {
        log::debug!("[{name}] needs {}", dependency.describe());
    }

    let mut callbacks = PluginCallbacks::empty();
    let host = super::host_api();
    let previous = {
        let mut guard = super::state();
        guard.set_dependencies(handle, declared.iter().map(external::Declared::describe));
        let previous = guard.phase;
        guard.phase = Phase::Starting(handle);
        previous
    };
    // SAFETY: `host` lives for the process's lifetime and `callbacks` is writable.
    let status =
        unsafe { super::guard::call(|| (exports.start)(host, handle, &raw mut callbacks)) };
    super::state().phase = previous;

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
        name: name.clone(),
        callbacks,
        stop: exports.stop,
        enabled: AtomicBool::new(true),
    })
}

/// Load every plugin in `paths.plugins_dir`, honouring the disable list.
pub(crate) fn load_all(paths: &Paths, disabled: &[String]) {
    let mut described = Vec::new();
    for file in plugin_files(&paths.plugins_dir) {
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if disabled.iter().any(|d| d.eq_ignore_ascii_case(&stem)) {
            log::info!("skipping {stem}: disabled in loader.toml");
            continue;
        }
        match describe_one(&file) {
            Ok(plugin) => described.push(plugin),
            Err(e) => log::error!("{}: {e}", file.display()),
        }
    }
    described.retain(
        |plugin| match external::unmet_external(&paths.game_dir, &plugin.declared) {
            Some(reason) => {
                log::error!("[{}] not started: {reason}", plugin.name);
                false
            }
            None => true,
        },
    );

    let candidates: Vec<Candidate> = described
        .iter()
        .map(|plugin| Candidate {
            name: plugin.name.clone(),
            version: plugin.version.clone(),
            requires: external::plugin_requirements(&plugin.name, &plugin.declared),
        })
        .collect();
    let plan = deps::plan(&candidates);
    for (index, reason) in &plan.rejected {
        log::error!("[{}] not started: {reason}", candidates[*index].name);
    }

    let mut started = Vec::new();
    for index in plan.order {
        match start_one(&described[index]) {
            Ok(active) => started.push(active),
            Err(e) => log::error!("{}: {e}", described[index].file.display()),
        }
    }
    publish(started);
}

/// Append one plugin that was started after the initial load.
pub(super) fn append(plugin: Active) {
    publish(vec![plugin]);
}

/// Call every plugin's stop export. Called at process shutdown.
pub(crate) fn stop_all() {
    for &plugin in active() {
        if plugin.is_enabled() {
            plugin.call_stop();
        }
    }
}
