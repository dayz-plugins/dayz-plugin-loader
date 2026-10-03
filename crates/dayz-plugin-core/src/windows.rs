//! Where the overlay's windows were left, and the preferences that live beside them.
//!
//! Any window that comes back — the console, the settings editor, a plugin's panel — has an
//! id, and that id is what this remembers it by: position, size and opacity. A window without
//! an id (a toast, a dialog) is not in here, because there is nothing to come back to.
//!
//! Kept in the core crate so the file format has tests that do not need a game, a device or
//! a window to run.

use std::collections::BTreeMap;

/// Least opacity a window can be set to. Lower and a window could be left invisible, with no
//  way to find it again short of editing the file by hand.
pub const MIN_ALPHA: f32 = 0.2;

/// Prefix of the per-window keys in the store.
const WINDOW_PREFIX: &str = "window.";
/// Key of the advanced-settings preference.
const ADVANCED_KEY: &str = "ui.advanced";

/// Where one window was and how transparent it was, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
    /// `1.0` is opaque, [`MIN_ALPHA`] the most transparent a user can ask for.
    pub alpha: f32,
}

impl Geometry {
    /// `"<x> <y> <w> <h> <alpha>"`, the form stored in the file.
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split_whitespace();
        let mut next = || parts.next()?.parse::<f32>().ok().filter(|n| n.is_finite());
        let geometry = Geometry {
            x: next()?,
            y: next()?,
            w: next()?,
            h: next()?,
            alpha: next().unwrap_or(1.0),
        };
        // A zero-sized or negative window is not something to restore someone into.
        if geometry.w < 1.0 || geometry.h < 1.0 {
            return None;
        }
        Some(geometry.clamped())
    }

    /// The same geometry with the opacity brought into range.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.alpha = if self.alpha.is_finite() {
            self.alpha.clamp(MIN_ALPHA, 1.0)
        } else {
            1.0
        };
        self
    }

    fn encode(&self) -> String {
        format!(
            "{:.0} {:.0} {:.0} {:.0} {:.2}",
            self.x, self.y, self.w, self.h, self.alpha
        )
    }

    /// Whether the two differ by enough to be worth writing to disk: half a point of
    /// movement, or a percent of opacity.
    fn differs_from(&self, other: &Self) -> bool {
        (self.x - other.x).abs() > 0.5
            || (self.y - other.y).abs() > 0.5
            || (self.w - other.w).abs() > 0.5
            || (self.h - other.h).abs() > 0.5
            || (self.alpha - other.alpha).abs() > 0.01
    }
}

/// Remembered geometry per window id, plus the editor's own preferences.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    windows: BTreeMap<String, Geometry>,
    advanced: bool,
    dirty: bool,
}

impl Layout {
    /// Read what a `windows.toml` store held. Unparseable entries are dropped: a window whose
    /// line is nonsense opens where it would have without a file.
    #[must_use]
    pub fn from_store(values: &BTreeMap<String, String>) -> Self {
        let mut layout = Layout::default();
        for (key, value) in values {
            if let Some(id) = key.strip_prefix(WINDOW_PREFIX) {
                if let Some(geometry) = Geometry::parse(value) {
                    layout.windows.insert(id.to_owned(), geometry);
                }
            } else if key == ADVANCED_KEY {
                layout.advanced = matches!(value.trim(), "true" | "1" | "yes" | "on");
            }
        }
        layout
    }

    /// The store to write back.
    #[must_use]
    pub fn to_store(&self) -> BTreeMap<String, String> {
        let mut values: BTreeMap<String, String> = self
            .windows
            .iter()
            .map(|(id, geometry)| (format!("{WINDOW_PREFIX}{id}"), geometry.encode()))
            .collect();
        values.insert(ADVANCED_KEY.to_owned(), self.advanced.to_string());
        values
    }

    /// Where this window was last seen, if it has been seen.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Geometry> {
        self.windows.get(id).copied()
    }

    /// Note where a window is now. Only a real change marks the layout for saving, so a
    /// window that merely sits there does not rewrite the file every frame.
    pub fn remember(&mut self, id: &str, geometry: Geometry) {
        let geometry = geometry.clamped();
        match self.windows.get(id) {
            Some(previous) if !geometry.differs_from(previous) => {}
            _ => {
                self.windows.insert(id.to_owned(), geometry);
                self.dirty = true;
            }
        }
    }

    /// Whether the settings editor shows settings marked advanced.
    #[must_use]
    pub fn advanced(&self) -> bool {
        self.advanced
    }

    /// Show or hide advanced settings.
    pub fn set_advanced(&mut self, advanced: bool) {
        if advanced != self.advanced {
            self.advanced = advanced;
            self.dirty = true;
        }
    }

    /// Whether anything changed since the last call, clearing the flag.
    pub fn take_dirty(&mut self) -> bool {
        core::mem::take(&mut self.dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn a_layout_survives_a_round_trip() {
        let layout = Layout::from_store(&store(&[
            ("window.loader.console", "120 80 640 360 0.90"),
            ("ui.advanced", "true"),
        ]));
        assert!(layout.advanced());
        let console = layout.get("loader.console");
        assert_eq!(console.map(|g| (g.x, g.w)), Some((120.0, 640.0)));
        let again = Layout::from_store(&layout.to_store());
        assert_eq!(again.get("loader.console"), layout.get("loader.console"));
        assert!(again.advanced());
    }

    #[test]
    fn nonsense_entries_are_dropped_rather_than_guessed_at() {
        let layout = Layout::from_store(&store(&[
            ("window.a", "nope"),
            ("window.b", "1 2 0 0 1"),
            ("window.c", "1 2 300 200"),
        ]));
        assert_eq!(layout.get("a"), None);
        assert_eq!(layout.get("b"), None, "a zero-sized window is not restored");
        assert_eq!(layout.get("c").map(|g| g.alpha), Some(1.0));
    }

    #[test]
    fn only_a_real_change_asks_for_a_save() {
        let mut layout = Layout::default();
        let geometry = Geometry {
            x: 10.0,
            y: 10.0,
            w: 200.0,
            h: 100.0,
            alpha: 1.0,
        };
        layout.remember("panel", geometry);
        assert!(layout.take_dirty(), "a window seen for the first time");
        layout.remember("panel", geometry);
        assert!(!layout.take_dirty(), "the same place again");
        layout.remember(
            "panel",
            Geometry {
                x: 10.2,
                ..geometry
            },
        );
        assert!(!layout.take_dirty(), "a fifth of a point is not a move");
        layout.remember(
            "panel",
            Geometry {
                x: 40.0,
                ..geometry
            },
        );
        assert!(layout.take_dirty());
    }

    #[test]
    fn opacity_cannot_be_set_to_invisible() {
        let mut layout = Layout::default();
        layout.remember(
            "panel",
            Geometry {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
                alpha: 0.0,
            },
        );
        assert_eq!(layout.get("panel").map(|g| g.alpha), Some(MIN_ALPHA));
    }
}
