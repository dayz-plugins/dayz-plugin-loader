//! The widget half of the plugin UI ABI.
//!
//! A plugin fills a panel body by calling `ui_widget` with the token its `on_ui` received.
//! The token is a number, not a pointer: the `egui::Ui` it stands for lives on this thread's
//! stack for exactly the length of that call, and a plugin that keeps the token and calls
//! later is told [`Status::WrongPhase`] rather than having a stale pointer dereferenced for
//! it.

use std::cell::Cell;

use dayz_plugin_api::{PluginHandle, Status, UiValue, UiWidget};
use dayz_plugin_core::settings::{Desc, Kind};

use crate::win::{dispatch, state};

/// Widest integer range that still makes sense as a slider; above it, a number field.
const SLIDER_STEPS: f64 = 1000.0;

thread_local! {
    /// The panel body currently being filled on this thread, if any.
    static CURRENT: Cell<(u64, *mut egui::Ui)> = const { Cell::new((0, core::ptr::null_mut())) };
}

/// Run `body` with `ui` reachable through `token`, then take it back.
pub(crate) fn with<R>(token: u64, ui: &mut egui::Ui, body: impl FnOnce() -> R) -> R {
    let previous = CURRENT.with(|c| c.replace((token, core::ptr::from_mut(ui))));
    let result = body();
    CURRENT.with(|c| c.set(previous));
    result
}

/// Borrow the panel body belonging to `token`.
fn borrow<R>(token: u64, body: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    let (current, ui) = CURRENT.with(Cell::get);
    if token == 0 || token != current || ui.is_null() {
        return None;
    }
    // SAFETY: the token matches the one `with` installed on this thread, so `ui` is the
    // `&mut egui::Ui` of the call still on this thread's stack, and nothing else can hold a
    // reference to it: `with` is only entered once per thread at a time and this borrow ends
    // before it returns.
    #[allow(unsafe_code)]
    Some(body(unsafe { &mut *ui }))
}

/// Add one widget to a panel body. This is `HostApi::ui_widget`.
pub(crate) fn widget(
    caller: PluginHandle,
    token: u64,
    kind: UiWidget,
    text: &str,
    value: Option<&mut UiValue>,
) -> Status {
    let outcome = borrow(token, |ui| match kind {
        UiWidget::Label => {
            ui.label(text);
            Status::Ok
        }
        UiWidget::Heading => {
            ui.heading(text);
            Status::Ok
        }
        UiWidget::Separator => {
            ui.separator();
            Status::Ok
        }
        UiWidget::Space => {
            let points = value.as_ref().map_or(0.0, |v| v.float);
            ui.add_space(if points > 0.0 { points } else { 6.0 });
            Status::Ok
        }
        UiWidget::Button => match value {
            Some(out) => {
                out.boolean = ui.button(text).clicked();
                Status::Ok
            }
            None => Status::InvalidArgument,
        },
        UiWidget::Checkbox => match value {
            Some(out) => {
                let mut on = out.boolean;
                ui.checkbox(&mut on, text);
                out.boolean = on;
                Status::Ok
            }
            None => Status::InvalidArgument,
        },
        UiWidget::Slider => match value {
            Some(out) => {
                let mut v = out.float;
                #[allow(clippy::cast_possible_truncation)]
                let (min, max) = (out.min as f32, out.max as f32);
                ui.add(egui::Slider::new(&mut v, min..=max).text(text));
                out.float = v;
                Status::Ok
            }
            None => Status::InvalidArgument,
        },
        UiWidget::SliderInt => match value {
            Some(out) => {
                let mut v = out.integer;
                #[allow(clippy::cast_possible_truncation)]
                let (min, max) = (out.min as i64, out.max as i64);
                ui.add(egui::Slider::new(&mut v, min..=max).text(text));
                out.integer = v;
                Status::Ok
            }
            None => Status::InvalidArgument,
        },
        UiWidget::Setting => setting(ui, caller, text),
        // A plugin built against a newer ABI asking for a widget this loader does not know.
        _ => Status::Unsupported,
    });
    outcome.unwrap_or(Status::WrongPhase)
}

