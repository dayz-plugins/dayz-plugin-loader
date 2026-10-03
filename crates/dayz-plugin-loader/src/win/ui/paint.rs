//! Drawing the toasts, notices and dialogs that [`super::overlays`] holds.
//!
//! Kept apart from the queue so the queue stays plain data that any thread may touch, and
//! from the panels because none of this belongs to a plugin's own window.

use dayz_plugin_api::{UiAnswer, UiDialog, UiLevel, UiNotice};
use egui::{Align2, Color32, Context, Frame, Id, Key, RichText, Vec2};

use super::overlays::{Entry, Kind};

/// Distance from the screen edge for the toast stack.
const MARGIN: f32 = 12.0;
/// Width of a toast card.
const TOAST_WIDTH: f32 = 320.0;

/// What one frame of drawing decided.
#[derive(Default)]
pub(crate) struct Outcome {
    /// Everything drawn this frame, so the queue can start their clocks.
    pub(crate) shown: Vec<u64>,
    /// Dialogs the user answered: id and how.
    pub(crate) answered: Vec<(u64, UiAnswer)>,
    /// Input dialogs whose text changed: id and the new text.
    pub(crate) typed: Vec<(u64, String)>,
}

/// The accent colour for a level.
fn colour(level: UiLevel) -> Color32 {
    match level {
        UiLevel::Success => Color32::from_rgb(120, 200, 120),
        UiLevel::Warning => Color32::from_rgb(230, 190, 100),
        UiLevel::Error => Color32::from_rgb(230, 120, 120),
        // Including a level from a newer ABI than this loader knows.
        _ => Color32::from_rgb(150, 180, 230),
    }
}

/// Draw everything queued. Returns what the caller must write back.
pub(crate) fn draw(ctx: &Context, entries: &[Entry]) -> Outcome {
    let mut outcome = Outcome::default();
    let mut toast_offset = MARGIN;
    for entry in entries {
        match &entry.kind {
            Kind::Passing(UiNotice::Toast, _) => {
                toast(ctx, entry, &mut toast_offset);
            }
            Kind::Passing(_, _) => notice(ctx, entry),
            Kind::Modal(kind, buffer) => dialog(ctx, entry, *kind, buffer, &mut outcome),
        }
        outcome.shown.push(entry.id);
    }
    outcome
}

/// A card in the corner, stacked under the ones before it.
fn toast(ctx: &Context, entry: &Entry, offset: &mut f32) {
    let opacity = entry.opacity();
    let area = egui::Area::new(Id::new(("dayz-toast", entry.id)))
        .anchor(Align2::RIGHT_TOP, Vec2::new(-MARGIN, *offset))
        .order(egui::Order::Foreground)
        .interactable(false);
    let response = area.show(ctx, |ui| {
        ui.set_opacity(opacity);
        Frame::popup(ui.style())
            .stroke(egui::Stroke::new(1.5, colour(entry.level)))
            .show(ui, |ui| {
                ui.set_width(TOAST_WIDTH);
                if !entry.title.is_empty() {
                    ui.label(
                        RichText::new(&entry.title)
                            .strong()
                            .color(colour(entry.level)),
                    );
                }
                ui.label(&entry.text);
            });
    });
    *offset += response.response.rect.height() + 8.0;
}

/// Large text in the middle, for the few things that must be read.
fn notice(ctx: &Context, entry: &Entry) {
    egui::Area::new(Id::new(("dayz-notice", entry.id)))
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_opacity(entry.opacity());
            Frame::popup(ui.style())
                .stroke(egui::Stroke::new(1.5, colour(entry.level)))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        if !entry.title.is_empty() {
                            ui.label(
                                RichText::new(&entry.title)
                                    .heading()
                                    .color(colour(entry.level)),
                            );
                        }
                        ui.label(RichText::new(&entry.text).size(20.0));
                    });
                });
        });
}

/// A modal window with buttons, and a line to type into for an input dialog.
fn dialog(ctx: &Context, entry: &Entry, kind: UiDialog, buffer: &str, outcome: &mut Outcome) {
    let title = if entry.title.is_empty() {
        "DayZ plugin loader".to_owned()
    } else {
        entry.title.clone()
    };
    let accept = if entry.accept.is_empty() {
        match kind {
            UiDialog::Message => "OK",
            _ => "Confirm",
        }
    } else {
        &entry.accept
    };
    let cancel = if entry.cancel.is_empty() {
        "Cancel"
    } else {
        &entry.cancel
    };
    let mut open = true;
    let mut text = buffer.to_owned();
    egui::Window::new(title)
        .id(Id::new(("dayz-dialog", entry.id)))
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(RichText::new(&entry.text).color(colour(entry.level)));
            ui.add_space(6.0);
            if kind == UiDialog::Input {
                let response = ui
                    .add(egui::TextEdit::singleline(&mut text).desired_width(ui.available_width()));
                response.request_focus();
                if response.has_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    outcome.answered.push((entry.id, UiAnswer::Accepted));
                }
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button(accept).clicked() {
                    outcome.answered.push((entry.id, UiAnswer::Accepted));
                }
                if kind != UiDialog::Message && ui.button(cancel).clicked() {
                    outcome.answered.push((entry.id, UiAnswer::Cancelled));
                }
            });
        });
    if text != buffer {
        outcome.typed.push((entry.id, text));
    }
    // Escape and the window's close button both mean no.
    if !open || ctx.input(|i| i.key_pressed(Key::Escape)) {
        outcome.answered.push((entry.id, UiAnswer::Cancelled));
    }
}
