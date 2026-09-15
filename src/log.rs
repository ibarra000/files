//! Somewhere for the toolkit's complaints to go.
//!
//! This module exists because of a specific, expensive silence. `eframe`,
//! `winit` and the renderer under them all report through the `log` facade,
//! and a facade with no implementation installed discards everything. The
//! `lens` overlay shipped opaque - a black rectangle over the entire desktop -
//! and the reason was sitting in a message that was emitted on every single
//! run:
//!
//! ```text
//! Transparent window was requested, but the active wgpu surface does not
//! support a `CompositeAlphaMode` with transparency.
//! ```
//!
//! Nobody was listening, so a diagnosis that the graphics stack had already
//! made cost a day to rediscover.
//!
//! That overlay is gone, but the lesson outlived it and now applies to the
//! panel: `files` is the window that asks for per-pixel alpha now, and it is
//! the window whose renderer changed. A transparency failure announces itself
//! through this facade or not at all, and `main.rs` installs this before it
//! opens anything.
//!
//! # Why it is hand-rolled
//!
//! Forty lines against `env_logger` and its transitive tail, for a program
//! whose entire logging requirement is "put warnings on stderr". That is the
//! same trade `clipboard::base64` and `util::rng` already make, and the note in
//! `Cargo.toml` about `toml_edit` makes explicitly: a crate for this would
//! bring more dependency than code.
//!
//! Warnings and errors only, by default. `info` and `debug` from the toolkit
//! are voluminous and are addressed to somebody working on those crates, not
//! to somebody running this one - but `FILES_LOG=debug` turns them on, because
//! the day one of them matters it matters a great deal.

use std::io::Write;

/// The environment variable that widens what is printed.
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
        // Assembled once rather than written in pieces: two programs writing to
        // the same console interleave at the write boundary, and a line made of
        // four writes comes out shuffled. It is also needed whole for the
        // debugger sink below.
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
        // Including "warn", and including anything unrecognised: a typo in a
        // log level must not silently switch logging off, which is the one
        // outcome that would reproduce the problem this module exists to stop.
        _ => log::LevelFilter::Warn,
    }
}

/// Installs the logger. Safe to call more than once; the second call does
/// nothing.
///
/// Never fails in a way a caller should act on - a program that would not start
/// because it could not install a logger would be worse than one that runs
/// quietly.
pub fn install() {
    let level = level();
    if log::set_boxed_logger(Box::new(Stderr { level })).is_ok() {
        log::set_max_level(level);
    }
}

/// Also says it where a windowed program can be heard.
///
/// `main.rs` sets `windows_subsystem = "windows"`, so `files.exe` is launched
/// with no console attached and everything written to stderr goes nowhere. That
/// is exactly the silence this module exists to break, so on Windows the line
/// goes to the debugger channel as well, where DebugView and any attached
/// debugger will show it without the program needing a terminal.
#[cfg(windows)]
fn debugger(line: &str) {
    use windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringW;

    let mut wide: Vec<u16> = line.encode_utf16().collect();
    wide.push(u16::from(b'\n'));
    wide.push(0);
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call,
    // which is the whole of this function's contract. `OutputDebugStringW` only
    // reads it.
    unsafe { OutputDebugStringW(wide.as_ptr()) };
}

#[cfg(not(windows))]
fn debugger(line: &str) {
    // Every other platform this builds on has a console, so stderr is already
    // somewhere a person can read.
    let _ = line;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Taken for the whole of any test that writes `FILES_LOG`.
    ///
    /// The environment is per *process*, and `cargo test` runs a binary's
    /// tests on threads of that one process - so the three tests below were
    /// overwriting each other's variable and failing at random, roughly one
    /// run in five. Their `SAFETY` notes each claimed nothing else in the
    /// binary touched `FILES_LOG`, which was true of every caller except the
    /// other two tests in this module.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Sets `FILES_LOG`, asks what level that means, and puts it back.
    ///
    /// One place doing the unsafe pair, so the lock cannot be forgotten at a
    /// fourth call site.
    fn level_for(text: Option<&str>) -> log::LevelFilter {
        // A poisoned lock means another of these tests panicked mid-assertion.
        // That is a failure to report, not a reason to stop taking the lock.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: `set_var` and `remove_var` are unsafe because another thread
        // reading the environment at the same instant is a data race. The lock
        // above is what makes that impossible: every write to `FILES_LOG` in
        // this binary happens here, and `level()` is called before the guard
        // is dropped.
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

    /// The default has to be loud enough to carry the message that was missed.
    #[test]
    fn warnings_are_printed_by_default() {
        assert_eq!(level_for(None), log::LevelFilter::Warn);
    }

    /// A typo must not turn logging off. Anything unrecognised falls back to
    /// the default rather than to silence.
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

    /// Installing twice is what happens when a test and a binary both do it.
    #[test]
    fn installing_more_than_once_is_harmless() {
        install();
        install();
    }
}