/// The right control for a registered setting, with the loader doing the writing.
///
/// This is why panels are worth having: a plugin gets a validated, persisted, change-notified
/// control for a setting it already registered, in one call and with no state of its own.
fn setting(ui: &mut egui::Ui, caller: PluginHandle, name: &str) -> Status {
    let Some(desc) = state().setting_desc(Some(caller), name) else {
        return Status::NotFound;
    };
    let Ok(current) = state().get_setting(Some(caller), name) else {
        return Status::NotFound;
    };
    let label = if desc.title.is_empty() {
        name.to_owned()
    } else {
        desc.title.clone()
    };
    let changed = row(ui, &desc, &label, &current);
    if !desc.description.is_empty() {
        ui.label(egui::RichText::new(&desc.description).weak().small());
    }
    let Some(new_value) = changed else {
        return Status::Ok;
    };
    // The same path the console's `set` takes: validation, persistence and the owning
    // plugin's `on_setting_changed`, so a slider and a typed command cannot disagree.
    let notify = state().set_setting(Some(caller), name, &new_value);
    match notify {
        Ok(notify) => {
            dispatch::deliver(notify.into_iter().collect());
            Status::Ok
        }
        Err((status, message)) => {
            log::debug!("ui: {name} = {new_value}: {message}");
            status
        }
    }
}

/// One setting as the user meets it: its control, and a reset beside it once the value is no
/// longer the default.
///
/// The reset is offered rather than always present, because a row for a setting nobody has
/// touched has nothing to reset to.
pub(super) fn row(ui: &mut egui::Ui, desc: &Desc, label: &str, current: &str) -> Option<String> {
    ui.push_id(&desc.key, |ui| {
        ui.horizontal(|ui| {
            let changed = control(ui, desc, label, current);
            if current == desc.default {
                return changed;
            }
            let hint = format!("Reset to {}", describe_default(desc));
            if ui.small_button("⟲").on_hover_text(hint).clicked() {
                return Some(desc.default.clone());
            }
            changed
        })
        .inner
    })
    .inner
}

/// The default as a tooltip says it, so an empty string does not read as a missing word.
fn describe_default(desc: &Desc) -> String {
    if desc.default.is_empty() {
        "nothing".to_owned()
    } else {
        format!("{:?}", desc.default)
    }
}

/// Draw the control for one setting kind; returns the new value when the user changed it.
///
/// Shared with the settings editor so a slider looks and behaves the same whether a plugin
/// put it in its own panel or the user found it in the loader's list.
pub(super) fn control(
    ui: &mut egui::Ui,
    desc: &Desc,
    label: &str,
    current: &str,
) -> Option<String> {
    match desc.kind {
        Kind::Bool => {
            let mut on = current == "true";
            ui.checkbox(&mut on, label)
                .changed()
                .then(|| on.to_string())
        }
        Kind::Int => {
            let mut v = current.parse::<i64>().unwrap_or_default();
            #[allow(clippy::cast_possible_truncation)]
            let range = (desc.min as i64)..=(desc.max as i64);
            // A slider is only a control when its travel means something. A port is a number
            // in 1024..=65535: sixty thousand values across eighty pixels is not a choice a
            // person can make, so a bounded range that wide gets a field that still clamps.
            let response = if desc.min < desc.max && desc.max - desc.min <= SLIDER_STEPS {
                ui.add(egui::Slider::new(&mut v, range).text(label))
            } else {
                ui.horizontal(|ui| {
                    let mut drag = egui::DragValue::new(&mut v);
                    if desc.min < desc.max {
                        drag = drag.range(range);
                    }
                    let r = ui.add(drag);
                    ui.label(label);
                    r
                })
                .inner
            };
            response.changed().then(|| v.to_string())
        }
        Kind::Float => {
            let mut v = current.parse::<f64>().unwrap_or_default();
            let response = if desc.min < desc.max {
                ui.add(egui::Slider::new(&mut v, desc.min..=desc.max).text(label))
            } else {
                ui.horizontal(|ui| {
                    let r = ui.add(egui::DragValue::new(&mut v).speed(0.01));
                    ui.label(label);
                    r
                })
                .inner
            };
            response.changed().then(|| v.to_string())
        }
        Kind::Enum => {
            let mut picked = current.to_owned();
            let mut changed = false;
            egui::ComboBox::from_label(label)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for choice in &desc.choices {
                        if ui.selectable_label(choice == current, choice).clicked() {
                            picked.clone_from(choice);
                            changed = true;
                        }
                    }
                });
            changed.then_some(picked)
        }
        Kind::String => {
            let mut text = current.to_owned();
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(140.0));
                ui.label(label);
                r
            })
            .inner
            .lost_focus()
            .then_some(text)
            .filter(|t| t != current)
        }
    }
}
