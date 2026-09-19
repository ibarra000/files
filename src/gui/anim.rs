//! The one thing on the panel that still takes time, and it is not decoration.
//!
//! Nothing here reads a clock, opens a window or knows the toolkit exists.
//! [`Motion::advance`] takes the seconds since the last frame and returns what
//! to draw - the same bargain [`crate::app::state::AppState::update`] makes
//! with `Instant`, and for the same reason: behaviour that can only be checked
//! by watching it is behaviour nobody checks.
//!
//! # Why there is almost nothing here now
//!
//! This file used to carry the panel's whole entrance: a presence scalar eased
//! at the point of reading, a height tween, a cross-fade between bodies, a
//! sliding selection, and an indeterminate progress bar. All of it worked, and
//! all of it was a claim about nothing anybody had asked to be told.
//!
//! The cost was not the curves, it was what drove them. The panel *is* the
//! window - [`crate::gui::overlay::show`] paints into `ui.max_rect()` - so
//! every frame of a height transition was a `SetWindowPos` and a swapchain
//! reconfigure. Sixty a second, to move a list by forty points.
//!
//! So the summon and the dismissal are instant, the height snaps, the body
//! changes in one pass, the selection appears where it belongs, and a search
//! in flight says so in words rather than by moving.
//!
//! What survives is [`Hold`]: a body that is empty for one frame on its way to
//! a perfectly good list must not be drawn as empty, and that is the only
//! reason this panel ever asks for a frame it was not given an event for.
//!
//! The curves are in this repository's history.

/// How long the body must go on wanting to be empty before it is allowed to.
///
/// The local matcher answers in well under a millisecond, but it answers on
/// another thread, so the frame that asks the question is drawn before the
/// answer arrives. Anything that empties the result list therefore empties it
/// on the way to a perfectly good result set - and an empty body is not a
/// shorter list, it is a *different* body: three hundred points shorter, with
/// the footer chips re-flowing around it.
///
/// This matters more now, not less. The cross-fade used to dissolve a
/// transient empty over a hundred and ten milliseconds and dissolve it back;
/// without it the same frame is an instant snap from four hundred points to a
/// hundred and sixty and back again. The one thing this guards against is the
/// thing that got louder.
///
/// `on_input_changed` no longer clears the list, so this is the backstop
/// rather than the fix. It exists because there is more than one way to end up
/// with no rows for a frame - a snapshot republishing under a live panel, a
/// verification replacing the list - and none of them should be able to make
/// the panel flinch.
///
/// Seven frames at sixty: long enough to swallow a transient, far too short to
/// delay a genuine "nothing matched" by anything anybody can see.
pub const EMPTY_HOLD: f32 = 0.120;

/// The most time one frame may claim.
///
/// The first frame after an hour in the notification area reports the hour.
/// Without a ceiling that one frame runs out [`EMPTY_HOLD`] before the body has
/// been drawn once - so the one thing this file still does would be skipped by
/// the frame that was meant to start it.
pub const MAX_DT: f32 = 0.100;

/// Which body the panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// The codes used before, while the Up arrow is browsing them. Never on
    /// screen otherwise.
    Recent,
    Results,
    /// A reason there are none.
    Empty,
    /// The drive picker, which is the one thing that still borrows the body.
    /// Help used to be here too, and is a window of its own now.
    Shares,
}

/// Where the panel is in its life.
///
/// Two states, where there were four. `Summoning` and `Dismissing` were the
/// entrance and the exit; both are now instant, so there is no longer any
/// moment at which the panel is partly present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Hidden,
    Shown,
}

/// Refuses to let the body go empty until it has meant it for a moment.
///
/// Sits between [`Motion::retarget`] and what is drawn, so the transient never
/// reaches the screen. The last thing in this file that takes time, and the
/// only reason the panel still asks for a frame it was not given an event for.
#[derive(Debug, Clone, Copy)]
struct Hold {
    granted: Target,
    /// How long an ungranted `Empty` has been asked for, in seconds.
    waited: f32,
}

impl Hold {
    fn new(granted: Target) -> Self {
        Self {
            granted,
            waited: 0.0,
        }
    }

