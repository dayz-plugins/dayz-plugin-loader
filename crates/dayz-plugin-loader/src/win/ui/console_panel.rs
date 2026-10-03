//! The loader's own console, drawn in the game.
//!
//! This is the overlay's first customer on purpose: it exercises scrollback, text input,
//! focus capture and command execution before any plugin depends on the machinery, and it
//! answers the question the separate console window could not — how to reach the console
//! without leaving the game.

use egui::{Context, Key, RichText, ScrollArea, TextEdit};

/// How many console lines the panel shows. The buffer behind it is bounded anyway.
const VISIBLE_LINES: usize = 400;

/// What the panel remembers between frames.
#[derive(Default)]
pub(crate) struct ConsolePanel {
    /// The line being typed.
    input: String,
    /// Previously submitted lines, newest last.
    history: Vec<String>,
    /// Where arrow-up has walked to, counted back from the end.
    recall: Option<usize>,
    /// Set when the input box should take the keyboard on the next frame.
    focus: bool,
}

impl ConsolePanel {
    /// Called when the panel is opened, so typing works without clicking first.
    pub(crate) fn opened(&mut self) {
        self.focus = true;
    }

    /// Draw the window. Returns a line to execute, if one was submitted.
    pub(crate) fn show(
        &mut self,
        ctx: &Context,
        lines: &[String],
        open: &mut bool,
    ) -> Option<String> {
        let mut submitted = None;
        egui::Window::new("DayZ plugin loader")
            .default_size([640.0, 360.0])
            .open(open)
            .show(ctx, |ui| {
                let rows = lines.len().min(VISIBLE_LINES);
                ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .max_height(ui.available_height() - 32.0)
                    .show(ui, |ui| {
                        for line in lines.iter().skip(lines.len() - rows) {
                            ui.label(RichText::new(line).monospace());
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(">").monospace());
                    let response = ui.add(
                        TextEdit::singleline(&mut self.input)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("type `help`")
                            .desired_width(ui.available_width()),
                    );
                    if self.focus {
                        response.request_focus();
                        self.focus = false;
                    }
                    if response.has_focus() {
                        self.recall_history(ui.ctx());
                    }
                    if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        submitted = self.submit();
                        // Enter in egui takes focus away; the console is a place where the
                        // next command follows the last one, so it is taken straight back.
                        self.focus = true;
                    }
                });
            });
        submitted
    }

    /// Walk the submitted-line history with the arrow keys.
    fn recall_history(&mut self, ctx: &Context) {
        let (up, down) =
            ctx.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown)));
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
        Some(line)
    }
}
