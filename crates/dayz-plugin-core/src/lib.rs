//! Platform independent logic of the DayZ plugin loader.
//!
//! Nothing in here touches Windows, DXGI or a DLL, so it builds and tests on any host. The
//! `dayz-plugin-loader` crate wraps these types with the OS specific glue.

pub mod cmdline;
pub mod console;
pub mod deps;
pub mod hotkeys;
pub mod keys;
pub mod names;
pub mod settings;
pub mod store;
pub mod windows;
