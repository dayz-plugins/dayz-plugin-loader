//! The input stream plugins subscribe to, and the events the window procedure feeds it.
//!
//! Subscriptions live here rather than in [`crate::state::State`] on purpose: this runs inside
//! the game's message loop, for every key and every mouse delta, and must never wait on the
//! lock the render thread or the console may be holding. The common case — nobody listening —
//! is one atomic load.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use dayz_plugin_api::{
    InputEvent, InputKind, InputMask, InputModifiers, InputResponse, PluginHandle, Status,
};

use super::dispatch;

/// Who is listening to what. Small and read far more often than written.
static SUBSCRIBERS: Mutex<Vec<(PluginHandle, InputMask)>> = Mutex::new(Vec::new());
/// Union of every subscriber's mask, so a kind nobody wants costs one load.
static COMBINED: AtomicU32 = AtomicU32::new(0);
/// How many events plugins have swallowed, for the log and the `input` command.
static SWALLOWED: AtomicU64 = AtomicU64::new(0);
/// Every swallow is logged until this many, then every thousandth.
const VERBOSE_SWALLOWS: u64 = 5;
/// How often a swallow is logged after that.
const SWALLOW_INTERVAL: u64 = 1000;

/// When the loader started, for the event timestamps.
static STARTED: Mutex<Option<Instant>> = Mutex::new(None);

fn subscribers() -> std::sync::MutexGuard<'static, Vec<(PluginHandle, InputMask)>> {
    SUBSCRIBERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Milliseconds since the first event, which is close enough to loader start and needs no
