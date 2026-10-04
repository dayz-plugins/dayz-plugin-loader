//! Reading an event's contents, using the layouts in `dayz-data`.
//!
//! [`super::game_events`] names an event by asking the event itself. Its *contents* cannot be
//! had that way: there is no generic marshaller in the engine to borrow from. What the engine
//! does instead is one 25-branch function that switches on the class and builds a script
//! parameter out of direct field reads — so every offset is a fact somebody has to establish
//! per class and per build, and facts like that belong in the symbol database.
//!
//! This module is therefore small on purpose. It turns the database's event section into a
//! lookup once, and then reading an event is: find the class by the name already decoded,
//! read each field at its offset, render it. A class the database says nothing about gets no
//! fields, which is "nobody has established this one yet" and not "it has none".
//!
//! Field order is the order the engine's own script parameters list them, which is **not**
//! memory order — `ChatMessageEvent`'s channel is its first parameter and sits at `+0x38`,
//! after all three of its strings. The database stores the parameter order because that is
//! the order a person reading a log expects, and the offsets make the reads independent of it.

use std::collections::HashMap;
use std::sync::OnceLock;

use dayz_data::{FieldKind as DataKind, SymbolTable};
use dayz_plugin_api::{FieldKind, GameClass, GameField, Str};

use super::gamemem::{self, Layout};

/// One field of one class, as the database describes it.
struct Spec {
    /// The field's name.
    name: &'static str,
    /// Where in the event object it sits.
    offset: usize,
    /// How to read it.
    kind: FieldKind,
}

/// One event class the database describes.
struct Class {
    /// What the event means, or empty.
    note: &'static str,
    /// Its fields, in script parameter order.
    fields: Vec<Spec>,
}

/// The database's event section, turned into a lookup.
///
/// Built once and never written again. The strings are deliberately *leaked* rather than
/// owned by the map: a plugin is handed a [`Str`] into them and told it may keep it, which is
/// a promise about the process and not about this map. Leaking says that in the type, where a
/// `Box<str>` inside the map would need the lifetime laundering back out again.
static CLASSES: OnceLock<HashMap<&'static str, Class>> = OnceLock::new();

/// One field, read and rendered. Owns its text so the [`GameField`] can borrow it.
pub(super) struct Decoded {
    /// The field's name.
    name: &'static str,
    /// How it was stored.
    kind: FieldKind,
    /// The value, formatted.
    text: String,
    /// Where the field is, for a plugin that wants it typed.
    address: u64,
}

impl Decoded {
    /// The ABI view of this field. Borrows from `self`, so both have to outlive the call.
    pub(super) fn as_abi(&self) -> GameField {
        GameField {
            struct_size: core::mem::size_of::<GameField>(),
            name: Str::new(self.name),
            kind: self.kind,
            text: Str::new(&self.text),
            address: self.address,
        }
    }
}

/// Translate the database's idea of a field kind into the ABI's.
///
/// Both are open sets, so a kind added to the database after this was written arrives as
/// [`FieldKind::UNKNOWN`] and is rendered as raw bytes rather than refused.
fn kind_of(kind: DataKind) -> FieldKind {
    match kind {
        DataKind::Int => FieldKind::INT,
        DataKind::Float => FieldKind::FLOAT,
        DataKind::Bool => FieldKind::BOOL,
        DataKind::String => FieldKind::STRING,
        DataKind::Vector => FieldKind::VECTOR,
        DataKind::Object => FieldKind::OBJECT,
        _ => FieldKind::UNKNOWN,
    }
}

/// Build the lookup from the resolved database. Called once, from initialisation.
pub(super) fn initialize(table: &SymbolTable) {
    let mut classes = HashMap::new();
    for (name, entry) in table.events() {
        let fields = entry
            .fields
            .iter()
            .filter_map(|field| {
                Some(Spec {
                    name: &*Box::leak(field.name.clone().into_boxed_str()),
                    offset: usize::try_from(field.offset).ok()?,
                    kind: kind_of(field.kind),
                })
            })
            .collect();
        let class = Class {
            note: &*Box::leak(entry.note.clone().unwrap_or_default().into_boxed_str()),
            fields,
        };
        classes.insert(&*Box::leak(name.to_owned().into_boxed_str()), class);
    }
    let decoded = classes.values().filter(|c| !c.fields.is_empty()).count();
    let total = classes.len();
    if CLASSES.set(classes).is_err() {
        return;
    }
    if total == 0 {
        log::info!("no event layouts for this build; events will be reported by name only");
    } else {
        log::info!("{total} event classes known, {decoded} with field layouts");
    }
}

/// Read every field of one event, or nothing if the class is not in the database.
///
/// Never fails as a whole: a field whose offset is not readable renders as `<unreadable>` and
/// the rest are still returned. An event is the engine's, not ours, and half of its contents
/// is more use than none of them.
pub(super) fn decode(event: usize, class: &str, layout: &Layout) -> Vec<Decoded> {
    let Some(classes) = CLASSES.get() else {
        return Vec::new();
    };
    let Some(class) = classes.get(class) else {
        return Vec::new();
    };
    class
        .fields
        .iter()
        .map(|spec| {
            let address = event.checked_add(spec.offset);
            Decoded {
                name: spec.name,
                kind: spec.kind,
                text: address.map_or_else(
                    || "<out of range>".to_owned(),
                    |at| render(at, spec.kind, layout),
                ),
                address: address.map_or(0, |at| at as u64),
            }
        })
        .collect()
}

