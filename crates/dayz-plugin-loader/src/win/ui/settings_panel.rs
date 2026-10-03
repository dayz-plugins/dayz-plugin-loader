//! The loader's settings editor: every plugin's settings and hotkeys in one window.
//!
//! Plugins get this for free. A plugin registers a setting because it needs the value and a
//! hotkey because it needs the action; neither costs it a line of UI, and a user who wants to
//! change either has one place to look rather than one panel per plugin.
//!
//! The editor writes through the same paths as the console: `set` for a setting, so it is
//! validated, persisted and notified, and the hotkey registry plus `hotkeys.toml` for a
//! binding.

use dayz_plugin_core::keys::{Chord, Modifiers};
use egui::{Context, Grid, RichText, ScrollArea};

use crate::state::EditorSection;

use super::frame;

/// What the editor remembers between frames.
#[derive(Default)]
pub(crate) struct SettingsPanel {
    /// The action whose key is being recorded, if any.
    recording: Option<String>,
}

/// What one frame of the editor decided, applied by the caller with no lock held.
#[derive(Default)]
pub(crate) struct Edits {
    /// Qualified setting name and its new value.
    pub(crate) settings: Vec<(String, String)>,
    /// Action name and its new binding, `None` to unbind.
    pub(crate) bindings: Vec<(String, Option<Chord>)>,
}

impl SettingsPanel {
    /// Draw the window. `sections` is a snapshot; nothing here touches loader state.
    pub(crate) fn show(
        &mut self,
        ctx: &Context,
        sections: &[EditorSection],
        pressed: Option<(u16, u16, Modifiers)>,
        open: &mut bool,
    ) -> Edits {
        let mut edits = Edits::default();
        // A key arriving while recording ends the recording, whichever window has focus.
        if let (Some(action), Some((vk, scancode, modifiers))) = (self.recording.clone(), pressed) {
            self.finish_recording(&action, vk, scancode, modifiers, &mut edits);
        }
        egui::Window::new("Loader settings")
            .default_size([560.0, 420.0])
            .open(open)
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for section in sections {
                            self.section(ui, section, &mut edits);
                        }
                        if sections.is_empty() {
                            ui.label("No plugin has registered a setting or a hotkey.");
                        }
                    });
            });
        edits
    }

    /// One plugin: its settings, then its hotkeys.
    fn section(&mut self, ui: &mut egui::Ui, section: &EditorSection, edits: &mut Edits) {
        let heading = if section.running {
            RichText::new(&section.plugin).heading()
        } else {
            RichText::new(format!("{} (stopped)", section.plugin))
                .heading()
                .weak()
        };
        egui::CollapsingHeader::new(heading)
            .id_salt(&section.plugin)
            .default_open(true)
            .show(ui, |ui| {
                for (desc, value) in &section.settings {
                    if let Some(new) = frame::control(ui, desc, &label_of(desc), value) {
                        edits
                            .settings
                            .push((format!("{}.{}", section.plugin, desc.key), new));
                    }
                    if !desc.description.is_empty() {
                        ui.label(RichText::new(&desc.description).weak().small());
                    }
                }
                if !section.settings.is_empty() && !section.hotkeys.is_empty() {
                    ui.separator();
                }
                if !section.hotkeys.is_empty() {
                    self.hotkeys(ui, section, edits);
                }
            });
    }

    /// The hotkey table: what it does, what it is bound to, and the buttons to change it.
    fn hotkeys(&mut self, ui: &mut egui::Ui, section: &EditorSection, edits: &mut Edits) {
        Grid::new(format!("{}-hotkeys", section.plugin))
            .num_columns(3)
            .spacing([12.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                for row in &section.hotkeys {
                    let title = if row.title.is_empty() {
                        row.name.clone()
                    } else {
                        row.title.clone()
                    };
                    ui.label(title);
                    let recording = self.recording.as_deref() == Some(row.name.as_str());
                    let text = if recording {
                        RichText::new("press a key…").strong()
                    } else {
                        RichText::new(&row.binding).monospace()
                    };
                    if ui.button(text).clicked() {
                        self.recording = if recording {
                            None
                        } else {
                            Some(row.name.clone())
                        };
                    }
                    ui.horizontal(|ui| {
                        if ui.small_button("Clear").clicked() {
                            self.recording = None;
                            edits.bindings.push((row.name.clone(), None));
                        }
                        // Only offered when it would change something, so the row stays quiet
                        // for the bindings nobody has touched.
                        let changed = row.binding != row.default;
                        if ui
                            .add_enabled(changed, egui::Button::small(egui::Button::new("Default")))
                            .clicked()
                        {
                            self.recording = None;
                            let default =
                                dayz_plugin_core::keys::parse(&row.default).ok().flatten();
                            edits.bindings.push((row.name.clone(), default));
                        }
                    });
                    ui.end_row();
                }
            });
        if self.recording.is_some() {
            ui.label(
                RichText::new("Escape cancels, and a modifier alone is ignored.")
                    .weak()
                    .small(),
            );
        }
    }

    /// Turn the key that was just pressed into a binding.
    fn finish_recording(
        &mut self,
        action: &str,
        vk: u16,
        scancode: u16,
        modifiers: Modifiers,
        edits: &mut Edits,
    ) {
        const VK_ESCAPE: u16 = 0x1B;
        const MODIFIER_KEYS: [u16; 9] = [0x10, 0x11, 0x12, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5];
        if MODIFIER_KEYS.contains(&vk) {
            // Still waiting: the user is holding Ctrl on the way to the real key.
            return;
        }
        self.recording = None;
        if vk == VK_ESCAPE {
            return;
        }
        // A key the grammar can spell is recorded by name, which reads well in hotkeys.toml
        // and follows the label on the key. Anything else — the key under Escape reports
        // `0xFC` under Wine, which no layout table claims — is recorded by position, so the
        // binding stays on that key whatever virtual key the layout gives it.
        let chord = if dayz_plugin_core::keys::has_name(vk) {
            Chord {
                vk,
                scancode: None,
                modifiers,
            }
        } else {
            Chord {
                vk,
                scancode: Some(scancode),
                modifiers,
            }
        };
        edits.bindings.push((action.to_owned(), Some(chord)));
    }
}

/// A setting's label: its title, or its key when it has none.
fn label_of(desc: &dayz_plugin_core::settings::Desc) -> String {
    if desc.title.is_empty() {
        desc.key.clone()
    } else {
        desc.title.clone()
    }
}
