//! The window subclass that feeds the overlay its input.
//!
//! Hotkeys read `GetAsyncKeyState` and need no window messages, but a UI does: text, the
//! wheel, press and release in order. So the loader subclasses the game's output window,
//! translates the messages egui understands, and — only while a panel is capturing — stops
//! them from reaching the game, so typing a console command does not also walk the player
//! forward.
//!
//! The pointer is tracked twice on purpose. `WM_MOUSEMOVE` gives an absolute position, which
//! is what the menus produce, and raw mouse input gives deltas, which is all there is once
//! the game has captured and recentred the cursor in play. Whichever arrives moves the same
//! virtual pointer, and the overlay draws its own cursor for it.

// FFI module: a window procedure, raw input and `SetWindowLongPtrW`.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTHEADER, RID_INPUT, RIM_TYPEMOUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, SetWindowLongPtrW, GWLP_WNDPROC, WM_CHAR, WM_INPUT, WM_KEYDOWN, WM_KEYUP,
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETFOCUS, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// The window procedure that was there before, as a raw pointer. Zero means not subclassed.
static PREVIOUS: AtomicIsize = AtomicIsize::new(0);

/// What the window procedure collected since the last frame.
struct Pending {
    events: Vec<Event>,
    modifiers: Modifiers,
    pointer: Pos2,
    focused: bool,
    screen: Vec2,
    /// The most recent key press as `(virtual key, scan code)`, for the hotkey recorder.
    last_key: Option<(u16, u16)>,
    /// Key presses since the last tick as `(scan code, virtual key)`, for scan code hotkeys.
    presses: Vec<(u16, u16)>,
}

static PENDING: Mutex<Pending> = Mutex::new(Pending {
    events: Vec::new(),
    modifiers: Modifiers::NONE,
    pointer: Pos2::ZERO,
    focused: true,
    screen: Vec2::new(1920.0, 1080.0),
    last_key: None,
    presses: Vec::new(),
});

fn pending() -> std::sync::MutexGuard<'static, Pending> {
    PENDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Subclass `hwnd` once. Later calls do nothing, so a recreated swapchain is harmless.
pub(crate) fn subclass(hwnd: *mut c_void) {
    if hwnd.is_null() || PREVIOUS.load(Ordering::Relaxed) != 0 {
        return;
    }
    let replacement = procedure as *const () as isize;
    // SAFETY: `hwnd` is the window the game just handed us through its swapchain
    // descriptor, and `procedure` has the signature `GWLP_WNDPROC` requires.
    let previous = unsafe { SetWindowLongPtrW(HWND(hwnd), GWLP_WNDPROC, replacement) };
    if previous == 0 {
        log::warn!("could not subclass the game window; the overlay will not take input");
        return;
    }
    PREVIOUS.store(previous, Ordering::Release);
    log::info!("subclassed the game window for overlay input");
}

/// Tell the input layer how big the backbuffer is, so the virtual pointer stays on screen.
pub(crate) fn set_screen(width: f32, height: f32) {
    pending().screen = Vec2::new(width, height);
}

/// Take everything collected since the last call and wrap it as one frame of egui input.
pub(crate) fn take_input(screen: Rect, time: f64) -> RawInput {
    let mut guard = pending();
    let events = core::mem::take(&mut guard.events);
    RawInput {
        screen_rect: Some(screen),
        time: Some(time),
        modifiers: guard.modifiers,
        events,
        focused: guard.focused,
        ..RawInput::default()
    }
}

/// The last key pressed while the overlay had the input, taken out on read.
///
/// The hotkey recorder needs the key itself rather than an egui event: egui has no notion of
/// `VK_OEM_5`, and a binding must be in the loader's own grammar.
pub(crate) fn take_last_key() -> Option<(u16, u16, dayz_plugin_core::keys::Modifiers)> {
    let mut guard = pending();
    let modifiers = dayz_plugin_core::keys::Modifiers {
        ctrl: guard.modifiers.ctrl,
        alt: guard.modifiers.alt,
        shift: guard.modifiers.shift,
    };
    guard
        .last_key
        .take()
        .map(|(vk, scancode)| (vk, scancode, modifiers))
}

/// Key presses the window received since the last call, as `(scan code, virtual key)`.
///
/// How a binding written as `sc<hex>` fires. The alternative, asking the platform which
/// virtual key sits at a scan code, is not trustworthy: under Wine `MapVirtualKeyW` answers
/// `VK_OEM_3` for the key under Escape on a German layout while the key itself arrives as
/// `0xFC`, so a poll of the mapped code would wait for a press that never comes. The message
/// carries both halves, and nothing has to be inferred.
pub(crate) fn take_presses() -> Vec<(u16, u16)> {
    core::mem::take(&mut pending().presses)
}

