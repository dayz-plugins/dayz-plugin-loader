//! The loader's registries. Pure data plus the rules for mutating it; the Windows side
//! holds this behind a mutex and performs the plugin calls that these methods *request*
//! (see [`Notify`]) after releasing the lock, so plugins can re-enter the host freely.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use dayz_plugin_api::{PluginHandle, Status};
use dayz_plugin_core::{hotkeys, keys, names, settings, store, windows};

use crate::config::{LoaderConfig, Paths};
use crate::scrollback;

/// Lines the console keeps for display.
pub const CONSOLE_HISTORY: usize = 500;

/// A console command registered by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInfo {
    /// One-line help.
    pub help: String,
    /// Argument syntax.
    pub usage: String,
}

/// One row of the settings editor's hotkey table.
#[derive(Debug, Clone)]
pub struct HotkeyRow {
    /// Qualified action name, `<plugin>.<action>`.
    pub name: String,
    /// What the action does, as the plugin described it.
    pub title: String,
    /// Current binding in config form.
    pub binding: String,
    /// What it would be without an override.
    pub default: String,
}

/// One plugin's worth of the settings editor.
#[derive(Debug, Clone)]
pub struct EditorSection {
    /// Plugin name, or `loader` for the loader's own actions.
    pub plugin: String,
    /// Whether the plugin is running; a stopped one is still listed, greyed out.
    pub running: bool,
    /// Settings with their current values.
    pub settings: Vec<(settings::Desc, String)>,
    /// Hotkeys belonging to this plugin.
    pub hotkeys: Vec<HotkeyRow>,
}

/// A UI panel a plugin registered. The loader owns the window and this state; the plugin
/// only fills the body.
#[derive(Debug, Clone)]
pub struct Panel {
    /// Unqualified name, which is also the hotkey action when one was requested.
    pub name: String,
    /// Window title.
    pub title: String,
    /// Whether the window is currently shown.
    pub open: bool,
}

/// Everything the loader tracks about one plugin DLL.
#[derive(Debug)]
pub struct PluginRecord {
    /// Unique name from the describe export.
    pub name: String,
    /// Version text from the describe export.
    pub version: String,
    /// DLL file name, for messages.
    pub file: String,
    /// Settings registry.
    pub settings: settings::Registry,
    /// Topics this plugin receives.
    pub subscriptions: BTreeSet<String>,
    /// Registered console commands by unqualified name.
    pub commands: BTreeMap<String, CommandInfo>,
    /// Dependencies the plugin declared, one display line each.
    pub dependencies: Vec<String>,
    /// UI panels by unqualified name, in registration order.
    pub panels: Vec<Panel>,
    /// Start returned success and no fault happened since.
    pub enabled: bool,
}

/// A plugin call the Windows side must make once the state lock is released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notify {
    /// `on_setting_changed(key, value)` on `plugin`.
    SettingChanged {
        /// Owner of the setting.
        plugin: PluginHandle,
        /// Unqualified key.
        key: String,
        /// Canonical value.
        value: String,
    },
}

/// Which registrations are currently allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Before any plugin starts; `request_backbuffer_size` and registrations are refused.
    Idle,
    /// Inside the start call of this plugin.
    Starting(PluginHandle),
    /// All plugins started; registrations are refused, the swapchain may exist.
    Running,
}

