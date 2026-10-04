//! Windows side of the loader: DXGI exports, vtable hooks, plugin DLLs, the host API, the
//! hotkey tick and the optional console window.
//!
//! Lock discipline: the [`state()`] mutex is never held across a call into a plugin. State
//! methods return what to notify and the callers here deliver it after dropping the guard.

mod clipboard;
mod console;
mod data;
mod deps;
mod detour;
mod dispatch;
mod exports;
mod guard;
mod hooks;
mod hostapi;
mod hostapi_input;
mod input;
mod input_send;
mod lifecycle;
mod memory;
mod plugin_hooks;
mod plugin_input;
mod plugins;
mod session;
mod ui;
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

/// The executable the data database is keyed on.
const EXECUTABLE: &str = "DayZ_x64.exe";

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
    let console_sink: Option<logging::ConsoleSink> =
        console::is_active().then_some((console::print, crate::scrollback::clock));
    if let Err(e) = logging::init(
        &paths.logs_dir,
        config.level(),
        console_sink,
        Some(crate::scrollback::record),
    ) {
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

    data::initialize(&paths.data_dir, &paths.game_dir.join(EXECUTABLE));
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
    guard.symbol_lines = data::console_lines;
    guard.hook_lines = plugin_hooks::console_lines;
    guard.read_lines = memory::console_lines;
    guard.input_lines = plugin_input::summary;
    guard.session = session::perform;
    register_loader_hotkeys(&mut guard);
    let running = guard.plugins.iter().filter(|p| p.enabled).count();
    log::info!("{running} of {} plugins running", guard.plugins.len());
    drop(guard);
    // Last, so a command typed in the first instant cannot race the registries.
    console::start_input();
    // Queued, not drawn: there is no swapchain yet. It appears on the first frame the game
    // presents, which is also the first moment anyone could have seen it.
    ui::loader_toast(
        dayz_plugin_api::UiLevel::Success,
        &format!("DayZ Plugin Loader v{} Ready", env!("CARGO_PKG_VERSION")),
        &plugin_summary(running, &state().plugins),
    );
    running > 0
}

/// Register the loader's own overlay actions, then log what every action ended up bound to.
///
/// The console's first binding is by scan code, not by key name: `0x29` is the key under
/// Escape on every layout, and its virtual key code is a different one on each — the reason
/// binding it by name worked on one keyboard and not on the next. The second one is for the
/// layouts where that key is a dead diacritic, and for anyone who would rather not reach for
/// it.
fn register_loader_hotkeys(guard: &mut State) {
    guard.register_loader_hotkey("console", "Show the in-game console", "sc29, ctrl+shift+c");
    guard.register_loader_hotkey("settings", "Show the settings editor", "f11");
    guard.register_loader_hotkey(
        "mouse",
        "Give the mouse back to the game, or take it",
        "ctrl+shift+m",
    );
    for entry in guard.hotkeys.iter() {
        // A scan code binding also reports what this keyboard layout makes of it, because
        // "the key under Escape does nothing" is otherwise impossible to diagnose from a log.
        let resolved: Vec<String> = entry
            .chords()
            .filter_map(|c| c.scancode)
            .filter_map(|sc| {
                dayz_plugin_core::hotkeys::KeyState::vk_for_scancode(&input::AsyncKeys, sc)
                    .map(|vk| format!("sc{sc:x} is vk 0x{vk:02x} on this layout"))
            })
            .collect();
        let binding = entry.binding();
        if resolved.is_empty() {
            log::info!("hotkey {} = {binding}", entry.name);
        } else {
            log::info!(
                "hotkey {} = {binding} ({})",
                entry.name,
                resolved.join(", ")
            );
        }
    }
}

/// The toast's second line: which plugins came up, or why none did.
fn plugin_summary(running: usize, plugins: &[crate::state::PluginRecord]) -> String {
    if plugins.is_empty() {
        return "no plugins found".to_owned();
    }
    let names: Vec<&str> = plugins
        .iter()
        .filter(|p| p.enabled)
        .map(|p| p.name.as_str())
        .collect();
    if names.is_empty() {
        return format!("{} plugins found, none running", plugins.len());
    }
    format!("{running} running: {}", names.join(", "))
}

/// The game's output window, or null before the swapchain exists.
pub(crate) fn game_window() -> *mut c_void {
    GAME_WINDOW.load(Ordering::Relaxed)
}

/// Remember the game's output window, used to gate hotkeys on focus.
pub(crate) fn set_game_window(hwnd: *mut c_void) {
    GAME_WINDOW.store(hwnd, Ordering::Relaxed);
}

