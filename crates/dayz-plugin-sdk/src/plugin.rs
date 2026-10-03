//! The trait a plugin implements.

use crate::deps::Dependency;
use crate::host::{Host, PluginError, PluginRef};

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

    /// The plugin is being unloaded (game exit or loader shutdown).
    fn stop(&self, _host: &Host) {}

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

    /// A subscribed broadcast topic was published.
    fn on_event(&self, _host: &Host, _from: PluginRef, _topic: &str, _payload: &[u8]) {}
}