/// Drop everything collected so far.
///
/// Called when a panel opens: the key that opened it was pressed while the game still had
/// the input, and its `WM_CHAR` would otherwise be typed into whatever the panel focuses.
pub(crate) fn flush() {
    pending().events.clear();
}

/// Where the overlay should draw its cursor.
pub(crate) fn pointer() -> Pos2 {
    pending().pointer
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let capturing = super::is_capturing();
    let input = record(msg, wparam, lparam, capturing);
    if capturing && input {
        // Swallowed: the overlay has the keyboard and mouse for as long as it is open.
        return LRESULT(0);
    }
    let previous = PREVIOUS.load(Ordering::Acquire);
    // SAFETY: `previous` is the procedure `SetWindowLongPtrW` returned for this window, which
    // is exactly what `CallWindowProcW` expects; the arguments are the ones we were given.
    unsafe {
        CallWindowProcW(
            Some(core::mem::transmute::<
                isize,
                unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            >(previous)),
            hwnd,
            msg,
            wparam,
            lparam,
        )
    }
}

/// Translate one message. Returns whether it was an input message, which is what decides
/// whether a capturing overlay swallows it.
fn record(msg: u32, wparam: WPARAM, lparam: LPARAM, capturing: bool) -> bool {
    match msg {
        WM_MOUSEMOVE => {
            let mut guard = pending();
            guard.pointer = client_position(lparam);
            let pos = guard.pointer;
            guard.events.push(Event::PointerMoved(pos));
            true
        }
        WM_INPUT => {
            // Only while capturing: parsing raw input for every mouse move the game makes
            // would cost a syscall per message for nothing.
            if capturing {
                if let Some(delta) = raw_mouse_delta(lparam) {
                    let mut guard = pending();
                    let screen = guard.screen;
                    let moved =
                        (guard.pointer + delta).clamp(Pos2::ZERO, Pos2::new(screen.x, screen.y));
                    guard.pointer = moved;
                    guard.events.push(Event::PointerMoved(moved));
                }
            }
            true
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
        | WM_MBUTTONUP => {
            let (button, pressed) = match msg {
                WM_LBUTTONDOWN => (PointerButton::Primary, true),
                WM_LBUTTONUP => (PointerButton::Primary, false),
                WM_RBUTTONDOWN => (PointerButton::Secondary, true),
                WM_RBUTTONUP => (PointerButton::Secondary, false),
                WM_MBUTTONDOWN => (PointerButton::Middle, true),
                _ => (PointerButton::Middle, false),
            };
            let mut guard = pending();
            let pos = guard.pointer;
            let modifiers = guard.modifiers;
            guard.events.push(Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            });
            true
        }
        WM_MOUSEWHEEL => {
            // The wheel delta is a signed 16-bit value in the high word of wparam.
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let ticks = f32::from((wparam.0 >> 16) as i16) / 120.0;
            let mut guard = pending();
            let modifiers = guard.modifiers;
            guard.events.push(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: Vec2::new(0.0, ticks),
                modifiers,
                phase: egui::TouchPhase::Move,
            });
            true
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            keyboard(msg, wparam, lparam);
            true
        }
        WM_CHAR => {
            #[allow(clippy::cast_possible_truncation)]
            let unit = wparam.0 as u32;
            // Control characters are already covered by the key events above; a UTF-16
            // surrogate half on its own is dropped rather than guessed at.
            if let Some(c) = char::from_u32(unit).filter(|c| !c.is_control()) {
                pending().events.push(Event::Text(c.to_string()));
            }
            true
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            let focused = msg == WM_SETFOCUS;
            let mut guard = pending();
            guard.focused = focused;
            guard.events.push(Event::WindowFocused(focused));
            if !focused {
                // Modifier releases are missed while another window has focus.
                guard.modifiers = Modifiers::NONE;
            }
            false
        }
        _ => false,
    }
}

/// Record one key message: modifier state, the hotkey queues, and the egui event.
fn keyboard(msg: u32, wparam: WPARAM, lparam: LPARAM) {
    let pressed = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    #[allow(clippy::cast_possible_truncation)]
    let vk = wparam.0 as u16;
    // Bits 16..24 of `lparam` are the scan code the keyboard sent, and bit 24 marks the
    // extended set (the keys the `E0` prefix distinguishes, such as right Alt).
    #[allow(clippy::cast_sign_loss, reason = "a bit field, not a number")]
    let bits = lparam.0 as u64;
    #[allow(clippy::cast_possible_truncation)]
    let mut scancode = ((bits >> 16) & 0xFF) as u16;
    if bits & (1 << 24) != 0 {
        scancode |= 0xE000;
    }
    let mut guard = pending();
    update_modifiers(&mut guard.modifiers, vk, pressed);
    if pressed {
        guard.last_key = Some((vk, scancode));
        guard.presses.push((scancode, vk));
    }
    if let Some(key) = key_of(vk) {
        let modifiers = guard.modifiers;
        guard.events.push(Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        });
    }
}

