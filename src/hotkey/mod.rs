//! Summoning the panel with a global hotkey.
//!
//! One thread for the life of the process, like everything else in
//! [`crate::app::actors`]. It owns a `GetMessageW` pump and - crucially -
//! **every call that touches a window**. That is not tidiness. Windows grants
//! the right to change the foreground window to the process that just received
//! a `WM_HOTKEY`, and only while it is handling it, so `SetForegroundWindow`
//! works from inside that handler and essentially nowhere else. A request from
//! the drawing thread therefore arrives as a `PostThreadMessageW` rather than
//! as a shared flag, and the window state lives in a plain struct on that
//! thread's stack with no synchronisation at all.
//!
//! There is **no window and no `WndProc` here**. `RegisterHotKey` with a null
//! window delivers `WM_HOTKEY` as a *thread* message, so a bare message pump
//! receives it - which means this module defines no `extern "system"` function
//! at all. That matters more than it sounds like it should, because `panic =
//! "unwind"` is kept deliberately (see `Cargo.toml`) and unwinding out of a
//! callback across the foreign boundary is undefined behaviour. The hazard is
//! removed by construction rather than by remembering to wrap things.
//!
//! # The three traps
//!
//! 1. **Foreground rights belong to the keypress, not to the process.** See
//!    above. This is why the drawing thread asks rather than acts.
//! 2. **`PostThreadMessageW` has nothing to post to until a queue exists.** A
//!    thread that has never called into user32 has no message queue and the
//!    post fails with `ERROR_INVALID_THREAD_ID`. The pump forces one into
//!    existence *before* it publishes its thread id; a hide lost there would
//!    leave the panel on screen with no way to shift it.
//! 3. **`MOD_NOREPEAT` is not optional.** Without it, holding the chord down
//!    summons and dismisses at the keyboard's autorepeat rate. See [`spec`].
//!
//! # Two traps that stopped existing
//!
//! This module used to summon the *terminal* the program was running in, and
//! two of its five traps were consequences of that. `GetConsoleWindow` lies
//! under Windows Terminal - it returns the pseudo-console's hidden window, and
//! `SetWindowPos` on that succeeds, moves nothing and reports no error - so
//! `win_hwnd` walked the process table and the Z-order to find the real one.
//! And a terminal's client area is not a whole number of character cells, so
//! [`geometry`] fitted `client_px = cell_px * cells + chrome_px` from two
//! samples to work out how big to make it.
//!
//! The window is this program's own now. There is nothing to find, nothing to
//! calibrate, and no way for either answer to be wrong. Both traps, and the
//! five hundred lines that handled them, are gone.

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

/// Everything the platform cannot do here.
///
/// Deliberately silent rather than apologetic: on a machine with no global
/// hotkeys the user did not ask for one, the default simply happens to be set,
/// and a warning about it would be noise on every start.
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

    /// The panel's window, which off Windows nothing ever asks about.
    #[derive(Debug, Default)]
    pub struct Panel;

    impl Panel {
        pub fn new() -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self)
        }

        pub fn publish(&self, _hwnd: isize) {}

        /// Accepted and discarded off Windows, for the reason the whole stub
        /// exists: `gui` names this unconditionally, and a method that vanishes
        /// on another platform takes the panel's entire test suite with it.
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

/// Starts the listener.
///
/// `Ok(None)` when the hotkey is switched off, when the platform has none, or
/// when the chord was refused - none of which may stop the program starting. A
/// search tool that will not run because another program already owns a key
/// combination is a worse tool than one without the shortcut, so a clash is
/// reported through `events` as [`crate::app::event::HotkeyMsg::Unavailable`]
/// and startup carries on.
pub fn spawn(
    spec: HotkeySpec,
    events: Events,
    panel: std::sync::Arc<Panel>,
) -> std::io::Result<Option<HotkeyThread>> {
    imp::spawn(spec, events, panel)
}

/// What `--doctor` reports about the overlay.
///
/// Assembled without starting the thread: where nothing already knows the
/// answer, the chord is registered and immediately released, so asking the
/// question costs nothing and changes nothing.
///
/// # Why the caller can supply the answer
///
/// `RegisterHotKey` is *per-thread*. Inside the running program the panel's
/// hotkey thread already owns the chord, and a second registration of the
/// same chord from any other thread fails with `ERROR_HOTKEY_ALREADY_
/// REGISTERED` - including this one. So probing from inside the program told
/// every user with a perfectly working hotkey that their chord was taken,
/// and named their own program as the thief.
///
/// The fix is not a cleverer probe. It is that the program already knows:
/// [`spawn`] returns whether the claim was accepted, and where that answer
/// exists it is the only correct one. `known` is that answer. Where there is
/// none - `--doctor` on the command line, or a settings window running as
/// its own process with no panel behind it - the registration is attempted
/// and the answer is honest, because nothing in that process owns the key.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    /// The configured chord, spelled the way a person would write it, or
    /// `None` when it is switched off.
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
/// `known` is what [`spawn`] said, where the caller has it. See [`Probe`].
pub fn probe(spec: HotkeySpec, known: Option<Result<(), String>>) -> Probe {
    imp::probe(spec, known)
}

/// How long the hotkey thread is given to stop.
///
/// Shares the budget every other worker uses. Unlike the input thread this one
/// is genuinely joinable - it parks in `GetMessageW`, and a posted `WM_QUIT`
/// is a wakeup that reaches it - so the budget is a formality rather than a
/// deadline it is expected to miss.
pub const SHUTDOWN_BUDGET: Duration = crate::config::SHUTDOWN_JOIN_BUDGET;
