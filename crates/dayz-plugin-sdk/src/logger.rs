//! `log` backend that forwards to the loader log.

use dayz_plugin_api::LogLevel;
use log::{Level, Log, Metadata, Record};

use crate::host::Host;

pub(crate) struct HostLogger {
    host: Host,
}

impl HostLogger {
    /// Install once per DLL. A second call (two plugins in one DLL) is harmless.
    pub(crate) fn install(host: Host) {
        let logger = Box::leak(Box::new(HostLogger { host }));
        if log::set_logger(logger).is_ok() {
            log::set_max_level(log::LevelFilter::Trace);
        }
    }
}

impl Log for HostLogger {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        let level = match record.level() {
            Level::Error => LogLevel::Error,
            Level::Warn => LogLevel::Warn,
            Level::Info => LogLevel::Info,
            Level::Debug => LogLevel::Debug,
            Level::Trace => LogLevel::Trace,
        };
        self.host.log(level, &record.args().to_string());
    }

    fn flush(&self) {}
}
