//! Windows side of the loader: DXGI exports, vtable hooks, plugin DLLs, the host API, the
//! hotkey tick and the optional console window.
//!
//! Lock discipline: the [`state()`] mutex is never held across a call into a plugin. State
//! methods return what to notify and the callers here deliver it after dropping the guard.

mod console;
mod dispatch;
mod exports;
mod guard;
mod hooks;
mod hostapi;
mod input;
mod plugins;
mod vtable;

use core::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use dayz_plugin_api::{HostApi, PluginHandle, Status};

use crate::config::{LoaderConfig, Paths};
use crate::process::{flags, Process};
use crate::state::{Phase, State};
use crate::{console as console_commands, logging};

/// The host table holds raw pointers, so it is not automatically `Sync`.
// The two marker impls below are the only unsafe code in this module.
#[allow(unsafe_code)]
struct SharedHost(&'static HostApi);

// SAFETY: every pointer in the table either is null, points at leaked `'static` storage, or
// is a function pointer; the ABI requires all of them to be usable from any thread.
#[allow(unsafe_code)]
unsafe impl Sync for SharedHost {}
// SAFETY: see the `Sync` impl.
#[allow(unsafe_code)]
unsafe impl Send for SharedHost {}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static HOST: OnceLock<SharedHost> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();
static GAME_WINDOW: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());

/// Lock the loader state.
///
/// A poisoned lock is recovered rather than propagated: the state is plain data, and a
/// panic elsewhere must not turn every later `Present` into a second panic inside the game.
pub(crate) fn state() -> MutexGuard<'static, State> {
    STATE
        .get_or_init(|| {
            Mutex::new(State::new(
                Paths::from_game_dir(&exports::game_dir()),
                LoaderConfig::default(),
            ))
        })
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The host table handed to plugins.
pub(crate) fn host_api() -> &'static HostApi {
    HOST.get().map_or_else(
        || {
            // Only reachable if a plugin is started before `init` finished, which it is not.
            let process = Process::current();
            hostapi::build(".", ".", &process)
        },
        |h| h.0,
    )
}

/// Initialise once; returns whether anything beyond plain DXGI forwarding is active.
pub(crate) fn initialize() -> bool {
    *ENABLED.get_or_init(init)
}

fn init() -> bool {
    let paths = Paths::from_game_dir(&exports::game_dir());
    let process = Process::current();
    if process.flag(flags::CONSOLE) {
        console::attach();
    }
    let (mut config, config_error) = LoaderConfig::load(&paths.config_dir.join("loader.toml"));
    if let Some(level) = process.command_line.value(flags::LOG_LEVEL) {
        level.clone_into(&mut config.log_level);
    }
    if let Err(e) = logging::init(&paths.logs_dir, config.level(), console::is_active()) {
        console::print(&format!("could not open the log file: {e}"));
    }
    log::info!(
        "dayz plugin loader {} in {}",
        env!("CARGO_PKG_VERSION"),
        paths.game_dir.display()
    );
    if let Some(e) = config_error {
        log::error!("loader.toml ignored: {e}");
    }
    for line in process.summary() {
        log::info!("{line}");
    }
    log::info!("config: {config:?}");
    if !guard::CONTAINS_HARDWARE_FAULTS {
        log::warn!("this build contains plugin panics but not hardware faults");
    }

    let host = hostapi::build(
        &paths.game_dir.to_string_lossy(),
        &paths.config_dir.to_string_lossy(),
        &process,
    );
    if HOST.set(SharedHost(host)).is_err() {
        log::error!("host table initialised twice");
    }
    if STATE.set(Mutex::new(State::new(paths, config))).is_err() {
        log::error!("state initialised twice");
    }

    // `--noplugins` overrides the config file, so a broken plugin never blocks a launch.
    let (settings, no_plugins) = {
        let guard = state();
        let off = !guard.config.enabled || process.flag(flags::NO_PLUGINS);
        (guard.paths.clone(), off)
    };
    if no_plugins {
        log::info!("plugins disabled; forwarding DXGI only");
        return false;
    }
    let disabled = state().config.disabled_plugins.clone();
    plugins::load_all(&settings, &disabled);
    let mut guard = state();
    guard.phase = Phase::Running;
    for entry in guard.hotkeys.iter() {
        let binding = entry
            .chord
            .map_or_else(|| "none".to_owned(), |c| c.to_string());
        log::info!("hotkey {} = {binding}", entry.name);
    }
    let running = guard.plugins.iter().filter(|p| p.enabled).count();
    log::info!("{running} of {} plugins running", guard.plugins.len());
    drop(guard);
    running > 0
}

/// Remember the game's output window, used to gate hotkeys on focus.
pub(crate) fn set_game_window(hwnd: *mut c_void) {
    GAME_WINDOW.store(hwnd, Ordering::Relaxed);
}

/// Once-per-frame work: poll hotkeys while the game window has focus.
///
/// Polling is the only option here. Hotkeys deliberately do not use `RegisterHotKey`, whose
/// grabs are global, and the loader has no window of its own to receive keyboard messages;
/// subclassing the game's window would fight the engine's own input handling.
pub(crate) fn tick(_swapchain: *mut c_void) {
    let focused = input::is_focused(GAME_WINDOW.load(Ordering::Relaxed));
    let fired = {
        let mut guard = state();
        if focused {
            guard.hotkeys.poll(&input::AsyncKeys)
        } else {
            guard.hotkeys.reset();
            Vec::new()
        }
    };
    for action in fired {
        log::debug!("hotkey {action}");
        dispatch::hotkey(&action);
    }
}

/// Execute a console line and deliver whatever it produced.
pub(crate) fn run_console_line(caller: Option<PluginHandle>, line: &str) -> Status {
    let (outcome, printed) = {
        let mut guard = state();
        let before = guard.console.len();
        let outcome = console_commands::execute(&mut guard, caller, line);
        let printed: Vec<String> = guard
            .console
            .iter()
            .skip(before.min(guard.console.len()))
            .cloned()
            .collect();
        (outcome, printed)
    };
    for line in printed {
        console::print(&line);
    }
    dispatch::deliver(outcome.notify);
    match outcome.command {
        Some((plugin, name, args)) => dispatch::command(plugin, &name, &args),
        None => outcome.status,
    }
}

/// Called from `DllMain` on process detach.
pub(crate) fn shutdown() {
    plugins::stop_all();
    log::info!("loader shut down");
}
