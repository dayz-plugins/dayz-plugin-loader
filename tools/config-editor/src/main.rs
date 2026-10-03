//! `dayz-vr-config`: a small graphical editor for `dayz_openxr.ini`.
//!
//! Usage: `dayz-vr-config [path/to/dayz_openxr.ini]`. Without a path the ini beside the
//! executable, in the working directory or in the Steam `DayZ` folder is opened.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod ini;
mod live;
mod locate;
mod schema;
mod widgets;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("DayZ-VR config")
            .with_inner_size([1000.0, 700.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "dayz-vr-config",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, path)))),
    )
}
