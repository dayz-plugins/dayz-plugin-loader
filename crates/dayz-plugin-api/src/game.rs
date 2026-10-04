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
//! What a plugin gets for free is the *name* of an event and, for chat and RPC, the
//! arguments the engine passed. What it does not get is the inside of an arbitrary event
//! object: the fields past the class name differ per class, [`GameEvent::event`] is the
//! pointer to read them from, and doing so needs that class's layout. The loader does not
//! guess it.

use core::ffi::c_void;

use crate::types::Str;

/// What a plugin is told about one event the engine raised.
///
/// `name` is the event's own class name, read out of the event through its virtual table, so
/// it is right for classes this loader has never heard of. `event` is the object itself,
/// valid only for the duration of the call.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GameEvent {
    /// `size_of::<GameEvent>()`.
    pub struct_size: usize,
    /// Class name, for example `ChatMessageEvent` or `MPConnectionCloseEvent`.
    pub name: Str,
    /// The `enf::Event` object. Reading past the class name needs that class's layout.
    pub event: *mut c_void,
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
    fn a_mask_covers_what_was_put_in_it() {
        let mask = GameMask::CHAT | GameMask::RPC;
        assert!(mask.covers(GameMask::CHAT));
        assert!(mask.covers(GameMask::RPC));
        assert!(!mask.covers(GameMask::EVENTS));
    }
}
