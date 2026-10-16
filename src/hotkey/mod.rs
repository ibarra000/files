//! Global hotkey that summons the panel.
//!
//! One long-lived thread runs a `GetMessageW` pump and owns every call that
//! touches the panel window. Windows grants foreground rights only while a
//! `WM_HOTKEY` is being handled, so other threads post requests with
//! `PostThreadMessageW` instead of acting directly.
//!
//! `RegisterHotKey` with a null window delivers `WM_HOTKEY` as a thread
//! message, so there is no `WndProc` and no `extern "system"` callback. This
//! keeps `panic = "unwind"` (see `Cargo.toml`) from unwinding across FFI.
//!
//! # Constraints
//!
//! 1. Foreground rights belong to the keypress, so the drawing thread asks
//!    rather than acts.
//! 2. `PostThreadMessageW` fails with `ERROR_INVALID_THREAD_ID` until the
//!    thread has a message queue. The pump creates one before publishing its
//!    thread id; a lost hide would leave the panel stuck on screen.
//! 3. `MOD_NOREPEAT` is required, or holding the chord toggles at the
//!    autorepeat rate. See [`spec`].

pub mod geometry;
pub mod spec;

use std::time::Duration;

use crate::app::event::Events;
use spec::HotkeySpec;

#[cfg(windows)]
mod win;
#[cfg(windows)]
mod win_hwnd;

#[cfg(windows)]
use win as imp;

/// No-op implementation for platforms without global hotkeys.
///
/// Silent, since the hotkey is on by default and a warning would appear on
/// every start.
#[cfg(not(windows))]
mod imp {
    use super::{Events, HotkeySpec};
    use std::time::Duration;

    pub struct HotkeyThread;

    impl HotkeyThread {
        pub fn dismiss(&self, _handing_over: bool) {}
        pub fn hide(&self) {}
        pub fn summon(&self) {}
        pub fn shutdown(&mut self, _budget: Duration) -> bool {
            true
        }
    }

    /// Stand-in for the panel window handle.
    #[derive(Debug, Default)]
    pub struct Panel;

    impl Panel {
        pub fn new() -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self)
        }

        pub fn publish(&self, _hwnd: isize) {}

        /// Discards the position; exists because `gui` calls it on every
        /// platform.
        pub fn remember(&self, _at: Option<(i32, i32)>) {}

        pub fn remembered(&self) -> Option<(i32, i32)> {
            None
        }
    }

    pub fn spawn(
        _spec: HotkeySpec,
        _events: Events,
        _panel: std::sync::Arc<Panel>,
    ) -> std::io::Result<Option<HotkeyThread>> {
        Ok(None)
    }

    pub fn probe(_spec: HotkeySpec, _known: Option<Result<(), String>>) -> super::Probe {
        super::Probe::unsupported()
    }
}

pub use imp::{HotkeyThread, Panel};

/// Starts the hotkey thread.
///
/// Returns `Ok(None)` when the hotkey is off, unsupported, or refused; none of
/// these stop startup. A clash is reported through `events` as
/// [`crate::app::event::HotkeyMsg::Unavailable`].
pub fn spawn(
    spec: HotkeySpec,
    events: Events,
    panel: std::sync::Arc<Panel>,
) -> std::io::Result<Option<HotkeyThread>> {
    imp::spawn(spec, events, panel)
}

/// The overlay section of the `--doctor` report.
///
/// Built without starting the hotkey thread. When no result is known, the
/// chord is registered and immediately released.
///
/// Registration is per thread, so inside the running program a test
/// registration fails with `ERROR_HOTKEY_ALREADY_REGISTERED` because the
/// hotkey thread already holds the chord. Callers that have the result from
/// [`spawn`] must pass it as `known`.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    /// The configured chord in display form, or `None` when switched off.
    pub chord: Option<String>,
    /// Whether the platform has global hotkeys at all.
    pub supported: bool,
    /// `None` when registration was not attempted.
    pub registered: Option<Result<(), String>>,
    /// Where the panel would be placed, or `None` where no monitor answered.
    pub window: Option<Result<String, String>>,
}

impl Probe {
    pub fn unsupported() -> Self {
        Self::default()
    }
}

/// Reports on the overlay without starting it.
///
/// `known` is the result from [`spawn`], if the caller has it. See [`Probe`].
pub fn probe(spec: HotkeySpec, known: Option<Result<(), String>>) -> Probe {
    imp::probe(spec, known)
}

/// How long the hotkey thread is given to stop.
///
/// A posted `WM_QUIT` wakes its `GetMessageW`, so it normally exits well
/// within this budget.
pub const SHUTDOWN_BUDGET: Duration = crate::config::SHUTDOWN_JOIN_BUDGET;
