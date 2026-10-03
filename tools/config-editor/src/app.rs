//! The editor window: section shortcuts on the left, one scrolling list of every key on
//! the right. Each key is drawn from the schema (display name, description, type, range,
//! enum values) with the ini's own comment block as the fallback. Changes stay in the
//! comment-preserving document until Save; keys the running game exposes as live
//! tunables can also be pushed to it without a restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui::{self, Color32, RichText};
use serde_json::Value;

use crate::ini::{Document, Section};
use crate::live::{self, LiveClient};
use crate::locate;
use crate::schema::Schema;
use crate::widgets::{self, Row};

const DEFAULT_DEBUG_PORT: u16 = 48621;
const DIRTY: Color32 = Color32::from_rgb(255, 200, 80);

/// Results from the worker thread that talks to the debug plugin.
enum LiveReply {
    Tunables(Result<BTreeMap<String, Value>, String>),
    Applied { ok: usize, failed: Vec<String> },
    Recentered(Result<(), String>),
}

pub struct App {
    path: Option<PathBuf>,
    path_input: String,
    doc: Option<Document>,
    saved_text: String,
    schema: Schema,
    filter: String,
    status: String,
    status_is_error: bool,
    tunables: Option<BTreeMap<String, Value>>,
    live_busy: bool,
    /// Section the list should scroll to on the next frame (sidebar click).
    jump_to: Option<String>,
    sender: Sender<LiveReply>,
    receiver: Receiver<LiveReply>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut app = Self {
            path: None,
            path_input: String::new(),
            doc: None,
            saved_text: String::new(),
            schema: Schema::embedded(),
            filter: String::new(),
            status: String::new(),
            status_is_error: false,
            tunables: None,
            live_busy: false,
            jump_to: None,
            sender,
            receiver,
        };
        match path.or_else(locate::find_default) {
            Some(found) => app.open(&found),
            None => app.set_error(format!(
                "no {} found; type a path or drop the file onto the window",
                locate::INI_NAME
            )),
        }
        app
    }

    fn set_status(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_is_error = false;
    }

    fn set_error(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_is_error = true;
    }

    fn open(&mut self, path: &Path) {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                self.doc = Some(Document::parse(&text));
                self.saved_text = text;
                self.schema = Schema::for_ini(path);
                self.path = Some(path.to_path_buf());
                self.path_input = path.display().to_string();
                self.set_status(format!("opened {}", path.display()));
            }
            Err(error) => self.set_error(format!("cannot read {}: {error}", path.display())),
        }
    }

    fn save(&mut self) {
        let (Some(doc), Some(path)) = (&self.doc, &self.path) else {
            return;
        };
        let text = doc.to_text();
        let temp = path.with_extension("ini.tmp");
        let result = std::fs::write(&temp, &text).and_then(|()| std::fs::rename(&temp, path));
        match result {
            Ok(()) => {
                self.saved_text = text;
                self.set_status(format!("saved {}", path.display()));
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temp);
                self.set_error(format!("cannot write {}: {error}", path.display()));
            }
        }
    }

    fn revert(&mut self) {
        if self.path.is_some() {
            self.doc = Some(Document::parse(&self.saved_text));
            self.set_status("reverted to the saved file");
        }
    }

    fn is_dirty(&self) -> bool {
        self.doc
            .as_ref()
            .is_some_and(|doc| doc.to_text() != self.saved_text)
    }

    fn debug_port(&self) -> u16 {
        self.doc
            .as_ref()
            .and_then(|doc| doc.get("debug", "port"))
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(DEFAULT_DEBUG_PORT)
    }

    /// Entries whose value differs from the saved file and that the game exposes live.
    fn live_changes(&self) -> Vec<(String, String)> {
        let (Some(doc), Some(tunables)) = (&self.doc, &self.tunables) else {
            return Vec::new();
        };
        let saved = Document::parse(&self.saved_text);
        doc.sections()
            .into_iter()
            .flat_map(|section| section.entries)
            .filter(|entry| tunables.contains_key(&entry.tunable_name()))
            .filter(|entry| saved.get(&entry.section, &entry.key) != Some(entry.value.as_str()))
            .map(|entry| (entry.tunable_name(), entry.value))
            .collect()
    }

    fn spawn_live<F>(&mut self, job: F)
    where
        F: FnOnce(Result<LiveClient, String>) -> LiveReply + Send + 'static,
    {
        self.live_busy = true;
        let port = self.debug_port();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let client = LiveClient::connect(port).map_err(|e| e.to_string());
            let _ = sender.send(job(client));
        });
    }

    fn fetch_tunables(&mut self) {
        self.spawn_live(|client| {
            LiveReply::Tunables(client.and_then(|mut c| c.tunables().map_err(|e| e.to_string())))
        });
    }

    fn apply_live(&mut self) {
        let changes = self.live_changes();
        self.spawn_live(move |client| {
            let mut client = match client {
                Ok(client) => client,
                Err(error) => {
                    return LiveReply::Applied {
                        ok: 0,
                        failed: vec![error],
                    }
                }
            };
            let mut ok = 0;
            let mut failed = Vec::new();
            for (name, value) in changes {
                match client.set(&name, &live::set_text(&value)) {
                    Ok(()) => ok += 1,
                    Err(error) => failed.push(format!("{name}: {error}")),
                }
            }
            LiveReply::Applied { ok, failed }
        });
    }

    fn recenter(&mut self) {
        self.spawn_live(|client| {
            LiveReply::Recentered(client.and_then(|mut c| c.recenter().map_err(|e| e.to_string())))
        });
    }

    fn poll_live(&mut self) {
        while let Ok(reply) = self.receiver.try_recv() {
            self.live_busy = false;
            match reply {
                LiveReply::Tunables(Ok(map)) => {
                    self.set_status(format!("game connected: {} live tunables", map.len()));
                    self.tunables = Some(map);
                }
                LiveReply::Tunables(Err(error)) => {
                    self.tunables = None;
                    self.set_error(format!("game not reachable: {error}"));
                }
                LiveReply::Applied { ok, failed } if failed.is_empty() => {
                    self.set_status(format!("applied {ok} value(s) to the running game"));
                    self.fetch_tunables();
                }
                LiveReply::Applied { ok, failed } => {
                    self.set_error(format!("applied {ok}, failed: {}", failed.join("; ")));
                }
                LiveReply::Recentered(Ok(())) => self.set_status("recentered"),
                LiveReply::Recentered(Err(error)) => self.set_error(format!("recenter: {error}")),
            }
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        if let Some(path) = dropped.into_iter().next() {
            self.open(&path);
        }
    }

    fn file_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("File");
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.path_input)
                    .desired_width(ui.available_width() - 330.0),
            );
            let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Open").clicked() || submitted {
                let path = PathBuf::from(self.path_input.trim());
                self.open(&path);
            }
            if ui
                .add_enabled(self.path.is_some(), egui::Button::new("Reload"))
                .clicked()
            {
                if let Some(path) = self.path.clone() {
                    self.open(&path);
                }
            }
            let dirty = self.is_dirty();
            if ui.add_enabled(dirty, egui::Button::new("Save")).clicked() {
                self.save();
            }
            if ui
                .add_enabled(dirty, egui::Button::new("Revert all"))
                .on_hover_text("discard every unsaved change")
                .clicked()
            {
                self.revert();
            }
        });
    }

    fn game_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Game");
            let connected = self.tunables.is_some();
            let label = if connected { "Refresh" } else { "Connect" };
            let can_connect = !self.live_busy && self.doc.is_some();
            if ui
                .add_enabled(can_connect, egui::Button::new(label))
                .on_hover_text("talk to the debug plugin of the running game ([debug] enabled)")
                .clicked()
            {
                self.fetch_tunables();
            }
            let pending = self.live_changes().len();
            let apply = egui::Button::new(format!("Apply {pending} live"));
            if ui
                .add_enabled(!self.live_busy && pending > 0, apply)
                .clicked()
            {
                self.apply_live();
            }
            if ui
                .add_enabled(!self.live_busy && connected, egui::Button::new("Recenter"))
                .clicked()
            {
                self.recenter();
            }
            ui.label(RichText::new(format!("port {}", self.debug_port())).weak());
            ui.separator();
            ui.label("Filter");
            ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(180.0));
            if !self.filter.is_empty() && ui.small_button("×").clicked() {
                self.filter.clear();
            }
        });
    }

    fn section_list(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = &self.doc else {
            return;
        };
        let filter = self.filter.to_lowercase();
        let saved = Document::parse(&self.saved_text);
        let rows: Vec<(String, String, bool)> = doc
            .sections()
            .iter()
            .filter(|section| visible_entries(section, &filter, &self.schema) > 0)
            .map(|section| {
                let dirty = section.entries.iter().any(|entry| {
                    saved.get(&entry.section, &entry.key) != Some(entry.value.as_str())
                });
                (
                    section.name.clone(),
                    self.section_title(&section.name),
                    dirty,
                )
            })
            .collect();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (name, title, dirty) in rows {
                let mut text = RichText::new(title);
                if dirty {
                    text = text.color(DIRTY);
                }
                if ui
                    .selectable_label(false, text)
                    .on_hover_text(format!("[{name}]"))
                    .clicked()
                {
                    self.jump_to = Some(name);
                }
            }
        });
    }

    fn section_title(&self, name: &str) -> String {
        self.schema
            .section(name)
            .filter(|info| !info.title.is_empty())
            .map_or_else(|| format!("[{name}]"), |info| info.title.clone())
    }

    fn entry_panel(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = &self.doc else {
            ui.label("Open a dayz_openxr.ini to start.");
            return;
        };
        let filter = self.filter.to_lowercase();
        let saved = Document::parse(&self.saved_text);
        let sections = doc.sections();
        let jump = self.jump_to.take();
        let mut edits: Vec<(usize, String)> = Vec::new();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for section in &sections {
                    if visible_entries(section, &filter, &self.schema) == 0 {
                        continue;
                    }
                    let header = self.section_header(ui, section);
                    if jump.as_deref() == Some(section.name.as_str()) {
                        header.scroll_to_me(Some(egui::Align::TOP));
                    }
                    for entry in &section.entries {
                        let info = self.schema.key(&entry.section, &entry.key);
                        if !widgets::matches_filter(entry, info, &filter) {
                            continue;
                        }
                        let row = Row {
                            entry,
                            info,
                            saved: saved.get(&entry.section, &entry.key),
                            live: self
                                .tunables
                                .as_ref()
                                .and_then(|map| map.get(&entry.tunable_name())),
                        };
                        if let Some(value) = widgets::entry_row(ui, &row) {
                            edits.push((entry.index, value));
                        }
                        ui.separator();
                    }
                    ui.add_space(12.0);
                }
            });
        if let Some(doc) = &mut self.doc {
            for (index, value) in edits {
                if let Err(error) = doc.set(index, &value) {
                    self.status = error.to_string();
                    self.status_is_error = true;
                }
            }
        }
    }

    fn section_header(&self, ui: &mut egui::Ui, section: &Section) -> egui::Response {
        let title = self.section_title(&section.name);
        let response = ui.horizontal(|ui| {
            ui.heading(title);
            ui.label(
                RichText::new(format!("[{}]", section.name))
                    .weak()
                    .monospace(),
            );
        });
        if let Some(info) = self.schema.section(&section.name) {
            if !info.description.is_empty() {
                ui.label(RichText::new(&info.description).weak());
            }
        }
        ui.add_space(4.0);
        response.response
    }
}

