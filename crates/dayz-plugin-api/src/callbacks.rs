//! Function table a plugin fills in during `start`. Every entry is optional.

use core::ffi::c_void;

use crate::host::ReplyFn;
use crate::types::{Bytes, PluginHandle, Status, Str, UiAnswer};

/// Information about the game's swapchain, passed when it is created or recreated.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SwapchainInfo {
    /// `size_of::<SwapchainInfo>()`.
    pub struct_size: usize,
    /// `IDXGISwapChain*`. Not add-ref'd; valid until the next swapchain callback.
    pub swapchain: *mut c_void,
    /// `ID3D11Device*` obtained from the swapchain. Not add-ref'd.
    pub device: *mut c_void,
    /// Output window handle.
    pub hwnd: *mut c_void,
    /// Current backbuffer width.
    pub width: u32,
    /// Current backbuffer height.
    pub height: u32,
}

/// Information about one `Present` call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PresentInfo {
    /// `size_of::<PresentInfo>()`.
    pub struct_size: usize,
    /// `IDXGISwapChain*` being presented.
    pub swapchain: *mut c_void,
    /// Sync interval the game asked for.
    pub sync_interval: u32,
    /// DXGI present flags the game asked for.
    pub flags: u32,
}

/// Callbacks a plugin exposes. Null entries are skipped.
///
/// Callbacks run on whichever game thread triggered them (`on_present` on the render
/// thread, `on_hotkey` on the window thread). The loader guards every call against panics
/// and hardware faults and disables a plugin that faults.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PluginCallbacks {
    /// `size_of::<PluginCallbacks>()` as compiled into the plugin.
    pub struct_size: usize,
    /// Opaque plugin state passed as the first argument to every callback and to `stop`.
    pub ctx: *mut c_void,
    /// The game's swapchain was created or recreated.
    pub on_swapchain: Option<unsafe extern "C" fn(ctx: *mut c_void, info: *const SwapchainInfo)>,
    /// Just before the game's `Present` reaches DXGI.
    pub on_present: Option<unsafe extern "C" fn(ctx: *mut c_void, info: *const PresentInfo)>,
    /// After a successful `ResizeBuffers`.
    pub on_resize: Option<unsafe extern "C" fn(ctx: *mut c_void, width: u32, height: u32)>,
    /// A registered hotkey action was pressed.
    pub on_hotkey: Option<unsafe extern "C" fn(ctx: *mut c_void, action: Str)>,
    /// One of this plugin's settings changed (console, UI, another plugin or config reload).
    pub on_setting_changed: Option<unsafe extern "C" fn(ctx: *mut c_void, key: Str, value: Str)>,
    /// A registered console command was invoked with the raw argument text.
    pub on_command: Option<unsafe extern "C" fn(ctx: *mut c_void, name: Str, args: Str) -> Status>,
    /// Another plugin sent a direct message.
    pub on_message: Option<
        unsafe extern "C" fn(
            ctx: *mut c_void,
            from: PluginHandle,
            topic: Str,
            payload: Bytes,
            reply: Option<ReplyFn>,
            reply_ctx: *mut c_void,
        ) -> Status,
    >,
    /// A subscribed broadcast topic was published.
    pub on_event: Option<
        unsafe extern "C" fn(ctx: *mut c_void, from: PluginHandle, topic: Str, payload: Bytes),
    >,
    /// Callback delivery resumed after a pause. Never called for the initial start.
    pub on_enable: Option<unsafe extern "C" fn(ctx: *mut c_void)>,
    /// Callback delivery is being paused. The plugin keeps its state and stays loaded; this
    /// is the moment to release anything that must not survive the pause, such as a
    /// swapchain reference or an overlay.
    ///
    /// Not called when the plugin faulted: a plugin whose callback just crashed is not asked
    /// to run more code.
    pub on_disable: Option<unsafe extern "C" fn(ctx: *mut c_void)>,

    /// Fill the body of one registered panel. `panel` is the qualified name, `frame` is the
    /// token every `ui_*` host function takes and is only valid for this call.
    ///
    /// Called once per frame per open panel, from the loader's render path, so it must be
    /// short and must not block. A token kept past the call is refused, not dereferenced.
    pub on_ui: Option<unsafe extern "C" fn(ctx: *mut c_void, panel: Str, frame: u64)>,

    /// A dialog this plugin opened was answered. `text` carries what was typed for an input
    /// dialog and is empty otherwise.
    pub on_dialog:
        Option<unsafe extern "C" fn(ctx: *mut c_void, id: u64, answer: UiAnswer, text: Str)>,
}

impl PluginCallbacks {
    /// A table with no callbacks, for the loader to pass into `start`.
    #[must_use]
    pub const fn empty() -> Self {
        PluginCallbacks {
            struct_size: core::mem::size_of::<PluginCallbacks>(),
            ctx: core::ptr::null_mut(),
            on_swapchain: None,
            on_present: None,
            on_resize: None,
            on_hotkey: None,
            on_setting_changed: None,
            on_command: None,
            on_message: None,
            on_event: None,
            on_enable: None,
            on_disable: None,
            on_ui: None,
            on_dialog: None,
        }
    }
}
