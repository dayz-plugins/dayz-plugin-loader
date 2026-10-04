//! Listening to the game: its events, its chat, its remote calls.
//!
//! Everything that happens to a DayZ session is an event object the engine broadcasts, and
//! the loader watches that broadcast. A plugin asks for the streams it wants with
//! [`Host::listen_game`](crate::Host::listen_game) and reads them in
//! [`Plugin::on_game_event`](crate::Plugin::on_game_event),
//! [`Plugin::on_chat`](crate::Plugin::on_chat) and [`Plugin::on_rpc`](crate::Plugin::on_rpc):
//!
//! ```ignore
//! fn start(host: Host) -> Result<Self, PluginError> {
//!     host.listen_game(Streams::EVENTS | Streams::CHAT)?;
//!     Ok(Watcher)
//! }
//!
//! fn on_game_event(&self, host: &Host, event: &Event<'_>) {
//!     if event.is("RespawnEvent") {
//!         host.console_print("spawned");
//!     }
//! }
//!
//! fn on_chat(&self, _host: &Host, chat: &Chat<'_>) -> ChatVerdict {
//!     // Keep one particular nuisance off the screen.
//!     if chat.from == "server" && chat.text.contains("vote") {
//!         ChatVerdict::SWALLOW
//!     } else {
//!         ChatVerdict::PASS
//!     }
//! }
//! ```
//!
//! ## What an event tells you, and what it does not
//!
//! [`Event::name`] is the event's own class name, read out of the engine, so it is right for
//! classes this loader has never heard of — `ConnectingStartEvent`, `MPConnectionCloseEvent`,
//! `ClientPrepareEvent`, `RespawnEvent`, `PlayerDeathEvent` and so on. Knowing *that* an
//! event happened is reliable.
//!
//! Reading what is *inside* one is not, and the SDK does not pretend otherwise. Each class
//! has its own fields at its own offsets, and only [`Event::object`] — the raw pointer — is
//! offered for a plugin that has worked out a particular class's layout and wants to read it
//! with the memory functions. [`Chat`] is the exception: it is caught where the engine still
//! has the parts as separate arguments, so its fields are real.

// Reading the loader's structures is a pointer dereference by nature; this is the one place
// a plugin does not have to write that itself.
#![allow(unsafe_code)]

use core::ffi::c_void;

use dayz_plugin_api::{ChatMessage, GameEvent, RemoteCall};

pub use dayz_plugin_api::GameMask as Streams;
/// What the game should do with a chat line a plugin has just seen.
///
/// Named apart from [`Verdict`](crate::Verdict), which is the input stream's answer: they
/// are different ABI types and a plugin that watches both should not be able to return one
/// where the other belongs.
pub use dayz_plugin_api::GameResponse as ChatVerdict;

/// One event the engine raised.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Event<'a> {
    /// The event's class name, for example `RespawnEvent`.
    pub name: &'a str,
    /// The event object itself, for a plugin that knows this class's layout. Valid only for
    /// the length of the callback.
    pub object: *mut c_void,
}

impl Event<'_> {
    /// Whether this is that class, by name.
    ///
    /// The obvious `event.name == "RespawnEvent"` works just as well; this exists because
    /// matching on a name is what nearly every use of an event starts with, and reads better
    /// in an `if`.
    #[must_use]
    pub fn is(&self, class: &str) -> bool {
        self.name == class
    }

    /// Borrow the loader's structure.
    ///
    /// # Safety
    /// `raw` must be the pointer the loader passed to `on_game_event`, valid for `'a`.
    pub(crate) unsafe fn from_abi<'a>(raw: *const GameEvent) -> Option<Event<'a>> {
        if raw.is_null() {
            return None;
        }
        // SAFETY: the ABI requires `raw` to point at one readable structure for the duration
        // of the call.
        let event = unsafe { &*raw };
        Some(Event {
            name: crate::host::str_ref(event.name),
            object: event.event,
        })
    }
}

/// One chat line, before the game draws it.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Chat<'a> {
    /// Channel the line is on, as the engine numbers them.
    pub channel: u32,
    /// Who said it, empty for a line the game itself produced.
    pub from: &'a str,
    /// What was said.
    pub text: &'a str,
    /// Name of the colour class the game would draw it in.
    pub colour: &'a str,
}

impl Chat<'_> {
    /// Borrow the loader's structure.
    ///
    /// # Safety
    /// `raw` must be the pointer the loader passed to `on_chat`, valid for `'a`.
    pub(crate) unsafe fn from_abi<'a>(raw: *const ChatMessage) -> Option<Chat<'a>> {
        if raw.is_null() {
            return None;
        }
        // SAFETY: the ABI requires `raw` to point at one readable structure for the duration
        // of the call.
        let message = unsafe { &*raw };
        Some(Chat {
            channel: message.channel,
            from: crate::host::str_ref(message.from),
            text: crate::host::str_ref(message.text),
            colour: crate::host::str_ref(message.colour),
        })
    }
}

/// One remote call on its way to script.
///
/// The parameters are not decoded: `params` is the engine's own serialised stream and its
/// shape belongs to whichever mod sent the call.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Rpc {
    /// The mod's own call number, script's `rpc_type`.
    pub kind: u32,
    /// Identity the call came from, script's `PlayerIdentity`. Null when the server sent it.
    pub sender: *mut c_void,
    /// Object the call is addressed to, script's `Object`. Null for a global call.
    pub target: *mut c_void,
    /// Serialised parameters, script's `ParamsReadContext`.
    pub params: *mut c_void,
}

impl Rpc {
    /// Borrow the loader's structure.
    ///
    /// # Safety
    /// `raw` must be the pointer the loader passed to `on_rpc`.
    pub(crate) unsafe fn from_abi(raw: *const RemoteCall) -> Option<Rpc> {
        if raw.is_null() {
            return None;
        }
        // SAFETY: the ABI requires `raw` to point at one readable structure for the duration
        // of the call.
        let call = unsafe { &*raw };
        Some(Rpc {
            kind: call.kind,
            sender: call.sender,
            target: call.target,
            params: call.params,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_matches_its_own_class_name() {
        let event = Event {
            name: "RespawnEvent",
            object: core::ptr::null_mut(),
        };
        assert!(event.is("RespawnEvent"));
        assert!(!event.is("PlayerDeathEvent"));
        assert!(!event.is("Respawn"));
    }

    #[test]
    fn a_null_structure_is_refused_rather_than_read() {
        // SAFETY: a null pointer is exactly what these are documented to reject.
        assert!(unsafe { Event::from_abi(core::ptr::null()) }.is_none());
        // SAFETY: as above.
        assert!(unsafe { Chat::from_abi(core::ptr::null()) }.is_none());
        // SAFETY: as above.
        assert!(unsafe { Rpc::from_abi(core::ptr::null()) }.is_none());
    }
}