/// Once-per-frame work: poll hotkeys while the game window has focus.
///
/// Polling rather than messages: hotkeys deliberately do not use `RegisterHotKey`, whose
/// grabs are global, and they must keep working while the overlay is swallowing the game's
/// window messages, which `GetAsyncKeyState` does because it reads physical key state. The
/// overlay's own input comes from the window subclass in `ui::wnd` instead.
pub(crate) fn tick(_swapchain: *mut c_void) {
    let focused = input::is_focused(GAME_WINDOW.load(Ordering::Relaxed));
    // Presses are drained whether or not the game has focus, so a key pressed elsewhere does
    // not sit in the queue waiting to fire on the way back in.
    let presses = ui::key_presses();
    let fired = {
        let mut guard = state();
        // Here rather than in the overlay: the console shows the log whether or not it is
        // open, so a line logged while it was closed is in the scrollback when it opens.
        guard.console_drain();
        if focused {
            let mut fired = guard.hotkeys.poll(&input::AsyncKeys);
            let held = dayz_plugin_core::hotkeys::KeyState::modifiers(&input::AsyncKeys);
            for (scancode, _vk) in presses {
                fired.extend(guard.hotkeys.press(scancode, held));
            }
            fired
        } else {
            guard.hotkeys.reset();
            Vec::new()
        }
    };
    for action in fired {
        log::debug!("hotkey {action}");
        if handled_by_loader(&action) {
            continue;
        }
        dispatch::hotkey(&action);
    }
}

/// Whether the loader itself answers this hotkey rather than a plugin.
///
/// Two cases: its own `loader.*` actions, and the action a plugin's panel registered, which
/// toggles that panel. A plugin never sees either, so a panel's key cannot also be swallowed
/// by the plugin's own `on_hotkey`.
fn handled_by_loader(action: &str) -> bool {
    let Some((owner, name)) = action.split_once('.') else {
        return false;
    };
    if owner == "loader" {
        match name {
            "console" => ui::toggle_console(),
            "settings" => ui::toggle_editor(),
            "mouse" => ui::toggle_grab(),
            other => log::warn!("no loader action named {other}"),
        }
        return true;
    }
    let toggled = {
        let mut guard = state();
        let handle = guard.find_plugin(owner);
        handle.and_then(|handle| guard.toggle_panel(handle, name))
    };
    match toggled {
        Some(open) => {
            log::debug!("panel {action} {}", if open { "opened" } else { "closed" });
            ui::opened();
            true
        }
        None => false,
    }
}

/// Execute a console line and deliver whatever it produced, discarding the output.
pub(crate) fn run_console_line(caller: Option<PluginHandle>, line: &str) -> Status {
    run_console_line_capture(caller, line).0
}

/// Execute a console line and return both the status and everything it printed.
pub(crate) fn run_console_line_capture(
    caller: Option<PluginHandle>,
    line: &str,
) -> (Status, Vec<String>) {
    let (outcome, printed) = {
        let mut guard = state();
        let before = guard.console_mark();
        let outcome = console_commands::execute(&mut guard, caller, line);
        let printed = guard.console_since(before);
        (outcome, printed)
    };
    for line in &printed {
        console::print(line);
    }
    // The console echoes the typed line so the window and the log show what was asked. A
    // caller capturing output already knows its own line, so it is not part of the answer.
    let echo = format!("> {}", line.trim());
    let printed: Vec<String> = printed.into_iter().filter(|l| *l != echo).collect();
    dispatch::deliver(outcome.notify);
    if let Some((op, name)) = outcome.lifecycle {
        // Performed out here, with no lock held: it loads a DLL and calls into it.
        let (status, lines) = lifecycle::perform(op, &name);
        {
            let mut guard = state();
            for line in &lines {
                guard.console_print(line.clone());
            }
        }
        for line in &lines {
            console::print(line);
        }
        return (status, [printed, lines].concat());
    }
    match outcome.command {
        // A plugin command prints through `console_print`, which the lines above already
        // captured for everything before this point; its own output lands in the buffer.
        Some((plugin, name, args)) => {
            let before = state().console_mark();
            let status = dispatch::command(plugin, &name, &args);
            let after = state().console_since(before);
            (status, [printed, after].concat())
        }
        None => (outcome.status, printed),
    }
}

/// Called from `DllMain` on process detach.
pub(crate) fn shutdown() {
    plugins::stop_all();
    log::info!("loader shut down");
}
