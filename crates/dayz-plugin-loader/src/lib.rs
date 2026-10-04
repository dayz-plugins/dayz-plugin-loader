//! DayZ plugin loader. Built as `dxgi.dll` and dropped next to `DayZ_x64.exe`, where the game
//! (under Proton with `WINEDLLOVERRIDES="dxgi=n,b"`) loads it instead of the system DXGI.
//!
//! The loader forwards every DXGI export to the real library, patches the factory and
//! swapchain vtables to see `Present` and `ResizeBuffers`, and hosts plugins found in
//! `<game>/plugins/*.dll`; its own config, data and logs live in `<game>/plugin-loader/`. The
//! platform independent half (registries, key grammar, console parsing, config files) lives
//! in `dayz-plugin-core`.
//!
//! On non-Windows hosts this crate compiles to an empty library so the workspace gate runs
//! everywhere; the DLL itself is cross-built with `scripts/build.sh`.

#![cfg_attr(not(windows), allow(dead_code))]

mod config;
mod console;
mod logging;
mod process;
mod scrollback;
mod state;

#[cfg(windows)]
mod win;

#[cfg(windows)]
mod dllmain;
