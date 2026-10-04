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
//! ## What an event tells you
//!
//! [`Event::name`] is the event's own class name, read out of the engine, so it is right for
//! classes this loader has never heard of — `ConnectingStartEvent`, `MPConnectionCloseEvent`,
//! `ClientPrepareEvent`, `RespawnEvent`, `PlayerDeathEvent` and so on. Knowing *that* an
//! event happened is always reliable.
//!
//! [`Event::fields`] is its contents, already read and rendered, for the classes whose layout
//! is in the loader's symbol database. The two differ because they come from different places:
//! the name is in the event, a layout is something somebody established per class and per
//! build. So an event with no fields means nobody has done that for this class yet, and
//! [`Event::object`] is still there for a plugin that knows better.
//!
//! [`Host::game_catalogue`](crate::Host::game_catalogue) is the same list up front, before
//! anything has happened — which is what a settings screen with a row per event needs.

// Reading the loader's structures is a pointer dereference by nature; this is the one place
// a plugin does not have to write that itself.
#![allow(unsafe_code)]

use core::ffi::c_void;

use dayz_plugin_api::{ChatMessage, GameEvent, RemoteCall};

/// How one of an event's fields is stored.
pub use dayz_plugin_api::FieldKind;
pub use dayz_plugin_api::GameMask as Streams;
/// What the game should do with a chat line a plugin has just seen.
///
/// Named apart from [`Verdict`](crate::Verdict), which is the input stream's answer: they
/// are different ABI types and a plugin that watches both should not be able to return one
/// where the other belongs.
pub use dayz_plugin_api::GameResponse as ChatVerdict;

/// One field of one event, read out of the event and rendered.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Field<'a> {
    /// The field's name, as the engine's own script parameters name it.
    pub name: &'a str,
    /// The value, formatted. This is the point of a field and is always set.
    pub text: &'a str,
    /// How it is stored, for a plugin that wants the value rather than a rendering of it.
    pub kind: FieldKind,
    /// Where the field is, inside the event object. Readable for the length of the callback.
    pub address: u64,
}

/// One event class the loader knows the shape of, whether or not it has been raised.
///
/// From [`Host::game_catalogue`](crate::Host::game_catalogue). The strings belong to the
/// loader's symbol database and last as long as the process, so unlike a live [`Event`]'s
/// fields these may be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Class {
    /// The class name, which is what [`Event::name`] will be.
    pub name: &'static str,
    /// What the event means, or empty.
    pub note: &'static str,
    /// How many fields the loader can decode for it. Zero for a class known by name only.
    pub field_count: usize,
}

/// One event the engine raised.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Event<'a> {
    /// The event's class name, for example `RespawnEvent`.
    pub name: &'a str,
    /// Its contents, in the order the engine's own script parameters list them.
    ///
    /// Empty for a class whose layout is not in the loader's symbol database, which is most
    /// of them: that is "nobody has established this one yet", not "it has no fields".
    pub fields: &'a [Field<'a>],
    /// The event object itself, for a plugin that knows this class's layout better than the
    /// database does. Valid only for the length of the callback.
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
            fields: &[],
            object: event.event,
        })
    }

    /// Read the fields out of the loader's structure into `out`, and borrow them.
    ///
    /// Separate from [`Event::from_abi`] because the borrowed slice has to live somewhere the
    /// caller owns: `on_game_event` keeps a small buffer on its own stack and fills it here,
    /// so an event with no fields costs no allocation and the common case costs one `Vec`
    /// that is dropped when the callback returns.
    ///
    /// # Safety
    /// `raw` must be the pointer the loader passed to `on_game_event`, valid for `'a`.
    pub(crate) unsafe fn read_fields<'a>(
        raw: *const GameEvent,
        out: &'a mut Vec<Field<'a>>,
    ) -> Option<&'a [Field<'a>]> {
        if raw.is_null() {
            return None;
        }
        // SAFETY: as in `from_abi`.
        let event = unsafe { &*raw };
        if event.fields.is_null() || event.field_count == 0 {
            return Some(&[]);
        }
        // SAFETY: the ABI requires `fields` to describe `field_count` readable structures for
        // the duration of the call.
        let fields = unsafe { core::slice::from_raw_parts(event.fields, event.field_count) };
        out.extend(fields.iter().map(|field| Field {
            name: crate::host::str_ref(field.name),
            text: crate::host::str_ref(field.text),
            kind: field.kind,
            address: field.address,
        }));
        Some(out.as_slice())
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
            fields: &[],
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
        let mut buffer = Vec::new();
        // SAFETY: as above.
        assert!(unsafe { Event::read_fields(core::ptr::null(), &mut buffer) }.is_none());
    }
}