/// clock of its own.
fn elapsed_ms() -> u64 {
    let mut guard = STARTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let started = guard.get_or_insert_with(Instant::now);
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Subscribe a plugin to a set of kinds. [`InputMask::NONE`] unsubscribes.
pub(crate) fn listen(plugin: PluginHandle, mask: InputMask) -> Status {
    let mut guard = subscribers();
    guard.retain(|(handle, _)| *handle != plugin);
    if mask != InputMask::NONE {
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

fn recombine(subscribers: &[(PluginHandle, InputMask)]) {
    let combined = subscribers.iter().fold(0, |acc, (_, mask)| acc | mask.0);
    COMBINED.store(combined, Ordering::Release);
}

/// Whether anyone is listening for this kind. The window procedure asks first, so an
/// unsubscribed kind costs nothing to produce.
pub(crate) fn wants(kind: InputKind) -> bool {
    InputMask(COMBINED.load(Ordering::Acquire)).covers(kind)
}

/// Lines for the console's `input` command: who listens to what, and how much has been
/// taken from the game.
pub(crate) fn summary() -> Vec<String> {
    let mut lines = console_lines();
    if !lines.is_empty() {
        lines.push(format!(
            "{} events swallowed since the game started",
            SWALLOWED.load(Ordering::Relaxed)
        ));
    }
    lines
}

/// Lines for the console, naming who listens to what.
fn console_lines() -> Vec<String> {
    let names: Vec<(PluginHandle, InputMask)> = subscribers().clone();
    names
        .into_iter()
        .map(|(handle, mask)| {
            let name = super::plugins::find(handle).map_or_else(
                || format!("plugin {}", handle.0),
                |plugin| plugin.name.clone(),
            );
            format!("{name} listens to {}", describe(mask))
        })
        .collect()
}

/// A mask as a list of kind names.
fn describe(mask: InputMask) -> String {
    const KINDS: [(InputKind, &str); 5] = [
        (InputKind::Key, "keys"),
        (InputKind::MouseButton, "mouse buttons"),
        (InputKind::MouseMove, "mouse movement"),
        (InputKind::MouseWheel, "the wheel"),
        (InputKind::Hid, "HID reports"),
    ];
    let listed: Vec<&str> = KINDS
        .iter()
        .filter(|(kind, _)| mask.covers(*kind))
        .map(|(_, name)| *name)
        .collect();
    if listed.is_empty() {
        "nothing".to_owned()
    } else {
        listed.join(", ")
    }
}

/// Offer one event to every subscriber. Returns whether the game must not see it.
///
/// Delivery stops at the first plugin that swallows, which is both cheaper and the only
/// behaviour that can be reasoned about: two plugins cannot each believe they have the event.
fn deliver(event: &InputEvent) -> bool {
    // Cloned and the lock released before any plugin is called: a plugin callback can call
    // back into the loader, including `input_listen`.
    let listeners: Vec<(PluginHandle, InputMask)> = subscribers().clone();
    for (handle, mask) in listeners {
        if !mask.covers(event.kind) {
            continue;
        }
        if dispatch::input(handle, event) == InputResponse::SWALLOW {
            let count = SWALLOWED.fetch_add(1, Ordering::Relaxed) + 1;
            if count <= VERBOSE_SWALLOWS || count % SWALLOW_INTERVAL == 0 {
                let name = super::plugins::find(handle)
                    .map_or_else(|| format!("plugin {}", handle.0), |p| p.name.clone());
                log::debug!("{name} swallowed a {:?} event ({count} so far)", event.kind);
            }
            return true;
        }
    }
    false
}

/// An event with everything zeroed but its kind, so a plugin can read any field safely.
fn blank(kind: InputKind) -> InputEvent {
    InputEvent {
        struct_size: core::mem::size_of::<InputEvent>(),
        kind,
        code: 0,
        scancode: 0,
        pressed: 1,
        repeat: 0,
        modifiers: InputModifiers::NONE,
        dx: 0.0,
        dy: 0.0,
        x: 0.0,
        y: 0.0,
        device: core::ptr::null_mut(),
        data: core::ptr::null(),
        data_len: 0,
        time_ms: elapsed_ms(),
    }
}

/// Modifier state as the ABI spells it.
pub(crate) fn modifiers(ctrl: bool, alt: bool, shift: bool) -> InputModifiers {
    let mut bits = 0;
    if ctrl {
        bits |= InputModifiers::CTRL.0;
    }
    if alt {
        bits |= InputModifiers::ALT.0;
    }
    if shift {
        bits |= InputModifiers::SHIFT.0;
    }
    InputModifiers(bits)
}

/// A key went down or came up. Returns whether to swallow it.
pub(crate) fn key(
    vk: u16,
    scancode: u16,
    pressed: bool,
    repeat: bool,
    modifiers: InputModifiers,
) -> bool {
    if !wants(InputKind::Key) {
        return false;
    }
    let event = InputEvent {
        code: u32::from(vk),
        scancode: u32::from(scancode),
        pressed: u32::from(pressed),
        repeat: u32::from(repeat),
        modifiers,
        ..blank(InputKind::Key)
    };
    deliver(&event)
}

/// A mouse button went down or came up, at a client position.
pub(crate) fn mouse_button(
    button: u32,
    pressed: bool,
    x: f32,
    y: f32,
    modifiers: InputModifiers,
) -> bool {
    if !wants(InputKind::MouseButton) {
        return false;
    }
    let event = InputEvent {
        code: button,
        pressed: u32::from(pressed),
        modifiers,
        x,
        y,
        ..blank(InputKind::MouseButton)
    };
    deliver(&event)
}

/// The mouse moved. `device` is null for a window message and the raw device otherwise.
pub(crate) fn mouse_move(dx: f32, dy: f32, x: f32, y: f32, device: *mut core::ffi::c_void) -> bool {
    if !wants(InputKind::MouseMove) {
        return false;
    }
    let event = InputEvent {
        dx,
        dy,
        x,
        y,
        device,
        ..blank(InputKind::MouseMove)
    };
    deliver(&event)
}

/// The wheel turned, in notches.
pub(crate) fn wheel(notches: f32, modifiers: InputModifiers) -> bool {
    if !wants(InputKind::MouseWheel) {
        return false;
    }
    let event = InputEvent {
        dx: notches,
        modifiers,
        ..blank(InputKind::MouseWheel)
    };
    deliver(&event)
}

/// A raw HID report. `report` is only borrowed for the call, as the ABI says.
pub(crate) fn hid(device: *mut core::ffi::c_void, report: &[u8]) -> bool {
    if !wants(InputKind::Hid) {
        return false;
    }
    let event = InputEvent {
        device,
        data: report.as_ptr(),
        data_len: report.len(),
        ..blank(InputKind::Hid)
    };
    deliver(&event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mask_is_the_union_of_its_subscribers() {
        let listeners = [
            (PluginHandle(1), InputMask::KEY),
            (PluginHandle(2), InputMask::HID),
        ];
        recombine(&listeners);
        assert!(wants(InputKind::Key));
        assert!(wants(InputKind::Hid));
        assert!(!wants(InputKind::MouseMove));
        recombine(&[]);
        assert!(!wants(InputKind::Key), "nobody left listening");
    }

    #[test]
    fn a_mask_reads_as_the_kinds_it_covers() {
        assert_eq!(describe(InputMask::NONE), "nothing");
        assert_eq!(describe(InputMask::KEY), "keys");
        assert_eq!(
            describe(InputMask(InputMask::KEY.0 | InputMask::MOUSE_WHEEL.0)),
            "keys, the wheel"
        );
    }
}
