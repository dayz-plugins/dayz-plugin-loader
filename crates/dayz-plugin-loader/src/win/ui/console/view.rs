//! The scrollback half of the console: one row per line, coloured by where it came from.
//!
//! Each row is laid out as a single galley rather than a row of labels. A console shows
//! hundreds of lines at once, and four widgets per line would be thousands of widgets a
//! frame; one galley per line is also what makes wrapping and text selection work across the
//! whole row instead of inside each column.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Label, ScrollArea, TextStyle, Ui};

use crate::scrollback::{Kind, Line};

/// Width the level column is padded to, in characters, so the text of every row starts at the
/// same place.
const TAG_WIDTH: usize = 5;

/// Which columns the console is showing.
#[derive(Debug, Clone, Copy)]
pub(super) struct Columns {
    /// The `HH:MM:SS` stamp.
    pub time: bool,
    /// The module a log record came from.
    pub target: bool,
    /// Whether long lines wrap instead of being cut off.
    pub wrap: bool,
}

/// The colour a line is drawn in.
fn colour(kind: Kind, ui: &Ui) -> Color32 {
    match kind {
        Kind::Echo => Color32::from_rgb(150, 205, 255),
        Kind::Output => ui.visuals().text_color(),
        Kind::Log(log::Level::Error) => Color32::from_rgb(240, 120, 120),
        Kind::Log(log::Level::Warn) => Color32::from_rgb(235, 195, 105),
        Kind::Log(log::Level::Info) => Color32::from_rgb(175, 195, 215),
        Kind::Log(log::Level::Debug) => Color32::from_gray(140),
        Kind::Log(log::Level::Trace) => Color32::from_gray(115),
    }
}

/// Lay one line out as a single galley.
fn row(line: &Line, ui: &Ui, columns: Columns, width: f32) -> LayoutJob {
    let font: FontId = TextStyle::Monospace.resolve(ui.style());
    let dim = Color32::from_gray(110);
    let body = colour(line.kind, ui);
    let mut job = LayoutJob::default();
    let mut put = |text: String, colour: Color32| {
        job.append(
            &text,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color: colour,
                ..TextFormat::default()
            },
        );
    };
    if columns.time {
        put(format!("{} ", line.clock), dim);
    }
    if let Kind::Log(_) = line.kind {
        put(format!("{:<TAG_WIDTH$} ", line.kind.tag()), body);
        if columns.target && !line.target.is_empty() {
            put(format!("{} ", short_target(&line.target)), dim);
        }
    }
    put(line.text.clone(), body);
    job.wrap.max_width = if columns.wrap { width } else { f32::INFINITY };
    job
}

/// A log target without the crate prefix.
///
/// Every record the loader makes is targeted at a module inside the one crate, so the first
/// segment is the same on every line and only costs width.
fn short_target(target: &str) -> &str {
    target.split_once("::").map_or(target, |(_, rest)| rest)
}

/// Draw the scrollback. Returns how many lines the filter hid.
pub(super) fn show(ui: &mut Ui, lines: &[Line], filter: &Filter, columns: Columns) -> usize {
    let shown: Vec<&Line> = lines.iter().filter(|line| filter.passes(line)).collect();
    let hidden = lines.len() - shown.len();
    let width = ui.available_width();
    ScrollArea::both()
        .stick_to_bottom(filter.follow)
        .auto_shrink([false, false])
        .show_rows(
            ui,
            ui.text_style_height(&TextStyle::Monospace),
            shown.len(),
            |ui, range| {
                // Only the rows in view are laid out, which is what keeps a full scrollback
                // from costing a galley per line per frame.
                for line in shown.get(range).unwrap_or_default() {
                    ui.add(Label::new(row(line, ui, columns, width)).selectable(true));
                }
            },
        );
    hidden
}

/// What the console is currently showing of its buffer.
#[derive(Debug, Clone)]
pub(super) struct Filter {
    /// Lowest log level shown. Command output is never filtered out.
    pub level: log::LevelFilter,
    /// Substring that a line must contain, case insensitively.
    pub text: String,
    /// Whether the view sticks to the newest line.
    pub follow: bool,
    /// Lines up to and including this sequence number are hidden.
    ///
    /// Clearing hides rather than deletes: the buffer is also what a plugin's `console_exec`
    /// reads back and what the next `Copy` would take, and one window's clear button has no
    /// business throwing away either. It is also undoable, which a delete is not.
    pub cleared_at: u64,
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            // Debug and trace are for a log file, not for a window someone is reading.
            level: log::LevelFilter::Info,
            text: String::new(),
            follow: true,
            cleared_at: 0,
        }
    }
}

impl Filter {
    /// Whether a line is shown.
    pub(super) fn passes(&self, line: &Line) -> bool {
        if line.seq <= self.cleared_at {
            return false;
        }
        if !line.kind.passes(self.level) {
            return false;
        }
        if self.text.is_empty() {
            return true;
        }
        let needle = self.text.to_ascii_lowercase();
        line.text.to_ascii_lowercase().contains(&needle)
            || line.target.to_ascii_lowercase().contains(&needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: Kind, target: &str, text: &str) -> Line {
        Line {
            seq: 1,
            kind,
            clock: "12:00:00".to_owned(),
            target: target.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn the_crate_prefix_is_dropped_from_a_target() {
        assert_eq!(short_target("dxgi::win::ui"), "win::ui");
        assert_eq!(short_target("dxgi"), "dxgi");
    }

    #[test]
    fn the_filter_matches_text_or_target_and_respects_the_level() {
        let mut filter = Filter::default();
        let warning = line(Kind::Log(log::Level::Warn), "dxgi::win", "no swapchain");
        let debug = line(Kind::Log(log::Level::Debug), "dxgi::win", "hooked Present");
        assert!(filter.passes(&warning));
        assert!(!filter.passes(&debug), "debug is below the default level");

        filter.text = "SWAP".to_owned();
        assert!(filter.passes(&warning), "the needle is case insensitive");
        filter.text = "win".to_owned();
        assert!(filter.passes(&warning), "the target counts as a match");
        filter.text = "nothing".to_owned();
        assert!(!filter.passes(&warning));

        filter.text.clear();
        filter.level = log::LevelFilter::Error;
        assert!(!filter.passes(&warning));
        assert!(
            filter.passes(&line(Kind::Output, "", "vr.ipd = 1")),
            "an answer to a typed command is never filtered away"
        );
    }

    #[test]
    fn clearing_hides_everything_up_to_the_newest_line_and_nothing_after_it() {
        let mut filter = Filter::default();
        let old = Line {
            seq: 7,
            ..line(Kind::Output, "", "before")
        };
        let new = Line {
            seq: 8,
            ..line(Kind::Output, "", "after")
        };
        filter.cleared_at = 7;
        assert!(!filter.passes(&old));
        assert!(filter.passes(&new));
    }
}
