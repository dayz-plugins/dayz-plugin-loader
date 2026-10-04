//! The game's own events, chat lines and remote calls, offered to plugins.
//!
//! Enfusion raises everything that happens to a session as an event object and broadcasts it
//! through one manager. Three hooks are enough to see all of it:
//!
//! - **`event.raise`** is the broadcaster. Every event in the process passes through it, so
//!   one detour there reports `ConnectingStartEvent`, `MPConnectionCloseEvent`,
//!   `RespawnEvent`, `PlayerDeathEvent` and seventy more without knowing where any of them
//!   live.
//! - **`chat.raise_message`** is one level below the chat event, where the engine still has
//!   the channel and the three strings as separate arguments. Catching it there means no
//!   struct layout has to be guessed, and not calling the original swallows the line.
//! - **`rpc.dispatch_to_script`** is a mod's remote call on its way to script's `OnRPC`.
//!
//! ## Naming an event without calling into the game
//!
//! Every event class carries a name getter in slot 2 of its virtual table, and the compiler
//! emits all of them as the same fourteen bytes: `lea rax, [rip+disp]; mov [rdx], rax; mov
//! rax, rdx; ret`. So instead of calling an unknown function pointer from inside a hook, the
//! loader reads those bytes, checks they are that exact shape, and decodes the displacement
//! to find the literal. Nothing in the game is executed, and the shape check is the
//! validation: of the 168 classes in this build whose name contains "Event", the 76 that are
//! really broadcaster events all match it and the rest — handlers, functors, AI slots — do
//! not, so an object that is not an event is reported by address rather than guessed at.
//!
//! The answer is cached per virtual table, which makes every event after the first of its
//! class a hash lookup.
//!
//! ## Why the hooks go in during initialisation
//!
//! `event.raise` is called constantly, and writing five bytes over the entry of a function
//! another thread may be executing is the one risk [`super::detour`] cannot remove. The
//! loader therefore patches during its own initialisation, on the game's first DXGI call,
//! while the engine is still building its renderer and before any session exists — not when
//! a plugin subscribes, which can happen in the middle of a frame. Subscribing afterwards
//! only flips a mask, and when nobody is subscribed each hook is one atomic load.
//!
//! ## Naming is not reading
//!
//! The name comes from the event. Its *contents* cannot: there is no generic marshaller in
//! the engine, only a per-class dispatcher, so every field offset is something somebody
//! establishes per class and per build. Those live in the symbol database and
//! [`super::event_fields`] does the reading; an event whose class is not in there still
//! arrives, named, with no fields.
//!
//! ## What swallowing a chat line costs
//!
//! `chat.raise_message` takes ownership of its three string holders — it moves them out of
//! the caller's pointers and releases them itself. Swallowing means not calling it, so those
//! holders are never released: a swallowed line leaks its own text, about `0x18` bytes plus
//! its length. That is deliberate. Releasing them here would mean reimplementing the
//! engine's reference counting against a pointer whose other owners are unknown, and getting
//! that wrong is a double free in the middle of a frame. A few dozen bytes per swallowed line
//! is the cheaper mistake, and this says so rather than hiding it.

// FFI module: detours the game's functions and reads its objects.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use dayz_data::SymbolTable;
use dayz_plugin_api::{
    ChatMessage, GameEvent, GameMask, GameResponse, PluginHandle, RemoteCall, Status, Str,
};

use super::gamemem::{self, Layout};
use super::{dispatch, event_fields};

/// `void raise(EventManager *this, Event *event)`.
type RaiseFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
/// `void raise_message(CGame *this, int channel, String **from, String **text, String **colour)`.
///
/// The three strings are moved out of the caller's pointers rather than borrowed; see the
/// module documentation for what that means for swallowing. The engine's own return value is
/// a leftover register, not a result.
type ChatFn = unsafe extern "C" fn(
    *mut c_void,
    u32,
    *mut *mut c_void,
    *mut *mut c_void,
    *mut *mut c_void,
) -> *mut c_void;
/// `void dispatch_to_script(CGame *this, Identity *sender, Object *target, int kind, Params *params)`.
type RpcFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, u32, *mut c_void);

