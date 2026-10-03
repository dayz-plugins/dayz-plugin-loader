//! Watching, swallowing and sending input.
//!
//! The loader sees every key and every mouse message before the game does, because it owns
//! the window procedure. A plugin asks for the kinds it cares about with
//! [`Host::listen_input`](crate::Host::listen_input), reads them in
//! [`Plugin::on_input`](crate::Plugin::on_input), and decides per event whether the game
//! still gets it:
//!
//! ```ignore
//! fn start(host: Host) -> Result<Self, PluginError> {
//!     host.listen_input(Watch::KEY | Watch::MOUSE_MOVE)?;
//!     Ok(Spy)
//! }
//!
//! fn on_input(&self, _host: &Host, input: &Input) -> Verdict {
//!     match input {
//!         // Hold the game still while this plugin is driving the camera.
//!         Input::MouseMove { dx, dy, .. } => { self.look(*dx, *dy); Verdict::SWALLOW }
//!         _ => Verdict::PASS,
//!     }
//! }
//! ```
//!
//! Sending goes the other way, through [`Host::send_input`](crate::Host::send_input) and the
//! constructors on [`Action`]. It is real system input, not a message posted to a window, which
//! is the only kind DayZ's raw-input mouse and polled keyboard both notice.

// Reading the loader's event structure is a pointer dereference by nature; this is the one
// place a plugin does not have to write that itself.
#![allow(unsafe_code)]

use dayz_plugin_api::{InputAction, InputActionFlags, InputActionKind, InputEvent, InputKind};

pub use dayz_plugin_api::{InputMask as Watch, InputResponse as Verdict};

/// Modifier keys held when an event happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Held {
    /// Either Control key.
    pub ctrl: bool,
    /// Either Alt key.
    pub alt: bool,
    /// Either Shift key.
    pub shift: bool,
}

/// One input event, before the game sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Input<'a> {
    /// A key went down or came up.
    Key {
        /// Win32 virtual key code.
        vk: u16,
        /// The scan code the keyboard sent, which is the same on every layout.
        scancode: u16,
        /// Whether it went down.
        pressed: bool,
        /// Whether this is the keyboard's auto-repeat rather than a new press.
        repeat: bool,
        /// Modifiers held at the time.
        held: Held,
    },
    /// A mouse button went down or came up.
    MouseButton {
        /// 0 left, 1 right, 2 middle, 3 and 4 the side buttons.
        button: u32,
        /// Whether it went down.
        pressed: bool,
        /// Client position, in points.
        x: f32,
        /// Client position, in points.
        y: f32,
        /// Modifiers held at the time.
        held: Held,
    },
    /// The mouse moved: a device delta from raw input, or a position from a window message.
    MouseMove {
        /// Horizontal movement. Zero for a window message, which carries a position instead.
        dx: f32,
        /// Vertical movement.
        dy: f32,
        /// Client position where the message carried one.
        x: f32,
        /// Client position where the message carried one.
        y: f32,
        /// Whether this came from raw input, which is the only kind the game itself reads
        /// once it has captured the cursor.
        raw: bool,
    },
    /// The wheel turned, in notches, positive away from the user.
    Wheel {
        /// Notches turned.
        notches: f32,
        /// Modifiers held at the time.
        held: Held,
    },
    /// A raw HID report, borrowed for the length of the callback.
    Hid {
        /// Raw input device handle, stable while the device stays plugged in.
        device: usize,
        /// The report bytes.
        report: &'a [u8],
    },
}

