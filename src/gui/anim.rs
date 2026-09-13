//! Transitions, as a pure function of elapsed time.
//!
//! Nothing here reads a clock, opens a window or knows the toolkit exists.
//! [`Motion::advance`] takes the seconds since the last frame and returns what
//! to draw - the same bargain [`crate::app::state::AppState::update`] makes
//! with `Instant`, and for the same reason: a transition that can only be
//! checked by watching it is a transition nobody checks.
//!
//! # One scalar, eased at the point of reading
//!
//! Presence is a single value moving linearly between 0 and 1, and every
//! curve below is a function of *that* rather than of a per-phase timer. Two
//! things fall out of it for free, and both are the difference between a
//! transition and a flicker:
//!
//! * a dismissal interrupting a summon leaves from where the panel actually
//!   is, rather than snapping to fully-present and then leaving;
//! * it leaves *proportionally* - from 0.4 it takes 0.4 of a dismissal - so a
//!   panel barely arrived does not linger on its way out.
//!
//! The entrance curve run backwards is also the correct exit: read forwards it
//! decelerates into place, read backwards it accelerates away.

/// How long the panel takes to arrive.
pub const SUMMON: f32 = 0.140;
/// And to leave. Shorter, because going away should not be something you wait
/// for.
pub const DISMISS: f32 = 0.090;
/// How long the panel takes to settle at a new height.
pub const HEIGHT: f32 = 0.160;
/// A cross-fade between two bodies.
pub const CROSS: f32 = 0.110;
/// The selection sliding from one row to the next.
pub const SLIDE: f32 = 0.090;

/// How long something must be busy before it is worth saying so.
pub const LOAD_DELAY: f32 = 0.250;
/// And how long the bar stays once it has appeared.
///
/// Without this, work finishing at 260ms shows a bar for ten milliseconds -
/// which is the flash the delay exists to prevent, arriving one frame later
/// and one frame smaller.
pub const LOAD_MIN_VISIBLE: f32 = 0.400;
/// One pass of the indeterminate sweep.
pub const LOAD_SWEEP: f32 = 1.100;

/// How far the panel rises as it arrives, in points.
pub const RISE: f32 = 8.0;
/// And how much smaller it starts.
pub const SCALE_FROM: f32 = 0.985;

/// How long the body must go on wanting to be empty before it is allowed to.
///
/// The local matcher answers in well under a millisecond, but it answers on
/// another thread, so the frame that asks the question is drawn before the
/// answer arrives. Anything that empties the result list therefore empties it
/// on the way to a perfectly good result set - and an empty body is not a
/// shorter list, it is a *different* body: three hundred points shorter,
/// reached through a cross-fade, with the footer chips re-flowing around it.
///
/// `on_input_changed` no longer clears the list, so this is the backstop
/// rather than the fix. It exists because there is more than one way to end up
/// with no rows for a frame - a snapshot republishing under a live panel, a
/// verification replacing the list - and none of them should be able to make
/// the panel flinch.
///
/// Seven frames at sixty. It delays a genuine "nothing matched" by less than
/// the cross-fade that then plays it in, which is to say by nothing anybody
/// can see.
pub const EMPTY_HOLD: f32 = 0.120;

/// The most time one frame may claim.
///
/// The first frame after an hour in the notification area reports the hour.
/// Without a ceiling every transition is over before the monitor has drawn it
/// once, which looks exactly like having no transitions at all.
pub const MAX_DT: f32 = 0.100;

#[inline]
pub fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t;
    1.0 - u * u * u
}

#[inline]
pub fn ease_out_quint(t: f32) -> f32 {
    let u = 1.0 - t;
    1.0 - u * u * u * u * u
}

#[inline]
pub fn ease_in_out_cubic(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let u = -2.0 * t + 2.0;
        1.0 - u * u * u / 2.0
    }
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Which body the panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// Nothing typed: recent codes, or the first-run block if there are none.
    Recent,
    Results,
    /// A reason there are none.
    Empty,
    /// The drive picker, which is the one thing that still borrows the body.
    /// Help used to be here too, and is a window of its own now.
    Shares,
}

