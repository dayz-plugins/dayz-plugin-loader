//! The loader's own console, drawn in the game.
//!
//! This is the overlay's first customer on purpose: it exercises scrollback, text input,
//! focus capture and command execution before any plugin depends on the machinery, and it
//! answers the question the separate console window could not — how to reach the console
//! without leaving the game.
//!
//! It shows the loader's log as well as what commands print ([`crate::scrollback`]), which is
//! what makes it worth opening when nothing has been typed. The layout is three panels inside
//! the window — toolbar, scrollback, input — so the input row stays on the bottom edge at
//! every window size instead of being placed by subtracting a guess from the height.

mod complete;
mod view;

use egui::{Context, Key, Modifiers, RichText, TextEdit};

use crate::scrollback::Line;
use view::{Columns, Filter};

use super::chrome::{self, Chrome, Placed};

/// How many submitted lines the arrow keys can walk back through.
const HISTORY: usize = 100;

/// What the input box's keys asked for this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pressed {
    /// Nothing that the caller has to act on.
    Nothing,
    /// Enter: run the line.
    Submit,
    /// Escape on an empty line: close the console.
    Close,
}

/// What the scrollback holds this frame, for the status line and the clear button.
#[derive(Debug, Clone, Copy)]
struct Stats {
    /// Lines in the buffer.
    total: usize,
    /// How many of them the filter hid.
    hidden: usize,
    /// Sequence number of the newest line, which is where `Clear` draws its line.
    newest: u64,
}

/// What one frame of the console decided.
pub(crate) struct Outcome {
    /// A line the user submitted, to execute after the frame.
    pub(crate) submitted: Option<String>,
    /// Set when the console asked to be closed.
    pub(crate) close: bool,
    /// Where the window ended up.
    pub(crate) placed: Placed,
}

/// What the panel remembers between frames.
pub(crate) struct ConsolePanel {
    /// The line being typed.
    input: String,
    /// Previously submitted lines, newest last.
    history: Vec<String>,
    /// Where arrow-up has walked to, counted back from the end.
    recall: Option<usize>,
    /// Set when the input box should take the keyboard on the next frame.
    focus: bool,
    /// What the scrollback is showing.
    filter: Filter,
    /// Which columns the scrollback is showing.
    columns: Columns,
    /// Candidates from the last ambiguous completion, shown under the input.
    offered: Vec<String>,
}

impl Default for ConsolePanel {
    fn default() -> Self {
        ConsolePanel {
            input: String::new(),
            history: Vec::new(),
            recall: None,
            focus: false,
            filter: Filter::default(),
            columns: Columns {
                time: true,
                target: true,
                wrap: true,
            },
            offered: Vec::new(),
        }
    }
}

impl ConsolePanel {
    /// Called when the panel is opened, so typing works without clicking first.
    pub(crate) fn opened(&mut self) {
        self.focus = true;
    }

    /// Draw the window.
    pub(crate) fn show(
        &mut self,
        ctx: &Context,
        lines: &[Line],
        names: &[String],
        chrome: &Chrome<'_>,
    ) -> Outcome {
        let mut outcome = Outcome {
            submitted: None,
            close: false,
            placed: Placed {
                geometry: None,
                open: true,
            },
        };
        let mut stats = Stats {
            total: lines.len(),
            hidden: 0,
            newest: lines.last().map_or(0, |line| line.seq),
        };
        outcome.placed = chrome::show(ctx, chrome, |ui| {
            egui::Panel::top(ui.id().with("toolbar"))
                .show(ui, |ui| self.toolbar(ui, lines, stats.newest));
            egui::Panel::bottom(ui.id().with("input")).show(ui, |ui| {
                self.prompt(ui, names, &mut outcome, stats);
            });
            // No frame: the window's own frame is already around all three panels, and a
            // second one would draw a box inside a box.
            egui::CentralPanel::no_frame().show(ui, |ui| {
                stats.hidden = view::show(ui, lines, &self.filter, self.columns);
            });
        });
        outcome
    }

