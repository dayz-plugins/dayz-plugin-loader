//! The `plugin load|stop|enable|disable` console operations.
//!
//! These are the lifecycle changes that are actually sound in-process:
//!
//! - **load** appends a plugin the loader did not start at launch, after the same dependency
//!   checks `load_all` applies. A plugin built while the game runs can be tried without
//!   restarting it, as long as the DLL has a file name nothing loaded before.
//! - **stop** calls the plugin's stop export and stops delivering callbacks. The DLL stays in
//!   the process: `FreeLibrary` would pull the ground out from under the hooks it installed,
//!   the threads it started and every pointer the loader and other plugins hold.
//! - **enable** and **disable** only pause and resume delivery, which is the cheap way to
//!   take a misbehaving plugin out of the frame path.
//!
//! Nothing here runs under the state lock while a plugin is being called, so a plugin may
//! issue these from its own command handler.

use std::path::PathBuf;

use dayz_plugin_api::Status;
use dayz_plugin_core::console::PluginOp;
use dayz_plugin_core::deps::PluginReq;

use super::{deps as external, plugins, state};

/// Perform one lifecycle operation. Returns the status and the lines to print.
pub(crate) fn perform(op: PluginOp, name: &str) -> (Status, Vec<String>) {
    match op {
        PluginOp::Load => load(name),
        PluginOp::Stop => stop(name),
        PluginOp::Enable | PluginOp::Disable => delivery(op, name),
        // The console answers the rest without reaching the platform layer.
        PluginOp::List | PluginOp::Deps | PluginOp::Reload => (Status::Unsupported, Vec::new()),
    }
}

/// Resolve `name` to a DLL in the plugin directory, accepting it with or without `.dll`.
fn plugin_file(name: &str) -> Option<PathBuf> {
    let dir = state().paths.plugins_dir.clone();
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    let with_extension = dir.join(format!("{name}.dll"));
    with_extension.is_file().then_some(with_extension)
}

/// Check the requirements of a plugin about to start against what is already running.
fn unmet_plugin_requirement(requirements: &[PluginReq]) -> Option<String> {
    let guard = state();
    for requirement in requirements {
        let found = guard
            .find_plugin(&requirement.name)
            .and_then(|h| guard.plugin(h));
        match found {
            None if requirement.optional => {}
            None => {
                return Some(format!(
                    "requires plugin {}, which is not loaded",
                    requirement.name
                ))
            }
            Some(record) if !requirement.version.matches(&record.version) => {
                return Some(format!(
                    "requires plugin {} {}, but {} is loaded",
                    requirement.name,
                    requirement.version.as_str(),
                    record.version
                ))
            }
            Some(record) => {
                if !record.enabled {
                    log::warn!(
                        "{} is required but currently stopped; starting anyway",
                        record.name
                    );
                }
            }
        }
    }
    None
}

/// Load, validate and start one plugin at runtime.
fn load(name: &str) -> (Status, Vec<String>) {
    let Some(file) = plugin_file(name) else {
        return (
            Status::NotFound,
            vec![format!(
                "no {name}.dll in {}",
                state().paths.plugins_dir.display()
            )],
        );
    };
    let described = match plugins::describe_one(&file) {
        Ok(described) => described,
        Err(e) => return (Status::Error, vec![format!("{}: {e}", file.display())]),
    };
    if state().find_plugin(&described.name).is_some() {
        return (
            Status::AlreadyExists,
            vec![format!(
                "{} is already known to this session; a plugin name can only be used once per \
                 launch, even after `plugin stop`",
                described.name
            )],
        );
    }
    let game_dir = state().paths.game_dir.clone();
    if let Some(reason) = external::unmet_external(&game_dir, &described.declared) {
        return (Status::Error, vec![format!("{}: {reason}", described.name)]);
    }
    let requirements = external::plugin_requirements(&described.name, &described.declared);
    if let Some(reason) = unmet_plugin_requirement(&requirements) {
        return (Status::Error, vec![format!("{}: {reason}", described.name)]);
    }
    match plugins::start_one(&described) {
        Ok(active) => {
            let line = format!("started {} {}", described.name, described.version);
            plugins::append(active);
            (Status::Ok, vec![line])
        }
        Err(e) => (Status::Error, vec![format!("{}: {e}", described.name)]),
    }
}

/// Call the plugin's stop export and stop delivering callbacks to it.
fn stop(name: &str) -> (Status, Vec<String>) {
    let Some(plugin) = plugins::find_by_name(name) else {
        return (Status::NotFound, vec![format!("no plugin named {name}")]);
    };
    if !plugin.is_enabled() {
        return (Status::Ok, vec![format!("{name} is already stopped")]);
    }
    plugin.call_stop();
    plugin.suspend();
    (
        Status::Ok,
        vec![format!(
            "stopped {name}; its DLL stays loaded until the game exits"
        )],
    )
}

/// Resume or pause callback delivery.
fn delivery(op: PluginOp, name: &str) -> (Status, Vec<String>) {
    let Some(plugin) = plugins::find_by_name(name) else {
        return (Status::NotFound, vec![format!("no plugin named {name}")]);
    };
    let enable = op == PluginOp::Enable;
    let changed = if enable {
        plugin.enable()
    } else {
        plugin.suspend()
    };
    let state = if enable { "enabled" } else { "disabled" };
    let line = if changed {
        format!("{name} {state}")
    } else {
        format!("{name} was already {state}")
    };
    (Status::Ok, vec![line])
}