impl Input<'_> {
    /// Borrow one event out of the ABI structure.
    ///
    /// # Safety
    /// `event` must be the pointer the loader passed to `on_input`, valid for the call.
    pub(crate) unsafe fn from_abi(event: *const InputEvent) -> Option<Self> {
        if event.is_null() {
            return None;
        }
        // SAFETY: the caller passes the loader's own pointer, which the ABI requires to be a
        // readable `InputEvent` for the duration of the call.
        let event = unsafe { &*event };
        let held = Held {
            ctrl: event.modifiers.0 & dayz_plugin_api::InputModifiers::CTRL.0 != 0,
            alt: event.modifiers.0 & dayz_plugin_api::InputModifiers::ALT.0 != 0,
            shift: event.modifiers.0 & dayz_plugin_api::InputModifiers::SHIFT.0 != 0,
        };
        #[allow(clippy::cast_possible_truncation)]
        let event = match event.kind {
            InputKind::Key => Input::Key {
                vk: event.code as u16,
                scancode: event.scancode as u16,
                pressed: event.pressed != 0,
                repeat: event.repeat != 0,
                held,
            },
            InputKind::MouseButton => Input::MouseButton {
                button: event.code,
                pressed: event.pressed != 0,
                x: event.x,
                y: event.y,
                held,
            },
            InputKind::MouseMove => Input::MouseMove {
                dx: event.dx,
                dy: event.dy,
                x: event.x,
                y: event.y,
                raw: !event.device.is_null(),
            },
            InputKind::MouseWheel => Input::Wheel {
                notches: event.dx,
                held,
            },
            InputKind::Hid => Input::Hid {
                device: event.device as usize,
                // SAFETY: the ABI requires the report to be readable for `data_len` bytes
                // for the duration of this call, which is where the borrow ends.
                report: unsafe { report(event.data, event.data_len) },
            },
        };
        Some(event)
    }
}

/// # Safety
/// `data` must be readable for `len` bytes, or null with `len` zero.
unsafe fn report<'a>(data: *const u8, len: usize) -> &'a [u8] {
    if data.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: the caller upholds the ABI's contract for these two fields.
    unsafe { core::slice::from_raw_parts(data, len) }
}

/// One thing to send. Build these with the constructors and pass a slice to
/// [`Host::send_input`](crate::Host::send_input), which sends the whole slice as one burst.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct Action(pub(crate) InputAction);

impl Action {
    fn new(kind: InputActionKind) -> Self {
        Action(InputAction {
            struct_size: core::mem::size_of::<InputAction>(),
            kind,
            code: 0,
            scancode: 0,
            pressed: 0,
            dx: 0,
            dy: 0,
            flags: InputActionFlags::NONE,
        })
    }

    /// Press a key, by Win32 virtual key code.
    #[must_use]
    pub fn key_down(vk: u16) -> Self {
        let mut action = Self::new(InputActionKind::KEY);
        action.0.code = u32::from(vk);
        action.0.pressed = 1;
        action
    }

    /// Release a key, by Win32 virtual key code.
    #[must_use]
    pub fn key_up(vk: u16) -> Self {
        let mut action = Self::new(InputActionKind::KEY);
        action.0.code = u32::from(vk);
        action
    }

    /// Press or release a key by its position instead of its code.
    ///
    /// What a game reading scan codes needs, which DayZ's own bindings do.
    #[must_use]
    pub fn scancode(scancode: u16, pressed: bool) -> Self {
        let mut action = Self::new(InputActionKind::KEY);
        action.0.scancode = u32::from(scancode);
        action.0.pressed = u32::from(pressed);
        action.0.flags = InputActionFlags::SCANCODE;
        action
    }

    /// Mark a key action as one of the extended set, the keys an `E0` prefix distinguishes.
    #[must_use]
    pub fn extended(mut self) -> Self {
        self.0.flags = InputActionFlags(self.0.flags.0 | InputActionFlags::EXTENDED.0);
        self
    }

    /// Press or release a mouse button: 0 left, 1 right, 2 middle, 3 and 4 the side buttons.
    #[must_use]
    pub fn mouse_button(button: u32, pressed: bool) -> Self {
        let mut action = Self::new(InputActionKind::MOUSE_BUTTON);
        action.0.code = button;
        action.0.pressed = u32::from(pressed);
        action
    }

    /// Move the mouse by a delta, the way a mouse does.
    #[must_use]
    pub fn mouse_move(dx: i32, dy: i32) -> Self {
        let mut action = Self::new(InputActionKind::MOUSE_MOVE);
        action.0.dx = dx;
        action.0.dy = dy;
        action
    }

    /// Move the mouse to a point on the virtual desktop, both axes in `0..=65535`.
    #[must_use]
    pub fn mouse_to(x: i32, y: i32) -> Self {
        let mut action = Self::new(InputActionKind::MOUSE_MOVE_ABSOLUTE);
        action.0.dx = x;
        action.0.dy = y;
        action
    }

    /// Turn the wheel, in notches, positive away from the user.
    #[must_use]
    pub fn wheel(notches: i32) -> Self {
        let mut action = Self::new(InputActionKind::MOUSE_WHEEL);
        action.0.dx = notches;
        action
    }
}