/// Loader state.
#[derive(Debug)]
pub struct State {
    /// Directory layout.
    pub paths: Paths,
    /// Parsed `loader.toml`.
    pub config: LoaderConfig,
    /// Plugins in load order; handle `n` is index `n - 1`.
    pub plugins: Vec<PluginRecord>,
    /// All hotkeys.
    pub hotkeys: hotkeys::Registry,
    /// User bindings from `hotkeys.toml`.
    pub hotkey_overrides: BTreeMap<String, String>,
    /// Current phase.
    pub phase: Phase,
    /// Backbuffer size requested by a plugin, if any.
    pub backbuffer_override: Option<(u32, u32)>,
    /// Plugins that described themselves but are waiting for a dependency: name and reason.
    ///
    /// They are not rejected, only not started yet. `plugin load <name>` starts one, and so
    /// does the dependency it waits for becoming available.
    pub pending: Vec<(String, String)>,
    /// Recent console output and log records, oldest first.
    pub console: VecDeque<scrollback::Line>,
    /// How many lines the console has ever held.
    ///
    /// The buffer above is bounded, so a position in it says nothing once it is full. This
    /// counter is what [`State::console_mark`] hands out and never goes backwards.
    pub console_printed: u64,
    /// Produces the lines the `symbols` command prints.
    ///
    /// A function pointer, because the symbol table belongs to the platform layer and this
    /// module must stay free of it. The platform layer installs the real one at startup.
    pub symbol_lines: fn(Option<&str>) -> Vec<String>,
    /// Produces the lines the `hooks` command prints. A function pointer for the same reason
    /// as [`State::symbol_lines`]: the hook registry belongs to the platform layer.
    pub hook_lines: fn() -> Vec<String>,
    /// Produces the lines the `read` command prints, for the same reason as the two above:
    /// reading the game's memory is the platform layer's business.
    pub read_lines: fn(&str, Option<usize>) -> Vec<String>,
    /// Produces the lines the `input` command prints: who watches the input stream.
    pub input_lines: fn() -> Vec<String>,
    /// Where the overlay's windows were left, and whether advanced settings are shown.
    pub windows: windows::Layout,
}

/// Default for [`State::symbol_lines`]: no database, nothing to print.
fn no_symbols(_prefix: Option<&str>) -> Vec<String> {
    Vec::new()
}

/// Default for [`State::read_lines`]: no platform layer, so no memory to read.
fn no_memory(_target: &str, _count: Option<usize>) -> Vec<String> {
    vec!["reading memory needs the platform layer".to_owned()]
}

/// Default for [`State::input_lines`]: no platform layer, so no window to watch.
fn no_input() -> Vec<String> {
    Vec::new()
}

/// Default for [`State::hook_lines`]: no platform layer, so no hooks.
fn no_hooks() -> Vec<String> {
    Vec::new()
}

impl State {
    /// Fresh state for a game directory.
    #[must_use]
    pub fn new(paths: Paths, config: LoaderConfig) -> Self {
        let hotkey_overrides = store::read(&paths.hotkeys_file()).unwrap_or_else(|e| {
            log::warn!("ignoring hotkey overrides: {e}");
            BTreeMap::new()
        });
        let windows =
            windows::Layout::from_store(&store::read(&paths.windows_file()).unwrap_or_else(|e| {
                log::warn!("ignoring window layout: {e}");
                BTreeMap::new()
            }));
        State {
            windows,
            paths,
            config,
            plugins: Vec::new(),
            hotkeys: hotkeys::Registry::default(),
            hotkey_overrides,
            phase: Phase::Idle,
            backbuffer_override: None,
            pending: Vec::new(),
            console: VecDeque::new(),
            console_printed: 0,
            symbol_lines: no_symbols,
            hook_lines: no_hooks,
            read_lines: no_memory,
            input_lines: no_input,
        }
    }

    /// Record a plugin after a successful describe; returns its handle.
    pub fn add_plugin(
        &mut self,
        name: &str,
        version: &str,
        file: &str,
    ) -> Result<PluginHandle, Status> {
        if names::plugin_name(name).is_err() || self.find_plugin(name).is_some() {
            return Err(Status::InvalidArgument);
        }
        self.plugins.push(PluginRecord {
            name: name.to_owned(),
            version: version.to_owned(),
            file: file.to_owned(),
            settings: settings::Registry::default(),
            subscriptions: BTreeSet::new(),
            commands: BTreeMap::new(),
            dependencies: Vec::new(),
            panels: Vec::new(),
            enabled: false,
        });
        Ok(PluginHandle(
            u32::try_from(self.plugins.len()).map_err(|_| Status::Error)?,
        ))
    }

