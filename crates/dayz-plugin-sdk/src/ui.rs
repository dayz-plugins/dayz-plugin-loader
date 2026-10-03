//! The panel body a plugin fills, and the widgets it can put in one.
//!
//! A plugin never links a UI library. It registers a panel with [`Host::panel`], and the
//! loader calls [`Plugin::on_ui`] once per frame while that panel is open with a [`Ui`] that
//! stands for its body. Every widget is one call into the loader, which owns the window, the
//! layout, the device and the frame — so the same panel can be drawn into a flat overlay now
//! and onto a world-space quad in VR later without this plugin changing.
//!
//! [`Host::panel`]: crate::Host::panel
//! [`Plugin::on_ui`]: crate::Plugin::on_ui

use dayz_plugin_api::{HostApi, PluginHandle, Status, Str, UiValue, UiWidget};

/// One panel body, valid only for the length of the `on_ui` call that handed it over.
///
/// Keeping it is harmless but useless: the loader rejects a stale token instead of touching
/// anything the frame owned.
#[derive(Clone, Copy)]
pub struct Ui {
    api: &'static HostApi,
    handle: PluginHandle,
    frame: u64,
}

impl core::fmt::Debug for Ui {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The host table is a function pointer wall and the handle is already in the log
        // prefix, so the token is the only part worth printing.
        f.debug_struct("Ui")
            .field("frame", &self.frame)
            .finish_non_exhaustive()
    }
}

impl Ui {
    pub(crate) fn new(api: &'static HostApi, handle: PluginHandle, frame: u64) -> Self {
        Ui { api, handle, frame }
    }

    fn call(&self, kind: UiWidget, text: &str, value: Option<&mut UiValue>) -> Status {
        let value = value.map_or(core::ptr::null_mut(), core::ptr::from_mut);
        // SAFETY: `text` outlives the call, and `value` is either null or points at the
        // caller's own `UiValue`, which the loader only touches during the call.
        #[allow(unsafe_code)]
        unsafe {
            (self.api.ui_widget)(
                self.api.host,
                self.handle,
                self.frame,
                kind,
                Str::new(text),
                value,
            )
        }
    }

    /// A line of text.
    pub fn label(&self, text: &str) {
        self.call(UiWidget::Label, text, None);
    }

    /// A section heading.
    pub fn heading(&self, text: &str) {
        self.call(UiWidget::Heading, text, None);
    }

    /// A horizontal rule.
    pub fn separator(&self) {
        self.call(UiWidget::Separator, "", None);
    }

    /// Vertical space; `points` of zero means the loader's default gap.
    pub fn space(&self, points: f32) {
        let mut value = UiValue {
            float: points,
            ..UiValue::new()
        };
        self.call(UiWidget::Space, "", Some(&mut value));
    }

    /// A button. Returns whether it was clicked this frame.
    #[must_use]
    pub fn button(&self, text: &str) -> bool {
        let mut value = UiValue::new();
        if self.call(UiWidget::Button, text, Some(&mut value)) == Status::Ok {
            value.boolean
        } else {
            false
        }
    }

    /// A checkbox over the caller's own flag. Returns whether it changed.
    pub fn checkbox(&self, text: &str, on: &mut bool) -> bool {
        let mut value = UiValue {
            boolean: *on,
            ..UiValue::new()
        };
        if self.call(UiWidget::Checkbox, text, Some(&mut value)) != Status::Ok {
            return false;
        }
        let changed = value.boolean != *on;
        *on = value.boolean;
        changed
    }

    /// A slider over the caller's own float. Returns whether it changed.
    pub fn slider(&self, text: &str, current: &mut f32, min: f32, max: f32) -> bool {
        let mut value = UiValue {
            float: *current,
            min: f64::from(min),
            max: f64::from(max),
            ..UiValue::new()
        };
        if self.call(UiWidget::Slider, text, Some(&mut value)) != Status::Ok {
            return false;
        }
        let changed = (value.float - *current).abs() > f32::EPSILON;
        *current = value.float;
        changed
    }

    /// A slider over the caller's own integer. Returns whether it changed.
    pub fn slider_int(&self, text: &str, current: &mut i64, min: i64, max: i64) -> bool {
        #[allow(clippy::cast_precision_loss)]
        let mut value = UiValue {
            integer: *current,
            min: min as f64,
            max: max as f64,
            ..UiValue::new()
        };
        if self.call(UiWidget::SliderInt, text, Some(&mut value)) != Status::Ok {
            return false;
        }
        let changed = value.integer != *current;
        *current = value.integer;
        changed
    }

    /// The right control for one of this plugin's registered settings, drawn and written by
    /// the loader: validated, persisted, and `on_setting_changed` fired, exactly as if it had
    /// been typed into the console.
    ///
    /// `name` is the plugin's own key, or `<plugin>.<key>` for another plugin's setting.
    ///
    /// # Errors
    /// No setting of that name.
    pub fn setting(&self, name: &str) -> Result<(), crate::PluginError> {
        match self.call(UiWidget::Setting, name, None) {
            Status::Ok => Ok(()),
            other => Err(crate::PluginError::Status(other)),
        }
    }
}