fn visible_entries(section: &Section, filter: &str, schema: &Schema) -> usize {
    section
        .entries
        .iter()
        .filter(|entry| {
            widgets::matches_filter(entry, schema.key(&entry.section, &entry.key), filter)
        })
        .count()
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_live();
        self.handle_drops(ui.ctx());
        if self.live_busy {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        egui::Panel::top("bar").show(ui, |ui| {
            ui.add_space(4.0);
            self.file_bar(ui);
            self.game_bar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            let color = if self.status_is_error {
                Color32::from_rgb(255, 120, 120)
            } else {
                Color32::GRAY
            };
            let dirty = if self.is_dirty() {
                "  (unsaved changes)"
            } else {
                ""
            };
            ui.label(RichText::new(format!("{}{dirty}", self.status)).color(color));
        });
        egui::Panel::left("sections")
            .default_size(190.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                self.section_list(ui);
            });
        egui::CentralPanel::default().show(ui, |ui| self.entry_panel(ui));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_sections_are_detected_by_value_difference() {
        let saved = Document::parse("[a]\nx=1\n[b]\ny=2\n");
        let mut doc = saved.clone();
        let index = doc.sections()[1].entries[0].index;
        doc.set(index, "3").unwrap_or_else(|e| panic!("{e}"));
        let dirty: Vec<String> = doc
            .sections()
            .iter()
            .filter(|s| {
                s.entries
                    .iter()
                    .any(|e| saved.get(&e.section, &e.key) != Some(e.value.as_str()))
            })
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(dirty, ["b"]);
    }
}