/// Where the panel is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Hidden,
    Summoning,
    Shown,
    Dismissing,
}

/// A value on its way to another value.
#[derive(Debug, Clone, Copy)]
pub struct Tween {
    from: f32,
    to: f32,
    cur: f32,
    t: f32,
    dur: f32,
    eps: f32,
}

impl Tween {
    pub fn new(value: f32, dur: f32, eps: f32) -> Self {
        Self {
            from: value,
            to: value,
            cur: value,
            t: 1.0,
            dur,
            eps,
        }
    }

    pub fn retarget(&mut self, to: f32) {
        // `eps` is what stops half a point of layout jitter restarting the
        // animation every frame and pinning the repaint request on for ever.
        if (to - self.to).abs() <= self.eps {
            return;
        }
        // From where it *is*, not from where it was aimed. Restarting from
        // `from` is how a panel already halfway to 300 points snaps back to
        // 200 before setting off for 400.
        self.from = self.cur;
        self.to = to;
        self.t = 0.0;
    }

    pub fn advance(&mut self, dt: f32) {
        if self.t >= 1.0 {
            return;
        }
        self.t = (self.t + dt / self.dur).min(1.0);
        self.cur = lerp(self.from, self.to, ease_in_out_cubic(self.t));
    }

    pub fn snap(&mut self) {
        self.cur = self.to;
        self.from = self.to;
        self.t = 1.0;
    }

    pub fn value(self) -> f32 {
        self.cur
    }

    pub fn target(self) -> f32 {
        self.to
    }

    pub fn running(self) -> bool {
        self.t < 1.0
    }
}

/// The selection highlight: a tween that knows the difference between moving
/// and appearing.
#[derive(Debug, Clone, Copy)]
struct Slide {
    inner: Tween,
    armed: bool,
}

impl Slide {
    fn new() -> Self {
        Self {
            inner: Tween::new(0.0, SLIDE, 0.25),
            armed: false,
        }
    }

    fn retarget(&mut self, to: Option<f32>) {
        match to {
            None => self.armed = false,
            Some(y) if !self.armed => {
                // The first selection since there was none has to *appear*
                // where it belongs. A plain tween would fly it in from the top
                // of the list every time a search returned.
                self.armed = true;
                self.inner = Tween::new(y, SLIDE, 0.25);
            }
            Some(y) => self.inner.retarget(y),
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn advance(&mut self, dt: f32) {
        if self.armed {
            self.inner.advance(dt);
        }
    }

    fn value(self) -> Option<f32> {
        self.armed.then(|| self.inner.value())
    }

    fn running(self) -> bool {
        self.armed && self.inner.running()
    }
}

/// What the two bodies look like mid-change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContentFade {
    pub leaving: Option<Content>,
    pub leaving_alpha: f32,
    pub showing: Content,
    pub showing_alpha: f32,
}

#[derive(Debug, Clone, Copy)]
struct Cross {
    showing: Content,
    leaving: Option<Content>,
    t: f32,
}

impl Cross {
    fn retarget(&mut self, next: Content) {
        if next == self.showing {
            return;
        }
        // Changing back to the thing that is still fading out. There is
        // nothing to cross-fade: what is on screen already *is* `next`, so the
        // honest answer is to cancel the fade rather than start a second one.
        //
        // Without this the result list dissolved into itself on every
        // keystroke. The body flipped to `Empty` for the one frame between the
        // matcher being asked and answering, and flipped straight back - which
        // left `leaving == showing == Results` with `t` mid-flight, and
        // `visual` below duly faded the list out over 45% of the duration and
        // back in over the remaining 55%, with no overlap. A hundred and ten
        // milliseconds of a list disappearing and returning, per character,
        // with nothing about it having changed.
        if self.leaving == Some(next) {
            self.leaving = None;
            self.t = 1.0;
            self.showing = next;
            return;
        }
        // Mid-flight the thing actually on screen is `leaving`; `showing` has
        // barely begun to appear. Promoting it would cross-fade *from*
        // something nobody has seen, which reads as a flicker rather than as a
        // change. Past halfway that is no longer true.
        if self.leaving.is_none() || self.t >= 0.5 {
            self.leaving = Some(self.showing);
            self.t = 0.0;
        }
        self.showing = next;
    }