    /// Clear, copy, the level filter, the search box and the view toggles.
    fn toolbar(&mut self, ui: &mut egui::Ui, lines: &[Line], newest: u64) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Clear")
                .on_hover_text("Hide everything above this point (ctrl+L)")
                .clicked()
            {
                self.filter.cleared_at = newest;
            }
            if ui
                .button("Copy")
                .on_hover_text("Copy what is shown to the clipboard")
                .clicked()
            {
                self.copy(lines);
            }
            ui.separator();
            egui::ComboBox::from_id_salt(ui.id().with("level"))
                .selected_text(level_name(self.filter.level))
                .width(78.0)
                .show_ui(ui, |ui| {
                    for level in [
                        log::LevelFilter::Error,
                        log::LevelFilter::Warn,
                        log::LevelFilter::Info,
                        log::LevelFilter::Debug,
                        log::LevelFilter::Trace,
                    ] {
                        ui.selectable_value(&mut self.filter.level, level, level_name(level));
                    }
                })
                .response
                .on_hover_text("Lowest log level shown. Command output is always shown.");
            ui.separator();
            ui.label("Find");
            ui.add(
                TextEdit::singleline(&mut self.filter.text)
                    .desired_width(120.0)
                    .hint_text("text"),
            );
            if !self.filter.text.is_empty() && super::icons::close(ui, "Clear the search") {
                self.filter.text.clear();
            }
            ui.separator();
            ui.checkbox(&mut self.filter.follow, "Follow")
                .on_hover_text("Stay at the newest line");
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.columns.wrap, "Wrap long lines");
                ui.checkbox(&mut self.columns.time, "Show the time");
                ui.checkbox(&mut self.columns.target, "Show the module");
            });
        });
    }

    /// The prompt, the completion hint and the status line.
    fn prompt(&mut self, ui: &mut egui::Ui, names: &[String], outcome: &mut Outcome, stats: Stats) {
        if !self.offered.is_empty() {
            ui.label(
                RichText::new(self.offered.join("  "))
                    .monospace()
                    .color(egui::Color32::from_gray(150)),
            );
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(">").monospace());
            // The keys are taken out of the queue *before* the box is drawn, while the focus
            // it had last frame still says whether they were meant for it. Letting the box
            // see them first is what made every other command disappear: egui's single-line
            // text edit answers Enter by surrendering focus, so by the time the response
            // could be asked whether Enter had arrived, the box no longer had the keyboard
            // and the line was dropped on the floor.
            let id = ui.id().with("line");
            let focused = ui.memory(|memory| memory.has_focus(id));
            let pressed = focused.then(|| self.take_keys(ui, names, stats.newest));
            let response = ui.add(
                TextEdit::singleline(&mut self.input)
                    .id(id)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("type `help`")
                    .desired_width(ui.available_width()),
            );
            if self.focus {
                response.request_focus();
                self.focus = false;
            }
            match pressed {
                Some(Pressed::Submit) => {
                    outcome.submitted = self.submit();
                    self.offered.clear();
                    // The box keeps the keyboard, so the next command can just be typed.
                    response.request_focus();
                }
                Some(Pressed::Close) => outcome.close = true,
                Some(Pressed::Nothing) | None => {}
            }
        });
        let hidden_note = if stats.hidden == 0 {
            String::new()
        } else {
            format!(", {} hidden by the filter", stats.hidden)
        };
        ui.label(
            RichText::new(format!(
                "{} lines{hidden_note} — Tab completes, Up recalls, Enter runs",
                stats.total
            ))
            .small()
            .color(egui::Color32::from_gray(130)),
        );
    }

    /// Take the keys the input box answers to out of this frame's queue.
    ///
    /// Every one of them is *consumed*, so the text edit drawn afterwards never sees it and
    /// cannot act on it as well — which is the whole point of doing this first.
    fn take_keys(&mut self, ui: &mut egui::Ui, names: &[String], newest: u64) -> Pressed {
        // Tab would otherwise move the focus to the next widget, which in a console means
        // the completion key closes the keyboard.
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab)) {
            self.complete(names);
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::L)) {
            self.filter.cleared_at = newest;
        }
        self.recall_history(ui);
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            return Pressed::Submit;
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            // The first Escape drops a half-typed line, the second closes the window: having
            // the only way out also lose what was typed is a trap.
            if self.input.is_empty() {
                return Pressed::Close;
            }
            self.input.clear();
            self.offered.clear();
        }
        Pressed::Nothing
    }

    /// Complete the typed line, or offer what it could be.
    fn complete(&mut self, names: &[String]) {
        self.offered.clear();
        let Some(completion) = complete::complete(&self.input, names) else {
            return;
        };
        self.input = completion.line;
        if completion.matches.len() > 1 {
            self.offered = completion.matches;
            self.offered.truncate(12);
        }
        // The caret follows the text: without this it stays where it was and the next
        // character typed lands in the middle of what was just completed.
        self.focus = true;
    }

    /// Put what the console is showing on the clipboard.
    fn copy(&self, lines: &[Line]) {
        let mut text = String::new();
        for line in lines.iter().filter(|line| self.filter.passes(line)) {
            use std::fmt::Write as _;
            // Into a string, so the only way this fails is an allocation failure, which
            // would have aborted before getting here.
            let _ = writeln!(text, "{} {}", line.clock, line.flat());
        }
        crate::win::clipboard::set_text(&text);
    }

    /// Walk the submitted-line history with the arrow keys.
    fn recall_history(&mut self, ui: &egui::Ui) {
        let (up, down) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
            )
        });
        if self.history.is_empty() || !(up || down) {
            return;
        }
        let last = self.history.len() - 1;
        self.recall = match (self.recall, up) {
            (None, true) => Some(last),
            (Some(0), true) => Some(0),
            (Some(n), true) => Some(n - 1),
            (Some(n), false) if n >= last => None,
            (Some(n), false) => Some(n + 1),
            (None, false) => None,
        };
        self.input = self
            .recall
            .and_then(|n| self.history.get(n))
            .cloned()
            .unwrap_or_default();
        self.focus = true;
    }

    /// Take the typed line, remember it, and hand it back for execution.
    fn submit(&mut self) -> Option<String> {
        let line = self.input.trim().to_owned();
        self.input.clear();
        self.recall = None;
        if line.is_empty() {
            return None;
        }
        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
        }
        if self.history.len() > HISTORY {
            self.history.remove(0);
        }
        Some(line)
    }
}

/// The name a level filter goes by in the toolbar.
fn level_name(level: log::LevelFilter) -> &'static str {
    match level {
        log::LevelFilter::Off => "Nothing",
        log::LevelFilter::Error => "Errors",
        log::LevelFilter::Warn => "Warnings",
        log::LevelFilter::Info => "Info",
        log::LevelFilter::Debug => "Debug",
        log::LevelFilter::Trace => "Trace",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded_and_skips_repeats() {
        let mut panel = ConsolePanel::default();
        for i in 0..HISTORY + 10 {
            panel.input = format!("line {i}");
            assert!(panel.submit().is_some());
        }
        assert_eq!(panel.history.len(), HISTORY);
        assert_eq!(panel.history.first().map(String::as_str), Some("line 10"));
        panel.input = format!("line {}", HISTORY + 9);
        assert!(panel.submit().is_some());
        assert_eq!(panel.history.len(), HISTORY, "a repeat is not stored twice");
        panel.input = "   ".to_owned();
        assert_eq!(panel.submit(), None);
    }
}
