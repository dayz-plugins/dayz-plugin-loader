//! The loader's registries. Pure data plus the rules for mutating it; the Windows side
//! holds this behind a mutex and performs the plugin calls that these methods *request*
//! (see [`Notify`]) after releasing the lock, so plugins can re-enter the host freely.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use dayz_plugin_api::{PluginHandle, Status};
use dayz_plugin_core::{hotkeys, names, settings, store};

use crate::config::{LoaderConfig, Paths};

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
    /// Recent console output.
    pub console: VecDeque<String>,
    /// Produces the lines the `symbols` command prints.
    ///
    /// A function pointer, because the symbol table belongs to the platform layer and this
    /// module must stay free of it. The platform layer installs the real one at startup.
    pub symbol_lines: fn(Option<&str>) -> Vec<String>,
    /// Produces the lines the `hooks` command prints. A function pointer for the same reason
    /// as [`State::symbol_lines`]: the hook registry belongs to the platform layer.
    pub hook_lines: fn() -> Vec<String>,
}

/// Default for [`State::symbol_lines`]: no database, nothing to print.
fn no_symbols(_prefix: Option<&str>) -> Vec<String> {
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
        State {
            paths,
            config,
            plugins: Vec::new(),
            hotkeys: hotkeys::Registry::default(),
            hotkey_overrides,
            phase: Phase::Idle,
            backbuffer_override: None,
            pending: Vec::new(),
            console: VecDeque::new(),
            symbol_lines: no_symbols,
            hook_lines: no_hooks,
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

    /// Append a console line, trimming history.
    pub fn console_print(&mut self, line: String) {
        log::debug!("console: {line}");
        if self.console.len() >= CONSOLE_HISTORY {
            self.console.pop_front();
        }
        self.console.push_back(line);
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
    fn console_history_is_bounded() {
        let mut s = state();
        for i in 0..(CONSOLE_HISTORY + 5) {
            s.console_print(i.to_string());
        }
        assert_eq!(s.console.len(), CONSOLE_HISTORY);
        assert_eq!(s.console.front().map(String::as_str), Some("5"));
    }
}
