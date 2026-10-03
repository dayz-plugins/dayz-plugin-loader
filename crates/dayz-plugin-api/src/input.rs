//! Raw input a plugin can watch, swallow and send.
//!
//! The loader already subclasses the game's window, so it sees every key, every mouse
//! message and every `WM_INPUT` before the game does. This is that stream, offered to
//! plugins: subscribe to the kinds you care about, answer whether the game should still see
//! each one, and send input of your own with [`HostApi::input_send`](crate::HostApi).
//!
//! Two plugins are the reason this exists. A VR plugin turns head and controller motion into
//! the input the game already understands, which is sending. A `DirectInput` plugin reads
//! devices the game never asked the system for, which is receiving — including the raw HID
//! reports the loader can register for on a plugin's behalf.

use core::ffi::c_void;

/// What an [`InputEvent`] is about.
///
/// An enum, unlike the two types that travel the other way, because a plugin only ever
/// receives a kind it subscribed to: a kind added in a later ABI has a mask bit an older
/// plugin never sets, so it is never delivered to one.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// A key went down or came up.
    Key = 0,
    /// A mouse button went down or came up.
    MouseButton = 1,
    /// The mouse moved. Raw events carry a device delta, window events a client position.
    MouseMove = 2,
    /// The mouse wheel turned.
    MouseWheel = 3,
    /// A raw HID report from a device the loader was asked to register.
    Hid = 4,
}

/// Bit flags for [`HostApi::input_listen`](crate::HostApi), one per [`InputKind`].
///
/// A plugin only hears about the kinds it asked for: the window procedure runs inside the
/// game's message loop, and a plugin that wants key presses should not be woken for every
/// mouse delta.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputMask(pub u32);

impl InputMask {
    /// Subscribe to nothing, which is how a plugin unsubscribes.
    pub const NONE: InputMask = InputMask(0);
    /// [`InputKind::Key`].
    pub const KEY: InputMask = InputMask(1);
    /// [`InputKind::MouseButton`].
    pub const MOUSE_BUTTON: InputMask = InputMask(2);
    /// [`InputKind::MouseMove`].
    pub const MOUSE_MOVE: InputMask = InputMask(4);
    /// [`InputKind::MouseWheel`].
    pub const MOUSE_WHEEL: InputMask = InputMask(8);
    /// [`InputKind::Hid`].
    pub const HID: InputMask = InputMask(16);
    /// Every kind this version knows.
    pub const ALL: InputMask = InputMask(31);

    /// The bit standing for one kind.
    #[must_use]
    pub const fn of(kind: InputKind) -> InputMask {
        InputMask(1 << (kind as u32))
    }

    /// Whether this mask includes that kind.
    #[must_use]
    pub const fn covers(self, kind: InputKind) -> bool {
        self.0 & InputMask::of(kind).0 != 0
    }
}

impl core::ops::BitOr for InputMask {
    type Output = InputMask;

    fn bitor(self, other: InputMask) -> InputMask {
        InputMask(self.0 | other.0)
    }
}

/// Modifier keys held when an event happened.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputModifiers(pub u32);

impl InputModifiers {
    /// Nothing held.
    pub const NONE: InputModifiers = InputModifiers(0);
    /// Either Control key.
    pub const CTRL: InputModifiers = InputModifiers(1);
    /// Either Alt key.
    pub const ALT: InputModifiers = InputModifiers(2);
    /// Either Shift key.
    pub const SHIFT: InputModifiers = InputModifiers(4);
}

/// What `on_input` returns: whether the game still gets to see the event.
///
/// A struct rather than an enum because this value travels from the plugin to the loader: a
/// plugin built against a newer ABI could return something this loader has never heard of,
/// and reading that into an enum would be undefined behaviour rather than a fallback.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputResponse(pub u32);

impl InputResponse {
    /// Pass it on. The default, and what an unknown value is read as.
    pub const PASS: InputResponse = InputResponse(0);
    /// Swallow it: the game's window procedure never sees this message.
    ///
    /// The first subscriber to swallow an event ends its delivery, so later subscribers do
    /// not see it either. The loader logs the first few times a plugin swallows.
    pub const SWALLOW: InputResponse = InputResponse(1);
}

