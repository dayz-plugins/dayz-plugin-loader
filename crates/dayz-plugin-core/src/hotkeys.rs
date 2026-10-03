//! Hotkey registry: actions, their effective chords and edge-triggered matching.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::keys::{self, Chord, KeyError, Modifiers};

/// Stable identifier of a registered action: `<plugin>.<action>`.
pub type ActionName = String;

/// One registered action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Qualified name.
    pub name: ActionName,
    /// Short label.
    pub title: String,
    /// Binding from the descriptor.
    pub default: Option<Chord>,
    /// Effective binding (user override or default).
    pub chord: Option<Chord>,
    /// Whether the main key was down at the last poll; used for edge detection.
    down: bool,
}

/// Why registration failed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HotkeyError {
    /// Action already registered.
    #[error("hotkey {0:?} is already registered")]
    Duplicate(String),
    /// The default binding did not parse.
    #[error("hotkey {0:?} default binding: {1}")]
    BadDefault(String, KeyError),
}

/// All actions of all plugins plus user overrides.
#[derive(Debug, Default)]
pub struct Registry {
    entries: Vec<Entry>,
}

/// Snapshot of key state the registry matches against. The loader fills it from the OS.
pub trait KeyState {
    /// Whether virtual key `vk` is currently held.
    fn is_down(&self, vk: u16) -> bool;
    /// Which modifiers are currently held.
    fn modifiers(&self) -> Modifiers;
}

impl Registry {
    /// Register an action. `overrides` holds user bindings by qualified name; an override
    /// that fails to parse is reported back so the loader can log it, and the default applies.
    ///
    /// # Errors
    /// Duplicate name or an unparsable default.
    pub fn register(
        &mut self,
        name: &str,
        title: &str,
        default_binding: &str,
        overrides: &BTreeMap<String, String>,
    ) -> Result<Option<KeyError>, HotkeyError> {
        if self.entries.iter().any(|e| e.name == name) {
            return Err(HotkeyError::Duplicate(name.to_owned()));
        }
        let default = keys::parse(default_binding)
            .map_err(|e| HotkeyError::BadDefault(name.to_owned(), e))?;
        let (chord, override_error) = match overrides.get(name).map(|b| keys::parse(b)) {
            Some(Ok(chord)) => (chord, None),
            Some(Err(e)) => (default, Some(e)),
            None => (default, None),
        };
        self.entries.push(Entry {
            name: name.to_owned(),
            title: title.to_owned(),
            default,
            chord,
            down: false,
        });
        Ok(override_error)
    }

    /// Change the effective binding of an action at runtime.
    #[must_use]
    pub fn rebind(&mut self, name: &str, chord: Option<Chord>) -> bool {
        match self.entries.iter_mut().find(|e| e.name == name) {
            Some(e) => {
                e.chord = chord;
                e.down = false;
                true
            }
            None => false,
        }
    }

    /// Registered actions in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    /// Compare against the current key state and return the actions whose key went from
    /// up to down with matching modifiers. Call once per frame while the game has focus.
    pub fn poll(&mut self, state: &impl KeyState) -> Vec<ActionName> {
        let held = state.modifiers();
        let mut fired = Vec::new();
        for entry in &mut self.entries {
            let Some(chord) = entry.chord else { continue };
            let now = state.is_down(chord.vk);
            if now && !entry.down && chord.modifiers == held {
                fired.push(entry.name.clone());
            }
            entry.down = now;
        }
        fired
    }

    /// Forget all key-down state, for example when the window loses focus.
    pub fn reset(&mut self) {
        for entry in &mut self.entries {
            entry.down = false;
        }
    }

    /// Bindings in config file form, for writing back after an in-game rebind.
    #[must_use]
    pub fn bindings(&self) -> BTreeMap<String, String> {
        self.entries
            .iter()
            .map(|e| {
                (
                    e.name.clone(),
                    e.chord.map_or_else(|| "none".to_owned(), |c| c.to_string()),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Keys {
        down: Vec<u16>,
        mods: Modifiers,
    }

    impl KeyState for Keys {
        fn is_down(&self, vk: u16) -> bool {
            self.down.contains(&vk)
        }
        fn modifiers(&self) -> Modifiers {
            self.mods
        }
    }

    fn registry() -> Registry {
        let mut overrides = BTreeMap::new();
        overrides.insert("vr.recenter".to_owned(), "ctrl+f11".to_owned());
        overrides.insert("vr.broken".to_owned(), "nokey".to_owned());
        let mut r = Registry::default();
        assert_eq!(
            r.register("vr.toggle", "Toggle", "f12", &overrides),
            Ok(None)
        );
        assert_eq!(
            r.register("vr.recenter", "Recenter", "f11", &overrides),
            Ok(None)
        );
        assert!(matches!(
            r.register("vr.broken", "Broken", "f10", &overrides),
            Ok(Some(KeyError::UnknownKey(_)))
        ));
        r
    }

    #[test]
    fn overrides_apply_and_bad_ones_fall_back() {
        let r = registry();
        let b = r.bindings();
        assert_eq!(b["vr.toggle"], "f12");
        assert_eq!(b["vr.recenter"], "ctrl+f11");
        assert_eq!(b["vr.broken"], "f10");
    }

    #[test]
    fn fires_on_edge_with_exact_modifiers() {
        let mut r = registry();
        let none = Modifiers::default();
        assert_eq!(
            r.poll(&Keys {
                down: vec![0x7B],
                mods: none
            }),
            vec!["vr.toggle"]
        );
        assert!(
            r.poll(&Keys {
                down: vec![0x7B],
                mods: none
            })
            .is_empty(),
            "held key fires once"
        );
        assert!(r
            .poll(&Keys {
                down: vec![],
                mods: none
            })
            .is_empty());
        assert!(
            r.poll(&Keys {
                down: vec![0x7A],
                mods: none
            })
            .is_empty(),
            "f11 needs ctrl"
        );
        r.reset();
        let ctrl = Modifiers { ctrl: true, ..none };
        assert_eq!(
            r.poll(&Keys {
                down: vec![0x7A],
                mods: ctrl
            }),
            vec!["vr.recenter"]
        );
    }

    #[test]
    fn duplicate_and_rebind() {
        let mut r = registry();
        assert_eq!(
            r.register("vr.toggle", "x", "f1", &BTreeMap::new()),
            Err(HotkeyError::Duplicate("vr.toggle".into()))
        );
        assert!(r.rebind("vr.toggle", None));
        assert!(!r.rebind("missing", None));
        assert_eq!(r.bindings()["vr.toggle"], "none");
    }
}
