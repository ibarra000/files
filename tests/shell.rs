//! Tests for the panel animator, run without a window.
//!
//! [`Motion`] is a pure function of elapsed time, so these tests step it at
//! sixty frames a second with no toolkit, GPU or clock. Its main job is the
//! empty gate, which keeps the body from going empty for a single frame while
//! results are in flight.

use files::gui::anim::{Content, EMPTY_HOLD, Motion, Phase};

/// One frame at the compositor's 60 Hz.
const FRAME: f32 = 1.0 / 60.0;

/// Slack for a transition to land on a frame boundary rather than exactly on
/// its deadline.
const SLACK: f32 = 3.0 * FRAME;

/// Runs `secs` of frames, handing each body to `f`.
fn play(motion: &mut Motion, secs: f32, mut f: impl FnMut(Content)) -> Content {
    let mut last = motion.advance(0.0);
    let mut elapsed = 0.0;
    while elapsed < secs {
        last = motion.advance(FRAME);
        f(last);
        elapsed += FRAME;
    }
    last
}

fn run(motion: &mut Motion, secs: f32) -> Content {
    play(motion, secs, |_| {})
}

/// A panel already shown and settled on results.
fn shown() -> Motion {
    let mut motion = Motion::new();
    motion.retarget(Content::Results);
    motion.summon();
    motion.advance(0.0);
    assert_eq!(motion.phase(), Phase::Shown);
    motion
}

// -- arriving and leaving ---------------------------------------------------

#[test]
fn a_summon_is_immediate() {
    let mut motion = Motion::new();
    motion.retarget(Content::Recent);
    motion.summon();

    // No frames run: the panel is shown on the same call.
    let visual = motion.advance(0.0);
    assert_eq!(motion.phase(), Phase::Shown);
    assert_eq!(visual, Content::Recent);
    assert!(
        !motion.is_animating(),
        "an arrived panel owes no further frames"
    );
}

#[test]
fn a_dismiss_is_immediate() {
    let mut motion = shown();
    motion.dismiss();

    // Hidden on the call: `Shell::park` hands the window to the hotkey thread
    // as soon as this goes hidden, so a delay would hold the keyboard.
    assert!(motion.is_hidden(), "Escape did not put the panel away");
    assert!(
        !motion.is_animating(),
        "a hidden panel owes no further frames"
    );
}

/// A hotkey pressed while the panel is shown is not an arrival and must not
/// restart the empty hold.
#[test]
fn re_summoning_a_panel_that_is_already_up_does_not_reset_the_empty_gate() {
    let mut motion = shown();
    motion.retarget(Content::Empty);
    run(&mut motion, EMPTY_HOLD / 2.0);
    assert_eq!(
        motion.advance(0.0),
        Content::Results,
        "the gate should still be holding the old body"
    );

    motion.summon();
    let after = run(&mut motion, EMPTY_HOLD / 2.0 + SLACK);
    assert_eq!(
        after,
        Content::Empty,
        "the re-summon restarted the hold instead of letting it finish"
    );
}

// -- the empty hold ---------------------------------------------------------

/// A single empty frame between a query and its results is never shown.
///
/// An empty list swaps the rows for a centred message, so an ungated
/// transient would flash on every keystroke.
#[test]
fn a_body_that_is_empty_for_a_frame_is_never_shown_as_empty() {
    let mut motion = shown();

    // One empty frame, as an in-flight query produces.
    motion.retarget(Content::Empty);
    let blink = motion.advance(FRAME);
    assert_eq!(blink, Content::Results, "it flinched");

    // The results come back.
    motion.retarget(Content::Results);
    let recovered = motion.advance(FRAME);
    assert_eq!(recovered, Content::Results);
}

/// The hold only delays: a query that matches nothing is eventually shown as
/// empty.
#[test]
fn a_body_that_stays_empty_is_shown_as_empty() {
    let mut motion = shown();
    motion.retarget(Content::Empty);

    let held = run(&mut motion, EMPTY_HOLD * 0.5);
    assert_eq!(held, Content::Results, "granted too early");
    assert!(
        motion.is_animating(),
        "the loop would park before granting it"
    );

    let granted = run(&mut motion, EMPTY_HOLD + SLACK);
    assert_eq!(granted, Content::Empty);
}

// -- frames that are not sixty a second -------------------------------------

#[test]
fn a_frame_time_that_is_not_a_number_cannot_wedge_the_panel() {
    let mut motion = shown();
    motion.retarget(Content::Empty);

    motion.advance(f32::NAN);
    motion.advance(f32::INFINITY);
    motion.advance(-1.0);

    // A poisoned accumulator would keep the gate closed forever.
    let visual = run(&mut motion, EMPTY_HOLD + SLACK);
    assert_eq!(
        visual,
        Content::Empty,
        "a bad frame time wedged the empty gate"
    );
    assert_eq!(motion.phase(), Phase::Shown);
}

/// An animator that was never summoned requests no frames.
#[test]
fn a_fresh_animator_is_idle() {
    let mut motion = Motion::new();
    assert!(motion.is_hidden());
    assert!(!motion.is_animating());

    motion.advance(FRAME);
    assert!(!motion.is_animating(), "an idle animator owes a frame");
}
