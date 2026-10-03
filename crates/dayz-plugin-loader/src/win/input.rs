//! Keyboard state and focus checks for hotkeys.

// FFI module: thin wrappers over user32 queries.
#![allow(unsafe_code)]

use core::ffi::c_void;

use dayz_plugin_core::hotkeys::KeyState;
use dayz_plugin_core::keys::Modifiers;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, MAPVK_VSC_TO_VK_EX, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

/// Live key state read through `GetAsyncKeyState`.
pub(crate) struct AsyncKeys;

fn down(vk: u16) -> bool {
    // SAFETY: GetAsyncKeyState has no preconditions.
    let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
    // The high bit reports "currently down"; the low bit ("pressed since last call") is
    // shared between all callers in the process and therefore useless here.
    state < 0
}

impl KeyState for AsyncKeys {
    fn is_down(&self, vk: u16) -> bool {
        down(vk)
    }

    fn vk_for_scancode(&self, scancode: u16) -> Option<u16> {
        // MAPVK_VSC_TO_VK_EX asks the *current* layout where that physical key is, which is
        // the whole point: scan code 0x29 is VK_OEM_3 on a US layout and VK_OEM_5 on a German
        // one, and the user should not have to know which they have.
        //
        // SAFETY: MapVirtualKeyW has no preconditions.
        let vk = unsafe { MapVirtualKeyW(u32::from(scancode), MAPVK_VSC_TO_VK_EX) };
        u16::try_from(vk).ok().filter(|vk| *vk != 0)
    }

    fn modifiers(&self) -> Modifiers {
        Modifiers {
            ctrl: down(VK_CONTROL.0),
            alt: down(VK_MENU.0),
            shift: down(VK_SHIFT.0),
        }
    }
}

/// Whether `hwnd` is the foreground window. Hotkeys only fire while this holds.
pub(crate) fn is_focused(hwnd: *mut c_void) -> bool {
    if hwnd.is_null() {
        return false;
    }
    // SAFETY: GetForegroundWindow has no preconditions.
    unsafe { GetForegroundWindow() == HWND(hwnd) }
}
