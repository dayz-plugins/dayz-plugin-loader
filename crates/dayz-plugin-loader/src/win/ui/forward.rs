//! What the window procedure hands to plugins before the game sees it.
//!
//! The order is deliberate: the overlay first, plugins second, the game last. While the
//! overlay has the keyboard the game gets nothing and neither do plugins — someone typing in
//! the console is not aiming — and a plugin that swallows an event takes it from the game but
//! never from the overlay.

// FFI module: it reads raw input buffers the messages point at.
#![allow(unsafe_code)]

use core::ffi::c_void;

use dayz_plugin_api::InputKind;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUTHEADER, RID_INPUT, RIM_TYPEHID, RIM_TYPEMOUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    WM_INPUT, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use crate::win::plugin_input;

/// Offer one message to the plugins subscribed to its kind.
///
/// Returns whether the game must not see it. Keyboard messages go through
/// [`keyboard`] instead, because the window procedure already decodes those.
pub(super) fn message(msg: u32, wparam: WPARAM, lparam: LPARAM, modifiers: Modifiers) -> bool {
    match msg {
        WM_INPUT => raw(lparam),
        WM_MOUSEMOVE => {
            if !plugin_input::wants(InputKind::MouseMove) {
                return false;
            }
            let (x, y) = position(lparam);
            plugin_input::mouse_move(0.0, 0.0, x, y, core::ptr::null_mut())
        }
        WM_MOUSEWHEEL => {
            if !plugin_input::wants(InputKind::MouseWheel) {
                return false;
            }
            // A signed 16-bit delta in the high word, 120 to a notch.
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let notches = f32::from((wparam.0 >> 16) as i16) / 120.0;
            plugin_input::wheel(notches, bits(modifiers))
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
        | WM_MBUTTONUP | WM_XBUTTONDOWN | WM_XBUTTONUP => {
            if !plugin_input::wants(InputKind::MouseButton) {
                return false;
            }
            let (button, pressed) = button(msg, wparam);
            let (x, y) = position(lparam);
            plugin_input::mouse_button(button, pressed, x, y, bits(modifiers))
        }
        _ => false,
    }
}

/// Offer one decoded key press or release. Returns whether the game must not see it.
pub(super) fn keyboard(
    vk: u16,
    scancode: u16,
    pressed: bool,
    repeat: bool,
    modifiers: Modifiers,
) -> bool {
    plugin_input::key(vk, scancode, pressed, repeat, bits(modifiers))
}

/// Modifier flags, as egui tracks them in the window procedure.
pub(super) type Modifiers = egui::Modifiers;

fn bits(modifiers: Modifiers) -> dayz_plugin_api::InputModifiers {
    plugin_input::modifiers(modifiers.ctrl, modifiers.alt, modifiers.shift)
}

/// Which button a message is about, and whether it went down.
fn button(msg: u32, wparam: WPARAM) -> (u32, bool) {
    match msg {
        WM_LBUTTONDOWN => (0, true),
        WM_LBUTTONUP => (0, false),
        WM_RBUTTONDOWN => (1, true),
        WM_RBUTTONUP => (1, false),
        WM_MBUTTONDOWN => (2, true),
        WM_MBUTTONUP => (2, false),
        // The side buttons share two messages and say which one in the high word.
        other => {
            let index = if wparam.0 >> 16 == 2 { 4 } else { 3 };
            (index, other == WM_XBUTTONDOWN)
        }
    }
}

/// Mouse position from a message's `lparam`, in client points.
fn position(lparam: LPARAM) -> (f32, f32) {
    // Two signed 16-bit halves, so truncating the whole is how it is meant to be read.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let packed = lparam.0 as u32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let (x, y) = ((packed & 0xffff) as i16, (packed >> 16) as i16);
    (f32::from(x), f32::from(y))
}

/// Read a `WM_INPUT` message and forward what it holds.
fn raw(lparam: LPARAM) -> bool {
    let wants_move = plugin_input::wants(InputKind::MouseMove);
    let wants_hid = plugin_input::wants(InputKind::Hid);
    if !wants_move && !wants_hid {
        return false;
    }
    let handle = HRAWINPUT(lparam.0 as *mut c_void);
    let Some(buffer) = read(handle) else {
        return false;
    };
    // SAFETY: `buffer` holds at least a RAWINPUTHEADER, which `read` checked.
    let header = unsafe { buffer.as_ptr().cast::<RAWINPUTHEADER>().read_unaligned() };
    let device = header.hDevice.0;
    if header.dwType == RIM_TYPEMOUSE.0 && wants_move {
        // SAFETY: the type says the union holds the mouse variant, and the buffer is large
        // enough for it because the device wrote it.
        let input = unsafe {
            buffer
                .as_ptr()
                .cast::<windows::Win32::UI::Input::RAWINPUT>()
                .read_unaligned()
        };
        // SAFETY: as above.
        let mouse = unsafe { input.data.mouse };
        #[allow(clippy::cast_precision_loss)]
        let (dx, dy) = (mouse.lLastX as f32, mouse.lLastY as f32);
        return plugin_input::mouse_move(dx, dy, 0.0, 0.0, device);
    }
    if header.dwType == RIM_TYPEHID.0 && wants_hid {
        // Everything after the header is the report; the HID structure's own count and size
        // fields describe it, but the bytes a plugin wants are simply the tail.
        let report = &buffer[core::mem::size_of::<RAWINPUTHEADER>()..];
        return plugin_input::hid(device, report);
    }
    false
}

/// The whole raw input record, sized by asking first. `None` when it cannot be read.
fn read(handle: HRAWINPUT) -> Option<Vec<u8>> {
    let header = u32::try_from(core::mem::size_of::<RAWINPUTHEADER>()).ok()?;
    let mut size = 0u32;
    // SAFETY: a null buffer asks for the size, which is what the `None` data pointer means.
    let asked = unsafe { GetRawInputData(handle, RID_INPUT, None, &raw mut size, header) };
    if asked == u32::MAX || size as usize <= core::mem::size_of::<RAWINPUTHEADER>() {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    // SAFETY: `buffer` is writable for `size` bytes, which is the size the call just gave.
    let read = unsafe {
        GetRawInputData(
            handle,
            RID_INPUT,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            &raw mut size,
            header,
        )
    };
    if read == u32::MAX {
        return None;
    }
    buffer.truncate(read as usize);
    Some(buffer)
}
