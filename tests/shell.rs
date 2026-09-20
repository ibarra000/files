//! What the animator still does, checked without opening a window.
//!
//! [`Motion`] is a pure function of elapsed time, which is the whole reason it
//! is a separate module: something that can only be checked by watching it is
//! something nobody checks. Everything below runs at sixty frames a second in
//! a loop, with no toolkit, no GPU and no clock.
//!
//! Most of this file used to be about the entrance, the height tween, the
//! cross-fade and the sliding selection. They are gone, and so are their
//! tests. The height went with them when the window became a fixed six
//! hundred by four hundred: there is nothing left for the animator to have an
//! opinion about the size of.
//!
//! What is left is the one thing that still takes time - the gate that stops
//! a body going empty for one frame on its way to a perfectly good list.

use files::gui::anim::{Content, EMPTY_HOLD, Motion, Phase, Target, Visual};

/// Sixty frames a second, which is what the compositor gives us.
const FRAME: f32 = 1.0 / 60.0;

/// Enough slack for a transition to land on a frame boundary rather than
/// exactly on its own deadline.
const SLACK: f32 = 3.0 * FRAME;

fn at(content: Content) -> Target {
    Target {
        content,
        selection_y: None,
    }
}

/// Runs `secs` of frames, handing each visual to `f`.
fn play(motion: &mut Motion, secs: f32, mut f: impl FnMut(Visual)) -> Visual {
    let mut last = motion.advance(0.0);
    let mut elapsed = 0.0;
    while elapsed < secs {
        last = motion.advance(FRAME);
        f(last);
        elapsed += FRAME;
    }
    last
}

fn run(motion: &mut Motion, secs: f32) -> Visual {
    play(motion, secs, |_| {})
}

/// A panel already up and settled, showing results, so a test about one
/// transition is not also a test about the entrance.
fn shown() -> Motion {
    let mut motion = Motion::new();
    motion.retarget(at(Content::Results));
    motion.summon();
    motion.advance(0.0);
    assert_eq!(motion.phase(), Phase::Shown);
    motion
}

// -- easing -----------------------------------------------------------------

// -- arriving and leaving ---------------------------------------------------

#[test]
fn a_summon_is_immediate() {
    let mut motion = Motion::new();
    motion.retarget(at(Content::Recent));
    motion.summon();

    // No frames run: the panel is up on the same call. The hotkey thread has
    // already put the window on screen by this point, and there is no longer a
    // curve to play alongside that.
    let visual = motion.advance(0.0);
    assert_eq!(motion.phase(), Phase::Shown);
    assert_eq!(visual.content, Content::Recent);
    assert!(
        !motion.is_animating(),
        "an arrived panel owes no further frames"
    );
}

/// And it leaves *proportionally*: a panel barely arrived does not linger.
#[test]
fn a_dismiss_is_immediate() {
    let mut motion = shown();
    motion.dismiss();

    // On the call, not on a later frame. This is what `Shell::park` rides on:
    // it hands the window to the hotkey thread the moment this goes hidden,
    // and a dismissal that took time would hold the keyboard for its duration.
    assert!(motion.is_hidden(), "Escape did not put the panel away");
    assert!(
        !motion.is_animating(),
        "a hidden panel owes no further frames"
    );
}

/// A hotkey pressed while the panel is already up is not an arrival, and must
/// not be treated as one.
#[test]
fn re_summoning_a_panel_that_is_already_up_does_not_reset_the_empty_gate() {
    // The gate is the one thing a re-summon could still disturb. A hotkey
    // pressed while the panel is up is not an arrival, and must not hand the
    // body a fresh hold under a live query.
    let mut motion = shown();
    motion.retarget(at(Content::Empty));
    run(&mut motion, EMPTY_HOLD / 2.0);
    assert_eq!(
        motion.advance(0.0).content,
        Content::Results,
        "the gate should still be holding the old body"
    );

    motion.summon();
    let after = run(&mut motion, EMPTY_HOLD / 2.0 + SLACK);
    assert_eq!(
        after.content,
        Content::Empty,
        "the re-summon restarted the hold instead of letting it finish"
    );
}

