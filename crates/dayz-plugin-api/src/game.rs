//! The game's own events, chat and remote calls, offered to plugins.
//!
//! Enfusion raises everything that happens to a session as an event object: connecting,
//! being kicked, the world finishing its load, respawning, dying, a chat line arriving. They
//! all pass through one broadcaster, and the loader hooks it, so a plugin hears about them
//! without knowing where any of them live.
//!
//! Three streams, because the engine keeps them in three places:
//!
//! - [`GameEvent`] is every event the broadcaster carries, named by its own class, from
//!   `ConnectingStartEvent` through `MPConnectionCloseEvent` to `RespawnEvent`.
//! - [`ChatMessage`] is caught one level lower, where the engine builds its chat event out of
//!   a channel and three strings, so the fields arrive already separated — and where
//!   returning [`GameResponse::SWALLOW`] keeps the line off the screen.
//! - [`RemoteCall`] is a mod's own RPC, on its way to script's `OnRPC`.
//!
//! What a plugin gets for free is the *name* of an event, and for the classes the symbol
//! database has a layout for, its [`fields`](GameEvent::fields) already read and rendered.
//! An event the database says nothing about still arrives, named, with no fields: the name
//! comes from the event itself and is right for classes this loader has never heard of,
//! where a layout is something somebody had to establish. [`GameEvent::event`] is the object,
//! for a plugin that knows better than the database does.
//!
//! [`HostApi::game_catalogue`](crate::HostApi) is the same knowledge up front, before
//! anything has been raised — which is what a settings panel with a row per event needs.

use core::ffi::c_void;

use crate::types::Str;

/// How one of an event's fields is stored, and what [`GameField::address`] points at.
///
/// A `#[repr(transparent)]` integer rather than an enum because a plugin built against a
/// newer ABI must be able to receive a kind this one has no name for: that reads as
/// [`FieldKind::UNKNOWN`], whose rendered text is still right.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldKind(pub u32);

impl FieldKind {
    /// A kind this ABI version has no name for. Only [`GameField::text`] is meaningful.
    pub const UNKNOWN: FieldKind = FieldKind(0);
    /// Four-byte signed integer.
    pub const INT: FieldKind = FieldKind(1);
    /// Four-byte float.
    pub const FLOAT: FieldKind = FieldKind(2);
    /// Four-byte integer holding 0 or 1.
    pub const BOOL: FieldKind = FieldKind(3);
    /// Pointer to an engine string holder. Already decoded into [`GameField::text`].
    pub const STRING: FieldKind = FieldKind(4);
    /// Three consecutive floats.
    pub const VECTOR: FieldKind = FieldKind(5);
    /// Pointer to some other object, reported but not followed.
    pub const OBJECT: FieldKind = FieldKind(6);
}

/// One field of one event, read out of the event object and rendered.
///
/// [`text`](GameField::text) is the point of this structure and is always set: it is the
/// value, formatted, and a plugin that logs events needs nothing else. The other two are for
/// a plugin that wants the value rather than a rendering of it — [`kind`](GameField::kind)
/// says how it is stored and [`address`](GameField::address) is where, inside the event
/// object, so reading it typed is a cast and a load.
///
/// `address` is the field's own address rather than its value because that is the one answer
/// with the same meaning for every kind: a `u64` cannot hold three floats, and a field that
/// reported its value for some kinds and its location for others would be a trap.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GameField {
    /// `size_of::<GameField>()`.
    pub struct_size: usize,
    /// The field's name, as the engine's own script parameters name it.
    pub name: Str,
    /// How it is stored.
    pub kind: FieldKind,
    /// The value, formatted. Valid for the duration of the call.
    pub text: Str,
    /// Where the field is, inside the event object. Readable for the duration of the call.
    pub address: u64,
}

/// What a plugin is told about one event the engine raised.
///
/// `name` is the event's own class name, read out of the event through its virtual table, so
/// it is right for classes this loader has never heard of. `event` is the object itself,
/// valid only for the duration of the call, and so is everything `fields` points at.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GameEvent {
    /// `size_of::<GameEvent>()`.
    pub struct_size: usize,
    /// Class name, for example `ChatMessageEvent` or `MPConnectionCloseEvent`.
    pub name: Str,
    /// The `enf::Event` object.
    pub event: *mut c_void,
    /// The decoded fields, in the order the engine's own script parameters list them.
    ///
    /// Null with a count of zero for a class the symbol database has no layout for, which is
    /// most of them. That is "nobody has established this one yet", not "it has no fields".
    pub fields: *const GameField,
    /// How many [`GameEvent::fields`] there are.
    pub field_count: usize,
}

