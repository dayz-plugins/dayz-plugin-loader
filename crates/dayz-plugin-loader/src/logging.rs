//! File logger: `<logs>/loader.log`, previous run kept as `loader.prev.log`.

use std::fs::{self, File};
use std::path::Path;

use log::LevelFilter;
use simplelog::{
    ColorChoice, CombinedLogger, ConfigBuilder, SharedLogger, TermLogger, TerminalMode, WriteLogger,
};

/// Name of the current log file.
pub const LOG_FILE: &str = "loader.log";
/// Name the previous run's log is renamed to.
pub const PREV_LOG_FILE: &str = "loader.prev.log";

/// Rotate and open the log. With `console`, log lines also go to the attached console
/// window. Errors are returned, not logged: there is no logger yet.
pub fn init(logs_dir: &Path, level: LevelFilter, console: bool) -> std::io::Result<()> {
    fs::create_dir_all(logs_dir)?;
    let current = logs_dir.join(LOG_FILE);
    if current.exists() {
        fs::rename(&current, logs_dir.join(PREV_LOG_FILE))?;
    }
    let config = ConfigBuilder::new()
        .set_time_format_rfc3339()
        .set_thread_level(LevelFilter::Off)
        .set_target_level(LevelFilter::Error)
        .build();
    let file = File::create(&current)?;
    let mut sinks: Vec<Box<dyn SharedLogger>> = vec![WriteLogger::new(level, config.clone(), file)];
    if console {
        sinks.push(TermLogger::new(
            level,
            config,
            TerminalMode::Mixed,
            ColorChoice::Never,
        ));
    }
    CombinedLogger::init(sinks).map_err(std::io::Error::other)
}