    /// Record by handle.
    #[must_use]
    pub fn plugin(&self, handle: PluginHandle) -> Option<&PluginRecord> {
        handle
            .0
            .checked_sub(1)
            .and_then(|i| self.plugins.get(i as usize))
    }

    fn plugin_mut(&mut self, handle: PluginHandle) -> Option<&mut PluginRecord> {
        handle
            .0
            .checked_sub(1)
            .and_then(|i| self.plugins.get_mut(i as usize))
    }

    /// Handle of a started plugin by name.
    #[must_use]
    pub fn find_plugin(&self, name: &str) -> Option<PluginHandle> {
        self.plugins
            .iter()
            .position(|p| p.name == name)
            .and_then(|i| u32::try_from(i + 1).ok())
            .map(PluginHandle)
    }

    fn require_starting(&self, plugin: PluginHandle) -> Result<(), Status> {
        if self.phase == Phase::Starting(plugin) {
            Ok(())
        } else {
            Err(Status::WrongPhase)
        }
    }

    /// Register a setting for the plugin currently starting.
    pub fn register_setting(
        &mut self,
        plugin: PluginHandle,
        desc: settings::Desc,
    ) -> Result<(), Status> {
        self.require_starting(plugin)?;
        names::setting_key(&desc.key).map_err(|_| Status::InvalidArgument)?;
        let record = self.plugin(plugin).ok_or(Status::NotFound)?;
        let stored = crate::config::load_plugin_values(&self.paths, &record.name);
        let value = stored.get(&desc.key).map(String::as_str);
        let record = self.plugin_mut(plugin).ok_or(Status::NotFound)?;
        record.settings.register(desc, value).map_err(|e| {
            log::error!("{}: {e}", record.name);
            match e {
                settings::SettingError::Duplicate(_) => Status::AlreadyExists,
                _ => Status::InvalidArgument,
            }
        })
    }

    /// Resolve `name` (own key or `plugin.key`) relative to `caller`.
    fn resolve(&self, caller: Option<PluginHandle>, name: &str) -> Option<(PluginHandle, String)> {
        if let Some(handle) = caller {
            if self.plugin(handle)?.settings.desc(name).is_some() {
                return Some((handle, name.to_owned()));
            }
        }
        // Plugin names contain no dots, so the first dot always separates plugin and key.
        let (plugin, key) = names::split_qualified(name)?;
        let handle = self.find_plugin(plugin)?;
        self.plugin(handle)?.settings.desc(key)?;
        Some((handle, key.to_owned()))
    }

    /// Read a setting as text.
    pub fn get_setting(&self, caller: Option<PluginHandle>, name: &str) -> Result<String, Status> {
        let (handle, key) = self.resolve(caller, name).ok_or(Status::NotFound)?;
        let record = self.plugin(handle).ok_or(Status::NotFound)?;
        record
            .settings
            .get(&key)
            .map(str::to_owned)
            .ok_or(Status::NotFound)
    }

    /// Validate, store and persist a setting. Returns the notification to deliver, if any,
    /// or the validation message on failure.
    pub fn set_setting(
        &mut self,
        caller: Option<PluginHandle>,
        name: &str,
        value: &str,
    ) -> Result<Option<Notify>, (Status, String)> {
        let (handle, key) = self
            .resolve(caller, name)
            .ok_or((Status::NotFound, format!("unknown setting {name}")))?;
        let config_path = self.paths.plugin_config(
            &self
                .plugin(handle)
                .map(|p| p.name.clone())
                .unwrap_or_default(),
        );
        let record = self
            .plugin_mut(handle)
            .ok_or((Status::NotFound, String::new()))?;
        let changed = record
            .settings
            .set(&key, value)
            .map_err(|e| (Status::InvalidArgument, e.to_string()))?;
        if changed.persist && changed.differs {
            if let Err(e) = store::write(&config_path, &record.settings.persistent_values()) {
                log::error!("could not save settings: {e}");
            }
        }
        if !changed.differs {
            return Ok(None);
        }
        Ok(Some(Notify::SettingChanged {
            plugin: handle,
            key,
            value: changed.value,
        }))
    }