    fn advance(&mut self, dt: f32) {
        if self.leaving.is_none() {
            return;
        }
        self.t = (self.t + dt / CROSS).min(1.0);
        if self.t >= 1.0 {
            self.leaving = None;
        }
    }

    fn visual(self) -> ContentFade {
        match self.leaving {
            None => ContentFade {
                leaving: None,
                leaving_alpha: 0.0,
                showing: self.showing,
                showing_alpha: 1.0,
            },
            // Out over the first 45%, in over the last 55%, with no overlap.
            // Text dissolving through text is unreadable: help and results at
            // half opacity each is a smear, not a transition.
            Some(prev) => ContentFade {
                leaving: Some(prev),
                leaving_alpha: (1.0 - self.t / 0.45).clamp(0.0, 1.0),
                showing: self.showing,
                showing_alpha: ((self.t - 0.45) / 0.55).clamp(0.0, 1.0),
            },
        }
    }

    fn running(self) -> bool {
        self.leaving.is_some()
    }
}

/// The indeterminate progress bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    pub alpha: f32,
    /// Where the sweep has got to, 0 to 1.
    pub sweep: f32,
}

#[derive(Debug, Clone, Copy)]
struct Loading {
    busy_for: f32,
    shown_for: f32,
    latched: bool,
    sweep: f32,
    alpha: Tween,
}

impl Loading {
    fn new() -> Self {
        Self {
            busy_for: 0.0,
            shown_for: 0.0,
            latched: false,
            sweep: 0.0,
            alpha: Tween::new(0.0, 0.12, 0.001),
        }
    }

    fn advance(&mut self, dt: f32, busy: bool) {
        if busy {
            self.busy_for += dt;
        } else {
            self.busy_for = 0.0;
        }

        if !self.latched && self.busy_for >= LOAD_DELAY {
            self.latched = true;
            self.shown_for = 0.0;
        }
        if self.latched {
            self.shown_for += dt;
            self.sweep = (self.sweep + dt / LOAD_SWEEP).fract();
            if !busy && self.shown_for >= LOAD_MIN_VISIBLE {
                self.latched = false;
            }
        }
        self.alpha.retarget(if self.latched { 1.0 } else { 0.0 });
        self.alpha.advance(dt);
    }

    fn visual(self) -> Option<Bar> {
        (self.alpha.value() > 0.001).then(|| Bar {
            alpha: self.alpha.value(),
            sweep: self.sweep,
        })
    }

    fn running(self) -> bool {
        self.latched || self.alpha.running()
    }
}

/// Refuses to let the body go empty until it has meant it for a moment.
///
/// Sits between [`Motion::retarget`] and the tweens, so every one of them is
/// spared the transient. Only the *body* is held: whether something is busy is
/// a claim about the program, not about what is on screen, and holding it back
/// would delay the progress bar for no reason.
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
        Target {
            busy: want.busy,
            ..self.granted
        }
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
    pub busy: bool,
}

/// What to draw. Pure data: no toolkit type appears here, so a test can assert
/// on it with no window, no context and no GPU.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Visual {
    pub alpha: f32,
    /// Points of downward displacement from rest. Zero is home.
    pub dy: f32,
    pub scale: f32,
    pub height: f32,
    pub content: ContentFade,
    pub selection_y: Option<f32>,
    pub loading: Option<Bar>,
}

