//! Loader directories and `loader.toml`.

use std::path::{Path, PathBuf};

use dayz_plugin_core::store;
use log::LevelFilter;
use serde::Deserialize;

/// Where everything lives, derived from the game directory.
///
/// Every field names a directory, so the shared `_dir` suffix is the point rather than
/// redundancy; dropping it would leave bare words like `game` and `config`.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone)]
pub struct Paths {
    /// Directory of `DayZ_x64.exe`.
    pub game_dir: PathBuf,
    /// `<game>/plugins`: plugin DLLs.
    ///
    /// Deliberately not under `dayz-plugins/`: this is the one directory a person puts files
    /// into, so it sits where they can find it, while everything the loader owns stays out
    /// of the game directory's way.
    pub plugins_dir: PathBuf,
    /// `<game>/dayz-plugins/config`: `loader.toml`, `hotkeys.toml`, `<plugin>.toml`.
    pub config_dir: PathBuf,
    /// `<game>/dayz-plugins/logs`.
    pub logs_dir: PathBuf,
    /// `<game>/dayz-plugins/data`: the dayz-data database.
    pub data_dir: PathBuf,
}

impl Paths {
    /// Standard layout under `game_dir`.
    #[must_use]
    pub fn from_game_dir(game_dir: &Path) -> Self {
        let root = game_dir.join("dayz-plugins");
        Paths {
            game_dir: game_dir.to_path_buf(),
            plugins_dir: game_dir.join("plugins"),
            config_dir: root.join("config"),
            logs_dir: root.join("logs"),
            data_dir: root.join(dayz_data::DIRECTORY_NAME),
        }
    }

    /// Config file of one plugin.
    #[must_use]
    pub fn plugin_config(&self, plugin: &str) -> PathBuf {
        self.config_dir.join(format!("{plugin}.toml"))
    }

    /// User hotkey overrides.
    #[must_use]
    pub fn hotkeys_file(&self) -> PathBuf {
        self.config_dir.join("hotkeys.toml")
    }
}

/// `loader.toml`. Every field is optional.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LoaderConfig {
    /// `false` turns the loader into a pure DXGI forwarder.
    pub enabled: bool,
    /// Plugin file names (without `.dll`) that are never loaded.
    pub disabled_plugins: Vec<String>,
    /// `error`, `warn`, `info`, `debug` or `trace`.
    pub log_level: String,
}

impl Default for LoaderConfig {
    fn default() -> Self {
        LoaderConfig {
            enabled: true,
            disabled_plugins: Vec::new(),
            log_level: "info".to_owned(),
        }
    }
}

impl LoaderConfig {
    /// Parse the file, or defaults when it does not exist. A malformed file is reported and
    /// treated as defaults so a typo can never turn the game into a black screen.
    #[must_use]
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<LoaderConfig>(&text) {
                Ok(cfg) => (cfg, None),
                Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
        }
    }

    /// Log level filter, falling back to `info` for unknown words.
    #[must_use]
    pub fn level(&self) -> LevelFilter {
        self.log_level.parse().unwrap_or(LevelFilter::Info)
    }
}

/// Load a plugin's stored settings (missing file is empty).
#[must_use]
pub fn load_plugin_values(
    paths: &Paths,
    plugin: &str,
) -> std::collections::BTreeMap<String, String> {
    let path = paths.plugin_config(plugin);
    match store::read(&path) {
        Ok(values) => values,
        Err(e) => {
            log::warn!("ignoring unreadable config: {e}");
            std::collections::BTreeMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_layout() {
        let p = Paths::from_game_dir(Path::new("/g"));
        assert_eq!(p.plugins_dir, Path::new("/g/plugins"));
        assert_eq!(p.data_dir, Path::new("/g/dayz-plugins/data"));
        assert_eq!(
            p.plugin_config("vr"),
            Path::new("/g/dayz-plugins/config/vr.toml")
        );
        assert_eq!(
            p.hotkeys_file(),
            Path::new("/g/dayz-plugins/config/hotkeys.toml")
        );
    }

    #[test]
    fn config_defaults_and_rejects_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("dayz-loader-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        let path = dir.join("loader.toml");
        assert_eq!(LoaderConfig::load(&path), (LoaderConfig::default(), None));
        std::fs::write(&path, "enabled = false\nlog_level = \"debug\"\n")
            .unwrap_or_else(|e| panic!("{e}"));
        let (cfg, err) = LoaderConfig::load(&path);
        assert!(err.is_none());
        assert!(!cfg.enabled);
        assert_eq!(cfg.level(), LevelFilter::Debug);
        std::fs::write(&path, "typo = 1\n").unwrap_or_else(|e| panic!("{e}"));
        let (cfg, err) = LoaderConfig::load(&path);
        assert_eq!(cfg, LoaderConfig::default());
        assert!(err.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