    /// Register a hotkey for the plugin currently starting. Returns the qualified name.
    pub fn register_hotkey(
        &mut self,
        plugin: PluginHandle,
        action: &str,
        title: &str,
        default: &str,
    ) -> Result<String, Status> {
        self.require_starting(plugin)?;
        names::action_name(action).map_err(|_| Status::InvalidArgument)?;
        let name = format!(
            "{}.{action}",
            self.plugin(plugin).ok_or(Status::NotFound)?.name
        );
        match self
            .hotkeys
            .register(&name, title, default, &self.hotkey_overrides)
        {
            Ok(None) => Ok(name),
            Ok(Some(bad_override)) => {
                log::warn!("hotkeys.toml: {name}: {bad_override}; using default {default:?}");
                Ok(name)
            }
            Err(hotkeys::HotkeyError::Duplicate(_)) => Err(Status::AlreadyExists),
            Err(e) => {
                log::error!("{e}");
                Err(Status::InvalidArgument)
            }
        }
    }

    /// Register a console command for the plugin currently starting.
    pub fn register_command(
        &mut self,
        plugin: PluginHandle,
        name: &str,
        info: CommandInfo,
    ) -> Result<(), Status> {
        self.require_starting(plugin)?;
        names::action_name(name).map_err(|_| Status::InvalidArgument)?;
        let record = self.plugin_mut(plugin).ok_or(Status::NotFound)?;
        if record.commands.contains_key(name) {
            return Err(Status::AlreadyExists);
        }
        record.commands.insert(name.to_owned(), info);
        Ok(())
    }

    /// Subscribe the plugin currently starting to a topic.
    pub fn subscribe(&mut self, plugin: PluginHandle, topic: &str) -> Result<(), Status> {
        self.require_starting(plugin)?;
        if topic.is_empty() {
            return Err(Status::InvalidArgument);
        }
        self.plugin_mut(plugin)
            .ok_or(Status::NotFound)?
            .subscriptions
            .insert(topic.to_owned());
        Ok(())
    }

    /// Enabled plugins subscribed to `topic`, excluding `from`.
    #[must_use]
    pub fn subscribers(&self, from: PluginHandle, topic: &str) -> Vec<PluginHandle> {
        self.plugins
            .iter()
            .enumerate()
            .filter(|(_, p)| p.enabled && p.subscriptions.contains(topic))
            .filter_map(|(i, _)| u32::try_from(i + 1).ok().map(PluginHandle))
            .filter(|h| *h != from)
            .collect()
    }

    /// Record a backbuffer size request from the plugin currently starting.
    pub fn request_backbuffer(
        &mut self,
        plugin: PluginHandle,
        width: u32,
        height: u32,
    ) -> Result<(), Status> {
        self.require_starting(plugin)?;
        if width == 0 || height == 0 {
            return Err(Status::InvalidArgument);
        }
        if let Some(previous) = self.backbuffer_override {
            log::warn!("backbuffer override {previous:?} replaced by {width}x{height}");
        }
        self.backbuffer_override = Some((width, height));
        Ok(())
    }

    /// Note that a plugin is waiting for a dependency, replacing any earlier note.
    pub fn set_pending(&mut self, plugin: &str, reason: &str) {
        self.pending.retain(|(name, _)| name != plugin);
        self.pending.push((plugin.to_owned(), reason.to_owned()));
    }

    /// Forget that a plugin was waiting, because it started or will never start.
    pub fn clear_pending(&mut self, plugin: &str) {
        self.pending.retain(|(name, _)| name != plugin);
    }