/// The bytes an event's name getter starts with: `lea rax, [rip+disp32]`.
const NAME_HEAD: [u8; 3] = [0x48, 0x8D, 0x05];
/// The bytes it ends with: `mov [rdx], rax; mov rax, rdx; ret`.
const NAME_TAIL: [u8; 7] = [0x48, 0x89, 0x02, 0x48, 0x8B, 0xC2, 0xC3];
/// Length of the whole getter, and where the displacement is counted from.
const NAME_LEN: usize = NAME_HEAD.len() + 4 + NAME_TAIL.len();
/// Who is listening to what.
static SUBSCRIBERS: Mutex<Vec<(PluginHandle, GameMask)>> = Mutex::new(Vec::new());
/// Union of every subscriber's mask, so a stream nobody wants costs one load.
static COMBINED: AtomicU32 = AtomicU32::new(0);
/// Trampolines back into the game, zero until the hook is installed.
static NEXT_RAISE: AtomicUsize = AtomicUsize::new(0);
static NEXT_CHAT: AtomicUsize = AtomicUsize::new(0);
static NEXT_RPC: AtomicUsize = AtomicUsize::new(0);
/// How much has come through each stream, for the console.
static SEEN_EVENTS: AtomicU64 = AtomicU64::new(0);
static SEEN_CHAT: AtomicU64 = AtomicU64::new(0);
static SEEN_RPC: AtomicU64 = AtomicU64::new(0);
/// How many chat lines plugins have kept off the screen.
static SWALLOWED: AtomicU64 = AtomicU64::new(0);
/// Class name per event virtual table. `None` is a table whose slot 2 is not a name getter,
/// remembered so it is not decoded again.
static NAMES: Mutex<Option<HashMap<usize, Option<Box<str>>>>> = Mutex::new(None);
/// The `CGame` instance, caught the first time the engine dispatches a remote call.
///
/// Not a guess and not a scan: `rpc.dispatch_to_script` is a `CGame` member function, so its
/// first argument *is* the instance, passed by the engine itself. That is the strongest
/// provenance a pointer can have, and it is what the session natives have been waiting for.
///
/// The cost is that it only arrives once the game has dispatched a remote call, which means
/// after joining a server at least once this session. `disconnect` is wanted in a session
/// anyway; `connect` from a cold main menu still has nothing, and says so.
static GAME: AtomicUsize = AtomicUsize::new(0);
/// The offsets the hooks read, resolved once.
static LAYOUT: OnceLock<Layout> = OnceLock::new();

fn subscribers() -> std::sync::MutexGuard<'static, Vec<(PluginHandle, GameMask)>> {
    SUBSCRIBERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Subscribe a plugin to a set of streams. [`GameMask::NONE`] unsubscribes.
pub(crate) fn listen(plugin: PluginHandle, mask: GameMask) -> Status {
    if mask != GameMask::NONE && LAYOUT.get().is_none() {
        return Status::Unsupported;
    }
    let mut guard = subscribers();
    guard.retain(|(handle, _)| *handle != plugin);
    if mask != GameMask::NONE {
        guard.push((plugin, mask));
    }
    recombine(&guard);
    Status::Ok
}

/// Drop a plugin's subscription, when it stops or faults.
pub(crate) fn forget(plugin: PluginHandle) {
    let mut guard = subscribers();
    let before = guard.len();
    guard.retain(|(handle, _)| *handle != plugin);
    if guard.len() != before {
        recombine(&guard);
    }
}

fn recombine(subscribers: &[(PluginHandle, GameMask)]) {
    let combined = subscribers.iter().fold(0, |acc, (_, mask)| acc | mask.0);
    COMBINED.store(combined, Ordering::Release);
}