/// Mouse position from a message's `lparam`, in client coordinates.
fn client_position(lparam: LPARAM) -> Pos2 {
    // The two coordinates are packed as signed 16-bit halves, so losing the sign of the
    // whole and truncating to 32 bits is how the value is meant to be read.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let packed = lparam.0 as u32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let (x, y) = ((packed & 0xffff) as i16, (packed >> 16) as i16);
    Pos2::new(f32::from(x), f32::from(y))
}

/// The relative movement in a `WM_INPUT` message, when it is a mouse one.
fn raw_mouse_delta(lparam: LPARAM) -> Option<Vec2> {
    let mut data = RAWINPUT::default();
    let mut size = u32::try_from(core::mem::size_of::<RAWINPUT>()).ok()?;
    let header = u32::try_from(core::mem::size_of::<RAWINPUTHEADER>()).ok()?;
    // SAFETY: `lparam` is the RAWINPUT handle the message carries, and `data` is a correctly
    // sized buffer for it; `GetRawInputData` writes at most `size` bytes.
    let read = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as *mut c_void),
            RID_INPUT,
            Some((&raw mut data).cast::<c_void>()),
            &raw mut size,
            header,
        )
    };
    if read == u32::MAX || data.header.dwType != RIM_TYPEMOUSE.0 {
        return None;
    }
    // SAFETY: the type field above says this union holds the mouse variant.
    let mouse = unsafe { data.data.mouse };
    #[allow(clippy::cast_precision_loss)]
    Some(Vec2::new(mouse.lLastX as f32, mouse.lLastY as f32))
}

fn update_modifiers(modifiers: &mut Modifiers, vk: u16, pressed: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_MENU, VK_SHIFT};
    if vk == VK_SHIFT.0 {
        modifiers.shift = pressed;
    } else if vk == VK_CONTROL.0 {
        modifiers.ctrl = pressed;
        modifiers.command = pressed;
    } else if vk == VK_MENU.0 {
        modifiers.alt = pressed;
    }
}

/// Virtual key to egui key. Unmapped keys produce no key event, which is correct: the
/// characters they produce still arrive as `WM_CHAR`.
#[allow(clippy::too_many_lines)]
fn key_of(vk: u16) -> Option<Key> {
    use windows::Win32::UI::Input::KeyboardAndMouse as vk_const;
    let key = match vk {
        _ if vk == vk_const::VK_LEFT.0 => Key::ArrowLeft,
        _ if vk == vk_const::VK_RIGHT.0 => Key::ArrowRight,
        _ if vk == vk_const::VK_UP.0 => Key::ArrowUp,
        _ if vk == vk_const::VK_DOWN.0 => Key::ArrowDown,
        _ if vk == vk_const::VK_ESCAPE.0 => Key::Escape,
        _ if vk == vk_const::VK_TAB.0 => Key::Tab,
        _ if vk == vk_const::VK_BACK.0 => Key::Backspace,
        _ if vk == vk_const::VK_RETURN.0 => Key::Enter,
        _ if vk == vk_const::VK_SPACE.0 => Key::Space,
        _ if vk == vk_const::VK_INSERT.0 => Key::Insert,
        _ if vk == vk_const::VK_DELETE.0 => Key::Delete,
        _ if vk == vk_const::VK_HOME.0 => Key::Home,
        _ if vk == vk_const::VK_END.0 => Key::End,
        _ if vk == vk_const::VK_PRIOR.0 => Key::PageUp,
        _ if vk == vk_const::VK_NEXT.0 => Key::PageDown,
        // ASCII digits and letters share their virtual key codes with their characters.
        0x30..=0x39 => return digit(vk - 0x30),
        0x41..=0x5A => return letter(vk - 0x41),
        0x70..=0x87 => return function_key(vk - 0x70),
        _ => return None,
    };
    Some(key)
}

fn digit(n: u16) -> Option<Key> {
    const DIGITS: [Key; 10] = [
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ];
    DIGITS.get(usize::from(n)).copied()
}

fn letter(n: u16) -> Option<Key> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    LETTERS.get(usize::from(n)).copied()
}

fn function_key(n: u16) -> Option<Key> {
    const FUNCTION: [Key; 24] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
        Key::F13,
        Key::F14,
        Key::F15,
        Key::F16,
        Key::F17,
        Key::F18,
        Key::F19,
        Key::F20,
        Key::F21,
        Key::F22,
        Key::F23,
        Key::F24,
    ];
    FUNCTION.get(usize::from(n)).copied()
}
