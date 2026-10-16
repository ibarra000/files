//! The panel's remaining timed behaviour: a short hold before the body goes empty.
//!
//! Pure and clock-free: [`Motion::advance`] takes the seconds since the last
//! frame and returns the body to draw, so the behaviour is testable without a
//! window. Summon, dismissal, height and selection changes are all instant,
//! because the panel is the window and animating it costs a `SetWindowPos` and
//! a swapchain reconfigure per frame.

/// How long, in seconds, the body must keep asking to be empty before it is.
///
/// The matcher answers on another thread, so the frame that issues a query is
/// drawn before its results arrive. Without this hold, any transient empty list
/// (a snapshot republish, a verification pass) would snap the panel to the
/// empty body and back. About seven frames at 60 Hz: long enough to absorb a
/// transient, too short to visibly delay a genuine "nothing matched".
pub const EMPTY_HOLD: f32 = 0.120;

/// The longest time step one frame may claim, in seconds.
///
/// The first frame after a long idle reports the whole idle time; without this
/// cap it would exhaust [`EMPTY_HOLD`] before the body was drawn once.
pub const MAX_DT: f32 = 0.100;

/// Which body the panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// Previously used codes, shown only while the Up arrow is browsing them.
    Recent,
    /// The configured shortcuts, shown in an empty box when any exist.
    Aliases,
    Results,
    /// An explanation of why there are no results. An untouched panel shows no
    /// body here, because `view::empty` returns no blocks for `NoQuery`.
    Empty,
    /// The drive picker.
    Shares,
}

/// Whether the panel is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Hidden,
    Shown,
}

/// Delays a switch to an empty body until it has been requested for
/// [`EMPTY_HOLD`].
///
/// Sits between [`Motion::retarget`] and what is drawn, and is the only reason
/// the panel requests a frame without an input event.
#[derive(Debug, Clone, Copy)]
struct Hold {
    granted: Content,
    /// How long an ungranted `Empty` has been requested, in seconds.
    waited: f32,
}

impl Hold {
    fn new(granted: Content) -> Self {
        Self {
            granted,
            waited: 0.0,
        }
    }

    fn admit(&mut self, want: Content, dt: f32) -> Content {
        // Prevents the body flapping between a list and a message when the
        // field is cleared and retyped.
        let settling = |c: Content| c == Content::Empty;
        // Already empty, or not asking to be: nothing to hold.
        if !settling(want) || settling(self.granted) {
            self.waited = 0.0;
            self.granted = want;
            return want;
        }
        self.waited += dt;
        if self.waited >= EMPTY_HOLD {
            self.waited = 0.0;
            self.granted = want;
            return want;
        }
        self.granted
    }

    /// Whether a decision is pending and another frame is therefore needed.
    /// Without this the event loop parks and the panel never goes empty.
    fn waiting(self) -> bool {
        self.waited > 0.0
    }
}

/// The panel's phase and the empty-body hold.
#[derive(Debug, Clone, Copy)]
pub struct Motion {
    phase: Phase,
    /// The body the view last requested.
    want: Content,
    hold: Hold,
}

impl Default for Motion {
    fn default() -> Self {
        Self::new()
    }
}

impl Motion {
    pub fn new() -> Self {
        let start = Content::Recent;
        Self {
            phase: Phase::Hidden,
            want: start,
            hold: Hold::new(start),
        }
    }

    /// Records the requested body; [`Motion::advance`] applies it.
    ///
    /// Recorded rather than applied because only `advance` knows elapsed time,
    /// which the hold needs. Call this before `advance` each frame.
    pub fn retarget(&mut self, want: Content) {
        self.want = want;
    }

    pub fn advance(&mut self, dt: f32) -> Content {
        // A non-finite `dt` would leave the hold permanently pending.
        let dt = if dt.is_finite() {
            dt.clamp(0.0, MAX_DT)
        } else {
            0.0
        };

        self.hold.admit(self.want, dt)
    }

    /// Marks the panel shown. Immediate; the hotkey thread has already shown
    /// the window.
    pub fn summon(&mut self) {
        // Reset the hold only when arriving from hidden, so a held body from a
        // previous session is not shown. A hotkey press while already shown
        // must not reset it under a live query.
        if self.phase == Phase::Hidden {
            self.hold = Hold::new(self.want);
        }
        self.phase = Phase::Shown;
    }

    pub fn dismiss(&mut self) {
        self.phase = Phase::Hidden;
    }

    pub fn phase(self) -> Phase {
        self.phase
    }

    pub fn is_hidden(self) -> bool {
        self.phase == Phase::Hidden
    }

    /// Whether another frame is needed.
    ///
    /// The shell requests a repaint if and only if this is true, so anything
    /// here that never finishes keeps the idle process rendering.
    pub fn is_animating(self) -> bool {
        match self.phase {
            Phase::Hidden => false,
            Phase::Shown => self.hold.waiting(),
        }
    }
}
