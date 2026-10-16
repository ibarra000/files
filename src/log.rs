//! Minimal `log` facade backend that writes to stderr and the debugger.
//!
//! `eframe`, `winit` and `wgpu` report problems (for example a surface that
//! cannot do per-pixel transparency) only through `log`, which discards
//! everything until a logger is installed. `main.rs` installs this before
//! opening any window. Warnings and errors by default; set `FILES_LOG` for more.

use std::io::Write;

/// Environment variable that sets the log level.
///
/// Values, case insensitive: `off`, `error`, `warn`, `info`, `debug`, `trace`.
pub const ENV: &str = "FILES_LOG";

struct Stderr {
    level: log::LevelFilter,
}

impl log::Log for Stderr {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Formatted once and written in a single call so concurrent writers to
        // the same console do not interleave mid-line.
        let line = format!(
            "{:<5} {}: {}",
            record.level(),
            record.target(),
            record.args()
        );
        let _ = writeln!(std::io::stderr(), "{line}");
        debugger(&line);
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

/// Parses [`ENV`], defaulting to warnings and errors.
fn level() -> log::LevelFilter {
    let Ok(text) = std::env::var(ENV) else {
        return log::LevelFilter::Warn;
    };
    match text.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => log::LevelFilter::Off,
        "error" => log::LevelFilter::Error,
        "info" => log::LevelFilter::Info,
        "debug" => log::LevelFilter::Debug,
        "trace" => log::LevelFilter::Trace,
        // "warn" and anything unrecognised: a typo must not disable logging.
        _ => log::LevelFilter::Warn,
    }
}

/// Installs the logger. Later calls do nothing, and failure is ignored.
pub fn install() {
    let level = level();
    if log::set_boxed_logger(Box::new(Stderr { level })).is_ok() {
        log::set_max_level(level);
    }
}

/// Sends `line` to `OutputDebugStringW`.
///
/// `files.exe` uses the `windows` subsystem and has no console, so stderr is
/// lost; DebugView or an attached debugger can still show these lines.
#[cfg(windows)]
fn debugger(line: &str) {
    use windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringW;

    let mut wide: Vec<u16> = line.encode_utf16().collect();
    wide.push(u16::from(b'\n'));
    wide.push(0);
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call.
    // `OutputDebugStringW` only reads it.
    unsafe { OutputDebugStringW(wide.as_ptr()) };
}

#[cfg(not(windows))]
fn debugger(line: &str) {
    // Other platforms have a console, so stderr is sufficient.
    let _ = line;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that write `FILES_LOG`, since the environment is shared
    /// by all test threads in the process.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Sets `FILES_LOG`, returns the parsed level, and removes the variable.
    ///
    /// The only place that writes `FILES_LOG`, so the lock is always held.
    fn level_for(text: Option<&str>) -> log::LevelFilter {
        // A poisoned lock only means another test panicked; keep using it.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: `set_var` and `remove_var` race with concurrent environment
        // reads. Every write to `FILES_LOG` in this binary happens here under
        // `ENV_LOCK`, and `level()` is called before the guard is dropped.
        unsafe {
            match text {
                Some(text) => std::env::set_var(ENV, text),
                None => std::env::remove_var(ENV),
            }
        }
        let level = level();
        // SAFETY: as above.
        unsafe { std::env::remove_var(ENV) };
        level
    }

    #[test]
    fn warnings_are_printed_by_default() {
        assert_eq!(level_for(None), log::LevelFilter::Warn);
    }

    #[test]
    fn an_unrecognised_level_stays_at_the_default() {
        for text in ["warning", "verbose", "yes", ""] {
            assert_eq!(level_for(Some(text)), log::LevelFilter::Warn, "{text:?}");
        }
    }

    #[test]
    fn every_level_name_is_understood() {
        for (text, want) in [
            ("off", log::LevelFilter::Off),
            ("error", log::LevelFilter::Error),
            ("info", log::LevelFilter::Info),
            ("DEBUG", log::LevelFilter::Debug),
            (" trace ", log::LevelFilter::Trace),
        ] {
            assert_eq!(level_for(Some(text)), want, "{text:?}");
        }
    }

    #[test]
    fn installing_more_than_once_is_harmless() {
        install();
        install();
    }
}
