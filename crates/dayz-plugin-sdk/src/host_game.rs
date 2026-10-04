//! The game-stream half of [`Host`], kept apart from `host.rs` for size.
//!
//! Nothing here is different in kind from the rest of the table — a call through a function
//! pointer, a status turned into a `Result` — these are just the ones about the game's own
//! events rather than about the loader.

// FFI module: every function here is a call through the loader's table.
#![allow(unsafe_code)]

use dayz_plugin_api::GameClass;

use crate::game::{Class, Streams};
use crate::host::{check, str_ref, Host, PluginError};

/// Room made for the catalogue before asking for it, to save the counting call in the common
/// case. This build has 77 classes; a round number above that means one call, and being wrong
/// costs a second one rather than a wrong answer.
const CATALOGUE_GUESS: usize = 128;

impl Host {
    /// Subscribe to the game's own streams, delivered to
    /// [`Plugin::on_game_event`](crate::Plugin::on_game_event),
    /// [`Plugin::on_chat`](crate::Plugin::on_chat) and [`Plugin::on_rpc`](crate::Plugin::on_rpc).
    ///
    /// Callable at any time, the last call wins, and [`Streams::NONE`] unsubscribes.
    ///
    /// # Errors
    /// The plugin has no callback for one of the streams it asked for, or the loader could
    /// not hook the engine on this build — in which case the game is untouched and nothing
    /// will ever be delivered.
    pub fn listen_game(&self, streams: Streams) -> Result<(), PluginError> {
        // SAFETY: valid table pointer.
        check(unsafe { (self.api().game_listen)(self.api().host, self.handle(), streams) })
    }

    /// Every event class the loader knows the shape of, sorted by name.
    ///
    /// This is the list *before* anything has happened, which is what a settings screen with
    /// a row per event needs: waiting for an event to appear would mean the screen filling up
    /// as the session goes on, and a player cannot tick a box for an event they have not seen
    /// yet.
    ///
    /// An empty list is not a failure. It means this build's symbol database has no event
    /// section, and events will still be delivered — named, with no fields.
    ///
    /// # Errors
    /// The loader refused the call, which at this point only happens if the table is older
    /// than this SDK.
    pub fn game_catalogue(&self) -> Result<Vec<Class>, PluginError> {
        let mut slots = vec![blank(); CATALOGUE_GUESS];
        let mut total = 0usize;
        // SAFETY: `slots` is a writable buffer of `slots.len()` structures and `total` is a
        // writable `usize`, which is the contract.
        let status = unsafe {
            (self.api().game_catalogue)(
                self.api().host,
                self.handle(),
                slots.as_mut_ptr(),
                slots.len(),
                &raw mut total,
            )
        };
        if total > slots.len() {
            // More classes than the guess. The count is the true total either way, so ask
            // again with room rather than returning a truncated list.
            slots = vec![blank(); total];
            // SAFETY: as above, with a buffer the loader has just said is big enough.
            check(unsafe {
                (self.api().game_catalogue)(
                    self.api().host,
                    self.handle(),
                    slots.as_mut_ptr(),
                    slots.len(),
                    &raw mut total,
                )
            })?;
        } else {
            check(status)?;
        }
        Ok(slots
            .iter()
            .take(total)
            .map(|slot| Class {
                // The loader's catalogue strings last as long as the process, so unlike a
                // live event's fields these are kept rather than copied.
                name: str_ref(slot.name),
                note: str_ref(slot.note),
                field_count: slot.field_count,
            })
            .collect())
    }

    /// Put one line in this client's own chat. Nobody else sees it.
    ///
    /// The engine draws it and forgets it: nothing is sent to the server and there is no
    /// history it can be taken out of, which makes it the cheapest way to tell a player
    /// something in passing. `colour` names one of the game's own colour classes —
    /// `ColorImportant`, `ColorFriendly`, `ColorEnemy` — and `""` takes the game's default.
    ///
    /// # Errors
    /// The game object is not known to the loader yet. It is caught from the engine rather
    /// than hunted for, so it arrives once the engine has dispatched a remote call — in
    /// practice, after joining a server once this session.
    pub fn chat_local(&self, text: &str, colour: &str) -> Result<(), PluginError> {
        // SAFETY: valid table pointer; both strings outlive the call.
        check(unsafe {
            (self.api().chat_local)(
                self.api().host,
                self.handle(),
                dayz_plugin_api::Str::new(text),
                dayz_plugin_api::Str::new(colour),
            )
        })
    }
}

/// An empty slot for the loader to fill, carrying its own size.
fn blank() -> GameClass {
    GameClass {
        struct_size: core::mem::size_of::<GameClass>(),
        name: dayz_plugin_api::Str::EMPTY,
        note: dayz_plugin_api::Str::EMPTY,
        field_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_slot_describes_its_own_size() {
        let slot = blank();
        assert_eq!(slot.struct_size, core::mem::size_of::<GameClass>());
        assert_eq!(slot.field_count, 0);
    }
}