/// Whether anyone is listening to this stream. Every hook asks first.
fn wants(stream: GameMask) -> bool {
    GameMask(COMBINED.load(Ordering::Acquire)).covers(stream)
}

/// Which streams are hooked, named, for the console and the log.
fn installed() -> Vec<&'static str> {
    [
        (&NEXT_RAISE, "events"),
        (&NEXT_CHAT, "chat"),
        (&NEXT_RPC, "rpc"),
    ]
    .into_iter()
    .filter(|(slot, _)| slot.load(Ordering::Acquire) != 0)
    .map(|(_, name)| name)
    .collect()
}

/// Install the three detours. Called once, from the loader's initialisation.
pub(crate) fn install(table: &SymbolTable, module_base: usize) {
    let layout = match Layout::resolve(table) {
        Ok(layout) => layout,
        Err(missing) => {
            log::info!("game events unavailable: {missing} is not in the database");
            return;
        }
    };
    let mut any = false;
    for (symbol, replacement, slot, what) in [
        (
            "event.raise",
            raise_hook as *const () as usize,
            &NEXT_RAISE,
            "the game's events",
        ),
        (
            "chat.raise_message",
            chat_hook as *const () as usize,
            &NEXT_CHAT,
            "chat lines",
        ),
        (
            "rpc.dispatch_to_script",
            rpc_hook as *const () as usize,
            &NEXT_RPC,
            "remote calls",
        ),
    ] {
        any |= hook_one(table, module_base, symbol, replacement, slot, what);
    }
    if any {
        let _ = LAYOUT.set(layout);
        log::info!("game events hooked: {}", installed().join(", "));
    }
}

/// Detour one symbol, storing the trampoline. Returns whether it went in.
fn hook_one(
    table: &SymbolTable,
    module_base: usize,
    symbol: &str,
    replacement: usize,
    slot: &AtomicUsize,
    what: &str,
) -> bool {
    let Some(resolved) = table.symbol(symbol) else {
        log::info!("not watching {what}: {symbol} is not in the database for this build");
        return false;
    };
    let Ok(rva) = usize::try_from(resolved.rva) else {
        return false;
    };
    let Some(target) = module_base.checked_add(rva) else {
        return false;
    };
    // SAFETY: the symbol resolved inside the mapped image and the database records it as a
    // function, so `target` is the entry point of one. Each replacement above is declared
    // with that function's signature, which is what the rest of this module exists to get
    // right; the detour itself refuses a prologue it cannot relocate.
    let installed = unsafe {
        super::detour::install(target, replacement, |at, bytes| {
            super::plugin_hooks::write_code(at, bytes)
        })
    };
    match installed {
        Ok(detour) => {
            slot.store(detour.trampoline as usize, Ordering::Release);
            // The detour is deliberately leaked: these hooks live as long as the process,
            // and a thread could be inside the trampoline whenever it were removed.
            core::mem::forget(detour);
            true
        }
        Err(why) => {
            log::warn!("not watching {what}: {symbol} at {target:#X} could not be hooked: {why}");
            false
        }
    }
}

/// Decode an event's class name out of its virtual table, executing nothing.
///
/// Returns `None` when slot 2 is not the fourteen byte name getter every event class has,
/// which is how an object that is not an event is rejected rather than misread.
fn decode_name(vtable: usize, layout: &Layout) -> Option<Box<str>> {
    let getter = gamemem::read_pointer(vtable.checked_add(layout.vtable_name)?)?;
    if !gamemem::readable(getter, NAME_LEN) {
        return None;
    }
    // SAFETY: `NAME_LEN` bytes from `getter` are committed and readable, checked above.
    let code = unsafe { core::slice::from_raw_parts(getter as *const u8, NAME_LEN) };
    if code[..NAME_HEAD.len()] != NAME_HEAD || code[NAME_HEAD.len() + 4..] != NAME_TAIL {
        return None;
    }
    let displacement =
        i32::from_ne_bytes(code[NAME_HEAD.len()..NAME_HEAD.len() + 4].try_into().ok()?);
    // A `lea` counts its displacement from the end of the instruction, which is seven bytes in.
    let literal = getter
        .checked_add(NAME_HEAD.len() + 4)?
        .checked_add_signed(displacement as isize)?;
    gamemem::read_c_string(literal)
}