/// One field's value as text.
fn render(address: usize, kind: FieldKind, layout: &Layout) -> String {
    match kind {
        FieldKind::INT => gamemem::read_u32(address).map_or_else(unreadable, |raw| {
            // Signed, because every int the engine puts in an event is: a kick reason, a
            // channel, a countdown. `cast_signed` would be neater but postdates the MSRV.
            format!("{}", i32::from_ne_bytes(raw.to_ne_bytes()))
        }),
        FieldKind::FLOAT => {
            gamemem::read_f32(address).map_or_else(unreadable, |raw| format!("{raw}"))
        }
        FieldKind::BOOL => gamemem::read_u32(address).map_or_else(unreadable, |raw| match raw {
            0 => "false".to_owned(),
            1 => "true".to_owned(),
            // Not a bool after all. Saying so is better than calling it true.
            other => format!("{other} (not a boolean)"),
        }),
        FieldKind::STRING => gamemem::read_pointer(address)
            .map_or_else(unreadable, |holder| gamemem::holder_string(holder, layout)),
        FieldKind::VECTOR => {
            let axes = [0usize, 4, 8].map(|step| {
                address
                    .checked_add(step)
                    .and_then(gamemem::read_f32)
                    .map_or_else(|| "?".to_owned(), |value| format!("{value}"))
            });
            format!("<{}, {}, {}>", axes[0], axes[1], axes[2])
        }
        // An object is reported, not followed: reading inside it needs that class's layout,
        // which is a different question from this one.
        FieldKind::OBJECT => gamemem::read_pointer(address).map_or_else(unreadable, |object| {
            if object == 0 {
                "none".to_owned()
            } else {
                format!("{object:#X}")
            }
        }),
        // A kind from a newer database than this loader. The bytes are still worth having.
        _ => gamemem::read_u32(address).map_or_else(unreadable, |raw| format!("{raw:#010X}")),
    }
}

/// What an unreadable field renders as. A whole word rather than an empty string, so a log
/// line says the read failed instead of looking like the field was blank.
fn unreadable() -> String {
    "<unreadable>".to_owned()
}

/// Fill `out` with every known class, and report how many there are.
///
/// This is `HostApi::game_catalogue`. The two-call shape is the caller's: `capacity` zero asks
/// for the count.
pub(super) fn catalogue(out: &mut [GameClass]) -> usize {
    let Some(classes) = CLASSES.get() else {
        return 0;
    };
    // Sorted, because a panel with a row per event wants the same order every time it opens
    // and a `HashMap` does not promise one.
    let mut names: Vec<&'static str> = classes.keys().copied().collect();
    names.sort_unstable();
    for (slot, name) in out.iter_mut().zip(&names) {
        let class = &classes[name];
        *slot = GameClass {
            struct_size: core::mem::size_of::<GameClass>(),
            name: Str::new(name),
            note: Str::new(class.note),
            field_count: class.fields.len(),
        };
    }
    names.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout {
            vtable_name: 0x10,
            string_length: 0x08,
            string_data: 0x10,
        }
    }

    #[test]
    fn every_database_kind_has_an_abi_kind() {
        let pairs = [
            (DataKind::Int, FieldKind::INT),
            (DataKind::Float, FieldKind::FLOAT),
            (DataKind::Bool, FieldKind::BOOL),
            (DataKind::String, FieldKind::STRING),
            (DataKind::Vector, FieldKind::VECTOR),
            (DataKind::Object, FieldKind::OBJECT),
        ];
        for (from, to) in pairs {
            assert_eq!(kind_of(from), to, "{from:?}");
            assert_ne!(to, FieldKind::UNKNOWN, "{from:?}");
        }
    }

    /// Nothing in the test process is the game, so every read fails — which is the case worth
    /// pinning down, because it is what a wrong offset in the database looks like at runtime.
    /// A field must render as a complaint, never as a plausible value.
    #[test]
    fn an_unreadable_field_says_so_rather_than_inventing_a_value() {
        let kinds = [
            FieldKind::INT,
            FieldKind::FLOAT,
            FieldKind::BOOL,
            FieldKind::STRING,
            FieldKind::OBJECT,
            FieldKind::UNKNOWN,
        ];
        for kind in kinds {
            assert_eq!(render(0, kind, &layout()), unreadable(), "{kind:?}");
        }
        // A vector is three reads, so it reports per axis instead of all or nothing.
        assert_eq!(render(0, FieldKind::VECTOR, &layout()), "<?, ?, ?>");
    }

    #[test]
    fn an_unknown_class_decodes_to_nothing_rather_than_guessing() {
        assert!(decode(0x1000, "NoSuchEvent", &layout()).is_empty());
    }

    #[test]
    fn asking_for_the_catalogue_with_no_room_still_counts() {
        // Before `initialize`, which is the state in a test process.
        assert_eq!(catalogue(&mut []), 0);
    }
}
