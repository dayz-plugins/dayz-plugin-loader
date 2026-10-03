//! File logger: `<logs>/loader.log`, previous run kept as `loader.prev.log`.

use std::fs::{self, File};
use std::path::Path;

use log::{LevelFilter, Log, Metadata, Record};
use simplelog::{Config, ConfigBuilder, SharedLogger, WriteLogger};

/// Where console lines go and how they are stamped: the platform layer's writer and clock.
///
/// A pair rather than a trait because there is exactly one implementation and it is two
/// function pointers; a trait object here would be ceremony around `console::print`.
pub type ConsoleSink = (fn(&str), fn() -> String);

/// Name of the current log file.
pub const LOG_FILE: &str = "loader.log";
/// Name the previous run's log is renamed to.
pub const PREV_LOG_FILE: &str = "loader.prev.log";

/// A logger that hands each line to one function.
///
/// `simplelog`'s `TermLogger` writes to the terminal itself, which made it a second,
/// unsynchronised writer on a console device that does not write atomically — the cause of
/// the spliced lines in the console window. Everything now goes through the one sink the
/// platform layer gives us.
struct SinkLogger {
    level: LevelFilter,
    config: Config,
    sink: fn(&str),
    /// Prefix each line with a short clock reading.
    clock: fn() -> String,
}

impl Log for SinkLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        (self.sink)(&format!(
            "{} [{:<5}] {}: {}",
            (self.clock)(),
            record.level(),
            record.target(),
            record.args()
        ));
    }

    fn flush(&self) {}
}

impl SharedLogger for SinkLogger {
    fn level(&self) -> LevelFilter {
        self.level
    }

    fn config(&self) -> Option<&Config> {
        Some(&self.config)
    }

    fn as_log(self: Box<Self>) -> Box<dyn Log> {
        self
    }
}

/// Rotate and open the log. When `console` is given, log lines also go to it, through that
/// one function. Errors are returned, not logged: there is no logger yet.
pub fn init(
    logs_dir: &Path,
    level: LevelFilter,
    console: Option<ConsoleSink>,
) -> std::io::Result<()> {
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
    if let Some((sink, clock)) = console {
        sinks.push(Box::new(SinkLogger {
            level,
            config,
            sink,
            clock,
        }));
    }
    simplelog::CombinedLogger::init(sinks).map_err(std::io::Error::other)
}