/// The class name of one event, decoding it the first time its class is seen.
fn name_of(event: usize, layout: &Layout) -> Option<Box<str>> {
    let vtable = gamemem::read_pointer(event)?;
    let mut guard = NAMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(known) = cache.get(&vtable) {
        return known.clone();
    }
    let name = decode_name(vtable, layout);
    if name.is_none() {
        log::debug!("an object raised as an event has no name getter at vtable {vtable:#X}");
    }
    cache.insert(vtable, name.clone());
    name
}

/// Hand one event to every plugin that asked for events, contents and all.
///
/// The fields are decoded once and shown to every subscriber, which is both cheaper and more
/// honest than decoding per plugin: each of them is looking at the same object, so they had
/// better see the same values.
fn report_event(event: *mut c_void, layout: &Layout) {
    SEEN_EVENTS.fetch_add(1, Ordering::Relaxed);
    let Some(name) = name_of(event as usize, layout) else {
        return;
    };
    let decoded = event_fields::decode(event as usize, &name, layout);
    let fields: Vec<_> = decoded.iter().map(event_fields::Decoded::as_abi).collect();
    let described = GameEvent {
        struct_size: core::mem::size_of::<GameEvent>(),
        name: Str::new(&name),
        event,
        fields: fields.as_ptr(),
        field_count: fields.len(),
    };
    for handle in listeners(GameMask::EVENTS) {
        dispatch::game_event(handle, &described);
    }
}

/// Offer one chat line around, and report whether anybody swallowed it.
fn offer_chat(
    channel: u32,
    from: *mut *mut c_void,
    text: *mut *mut c_void,
    colour: *mut *mut c_void,
    layout: &Layout,
) -> GameResponse {
    SEEN_CHAT.fetch_add(1, Ordering::Relaxed);
    let from = gamemem::engine_string(from, layout);
    let text = gamemem::engine_string(text, layout);
    let colour = gamemem::engine_string(colour, layout);
    let message = ChatMessage {
        struct_size: core::mem::size_of::<ChatMessage>(),
        channel,
        from: Str::new(&from),
        text: Str::new(&text),
        colour: Str::new(&colour),
    };
    for handle in listeners(GameMask::CHAT) {
        if dispatch::chat(handle, &message) == GameResponse::SWALLOW {
            let swallowed = SWALLOWED.fetch_add(1, Ordering::Relaxed) + 1;
            log::debug!("a plugin swallowed a chat line on channel {channel} ({swallowed} so far)");
            return GameResponse::SWALLOW;
        }
    }
    GameResponse::PASS
}

/// Hand one remote call to every plugin that asked for them.
fn report_rpc(sender: *mut c_void, target: *mut c_void, kind: u32, params: *mut c_void) {
    SEEN_RPC.fetch_add(1, Ordering::Relaxed);
    let call = RemoteCall {
        struct_size: core::mem::size_of::<RemoteCall>(),
        sender,
        target,
        kind,
        params,
    };
    for handle in listeners(GameMask::RPC) {
        dispatch::rpc(handle, &call);
    }
}

/// Handles subscribed to one stream, collected so the lock is not held across a plugin call.
fn listeners(stream: GameMask) -> Vec<PluginHandle> {
    subscribers()
        .iter()
        .filter(|(_, mask)| mask.covers(stream))
        .map(|(handle, _)| *handle)
        .collect()
}