    /// Record the dependencies a plugin declared, for the console to show.
    pub fn set_dependencies(
        &mut self,
        plugin: PluginHandle,
        lines: impl IntoIterator<Item = String>,
    ) {
        if let Some(record) = self.plugin_mut(plugin) {
            record.dependencies = lines.into_iter().collect();
        }
    }

    /// Mark a plugin started or stopped/faulted.
    pub fn set_enabled(&mut self, plugin: PluginHandle, enabled: bool) {
        if let Some(record) = self.plugin_mut(plugin) {
            record.enabled = enabled;
        }
    }

    /// The descriptor of a setting, for a UI that has to pick a widget for it.
    pub fn setting_desc(&self, caller: Option<PluginHandle>, name: &str) -> Option<settings::Desc> {
        let (handle, key) = self.resolve(caller, name)?;
        self.plugin(handle)?.settings.desc(&key).cloned()
    }

    /// Register a UI panel for the plugin currently starting. Returns the qualified name.
    pub fn register_panel(
        &mut self,
        plugin: PluginHandle,
        name: &str,
        title: &str,
        default_open: bool,
    ) -> Result<String, Status> {
        self.require_starting(plugin)?;
        names::action_name(name).map_err(|_| Status::InvalidArgument)?;
        let record = self.plugin_mut(plugin).ok_or(Status::NotFound)?;
        if record.panels.iter().any(|p| p.name == name) {
            return Err(Status::AlreadyExists);
        }
        record.panels.push(Panel {
            name: name.to_owned(),
            title: title.to_owned(),
            open: default_open,
        });
        Ok(format!("{}.{name}", record.name))
    }

    /// Open or close one panel of one plugin. Returns whether it exists.
    pub fn set_panel_open(&mut self, plugin: PluginHandle, name: &str, open: bool) -> bool {
        let Some(record) = self.plugin_mut(plugin) else {
            return false;
        };
        match record.panels.iter_mut().find(|p| p.name == name) {
            Some(panel) => {
                panel.open = open;
                true
            }
            None => false,
        }
    }

    /// Flip one panel open or closed. Returns the new state, or `None` if it does not exist.
    pub fn toggle_panel(&mut self, plugin: PluginHandle, name: &str) -> Option<bool> {
        let record = self.plugin_mut(plugin)?;
        let panel = record.panels.iter_mut().find(|p| p.name == name)?;
        panel.open = !panel.open;
        Some(panel.open)
    }

