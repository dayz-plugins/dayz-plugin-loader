//! Example plugin: counts frames, greets on a hotkey, exposes a setting, a command and a UI
//! panel, and shows what each lifecycle callback is for.

use std::sync::atomic::{AtomicU64, Ordering};

use dayz_plugin_sdk::api::{StopReason, UiAnswer, UiLevel};
use dayz_plugin_sdk::{
    export_plugin, Dialog, Host, Notice, Plugin, PluginError, PresentInfo, Setting, Shown, Ui,
};

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
        // The loader owns the window, the layout and the device; this plugin only fills the
        // body in `on_ui`. F10 toggles it, and the user can rebind that in hotkeys.toml.
        host.panel("demo", "Hello plugin", false, "f10")?;
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

    fn on_ui(&self, host: &Host, ui: &Ui, _panel: &str) {
        ui.heading("Hello plugin");
        ui.label(&format!(
            "{} frames presented",
            self.frames.load(Ordering::Relaxed)
        ));
        ui.separator();
        // Both settings, drawn as the right control for their type and written back through
        // the loader: validated, saved to hello.toml and `on_setting_changed` fired, exactly
        // as if they had been typed into the console.
        let _ = ui.setting("greeting");
        let _ = ui.setting("log_frames");
        ui.separator();
        if ui.button("Print the greeting") {
            host.console_print(&host.get("greeting").unwrap_or_default());
        }
        if ui.button("Reset the frame counter") {
            self.frames.store(0, Ordering::Relaxed);
        }
        ui.separator();
        ui.label("Everything the loader can put on screen:");
        if ui.button("Toast") {
            let _ = host.show(&Notice::toast("The greeting was printed.").level(UiLevel::Success));
        }
        if ui.button("Notice in the middle") {
            let _ = host.show(
                &Notice::centred("Watch out")
                    .level(UiLevel::Warning)
                    .seconds(2.0),
            );
        }
        if ui.button("Message box") {
            let _ = host.ask(&Dialog::message("Hello", "This is a message box."));
        }
        if ui.button("Confirm") {
            let _ = host.ask(
                &Dialog::confirm("Reset?", "Set the frame counter back to zero?")
                    .buttons("Reset", "Keep"),
            );
        }
        if ui.button("Ask for the greeting") {
            let _ = host.ask(
                &Dialog::input("Greeting", "What should the greeting be?")
                    .default_text(&host.get("greeting").unwrap_or_default()),
            );
        }
    }

    fn on_dialog(&self, host: &Host, _dialog: Shown, answer: UiAnswer, text: &str) {
        // One place where every dialog ends, including the ones the loader closed because
        // this plugin was stopped.
        if answer != UiAnswer::Accepted {
            return;
        }
        if text.is_empty() {
            self.frames.store(0, Ordering::Relaxed);
        } else if let Err(e) = host.set("greeting", text) {
            let _ = host.show(&Notice::toast(&format!("{e}")).level(UiLevel::Error));
        }
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