/// The panel's motion.
#[derive(Debug, Clone, Copy)]
pub struct Motion {
    phase: Phase,
    /// Presence, linear in time. See the module note.
    a: f32,
    height: Tween,
    cross: Cross,
    sel: Slide,
    load: Loading,
    busy: bool,
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
            busy: false,
        };
        Self {
            phase: Phase::Hidden,
            a: 0.0,
            height: Tween::new(0.0, HEIGHT, 0.5),
            cross: Cross {
                showing: Content::Recent,
                leaving: None,
                t: 1.0,
            },
            sel: Slide::new(),
            load: Loading::new(),
            busy: false,
            want: start,
            hold: Hold::new(start),
        }
    }

    /// Told what the world looks like, before being told how much time passed.
    ///
    /// One retarget point and one advance point, so the order cannot vary
    /// between call sites. Recorded rather than applied, because the gate on
    /// an empty body needs to know how long one has been asked for before it
    /// can decide whether to grant it, and only `advance` is told about time.
    pub fn retarget(&mut self, target: Target) {
        self.want = target;
    }

    pub fn advance(&mut self, dt: f32) -> Visual {
        // A non-finite `dt` would poison every accumulator in here permanently
        // - a panel stuck half-arrived, with no frame able to unstick it. The
        // toolkit should never hand one over; the cost of not depending on
        // that is one comparison.
        let dt = if dt.is_finite() {
            dt.clamp(0.0, MAX_DT)
        } else {
            0.0
        };

        let target = self.hold.admit(self.want, dt);
        self.height.retarget(target.height);
        self.cross.retarget(target.content);
        self.sel.retarget(target.selection_y);
        self.busy = target.busy;

        match self.phase {
            Phase::Summoning => {
                self.a = (self.a + dt / SUMMON).min(1.0);
                if self.a >= 1.0 {
                    self.phase = Phase::Shown;
                }
            }
            Phase::Dismissing => {
                self.a = (self.a - dt / DISMISS).max(0.0);
                if self.a <= 0.0 {
                    self.phase = Phase::Hidden;
                }
            }
            Phase::Hidden | Phase::Shown => {}
        }

        // The panel does not grow while it is arriving. The entrance already
        // carries the motion, and something that rises *and* grows reads as
        // two things happening to one object rather than one thing happening.
        if self.phase == Phase::Shown {
            self.height.advance(dt);
        } else {
            self.height.snap();
        }

        self.cross.advance(dt);
        self.sel.advance(dt);
        self.load.advance(dt, self.busy);

        let motion = ease_out_cubic(self.a);
        Visual {
            // Ahead of the motion, deliberately: the panel should be legible
            // before it has finished moving, rather than arriving and only
            // then appearing.
            alpha: ease_out_quint(self.a),
            dy: RISE * (1.0 - motion),
            scale: lerp(SCALE_FROM, 1.0, motion),
            height: self.height.value(),
            content: self.cross.visual(),
            selection_y: self.sel.value(),
            loading: self.load.visual(),
        }
    }

    /// From wherever it is.
    ///
    /// A hotkey pressed forty milliseconds into a dismissal resumes from where
    /// the panel actually is; it does not restart, and it does not flash.
    pub fn summon(&mut self) {
        // Arriving is an entrance, not a resize: whatever height the content
        // wants, start there, with nothing left over from last time.
        //
        // Only from hidden, though. A hotkey pressed while the panel is
        // already up must not snap a height mid-flight or blink the selection
        // out - from there it is not an arrival at all.
        if self.phase == Phase::Hidden {
            self.height.snap();
            self.sel.disarm();
        }
        self.phase = if self.a >= 1.0 {
            Phase::Shown
        } else {
            Phase::Summoning
        };
    }

    pub fn dismiss(&mut self) {
        self.phase = if self.a <= 0.0 {
            Phase::Hidden
        } else {
            Phase::Dismissing
        };
    }

    /// The height the panel is heading for, which is not the height it is at.
    ///
    /// The window is sized from this rather than from the current value, so a
    /// transition costs one resize instead of one per frame. See
    /// [`crate::gui::frame::Frame::resize`].
    pub fn height_target(self) -> f32 {
        self.height.target()
    }

    /// Whether the height has arrived.
    pub fn height_settled(self) -> bool {
        !self.height.running()
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
    /// if and only if this is true, so a transition that forgets to finish is
    /// a process that renders at sixty frames a second in the notification
    /// area for ever.
    pub fn is_animating(self) -> bool {
        match self.phase {
            Phase::Hidden => false,
            Phase::Summoning | Phase::Dismissing => true,
            Phase::Shown => {
                self.height.running()
                    || self.cross.running()
                    || self.sel.running()
                    || self.load.running()
                    || self.hold.waiting()
            }
        }
    }
}