// -- the body ---------------------------------------------------------------

// -- the empty hold ---------------------------------------------------------

/// The transient that made the panel flinch on every keystroke.
///
/// Between the matcher being asked and answering there is at least one frame
/// with no results in it, and an empty result list is not a shorter list - it
/// is a different body, with a centred line of prose where the rows were. The
/// state machine no longer produces that transient, and this is the guard
/// that says no other path may either.
#[test]
fn a_body_that_is_empty_for_a_frame_is_never_shown_as_empty() {
    let mut motion = shown();

    // One frame of nothing, exactly as an in-flight query produces.
    motion.retarget(at(Content::Empty));
    let blink = motion.advance(FRAME);
    assert_eq!(blink.content, Content::Results, "it flinched");

    // And the results come back. This matters more without the cross-fade
    // than it did with one: an ungated transient is now a list of rows
    // replaced by a line of prose and back, in two frames.
    motion.retarget(at(Content::Results));
    let recovered = motion.advance(FRAME);
    assert_eq!(recovered.content, Content::Results);
}

/// The hold is a delay, not a refusal: a code that really matches nothing
/// still says so.
#[test]
fn a_body_that_stays_empty_is_shown_as_empty() {
    let mut motion = shown();
    motion.retarget(at(Content::Empty));

    let held = run(&mut motion, EMPTY_HOLD * 0.5);
    assert_eq!(held.content, Content::Results, "granted too early");
    assert!(
        motion.is_animating(),
        "the loop would park before granting it"
    );

    let granted = run(&mut motion, EMPTY_HOLD + SLACK);
    assert_eq!(granted.content, Content::Empty);
}

// -- the selection ----------------------------------------------------------

/// The first selection since there was none appears where it belongs. Kept as
/// a regression guard: this used to need a special case, because a plain tween
/// flew the highlight in from the top of the list every time a search returned.
#[test]
fn the_first_selection_appears_in_place_rather_than_sliding_in() {
    let mut motion = shown();
    motion.retarget(Target {
        selection_y: Some(120.0),
        ..at(Content::Results)
    });

    let visual = motion.advance(FRAME);
    assert_eq!(visual.selection_y, Some(120.0));
}

#[test]
fn the_selection_lands_on_the_row_it_was_given() {
    let mut motion = shown();
    let rows = |y: f32| Target {
        selection_y: Some(y),
        ..at(Content::Results)
    };
    motion.retarget(rows(120.0));
    assert_eq!(motion.advance(FRAME).selection_y, Some(120.0));

    motion.retarget(rows(160.0));
    assert_eq!(
        motion.advance(FRAME).selection_y,
        Some(160.0),
        "the highlight took a frame to get there"
    );
    assert!(!motion.is_animating());
}

#[test]
fn a_cleared_selection_draws_nothing() {
    let mut motion = shown();
    motion.retarget(Target {
        selection_y: Some(120.0),
        ..at(Content::Results)
    });
    motion.advance(FRAME);

    motion.retarget(at(Content::Recent));
    assert_eq!(motion.advance(FRAME).selection_y, None);
}

// -- frames that are not sixty a second -------------------------------------

#[test]
fn a_frame_time_that_is_not_a_number_cannot_wedge_the_panel() {
    let mut motion = shown();
    motion.retarget(at(Content::Empty));

    motion.advance(f32::NAN);
    motion.advance(f32::INFINITY);
    motion.advance(-1.0);

    // A poisoned accumulator would be a gate no frame could ever open, and the
    // body would never be allowed to go empty at all.
    let visual = run(&mut motion, EMPTY_HOLD + SLACK);
    assert_eq!(
        visual.content,
        Content::Empty,
        "a bad frame time wedged the empty gate"
    );
    assert_eq!(motion.phase(), Phase::Shown);
}

/// A panel nobody has summoned draws nothing and asks for nothing.
#[test]
fn a_fresh_animator_is_idle() {
    let mut motion = Motion::new();
    assert!(motion.is_hidden());
    assert!(!motion.is_animating());

    let visual = motion.advance(FRAME);
    assert_eq!(visual.selection_y, None);
}