/// One event class the loader knows the shape of, whether or not it has been raised.
///
/// Returned in a block by [`HostApi::game_catalogue`](crate::HostApi). The strings belong to
/// the loader and last as long as the symbol database does, which is the life of the process,
/// so unlike the fields on a live [`GameEvent`] these may be kept.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GameClass {
    /// `size_of::<GameClass>()`.
    pub struct_size: usize,
    /// The class name, which is the key: it is what [`GameEvent::name`] will be.
    pub name: Str,
    /// What the event means, or empty.
    pub note: Str,
    /// How many fields the loader can decode for it. Zero for a class that is known by name
    /// only.
    pub field_count: usize,
}

/// One chat line, as the engine was about to show it.
///
/// Caught where the engine assembles its chat event, so the parts are still separate. Empty
/// strings are empty rather than absent: the engine represents an empty string as no string
/// at all, and that arrives here as `""`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ChatMessage {
    /// `size_of::<ChatMessage>()`.
    pub struct_size: usize,
    /// Channel the line is on, as the engine numbers them.
    pub channel: u32,
    /// Who said it. Empty for a line the game itself produced.
    pub from: Str,
    /// What was said.
    pub text: Str,
    /// Name of the colour class the game would draw it in.
    pub colour: Str,
}

/// One remote call on its way to script.
///
/// The four values are what the engine hands `OnRPC`, in that order. The loader passes them
/// through without interpreting them: `params` is a serialised parameter stream whose shape
/// is up to whichever mod sent it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RemoteCall {
    /// `size_of::<RemoteCall>()`.
    pub struct_size: usize,
    /// Identity the call came from, script's `PlayerIdentity`. Null when the server sent it.
    pub sender: *mut c_void,
    /// Object the call is addressed to, script's `Object`. Null for a global call.
    pub target: *mut c_void,
    /// The mod's own call number, script's `rpc_type`.
    pub kind: u32,
    /// Serialised parameters, script's `ParamsReadContext`.
    pub params: *mut c_void,
}

/// Whether the game should still see what a plugin was just shown.
///
/// A `#[repr(transparent)]` integer rather than an enum for the same reason
/// [`InputResponse`](crate::InputResponse) is one: a value this loader does not know reads as
/// [`GameResponse::PASS`], so a plugin built against a newer ABI cannot accidentally
/// swallow the game's own traffic.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameResponse(pub u32);

impl GameResponse {
    /// Let it through. The default, and what an unknown value is read as.
    pub const PASS: GameResponse = GameResponse(0);
    /// Swallow it: the engine never finishes what it was doing.
    ///
    /// The first subscriber to swallow ends delivery, so later subscribers do not see it
    /// either. Only [`ChatMessage`] can be swallowed; the other two streams report what
    /// happened and the answer is ignored.
    pub const SWALLOW: GameResponse = GameResponse(1);
}

/// Bit flags for [`HostApi::game_listen`](crate::HostApi), one per stream.
///
/// A plugin hears only what it asked for. These hooks sit on the engine's own call paths —
/// chat on every line drawn, events on every event raised — so a plugin that wants chat is
/// not woken for a hundred connectivity updates a minute.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameMask(pub u32);

impl GameMask {
    /// Subscribe to nothing, which is how a plugin unsubscribes.
    pub const NONE: GameMask = GameMask(0);
    /// [`GameEvent`], every event the broadcaster carries.
    pub const EVENTS: GameMask = GameMask(1);
    /// [`ChatMessage`], with the chance to swallow.
    pub const CHAT: GameMask = GameMask(2);
    /// [`RemoteCall`], a mod's RPC on its way to script.
    pub const RPC: GameMask = GameMask(4);
    /// Every stream this version knows.
    pub const ALL: GameMask = GameMask(7);

    /// Whether this mask includes that stream.
    #[must_use]
    pub const fn covers(self, other: GameMask) -> bool {
        self.0 & other.0 != 0
    }
}

impl core::ops::BitOr for GameMask {
    type Output = GameMask;

    fn bitor(self, other: GameMask) -> GameMask {
        GameMask(self.0 | other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_covers_every_named_stream() {
        for one in [GameMask::EVENTS, GameMask::CHAT, GameMask::RPC] {
            assert!(GameMask::ALL.covers(one), "{one:?}");
            assert!(!GameMask::NONE.covers(one), "{one:?}");
        }
    }

    #[test]
    fn an_unnamed_field_kind_is_distinguishable_from_every_named_one() {
        let named = [
            FieldKind::INT,
            FieldKind::FLOAT,
            FieldKind::BOOL,
            FieldKind::STRING,
            FieldKind::VECTOR,
            FieldKind::OBJECT,
        ];
        for kind in named {
            assert_ne!(kind, FieldKind::UNKNOWN, "{kind:?}");
        }
        assert_eq!(
            FieldKind(u32::MAX),
            FieldKind(u32::MAX),
            "a kind from a newer ABI is carried, not rejected"
        );
    }

    #[test]
    fn a_mask_covers_what_was_put_in_it() {
        let mask = GameMask::CHAT | GameMask::RPC;
        assert!(mask.covers(GameMask::CHAT));
        assert!(mask.covers(GameMask::RPC));
        assert!(!mask.covers(GameMask::EVENTS));
    }
}