    fn admit(&mut self, want: Target, dt: f32) -> Target {
        // Already empty, or not asking to be: nothing to hold.
        if want.content != Content::Empty || self.granted.content == Content::Empty {
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

    /// Whether a decision is still pending, and therefore whether another
    /// frame is owed. Without this the loop parks on the frame that asked to
    /// go empty and the panel never gets there.
    fn waiting(self) -> bool {
        self.waited > 0.0
    }
}

/// Everything the view measured this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub height: f32,
    pub content: Content,
    /// Top of the selected row, in points from the top of the list, or `None`
    /// when nothing is selected.
    pub selection_y: Option<f32>,
}

/// What to draw. Pure data: no toolkit type appears here, so a test can assert
/// on it with no window, no context and no GPU.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Visual {
    pub height: f32,
    pub content: Content,
    pub selection_y: Option<f32>,
    /// Whether the pane beside the list is in play.
    ///
    /// Carried on the frame rather than passed to `overlay::show` beside it,
    /// because it is decided by the same thing that decides the height and at
    /// the same moment - and a renderer that had to be told twice is a renderer
    /// that can be told two different things. Set by
    /// [`crate::gui::frame::Frame::advance`]; never animated, because a width
    /// that eased would be a `SetWindowPos` per frame of the ease.
    pub layout: crate::gui::theme::Layout,
}

/// The panel's motion, such as it is.
#[derive(Debug, Clone, Copy)]
pub struct Motion {
    phase: Phase,
    /// What the view last asked for, and the gate it has to get through.
    want: Target,
    hold: Hold,
}

impl Default for Motion {
    fn default() -> Self {
        Self::new()
    }
}

impl Motion {
    pub fn new() -> Self {
        let start = Target {
            height: 0.0,
            content: Content::Recent,
            selection_y: None,
        };
        Self {
            phase: Phase::Hidden,
            want: start,
            hold: Hold::new(start),
        }
    }

    /// Told what the world looks like, before being told how much time passed.
    ///
    /// One retarget point and one advance point, so the order cannot vary
    /// between call sites. Recorded rather than applied, because the gate on an
    /// empty body needs to know how long one has been asked for before it can
    /// decide whether to grant it, and only `advance` is told about time.
    pub fn retarget(&mut self, target: Target) {
        self.want = target;
    }

    /// `layout` is carried through rather than decided here: it is not
    /// animated - a width that eased would be one `SetWindowPos` per frame of
    /// the ease - but it belongs on the `Visual`, which is the whole of what a
    /// frame is drawn from. Passing it in keeps that true without giving this
    /// module an opinion about monitors.
    pub fn advance(&mut self, dt: f32, layout: crate::gui::theme::Layout) -> Visual {
        // A non-finite `dt` would poison the gate permanently - a body waiting
        // to go empty with no frame able to let it. The toolkit should never
        // hand one over; the cost of not depending on that is one comparison.
        let dt = if dt.is_finite() {
            dt.clamp(0.0, MAX_DT)
        } else {
            0.0
        };

        let target = self.hold.admit(self.want, dt);

        Visual {
            height: target.height,
            content: target.content,
            selection_y: target.selection_y,
            layout,
        }
    }

    /// Immediate.
    ///
    /// The panel is the window, and the hotkey thread has already put the
    /// window on screen by the time this is called - so there was never
    /// anything here but a curve to run alongside that.
    pub fn summon(&mut self) {
        // Nothing is inherited from the last time the panel was up: a held body
        // from a previous session would be a hundred and twenty milliseconds of
        // somebody else's search. Only from hidden, though - a hotkey pressed
        // while the panel is already up is not an arrival at all, and must not
        // reset the gate under a live query.
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

    /// Whether another frame is owed.
    ///
    /// The single guard on "idle costs nothing": the shell asks for a repaint
    /// if and only if this is true, so anything in here that forgets to finish
    /// is a process that renders in the notification area for ever. With the
    /// transitions gone there are exactly two things that can still owe a
    /// frame, and it is a body waiting to be allowed to go empty.
    pub fn is_animating(self) -> bool {
        match self.phase {
            Phase::Hidden => false,
            Phase::Shown => self.hold.waiting(),
        }
    }
}
