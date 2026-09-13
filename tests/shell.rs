//! The transitions, checked without opening a window.
//!
//! [`Motion`] is a pure function of elapsed time, which is the whole reason it
//! is a separate module: a transition that can only be checked by watching it
//! is a transition nobody checks. Everything below runs at sixty frames a
//! second in a loop, with no toolkit, no GPU and no clock.

use files::gui::anim::{
    CROSS, Content, DISMISS, EMPTY_HOLD, HEIGHT, LOAD_DELAY, LOAD_MIN_VISIBLE, Motion, Phase, RISE,
    SLIDE, SUMMON, Target, Visual, ease_in_out_cubic, ease_out_cubic, ease_out_quint,
};

/// Sixty frames a second, which is what the compositor gives us.
const FRAME: f32 = 1.0 / 60.0;

/// Enough slack for a transition to land on a frame boundary rather than
/// exactly on its own deadline.
const SLACK: f32 = 3.0 * FRAME;

fn at(height: f32, content: Content) -> Target {
    Target {
        height,
        content,
        selection_y: None,
        busy: false,
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
fn shown(height: f32) -> Motion {
    let mut motion = Motion::new();
    motion.retarget(at(height, Content::Results));
    motion.summon();
    run(&mut motion, SUMMON + SLACK);
    assert_eq!(motion.phase(), Phase::Shown);
    motion
}

// -- easing -----------------------------------------------------------------

/// The property every curve here is relied on for: it goes from nought to one,
/// it only ever goes forwards, and it never leaves the interval.
///
/// An overshoot is not a rounding error in this context - it is a panel that
/// is briefly more than opaque, or a highlight that sails past the row it was
/// going to and comes back.
#[test]
fn every_curve_reaches_its_endpoints_without_overshooting() {
    for (name, f) in [
        ("ease_out_cubic", ease_out_cubic as fn(f32) -> f32),
        ("ease_out_quint", ease_out_quint),
        ("ease_in_out_cubic", ease_in_out_cubic),
    ] {
        assert_eq!(f(0.0), 0.0, "{name} must start at rest");
        assert_eq!(f(1.0), 1.0, "{name} must arrive exactly");

        let mut previous = 0.0;
        for step in 0..=1000 {
            let t = step as f32 / 1000.0;
            let value = f(t);
            assert!(
                (0.0..=1.0).contains(&value),
                "{name}({t}) = {value} left the unit interval"
            );
            assert!(
                value >= previous,
                "{name} went backwards at {t}: {previous} then {value}"
            );
            previous = value;
        }
    }
}

// -- arriving and leaving ---------------------------------------------------

#[test]
fn a_summon_arrives_fully_present_and_at_rest() {
    let mut motion = Motion::new();
    motion.retarget(at(240.0, Content::Recent));
    motion.summon();

    let visual = run(&mut motion, SUMMON + SLACK);
    assert_eq!(motion.phase(), Phase::Shown);
    assert_eq!(visual.alpha, 1.0);
    assert_eq!(visual.dy, 0.0, "the panel must come to rest at its home");
    assert_eq!(visual.scale, 1.0);
    assert!(
        !motion.is_animating(),
        "an arrived panel owes no further frames"
    );
}

/// Opacity runs ahead of the motion on purpose: the panel should be legible
/// before it has finished moving, rather than arriving and only then
/// appearing.
#[test]
fn the_panel_is_legible_before_it_has_finished_moving() {
    let mut motion = Motion::new();
    motion.retarget(at(240.0, Content::Recent));
    motion.summon();

    let visual = run(&mut motion, SUMMON / 2.0);
    assert!(
        visual.dy > 0.0,
        "this test is about a frame that is still moving"
    );
    // `dy` is the position curve read backwards, so this compares opacity
    // against exactly the progress the movement has made.
    assert!(
        visual.alpha > 1.0 - visual.dy / RISE,
        "opacity {} should lead the motion, which is at {}",
        visual.alpha,
        1.0 - visual.dy / RISE
    );
}

/// The interruption that a naive two-timer implementation gets wrong: it
/// completes the summon, then starts the dismiss, so a panel you dismissed
/// early flashes fully into view on its way out.
#[test]
fn a_dismiss_leaves_from_where_the_summon_had_actually_got_to() {
    let mut motion = Motion::new();
    motion.retarget(at(240.0, Content::Recent));
    motion.summon();

    let midway = run(&mut motion, SUMMON / 2.0);
    assert!(
        midway.alpha < 1.0,
        "the test needs an interruption, not a completed summon"
    );

    motion.dismiss();
    let after = motion.advance(FRAME);
    assert!(
        after.alpha < midway.alpha,
        "alpha rose from {} to {} on the way out",
        midway.alpha,
        after.alpha
    );
    assert!(after.dy > midway.dy, "the panel moved home while leaving");
}

/// And it leaves *proportionally*: a panel barely arrived does not linger.
#[test]
fn a_half_arrived_panel_leaves_in_half_the_time() {
    let mut half = Motion::new();
    half.retarget(at(240.0, Content::Recent));
    half.summon();
    run(&mut half, SUMMON / 2.0);
    half.dismiss();
    run(&mut half, DISMISS / 2.0 + SLACK);
    assert!(
        half.is_hidden(),
        "a half-present panel should be gone by now"
    );

    // The other half of the claim, without which the first is satisfied by a
    // dismissal that is simply instant.
    let mut full = shown(240.0);
    full.dismiss();
    run(&mut full, DISMISS / 2.0 - FRAME);
    assert!(
        !full.is_hidden(),
        "a fully present panel must still be leaving after half a dismissal"
    );
}

#[test]
fn opacity_only_falls_while_the_panel_is_leaving() {
    let mut motion = shown(240.0);
    motion.dismiss();

    let mut previous = 1.0;
    play(&mut motion, DISMISS + SLACK, |visual| {
        assert!(
            visual.alpha <= previous,
            "alpha rose from {previous} to {} mid-dismissal",
            visual.alpha
        );
        previous = visual.alpha;
    });
    assert!(motion.is_hidden());
    assert_eq!(previous, 0.0);
    assert!(
        !motion.is_animating(),
        "a hidden panel must stop asking to be drawn - this is the whole of \
         the idle-costs-nothing guarantee"
    );
}

/// A hotkey pressed while the panel is already up is not an arrival, and must
/// not be treated as one.
#[test]
fn summoning_a_panel_that_is_already_up_does_not_disturb_it() {
    let mut motion = shown(200.0);
    motion.retarget(at(400.0, Content::Results));
    let midway = run(&mut motion, HEIGHT / 2.0);
    assert!(
        midway.height > 200.0 && midway.height < 400.0,
        "the test needs a height mid-flight, got {}",
        midway.height
    );

    motion.summon();
    let after = motion.advance(FRAME);
    assert!(
        after.height < 400.0,
        "a re-summon snapped the height to {} instead of letting it settle",
        after.height
    );
    assert!(
        after.height > midway.height,
        "and it should still be moving"
    );
}

// -- height -----------------------------------------------------------------

#[test]
fn the_height_settles_on_what_it_was_asked_for() {
    let mut motion = shown(200.0);
    motion.retarget(at(376.0, Content::Results));
    let visual = run(&mut motion, HEIGHT + SLACK);

    assert!(
        (visual.height - 376.0).abs() < 0.01,
        "settled at {} rather than 376",
        visual.height
    );
    assert!(!motion.is_animating());
}

/// Retargeting from where it *is*, not from where it was aimed. Restarting
/// from the origin is how a panel halfway to 300 snaps back to 200 before
/// setting off for 400.
#[test]
fn a_height_retargeted_mid_flight_never_goes_backwards() {
    let mut motion = shown(200.0);
    motion.retarget(at(300.0, Content::Results));
    let midway = run(&mut motion, HEIGHT / 2.0);
    assert!(midway.height > 200.0 && midway.height < 300.0);

    motion.retarget(at(400.0, Content::Results));
    let mut previous = midway.height;
    play(&mut motion, HEIGHT + SLACK, |visual| {
        assert!(
            visual.height >= previous - 0.01,
            "height fell back from {previous} to {}",
            visual.height
        );
        previous = visual.height;
    });
    assert!((previous - 400.0).abs() < 0.01);
}

/// Half a point of layout jitter must not restart the animation every frame -
/// that would pin the repaint request on for ever, which is a process that
/// renders at sixty frames a second in the notification area.
#[test]
fn a_negligible_height_change_does_not_start_an_animation() {
    let mut motion = shown(200.0);
    motion.retarget(at(200.2, Content::Results));
    motion.advance(FRAME);
    assert!(
        !motion.is_animating(),
        "a fifth of a point of jitter woke the animator"
    );
}

/// The panel does not grow while it is arriving: the entrance already carries
/// the motion, and something that rises *and* grows reads as two things
/// happening to one object.
#[test]
fn the_height_does_not_animate_while_the_panel_is_arriving() {
    let mut motion = Motion::new();
    motion.retarget(at(150.0, Content::Recent));
    motion.summon();

    // A search lands mid-entrance and the list grows. The height should simply
    // be right on the next frame.
    run(&mut motion, SUMMON / 3.0);
    motion.retarget(at(420.0, Content::Results));
    let visual = motion.advance(FRAME);
    assert_eq!(
        visual.height, 420.0,
        "the panel animated its height while still arriving"
    );
}

// -- the body ---------------------------------------------------------------

/// Text dissolving through text is unreadable. Help and results at half
/// opacity each is a smear, not a transition.
#[test]
fn the_two_bodies_are_never_visible_at_once() {
    let mut motion = shown(200.0);
    motion.retarget(at(200.0, Content::Shares));

    let mut saw_both_phases = (false, false);
    play(&mut motion, CROSS + SLACK, |visual| {
        let fade = visual.content;
        assert!(
            fade.leaving_alpha == 0.0 || fade.showing_alpha == 0.0,
            "both bodies were drawn at once: {} and {}",
            fade.leaving_alpha,
            fade.showing_alpha
        );
        saw_both_phases.0 |= fade.leaving_alpha > 0.0;
        saw_both_phases.1 |= fade.showing_alpha > 0.0;
    });
    assert_eq!(
        saw_both_phases,
        (true, true),
        "the test never observed a cross-fade at all"
    );
}

/// Deliberately not routed through `Content::Empty`: that one is held back by
/// [`EMPTY_HOLD`] and would never be granted inside a single cross-fade, which
/// would make this a test of the hold rather than of the fade.
#[test]
fn a_cross_fade_ends_on_whichever_body_was_asked_for_last() {
    // `shown` settles on `Results`, so this is the fade that starts it.
    let mut motion = shown(200.0);
    motion.retarget(at(200.0, Content::Recent));
    // Past the half-way point, so the body on screen really is the new one.
    run(&mut motion, CROSS * 0.7);

    motion.retarget(at(200.0, Content::Shares));
    let promoted = motion.advance(0.0);
    assert_eq!(
        promoted.content.leaving,
        Some(Content::Recent),
        "past half way the incoming body is the one on screen, so it is what          the next change fades away from"
    );

    let visual = run(&mut motion, CROSS + SLACK);
    assert_eq!(visual.content.showing, Content::Shares);
    assert_eq!(visual.content.leaving, None);
    assert_eq!(visual.content.showing_alpha, 1.0);
    assert!(!motion.is_animating());
}

/// Mid-flight the thing actually on screen is what is *leaving*; the incoming
/// body has barely begun to appear. Promoting it would cross-fade from
/// something nobody has seen, which reads as a flicker rather than a change.
#[test]
fn a_second_change_still_fades_from_what_the_user_can_see() {
    let mut motion = shown(200.0);
    motion.retarget(at(200.0, Content::Shares));
    // Well before half way: `Results` is still the body on screen.
    let early = run(&mut motion, CROSS * 0.15);
    assert_eq!(early.content.leaving, Some(Content::Results));
    assert!(early.content.leaving_alpha > 0.0);
    assert_eq!(
        early.content.showing_alpha, 0.0,
        "the incoming body has not begun to appear yet"
    );

    motion.retarget(at(200.0, Content::Recent));
    let visual = motion.advance(0.0);
    assert_eq!(
        visual.content.leaving,
        Some(Content::Results),
        "faded from a body the user never saw"
    );
    assert_eq!(visual.content.showing, Content::Recent);
}

// -- the empty hold ---------------------------------------------------------

/// The transient that made the panel flinch on every keystroke.
///
/// Between the matcher being asked and answering there is at least one frame
/// with no results in it, and an empty result list is not a shorter list - it
/// is a different body, three hundred points shorter, reached through a
/// cross-fade with the footer chips re-flowing around it. The state machine no
/// longer produces that transient, and this is the guard that says no other
/// path may either.
#[test]
fn a_body_that_is_empty_for_a_frame_is_never_shown_as_empty() {
    let mut motion = shown(440.0);

    // One frame of nothing, exactly as an in-flight query produces.
    motion.retarget(at(160.0, Content::Empty));
    let blink = motion.advance(FRAME);
    assert_eq!(blink.content.showing, Content::Results, "it flinched");
    assert_eq!(blink.content.leaving, None, "and started a cross-fade");
    assert_eq!(blink.height, 440.0, "and went looking for a new height");

    // And the results come back.
    motion.retarget(at(440.0, Content::Results));
    let recovered = run(&mut motion, CROSS + SLACK);
    assert_eq!(recovered.content.showing, Content::Results);
    assert_eq!(recovered.content.leaving, None);
    assert_eq!(recovered.height, 440.0);
}

/// The hold is a delay, not a refusal: a code that really matches nothing
/// still says so.
#[test]
fn a_body_that_stays_empty_is_shown_as_empty() {
    let mut motion = shown(440.0);
    motion.retarget(at(160.0, Content::Empty));

    let held = run(&mut motion, EMPTY_HOLD * 0.5);
    assert_eq!(held.content.showing, Content::Results, "granted too early");
    assert!(
        motion.is_animating(),
        "the loop would park before granting it"
    );

    let granted = run(&mut motion, EMPTY_HOLD + CROSS + SLACK);
    assert_eq!(granted.content.showing, Content::Empty);
    assert_eq!(granted.content.leaving, None);
}

/// The other half of the same transient. A body returning to what it was
/// mid-fade has nothing to fade *to*: what is on screen already is the thing
/// being asked for, so a second fade is a list dissolving into itself.
#[test]
fn a_body_that_changes_back_mid_fade_cancels_rather_than_fading_again() {
    let mut motion = shown(200.0);
    motion.retarget(at(200.0, Content::Shares));
    let leaving = run(&mut motion, CROSS * 0.3);
    assert_eq!(leaving.content.leaving, Some(Content::Results));

    motion.retarget(at(200.0, Content::Results));
    let cancelled = motion.advance(0.0);
    assert_eq!(cancelled.content.showing, Content::Results);
    assert_eq!(cancelled.content.leaving, None, "it faded itself out");
    assert_eq!(cancelled.content.showing_alpha, 1.0);
    assert!(!motion.is_animating(), "and kept asking for frames");
}

#[test]
fn a_settled_body_is_simply_drawn() {
    let visual = shown(200.0).advance(0.0);
    assert_eq!(visual.content.leaving, None);
    assert_eq!(visual.content.showing_alpha, 1.0);
    assert_eq!(visual.content.leaving_alpha, 0.0);
}

// -- the selection ----------------------------------------------------------

/// The first selection since there was none has to *appear* where it belongs.
/// A plain tween would fly it in from the top of the list every time a search
/// returned.
#[test]
fn the_first_selection_appears_in_place_rather_than_sliding_in() {
    let mut motion = shown(300.0);
    motion.retarget(Target {
        selection_y: Some(120.0),
        ..at(300.0, Content::Results)
    });

    let visual = motion.advance(FRAME);
    assert_eq!(visual.selection_y, Some(120.0));
}

#[test]
fn the_selection_slides_between_rows() {
    let mut motion = shown(300.0);
    let rows = |y: f32| Target {
        selection_y: Some(y),
        ..at(300.0, Content::Results)
    };
    motion.retarget(rows(120.0));
    run(&mut motion, SLIDE);

    motion.retarget(rows(160.0));
    let midway = motion.advance(FRAME);
    let y = midway.selection_y.expect("a selection was set");
    assert!(
        y > 120.0 && y < 160.0,
        "the highlight jumped straight to {y} instead of sliding"
    );

    let settled = run(&mut motion, SLIDE + SLACK);
    assert!((settled.selection_y.unwrap() - 160.0).abs() < 0.01);
    assert!(!motion.is_animating());
}

#[test]
fn a_cleared_selection_draws_nothing() {
    let mut motion = shown(300.0);
    motion.retarget(Target {
        selection_y: Some(120.0),
        ..at(300.0, Content::Results)
    });
    run(&mut motion, SLIDE + SLACK);

    motion.retarget(at(300.0, Content::Recent));
    assert_eq!(motion.advance(FRAME).selection_y, None);
}

// -- the loading bar --------------------------------------------------------

fn busy(height: f32, busy: bool) -> Target {
    Target {
        busy,
        ..at(height, Content::Results)
    }
}

/// The whole point of the delay: a fast index never flashes a progress bar.
#[test]
fn work_that_finishes_quickly_never_shows_a_bar() {
    let mut motion = shown(200.0);
    motion.retarget(busy(200.0, true));
    play(&mut motion, LOAD_DELAY - FRAME, |visual| {
        assert_eq!(visual.loading, None, "the bar flashed for quick work");
    });

    motion.retarget(busy(200.0, false));
    play(&mut motion, 1.0, |visual| {
        assert_eq!(
            visual.loading, None,
            "the bar appeared after the work ended"
        );
    });
}

#[test]
fn work_that_outlasts_the_delay_shows_a_bar() {
    let mut motion = shown(200.0);
    motion.retarget(busy(200.0, true));
    let visual = run(&mut motion, LOAD_DELAY + SLACK + 0.1);

    let bar = visual.loading.expect("slow work must say so");
    assert!(bar.alpha > 0.5, "the bar faded in to only {}", bar.alpha);
}

/// Without a minimum, work finishing at 260ms shows a bar for ten
/// milliseconds - which is the flash the delay exists to prevent, arriving one
/// frame later and one frame smaller.
#[test]
fn a_bar_that_has_appeared_stays_long_enough_to_be_seen() {
    let mut motion = shown(200.0);
    motion.retarget(busy(200.0, true));
    // Just past the delay, then the work stops immediately.
    run(&mut motion, LOAD_DELAY + SLACK);
    motion.retarget(busy(200.0, false));

    let mut first = None;
    let mut last = 0.0;
    let mut elapsed = 0.0;
    play(&mut motion, 2.0, |visual| {
        elapsed += FRAME;
        if visual.loading.is_some() {
            first.get_or_insert(elapsed);
            last = elapsed;
        }
    });

    let first = first.expect("the bar was already on screen when the work ended");
    assert!(
        last - first >= LOAD_MIN_VISIBLE - FRAME,
        "the bar was on screen for only {}s",
        last - first
    );
    assert_eq!(
        motion.advance(FRAME).loading,
        None,
        "and it must eventually go away"
    );
    assert!(!motion.is_animating());
}

#[test]
fn the_sweep_stays_within_the_bar() {
    let mut motion = shown(200.0);
    motion.retarget(busy(200.0, true));
    play(&mut motion, 4.0, |visual| {
        if let Some(bar) = visual.loading {
            assert!(
                (0.0..1.0).contains(&bar.sweep),
                "the sweep left the bar at {}",
                bar.sweep
            );
            assert!((0.0..=1.0).contains(&bar.alpha));
        }
    });
    assert!(
        motion.is_animating(),
        "an indeterminate bar must keep asking for frames while it sweeps"
    );
}

// -- frames that are not sixty a second -------------------------------------

/// The first frame after an hour in the notification area reports the hour.
/// Without a ceiling every transition is over before the monitor has drawn it
/// once, which looks exactly like having no transitions at all.
#[test]
fn one_enormous_frame_does_not_skip_the_transition() {
    let mut motion = Motion::new();
    motion.retarget(at(240.0, Content::Recent));
    motion.summon();

    let visual = motion.advance(3600.0);
    assert!(
        visual.alpha < 1.0,
        "an hour-long frame completed the entrance instantly"
    );
    assert_eq!(motion.phase(), Phase::Summoning);
}

#[test]
fn a_frame_time_that_is_not_a_number_cannot_wedge_the_panel() {
    let mut motion = Motion::new();
    motion.retarget(at(240.0, Content::Recent));
    motion.summon();

    motion.advance(f32::NAN);
    motion.advance(f32::INFINITY);
    motion.advance(-1.0);

    let visual = run(&mut motion, SUMMON + SLACK);
    assert_eq!(visual.alpha, 1.0, "the panel never arrived");
    assert_eq!(motion.phase(), Phase::Shown);
}

/// A panel nobody has summoned draws nothing and asks for nothing.
#[test]
fn a_fresh_animator_is_idle() {
    let mut motion = Motion::new();
    assert!(motion.is_hidden());
    assert!(!motion.is_animating());

    let visual = motion.advance(FRAME);
    assert_eq!(visual.alpha, 0.0);
    assert_eq!(visual.loading, None);
    assert_eq!(visual.selection_y, None);
}