/// One input event, as the loader's window procedure saw it.
///
/// Unused fields are zero for that kind rather than undefined, so a plugin can read `dx` on a
/// key event without checking first.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputEvent {
    /// `size_of::<InputEvent>()`.
    pub struct_size: usize,
    /// Which fields below mean anything.
    pub kind: InputKind,
    /// `Key`: the Win32 virtual key code. `MouseButton`: 0 left, 1 right, 2 middle, 3 and 4
    /// the side buttons. Zero otherwise.
    pub code: u32,
    /// `Key`: the scan code the keyboard sent, which is the same on every layout. Zero
    /// otherwise.
    pub scancode: u32,
    /// 1 while going down, 0 on the way up. Always 1 for a wheel or a move.
    pub pressed: u32,
    /// 1 when this is the keyboard's auto-repeat rather than a new press.
    pub repeat: u32,
    /// Modifier keys held at the time.
    pub modifiers: InputModifiers,
    /// `MouseMove`: horizontal movement, in device units for a raw event and in points for a
    /// window one. `MouseWheel`: notches turned, positive away from the user.
    pub dx: f32,
    /// `MouseMove`: vertical movement. Zero for a wheel.
    pub dy: f32,
    /// Mouse position in client points, where the message carried one.
    pub x: f32,
    /// Mouse position in client points, where the message carried one.
    pub y: f32,
    /// `HRAWINPUT` device handle for an event that came from `WM_INPUT`, else null. Stable
    /// for as long as the device stays plugged in.
    pub device: *mut c_void,
    /// `Hid`: the report bytes, valid only while the callback runs. Null otherwise.
    pub data: *const u8,
    /// Length of [`InputEvent::data`].
    pub data_len: usize,
    /// Milliseconds since the loader initialised.
    pub time_ms: u64,
}

/// What an [`InputAction`] does.
///
/// A struct rather than an enum for the same reason as [`InputResponse`]: the value is
/// written by the plugin and read by the loader, which must be able to refuse one it does not
/// know instead of having read an invalid enum.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputActionKind(pub u32);

impl InputActionKind {
    /// Press or release a key.
    pub const KEY: InputActionKind = InputActionKind(0);
    /// Press or release a mouse button.
    pub const MOUSE_BUTTON: InputActionKind = InputActionKind(1);
    /// Move the mouse by a delta, the way a mouse does.
    pub const MOUSE_MOVE: InputActionKind = InputActionKind(2);
    /// Move the mouse to a position on the virtual desktop, in `0..=65535` on both axes.
    pub const MOUSE_MOVE_ABSOLUTE: InputActionKind = InputActionKind(3);
    /// Turn the wheel. `dx` is notches, positive away from the user.
    pub const MOUSE_WHEEL: InputActionKind = InputActionKind(4);
}

/// Bit flags for [`InputAction::flags`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputActionFlags(pub u32);

impl InputActionFlags {
    /// No flags.
    pub const NONE: InputActionFlags = InputActionFlags(0);
    /// Send the key by its scan code (`InputAction::scancode`) rather than its virtual key.
    ///
    /// What a game that reads scan codes needs, which DayZ's own key bindings do.
    pub const SCANCODE: InputActionFlags = InputActionFlags(1);
    /// Mark the key as one of the extended set, the keys an `E0` prefix distinguishes.
    pub const EXTENDED: InputActionFlags = InputActionFlags(2);
}

/// One thing to send. [`HostApi::input_send`](crate::HostApi) takes an array of them and
/// sends them in order, as one burst, so a chord arrives as a chord.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputAction {
    /// `size_of::<InputAction>()`.
    pub struct_size: usize,
    /// What this action does.
    pub kind: InputActionKind,
    /// `Key`: the Win32 virtual key code. `MouseButton`: 0 left, 1 right, 2 middle, 3 and 4
    /// the side buttons.
    pub code: u32,
    /// Scan code, used instead of `code` when [`InputActionFlags::SCANCODE`] is set.
    pub scancode: u32,
    /// 1 to press, 0 to release. Ignored by the move and wheel kinds.
    pub pressed: u32,
    /// Horizontal movement, absolute position, or wheel notches.
    pub dx: i32,
    /// Vertical movement or absolute position.
    pub dy: i32,
    /// See [`InputActionFlags`].
    pub flags: InputActionFlags,
}
