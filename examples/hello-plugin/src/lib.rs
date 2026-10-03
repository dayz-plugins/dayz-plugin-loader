//! Example plugin: counts frames, greets on a hotkey, exposes a setting and a command, and
//! shows what each lifecycle callback is for.

use std::sync::atomic::{AtomicU64, Ordering};

use dayz_plugin_sdk::api::StopReason;
use dayz_plugin_sdk::{export_plugin, Host, Plugin, PluginError, PresentInfo, Setting};

struct Hello {
    frames: AtomicU64,
}

impl Plugin for Hello {
    const NAME: &'static str = "hello";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const DESCRIPTION: &'static str = "Example plugin that greets from a hotkey and a command.";

    fn start(host: Host) -> Result<Self, PluginError> {
        host.setting(&Setting::text(
            "greeting",
            "Greeting",
            "Hello from the plugin loader",
        ))?;
        host.setting(&Setting::bool("log_frames", "Log every 600th frame", false))?;
        host.hotkey("greet", "Print the greeting", "f9")?;
        host.command(
            "greet",
            "Print the greeting with an optional name.",
            "[name]",
        )?;
        log::info!("started in {}", host.game_dir());
        Ok(Hello {
            frames: AtomicU64::new(0),
        })
    }

    fn on_present(&self, host: &Host, _info: &PresentInfo) {
        let n = self.frames.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 600 == 0 && host.get_as::<bool>("log_frames").unwrap_or(false) {
            log::info!("{n} frames presented");
        }
    }

    fn on_hotkey(&self, host: &Host, action: &str) {
        if action == "greet" {
            host.console_print(&host.get("greeting").unwrap_or_default());
        }
    }

    fn on_command(&self, host: &Host, name: &str, args: &str) -> Result<(), PluginError> {
        if name == "where" {
            return Self::where_is(host, args);
        }
        if name != "greet" {
            return Err(PluginError::Unsupported);
        }
        let greeting = host.get("greeting")?;
        let line = if args.is_empty() {
            greeting
        } else {
            format!("{greeting}, {args}")
        };
        host.console_print(&line);
        Ok(())
    }

    fn on_setting_changed(&self, _host: &Host, key: &str, value: &str) {
        log::info!("setting {key} is now {value:?}");
    }

    fn on_disable(&self, _host: &Host) {
        // Nothing of this plugin's own survives a pause, so the count is all there is to
        // report; a plugin with an overlay would take it down here.
        log::info!(
            "paused after {} frames",
            self.frames.load(Ordering::Relaxed)
        );
    }

    fn on_enable(&self, _host: &Host) {
        log::info!("resumed");
    }

    fn stop(&self, _host: &Host, reason: StopReason) {
        // `Exit` means the process is going away regardless, so there is nothing worth
        // doing; `Unload` is where a real plugin releases what it holds.
        log::info!(
            "stopping ({reason:?}) after {} frames",
            self.frames.load(Ordering::Relaxed)
        );
    }
}

impl Hello {
    /// Look a game address up by name. A plugin never carries an address of its own: the
    /// name resolves through the loader's dayz-data database, so a game update changes the
    /// database rather than this plugin.
    fn where_is(host: &Host, symbol: &str) -> Result<(), PluginError> {
        if symbol.is_empty() {
            return Err(PluginError::Message(
                "usage: hello.where <symbol>".to_owned(),
            ));
        }
        let address = host.symbol(symbol)?;
        host.console_print(&format!("{symbol} is at {address:p}"));
        Ok(())
    }
}

export_plugin!(Hello);