/// Every event the engine raises.
unsafe extern "C" fn raise_hook(manager: *mut c_void, event: *mut c_void) {
    if let (true, Some(layout)) = (wants(GameMask::EVENTS), LAYOUT.get()) {
        report_event(event, layout);
    }
    let next = NEXT_RAISE.load(Ordering::Acquire);
    if next != 0 {
        // SAFETY: the trampoline was built from this function's own entry and reaches the
        // rest of it, so it has exactly this signature.
        let next: RaiseFn = unsafe { core::mem::transmute(next) };
        // SAFETY: both arguments are passed through untouched.
        unsafe { next(manager, event) };
    }
}

/// Every chat line, before the game draws it.
unsafe extern "C" fn chat_hook(
    game: *mut c_void,
    channel: u32,
    from: *mut *mut c_void,
    text: *mut *mut c_void,
    colour: *mut *mut c_void,
) -> *mut c_void {
    if let (true, Some(layout)) = (wants(GameMask::CHAT), LAYOUT.get()) {
        if offer_chat(channel, from, text, colour, layout) == GameResponse::SWALLOW {
            // The engine's return value is a leftover register rather than a result, so
            // there is nothing to invent here. The three holders stay with the caller; see
            // the module documentation for why they are not released.
            return core::ptr::null_mut();
        }
    }
    let next = NEXT_CHAT.load(Ordering::Acquire);
    if next == 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: the trampoline reaches the rest of the hooked function and has its signature.
    let next: ChatFn = unsafe { core::mem::transmute(next) };
    // SAFETY: every argument is passed through untouched, including the three holder
    // pointers the engine moves out of.
    unsafe { next(game, channel, from, text, colour) }
}

/// Every remote call on its way to script.
unsafe extern "C" fn rpc_hook(
    game: *mut c_void,
    sender: *mut c_void,
    target: *mut c_void,
    kind: u32,
    params: *mut c_void,
) {
    // The engine passed its own `this`, so this is the `CGame` instance rather than a
    // candidate for one. Stored unconditionally — it costs one relaxed store and it is what
    // the session natives and the chat sink need.
    remember_game(game);
    if wants(GameMask::RPC) {
        report_rpc(sender, target, kind, params);
    }
    let next = NEXT_RPC.load(Ordering::Acquire);
    if next != 0 {
        // SAFETY: the trampoline reaches the rest of the hooked function and has its signature.
        let next: RpcFn = unsafe { core::mem::transmute(next) };
        // SAFETY: every argument is passed through untouched.
        unsafe { next(game, sender, target, kind, params) };
    }
}

/// Record the `CGame` instance the engine just handed one of the hooks.
///
/// Logged the first time, and again if it ever changes: the game object is built once per
/// process as far as anything here knows, so a second value would mean that assumption is
/// wrong and the session natives are aimed at a stale object.
fn remember_game(game: *mut c_void) {
    let value = game as usize;
    if value == 0 {
        return;
    }
    let previous = GAME.swap(value, Ordering::Release);
    if previous == 0 {
        log::info!("the game object is at {value:#X}, caught from a remote call");
    } else if previous != value {
        log::warn!("the game object moved from {previous:#X} to {value:#X}");
    }
}

/// The `CGame` instance, or `None` before the engine has handed one over.
pub(crate) fn game_instance() -> Option<*mut c_void> {
    let value = GAME.load(Ordering::Acquire);
    (value != 0).then_some(value as *mut c_void)
}

