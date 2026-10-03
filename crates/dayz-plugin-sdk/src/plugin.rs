//! The trait a plugin implements.

use dayz_plugin_api::StopReason;

use crate::deps::Dependency;
use crate::dialogs::Shown;
use crate::host::{Host, PluginError, PluginRef};
use crate::input::{Input, Verdict};
use crate::ui::Ui;

/// The game's swapchain, as handed to [`Plugin::on_swapchain`].
#[derive(Debug, Clone, Copy)]
pub struct SwapchainInfo {
    /// `IDXGISwapChain*`, not add-ref'd.
    pub swapchain: *mut core::ffi::c_void,
    /// `ID3D11Device*`, not add-ref'd.
    pub device: *mut core::ffi::c_void,
    /// Output window.
    pub hwnd: *mut core::ffi::c_void,
    /// Backbuffer width.
    pub width: u32,
    /// Backbuffer height.
    pub height: u32,
}

/// One `Present` call, as handed to [`Plugin::on_present`].
#[derive(Debug, Clone, Copy)]
pub struct PresentInfo {
    /// `IDXGISwapChain*` being presented.
    pub swapchain: *mut core::ffi::c_void,
    /// Requested sync interval.
    pub sync_interval: u32,
    /// Requested DXGI present flags.
    pub flags: u32,
}

/// A loader plugin. Every callback has a no-op default.
///
/// The lifecycle, in order: [`Plugin::start`] creates the plugin and is the only place
/// registrations are allowed; callbacks run until it is paused by `plugin disable`
/// ([`Plugin::on_disable`]) and resumed ([`Plugin::on_enable`]); [`Plugin::stop`] runs once,
/// with a [`StopReason`] saying whether the plugin was unloaded or the game is exiting.
///
/// Callbacks take `&self` because the loader may re-enter the plugin (for example a setting
/// change triggered from `on_present`), so state needs interior mutability (`Mutex`,
/// atomics). Callbacks run on game threads; keep them short.
pub trait Plugin: Sized + Send + Sync + 'static {
    /// Unique short name, `[a-z0-9_-]+`. Names settings, hotkeys, commands and the config file.
    const NAME: &'static str;
    /// Human readable version.
    const VERSION: &'static str;
    /// One-line description.
    const DESCRIPTION: &'static str;
    /// What must be present before this plugin starts: other plugins, libraries, files or
    /// `dayz-data` symbols. The loader checks the list and logs why it skipped the plugin,
    /// rather than letting the plugin discover the problem at runtime.
    const DEPENDENCIES: &'static [Dependency] = &[];

    /// Create the plugin. Register settings, hotkeys and commands here; registration is only
    /// allowed during this call.
    ///
    /// # Errors
    /// Return an error to stay unloaded; the loader logs it and continues with other plugins.
    fn start(host: Host) -> Result<Self, PluginError>;

    /// The plugin is being stopped: `plugin stop` in the console, the game exiting, or a
    /// `start` that failed partway. `reason` says which, and
    /// [`StopReason::Exit`](crate::api::StopReason::Exit) is the one case where doing less is
    /// better, because the process is about to vanish anyway.
    ///
    /// Hooks registered through the host are torn down by the loader either way; this is for
    /// everything the plugin owns itself.
    fn stop(&self, _host: &Host, _reason: StopReason) {}

    /// Callback delivery resumed after `plugin disable`.
    fn on_enable(&self, _host: &Host) {}

    /// Callback delivery is being paused by `plugin disable`. The plugin stays loaded and
    /// keeps its state; nothing but this callback runs until it is enabled again.
    fn on_disable(&self, _host: &Host) {}

    /// The game's swapchain was created or recreated.
    fn on_swapchain(&self, _host: &Host, _info: &SwapchainInfo) {}

    /// Just before the game's `Present`. Runs on the render thread every frame.
    fn on_present(&self, _host: &Host, _info: &PresentInfo) {}

    /// After a successful `ResizeBuffers`.
    fn on_resize(&self, _host: &Host, _width: u32, _height: u32) {}

    /// A registered hotkey action was pressed while the game window had focus.
    fn on_hotkey(&self, _host: &Host, _action: &str) {}

    /// One of this plugin's settings changed. `value` is the canonical text form.
    fn on_setting_changed(&self, _host: &Host, _key: &str, _value: &str) {}

    /// A registered console command was invoked with its raw argument text.
    ///
    /// # Errors
    /// Return an error to have the console print it.
    fn on_command(&self, _host: &Host, _name: &str, _args: &str) -> Result<(), PluginError> {
        Err(PluginError::Unsupported)
    }

    /// Another plugin sent a direct message. Return bytes to reply synchronously.
    ///
    /// # Errors
    /// Return an error to have the sender see a failure status.
    fn on_message(
        &self,
        _host: &Host,
        _from: PluginRef,
        _topic: &str,
        _payload: &[u8],
    ) -> Result<Option<Vec<u8>>, PluginError> {
        Err(PluginError::Unsupported)
    }

    /// Fill the body of one of this plugin's panels. `panel` is the qualified
    /// `<plugin>.<name>`, so one implementation can serve several panels.
    ///
    /// Called once per frame per open panel, on the render thread, with the loader's own
    /// window and layout around it. Keep it short and do not block: this runs between the
    /// game's last draw call and its `Present`.
    fn on_ui(&self, _host: &Host, _ui: &Ui, _panel: &str) {}

    /// A dialog this plugin opened has ended, however it ended. `text` is what was typed in
    /// an input dialog that was accepted, and empty otherwise.
    fn on_dialog(
        &self,
        _host: &Host,
        _dialog: Shown,
        _answer: dayz_plugin_api::UiAnswer,
        _text: &str,
    ) {
    }

    /// An input event of a kind this plugin subscribed to with
    /// [`Host::listen_input`](crate::Host::listen_input), before the game sees it.
    ///
    /// Return [`Verdict::SWALLOW`] to keep it from the game, [`Verdict::PASS`] to let it
    /// through. Called from inside the game's message loop, for every matching event, so it
    /// must return immediately: hand the work to the plugin's own thread rather than doing
    /// it here.
    fn on_input(&self, _host: &Host, _input: &Input<'_>) -> Verdict {
        Verdict::PASS
    }

    /// A subscribed broadcast topic was published.
    fn on_event(&self, _host: &Host, _from: PluginRef, _topic: &str, _payload: &[u8]) {}
}