    /// Whether one panel of one plugin is open.
    pub fn panel_open(&self, plugin: PluginHandle, name: &str) -> Option<bool> {
        let record = self.plugin(plugin)?;
        record
            .panels
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.open)
    }

    /// Every panel of every running plugin: owner, plugin name, panel name, title, open.
    ///
    /// Taken as a snapshot so the overlay can draw without holding the state lock, which it
    /// must not do: filling a panel body calls into the plugin.
    pub fn panel_list(&self) -> Vec<(PluginHandle, String, String, String, bool)> {
        self.plugins
            .iter()
            .enumerate()
            .filter(|(_, record)| record.enabled)
            .flat_map(|(index, record)| {
                let handle = PluginHandle(u32::try_from(index + 1).unwrap_or(0));
                record.panels.iter().map(move |panel| {
                    (
                        handle,
                        record.name.clone(),
                        panel.name.clone(),
                        panel.title.clone(),
                        panel.open,
                    )
                })
            })
            .collect()
    }

    /// Everything the settings editor shows for one plugin.
    ///
    /// Taken as a snapshot because the editor draws without the lock held: writing a setting
    /// calls into the owning plugin, and the lock is never held across a call into a plugin.
    pub fn editor_snapshot(&self) -> Vec<EditorSection> {
        let mut sections: Vec<EditorSection> = self
            .plugins
            .iter()
            .map(|record| EditorSection {
                plugin: record.name.clone(),
                running: record.enabled,
                settings: record
                    .settings
                    .iter()
                    .map(|(desc, value)| (desc.clone(), value.to_owned()))
                    .collect(),
                hotkeys: Vec::new(),
            })
            .collect();
        // The loader's own actions belong to nobody's plugin, and are worth editing too.
        sections.push(EditorSection {
            plugin: "loader".to_owned(),
            running: true,
            settings: Vec::new(),
            hotkeys: Vec::new(),
        });
        for entry in self.hotkeys.iter() {
            let Some((owner, _)) = entry.name.split_once('.') else {
                continue;
            };
            let Some(section) = sections.iter_mut().find(|s| s.plugin == owner) else {
                continue;
            };
            section.hotkeys.push(HotkeyRow {
                name: entry.name.clone(),
                title: entry.title.clone(),
                binding: entry.binding(),
                default: entry.default_binding(),
            });
        }
        sections.retain(|s| !s.settings.is_empty() || !s.hotkeys.is_empty());
        sections
    }

    /// Write the window layout back to `windows.toml` if anything moved.
    ///
    /// Called when the user lets go of the mouse rather than every frame: a window being
    /// dragged changes position sixty times a second and none of those are worth a file.
    pub fn save_windows(&mut self) {
        if !self.windows.take_dirty() {
            return;
        }
        let path = self.paths.windows_file();
        if let Err(e) = store::write(&path, &self.windows.to_store()) {
            log::error!("could not save the window layout: {e}");
        }
    }

    /// Change one binding and remember it in `hotkeys.toml`.
    ///
    /// Returns whether the action exists. A binding equal to the default is removed from the
    /// overrides rather than written, so a changed default still reaches the user later.
    pub fn rebind_hotkey(&mut self, name: &str, chord: Option<keys::Chord>) -> bool {
        let default = self
            .hotkeys
            .iter()
            .find(|e| e.name == name)
            .map(hotkeys::Entry::default_binding);
        if !self.hotkeys.rebind(name, chord) {
            return false;
        }
        let binding = keys::format_list(&chord.into_iter().collect::<Vec<keys::Chord>>());
        if default.as_deref() == Some(binding.as_str()) {
            self.hotkey_overrides.remove(name);
        } else {
            self.hotkey_overrides.insert(name.to_owned(), binding);
        }
        let path = self.paths.hotkeys_file();
        if let Err(e) = store::write(&path, &self.hotkey_overrides) {
            log::error!("could not save hotkeys: {e}");
        }
        true
    }

    /// Register a hotkey the loader itself handles, named `loader.<action>`.
    ///
    /// Plugin hotkeys go through [`State::register_hotkey`], which refuses anything outside a
    /// plugin's `start`. The loader has no such phase, and its own actions must still appear
    /// in `hotkeys.toml` and the hotkey listing like everyone else's.
    pub fn register_loader_hotkey(&mut self, action: &str, title: &str, default: &str) {
        let name = format!("loader.{action}");
        match self
            .hotkeys
            .register(&name, title, default, &self.hotkey_overrides)
        {
            Ok(None) => {}
            Ok(Some(bad_override)) => {
                log::warn!("hotkeys.toml: {name}: {bad_override}; using default {default:?}");
            }
            Err(e) => log::error!("{e}"),
        }
    }

    /// Append a console line, trimming history.
    ///
    /// Queued log records are taken first, so a command's output lands after whatever the
    /// loader logged on the way to producing it rather than in front of it.
    pub fn console_print(&mut self, line: String) {
        // Under a target the scrollback drops, so the log file carries the console session
        // without the console showing every line twice. See `scrollback::ECHO_TARGET`.
        log::debug!(target: scrollback::ECHO_TARGET, "{line}");
        self.console_drain();
        let kind = if line.starts_with("> ") {
            scrollback::Kind::Echo
        } else {
            scrollback::Kind::Output
        };
        self.console_push(kind, String::new(), line);
    }

    /// Move everything the logger has queued into the buffer.
    ///
    /// Called from [`State::console_print`] and once a frame by the platform layer, which are
    /// the two moments this lock is already held.
    pub fn console_drain(&mut self) {
        for (level, target, message) in scrollback::take() {
            self.console_push(scrollback::Kind::Log(level), target, message);
        }
    }

    fn console_push(&mut self, kind: scrollback::Kind, target: String, text: String) {
        if self.console.len() >= CONSOLE_HISTORY {
            self.console.pop_front();
        }
        self.console_printed += 1;
        self.console.push_back(scrollback::Line {
            seq: self.console_printed,
            kind,
            clock: scrollback::clock(),
            target,
            text,
        });
    }

    /// Everything a console line could name, for the input box's completion.
    ///
    /// The built-in words plus every registered setting and command, qualified. Taken as a
    /// snapshot like the rest of what the overlay draws, because the lock is not held while
    /// the frame runs.
    pub fn completion_names(&self) -> Vec<String> {
        let mut names: Vec<String> = dayz_plugin_core::console::BUILTIN_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        for record in &self.plugins {
            for (desc, _) in record.settings.iter() {
                names.push(format!("{}.{}", record.name, desc.key));
            }
            for command in record.commands.keys() {
                names.push(format!("{}.{command}", record.name));
            }
        }
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Remember where the console is, for [`State::console_since`].
    pub fn console_mark(&self) -> u64 {
        self.console_printed
    }

    /// Every line a command printed since `mark`, oldest first.
    ///
    /// Log records are left out: this is what a plugin calling `console_exec` gets back as
    /// the answer to its own line, and whatever else the loader happened to log while the
    /// command ran is not part of that answer.
    ///
    /// Lines the bounded buffer has already dropped cannot be returned, so a command that
    /// printed more than the whole history is reported from where the history starts.
    pub fn console_since(&self, mark: u64) -> Vec<String> {
        self.console
            .iter()
            .filter(|line| line.seq > mark && !matches!(line.kind, scrollback::Kind::Log(_)))
            .map(scrollback::Line::flat)
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn state() -> State {
        let dir = std::env::temp_dir().join(format!(
            "dayz-loader-state-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        State::new(Paths::from_game_dir(&dir), LoaderConfig::default())
    }

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn desc(key: &str) -> settings::Desc {
        settings::Desc {
            key: key.into(),
            title: String::new(),
            description: String::new(),
            kind: settings::Kind::Int,
            default: "1".into(),
            min: 0.0,
            max: 10.0,
            choices: Vec::new(),
            restart_required: false,
            advanced: false,
            transient: false,
        }
    }

    pub(crate) fn started(s: &mut State, name: &str) -> PluginHandle {
        let h = s
            .add_plugin(name, "1", &format!("{name}.dll"))
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.phase = Phase::Starting(h);
        h
    }

    #[test]
    fn registrations_require_start_phase() {
        let mut s = state();
        let h = s
            .add_plugin("a", "1", "a.dll")
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(s.register_setting(h, desc("x")), Err(Status::WrongPhase));
        s.phase = Phase::Starting(h);
        assert_eq!(s.register_setting(h, desc("x")), Ok(()));
        assert_eq!(s.register_setting(h, desc("x")), Err(Status::AlreadyExists));
        assert_eq!(
            s.register_setting(h, desc("Bad Key")),
            Err(Status::InvalidArgument)
        );
        assert_eq!(
            s.register_hotkey(h, "go", "Go", "f5"),
            Ok("a.go".to_owned())
        );
        assert_eq!(
            s.register_hotkey(h, "go", "Go", "f5"),
            Err(Status::AlreadyExists)
        );
        assert_eq!(
            s.register_hotkey(h, "bad", "Bad", "nokey"),
            Err(Status::InvalidArgument)
        );
        assert_eq!(s.request_backbuffer(h, 0, 1), Err(Status::InvalidArgument));
        assert_eq!(s.request_backbuffer(h, 1600, 1600), Ok(()));
        s.phase = Phase::Running;
        assert_eq!(s.request_backbuffer(h, 1, 1), Err(Status::WrongPhase));
        assert_eq!(
            s.add_plugin("a", "1", "a2.dll"),
            Err(Status::InvalidArgument)
        );
    }

    #[test]
    fn settings_resolve_own_and_qualified_names() {
        let mut s = state();
        let a = started(&mut s, "a");
        s.register_setting(a, desc("x"))
            .unwrap_or_else(|e| panic!("{e:?}"));
        let b = started(&mut s, "b");
        s.register_setting(b, desc("x"))
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.phase = Phase::Running;
        s.set_enabled(a, true);
        s.set_enabled(b, true);

        assert_eq!(s.get_setting(Some(a), "x"), Ok("1".into()));
        assert_eq!(s.get_setting(None, "x"), Err(Status::NotFound));
        assert_eq!(s.get_setting(None, "b.x"), Ok("1".into()));
        let notify = s
            .set_setting(Some(a), "b.x", "7")
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            notify,
            Some(Notify::SettingChanged {
                plugin: b,
                key: "x".into(),
                value: "7".into()
            })
        );
        assert_eq!(
            s.set_setting(None, "b.x", "7"),
            Ok(None),
            "unchanged value notifies nobody"
        );
        assert!(matches!(
            s.set_setting(None, "b.x", "99"),
            Err((Status::InvalidArgument, _))
        ));
        assert!(matches!(
            s.set_setting(None, "c.x", "1"),
            Err((Status::NotFound, _))
        ));
        assert!(s.paths.plugin_config("b").exists(), "persisted to disk");
        assert_eq!(crate::config::load_plugin_values(&s.paths, "b")["x"], "7");
        let _ = std::fs::remove_dir_all(&s.paths.game_dir);
    }

    #[test]
    fn subscribers_exclude_sender_and_disabled() {
        let mut s = state();
        let a = started(&mut s, "a");
        s.subscribe(a, "pose").unwrap_or_else(|e| panic!("{e:?}"));
        let b = started(&mut s, "b");
        s.subscribe(b, "pose").unwrap_or_else(|e| panic!("{e:?}"));
        let c = started(&mut s, "c");
        s.subscribe(c, "pose").unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(s.subscribe(c, ""), Err(Status::InvalidArgument));
        for h in [a, b] {
            s.set_enabled(h, true);
        }
        assert_eq!(s.subscribers(a, "pose"), vec![b]);
        assert!(s.subscribers(a, "other").is_empty());
    }

    #[test]
    fn captured_output_survives_a_full_history() {
        let mut s = state();
        // Fill the buffer first: a position in it is then meaningless, which is the bug this
        // guards against — capture used to skip by index and returned nothing.
        for i in 0..(CONSOLE_HISTORY + 5) {
            s.console_print(format!("old {i}"));
        }
        let mark = s.console_mark();
        s.console_print("new 1".to_owned());
        s.console_print("new 2".to_owned());
        assert_eq!(
            s.console_since(mark),
            vec!["new 1".to_owned(), "new 2".to_owned()]
        );
    }

    #[test]
    fn capture_cannot_return_evicted_lines() {
        let mut s = state();
        let mark = s.console_mark();
        for i in 0..(CONSOLE_HISTORY + 5) {
            s.console_print(i.to_string());
        }
        let captured = s.console_since(mark);
        assert_eq!(captured.len(), CONSOLE_HISTORY);
        assert_eq!(captured.first().map(String::as_str), Some("5"));
    }

    #[test]
    fn console_history_is_bounded() {
        let mut s = state();
        for i in 0..(CONSOLE_HISTORY + 5) {
            s.console_print(i.to_string());
        }
        assert_eq!(s.console.len(), CONSOLE_HISTORY);
        assert_eq!(s.console.front().map(|line| line.text.as_str()), Some("5"));
    }
}