/// Lines for the console's `game` command: what is hooked, who listens, how much has passed.
pub(crate) fn summary() -> Vec<String> {
    let mut lines = Vec::new();
    let hooked = installed();
    if hooked.is_empty() {
        lines.push("the game's own streams are not hooked on this build".to_owned());
        return lines;
    }
    lines.push(format!("hooked: {}", hooked.join(", ")));
    lines.push(format!(
        "seen: {} events, {} chat lines ({} swallowed), {} remote calls",
        SEEN_EVENTS.load(Ordering::Relaxed),
        SEEN_CHAT.load(Ordering::Relaxed),
        SWALLOWED.load(Ordering::Relaxed),
        SEEN_RPC.load(Ordering::Relaxed),
    ));
    for (handle, mask) in subscribers().iter() {
        let name = super::plugins::find(*handle).map_or_else(
            || format!("plugin {}", handle.0),
            |plugin| plugin.name.clone(),
        );
        lines.push(format!("{name} listens to {}", describe(*mask)));
    }
    if let Some(cache) = NAMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        let named = cache.values().filter(|name| name.is_some()).count();
        lines.push(format!(
            "{named} event classes named, {} rejected",
            cache.len() - named
        ));
    }
    lines
}

/// A mask as a list of stream names.
fn describe(mask: GameMask) -> String {
    const STREAMS: [(GameMask, &str); 3] = [
        (GameMask::EVENTS, "events"),
        (GameMask::CHAT, "chat"),
        (GameMask::RPC, "rpc"),
    ];
    let names: Vec<&str> = STREAMS
        .into_iter()
        .filter(|(stream, _)| mask.covers(*stream))
        .map(|(_, name)| name)
        .collect();
    if names.is_empty() {
        "nothing".to_owned()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real bytes of `ChatMessageEvent`'s name getter in 1.29.163709, which is the shape
    /// the decoder is built around:
    /// `lea rax, [rip+0x5CBE79]; mov [rdx], rax; mov rax, rdx; ret`.
    const REAL_GETTER: [u8; NAME_LEN] = [
        0x48, 0x8D, 0x05, 0x79, 0xBE, 0x5C, 0x00, 0x48, 0x89, 0x02, 0x48, 0x8B, 0xC2, 0xC3,
    ];

    #[test]
    fn the_name_shape_matches_the_real_getter() {
        assert_eq!(REAL_GETTER[..NAME_HEAD.len()], NAME_HEAD);
        assert_eq!(REAL_GETTER[NAME_HEAD.len() + 4..], NAME_TAIL);
    }

    #[test]
    fn a_getter_that_is_not_one_is_rejected_by_its_bytes() {
        // `mov al, 1; ret`, which is what the one non-event class with an Event name has in
        // slot 2. Nothing about it is a name.
        let predicate = [0xB0u8, 0x01, 0xC3];
        assert_ne!(predicate[..], NAME_HEAD[..]);
        // And a normal function prologue, which is what a handler has there.
        let prologue = [0x40u8, 0x53, 0x48, 0x83, 0xEC, 0x20];
        assert_ne!(prologue[..NAME_HEAD.len()], NAME_HEAD);
    }

    #[test]
    fn the_displacement_is_counted_from_the_end_of_the_lea() {
        let displacement = i32::from_ne_bytes([
            REAL_GETTER[3],
            REAL_GETTER[4],
            REAL_GETTER[5],
            REAL_GETTER[6],
        ]);
        assert_eq!(displacement, 0x005C_BE79);
        // The getter sits at RVA 0x71B5A0 in this build and the literal at 0xCE7420.
        let getter = 0x0071_B5A0usize;
        let Ok(offset) = usize::try_from(displacement) else {
            panic!("the real getter's displacement is positive");
        };
        let literal = getter + NAME_HEAD.len() + 4 + offset;
        assert_eq!(literal, 0x00CE_7420);
    }

    #[test]
    fn a_mask_is_described_by_the_streams_in_it() {
        assert_eq!(describe(GameMask::NONE), "nothing");
        assert_eq!(describe(GameMask::CHAT), "chat");
        assert_eq!(describe(GameMask::ALL), "events, chat, rpc");
        assert_eq!(describe(GameMask::EVENTS | GameMask::RPC), "events, rpc");
    }
}
