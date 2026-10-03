//! Sending input, and asking the system for devices the game never registered.
//!
//! `SendInput` rather than posting messages to the game's window: DayZ reads the mouse
//! through raw input and the keyboard through a polled table, and neither of those sees a
//! synthesised `WM_KEYDOWN`. Real system input goes through both, which is what a VR plugin
//! turning head motion into look input needs.

// FFI module: the user32 input functions.
#![allow(unsafe_code)]

use core::ffi::c_void;

use dayz_plugin_api::{InputAction, InputActionFlags, InputActionKind, Status};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::Input::{RegisterRawInputDevices, RAWINPUTDEVICE, RIDEV_INPUTSINK};

/// How far one wheel notch is, as `SendInput` counts it.
const WHEEL_DELTA: i32 = 120;
/// The two side buttons, as `mouseData` names them. Spelled out here rather than pulling in
/// the whole `Win32_Devices` feature of the windows crate for two integers.
const XBUTTON1: i32 = 0x0001;
/// See [`XBUTTON1`].
const XBUTTON2: i32 = 0x0002;
/// HID usage page 1: joysticks, gamepads, multi-axis controllers.
const HID_PAGE_GENERIC: u16 = 0x01;
/// HID usage page 3: head trackers and the rest of the VR controls page.
const HID_PAGE_VR: u16 = 0x03;

/// Send a burst of actions in order. An empty array is accepted and does nothing.
pub(crate) fn send(actions: &[InputAction]) -> Status {
    if actions.is_empty() {
        return Status::Ok;
    }
    let mut inputs: Vec<INPUT> = Vec::with_capacity(actions.len());
    for action in actions {
        if action.struct_size < core::mem::size_of::<InputAction>() {
            return Status::Unsupported;
        }
        match translate(action) {
            Some(input) => inputs.push(input),
            None => return Status::InvalidArgument,
        }
    }
    let Ok(size) = i32::try_from(core::mem::size_of::<INPUT>()) else {
        return Status::Error;
    };
    // SAFETY: `inputs` is a slice of correctly initialised INPUT structures and `size`
    // describes one of them, which is the whole contract of SendInput.
    let sent = unsafe { SendInput(&inputs, size) };
    if sent as usize == inputs.len() {
        Status::Ok
    } else {
        // Partial delivery means something blocked the queue, most often UIPI.
        log::warn!("sent {sent} of {} input events", inputs.len());
        Status::Error
    }
}

/// One action as the `INPUT` structure that expresses it.
fn translate(action: &InputAction) -> Option<INPUT> {
    match action.kind {
        InputActionKind::KEY => Some(keyboard(action)),
        InputActionKind::MOUSE_BUTTON => mouse_button(action),
        InputActionKind::MOUSE_MOVE => Some(mouse(MOUSEEVENTF_MOVE, action.dx, action.dy, 0)),
        InputActionKind::MOUSE_MOVE_ABSOLUTE => Some(mouse(
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
            action.dx,
            action.dy,
            0,
        )),
        InputActionKind::MOUSE_WHEEL => Some(mouse(
            MOUSEEVENTF_WHEEL,
            0,
            0,
            action.dx.saturating_mul(WHEEL_DELTA),
        )),
        // An action kind from a newer ABI: refused rather than guessed at.
        _ => None,
    }
}

/// A key press or release, by virtual key or by scan code.
fn keyboard(action: &InputAction) -> INPUT {
    let by_scancode = action.flags.0 & InputActionFlags::SCANCODE.0 != 0;
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if action.pressed == 0 {
        flags |= KEYEVENTF_KEYUP;
    }
    if by_scancode {
        flags |= KEYEVENTF_SCANCODE;
    }
    if action.flags.0 & InputActionFlags::EXTENDED.0 != 0 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    #[allow(clippy::cast_possible_truncation)]
    let (vk, scan) = (action.code as u16, action.scancode as u16);
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                // With KEYEVENTF_SCANCODE the virtual key must be zero, or the injection is
                // ambiguous and Windows picks for us.
                wVk: if by_scancode {
                    VIRTUAL_KEY(0)
                } else {
                    VIRTUAL_KEY(vk)
                },
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// A mouse button press or release. `None` for a button index that does not exist.
fn mouse_button(action: &InputAction) -> Option<INPUT> {
    let pressed = action.pressed != 0;
    let (flags, data) = match (action.code, pressed) {
        (0, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (0, false) => (MOUSEEVENTF_LEFTUP, 0),
        (1, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (1, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (2, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (2, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (3, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
        (3, false) => (MOUSEEVENTF_XUP, XBUTTON1),
        (4, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
        (4, false) => (MOUSEEVENTF_XUP, XBUTTON2),
        _ => return None,
    };
    Some(mouse(flags, 0, 0, data))
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                #[allow(
                    clippy::cast_sign_loss,
                    reason = "mouseData is a u32 field holding a signed wheel delta"
                )]
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Register for raw input from one HID usage, on top of whatever the game registered.
///
/// `RIDEV_INPUTSINK` so reports keep arriving while the game is not the foreground window,
/// which is what a VR or device plugin wants; the window is the game's own, because that is
/// where the loader's window procedure is.
pub(crate) fn register_hid(window: *mut c_void, usage_page: u16, usage: u16) -> Status {
    if window.is_null() {
        return Status::WrongPhase;
    }
    // The generic desktop page is where joysticks, gamepads and multi-axis controllers live,
    // and the VR page is where head trackers do. Anything else is refused: registering the
    // mouse or keyboard page again would change the flags the game chose for itself.
    if usage_page != HID_PAGE_GENERIC && usage_page != HID_PAGE_VR {
        return Status::InvalidArgument;
    }
    let devices = [RAWINPUTDEVICE {
        usUsagePage: usage_page,
        usUsage: usage,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: HWND(window),
    }];
    let Ok(size) = u32::try_from(core::mem::size_of::<RAWINPUTDEVICE>()) else {
        return Status::Error;
    };
    // SAFETY: one correctly initialised descriptor, with `size` describing it, and a window
    // handle the loader owns the procedure of.
    let registered = unsafe { RegisterRawInputDevices(&devices, size) };
    match registered {
        Ok(()) => {
            log::info!("registered for raw HID input, usage {usage_page:#x}/{usage:#x}");
            Status::Ok
        }
        Err(e) => {
            log::warn!("could not register usage {usage_page:#x}/{usage:#x}: {e}");
            Status::Error
        }
    }
}
