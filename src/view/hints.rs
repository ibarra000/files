//! The key hints along the bottom.
//!
//! A list of independent chips rather than one sentence, and fitted rather
//! than truncated.
//!
//! The sentence it replaces was about a hundred and ten characters long and
//! was handed to a `Paragraph`, which clips. On an eighty-column terminal it
//! was cut around `Esc clear` - which is to say it cut `Ctrl+Q quit`, and
//! Ctrl+Q is the only key that leaves. It is not guessable either: Ctrl+C
//! copies here and Esc deliberately stays. Somebody on a standard terminal had
//! no way out of this program except closing the window.
//!
//! A string that is truncated loses whatever happens to be last. A list that
//! is fitted loses whatever is least important, and some hints are never
//! allowed to be least important.
//!
//! The other half of that fix is not here: the missing-viewer warning moved
//! off this line onto the notice row, where nothing can clip it.
//!
//! The *policy* is what this module is. Which hints mean something right now,
//! how hard each fights for its place, and the guarantee that falls out of
//! that - exactly one way out of every context, Essential, and therefore on
//! screen at every width. None of it is a fact about cells or pixels, so the
//! measuring is handed in by whoever is drawing.

use crate::app::state::AppState;
use crate::config::ViewerKind;

/// How hard a hint fights for its place on the line.
///
/// Ordered so `Low` sorts highest and is therefore dropped first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Never dropped, at any width: the action the user came here for, and the
    /// way out.
    Essential,
    High,
    Normal,
    Low,
}

/// One key hint, as it will be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hint {
    pub key: &'static str,
    pub label: &'static str,
    pub priority: Priority,
    /// What a click on this chip does, where clicking it does anything.
    pub action: Option<Action>,
}

/// What a clicked chip runs.
///
/// Deliberately not every hint. `Ctrl+Q` and `Esc` are absent: a control
/// surface where one mis-click ends the session, or empties a half-typed code,
/// with no confirmation is worse than no control surface. Those two stay
/// keyboard-only, and are still advertised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    Recall,
    Results,
    Refresh,
    Help,
}

const fn hint(key: &'static str, label: &'static str, priority: Priority) -> Hint {
    Hint {
        key,
        label,
        priority,
        action: None,
    }
}

const fn clickable(
    key: &'static str,
    label: &'static str,
    priority: Priority,
    action: Action,
) -> Hint {
    Hint {
        key,
        label,
        priority,
        action: Some(action),
    }
}

/// What the hint bar needs to know, lifted out of the state so the sets can be
/// tested without building an `AppState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// The drive picker is open, which is the one thing that borrows the body.
    pub picking_share: bool,
    /// Nothing typed, so the body is the codes used before.
    pub showing_recent: bool,
    pub has_results: bool,
    pub has_text: bool,
    pub has_text_selection: bool,
    /// In the overlay, Escape closes rather than clears, and Ctrl+Q is left
    /// out. The overlay is dismissed, not quit, and putting the one key that
    /// takes the whole index down with it in front of somebody in a hurry is
    /// the wrong thing to advertise.
    pub compact: bool,
    /// Which viewer Enter uses, because the F2 chip names it.
    pub viewer: ViewerKind,
}

/// What the F2 chip says.
///
/// The mode, not the verb. F2 decides whether Enter assembles a document or
/// launches a program, and a chip reading a flat "viewer" said which key to
/// press without ever saying what pressing it had done - so the one key whose
/// whole job is to change a mode gave no sign of which mode it was in.
///
/// Three constants rather than a `format!` because [`Hint::label`] is
/// `&'static str`: the hint sets are built by `const fn`s and the whole module
/// is allocation-free by construction. Written out rather than built from
/// [`ViewerKind::display`] for that reason, and pinned to it by a test.
const fn viewer_label(viewer: ViewerKind) -> &'static str {
    match viewer {
        ViewerKind::Auto => "Viewer: Auto",
        ViewerKind::Pdf => "Viewer: PDF",
        ViewerKind::Avwin => "Viewer: avwin",
    }
}

impl Context {
    pub fn of(state: &AppState) -> Self {
        Self {
            picking_share: state.picking_share,
            showing_recent: state.showing_recent(),
            has_results: !state.hits.is_empty(),
            has_text: !state.input.is_empty(),
            has_text_selection: state.input.has_selection(),
            compact: state.overlay_up,
            viewer: state.viewer,
        }
    }
}

/// The hints that mean something right now, in reading order.
///
/// `Enter open` is *absent* rather than dropped when there is nothing to open:
/// advertising a key whose whole effect is to raise "nothing to open" is worse
/// than saying nothing at all.
pub fn hints(cx: Context) -> Vec<Hint> {
    // Quit is Essential in every set, and that is the invariant this module
    // exists to hold.
    let quit = hint("Ctrl+Q", "Quit", Priority::Essential);

    // The overlay has its own short set. Escape is Essential here and merely
    // High or Low elsewhere, because closing is the one thing somebody must be
    // able to do from a window covering their work, and because it means
    // something different here than it does anywhere else in the program.
    if cx.compact {
        let mut v = Vec::with_capacity(6);
        if cx.has_results {
            v.push(clickable(
                "Enter",
                "Open",
                Priority::Essential,
                Action::Open,
            ));
        }
        v.push(hint("Esc", "Close", Priority::Essential));
        v.push(clickable(
            "\u{2191}",
            "Recent codes",
            Priority::High,
            Action::Recall,
        ));
        if cx.has_results {
            v.push(clickable(
                "\u{2193}",
                "Results",
                Priority::High,
                Action::Results,
            ));
        }
        // The chip that stands for every key this bar could not fit, which is
        // why it outranks the viewer indicator here and does not in the full
        // set. A bar narrow enough to be dropping chips is exactly the bar
        // whose reader most needs the window that lists all of them - and in
        // the overlay there is nothing else on screen that could teach a key.
        // The viewer chip names one fact; this one is the index to every fact,
        // and F1 was reachable from the keyboard and advertised nowhere.
        v.push(clickable("F1", "All keys", Priority::Normal, Action::Help));
        v.push(hint("F2", viewer_label(cx.viewer), Priority::Low));
        return v;
    }

    // Two special cases and the ordinary one. The ordinary one is *most* of
    // the time now: the text field always has the keyboard, so there is no
    // "which pane am I in" for this bar to answer.
    if cx.picking_share {
        // Choosing which drive to update. The two answers and the way out,
        // because the list above already says what each row is.
        return vec![
            hint("Enter", "Update this drive", Priority::Essential),
            quit,
            hint("\u{2191}\u{2193}", "Choose", Priority::High),
            hint("A", "Update all", Priority::Normal),
            hint("Esc", "Back", Priority::High),
        ];
    }

    // No `F1` here, unlike the compact bar. This list has four keys and the bar
    // already names all four, so the chip that stands for "every other key"
    // would be standing for nothing - and it costs exactly the room the
    // position readout needs, which is the one thing only the status line can
    // say.
    if cx.showing_recent {
        // `Esc` of its own again, and truthfully this time. It used to say
        // "keep what you typed", which recall could not deliver; then the list
        // became what an empty field showed and Escape simply dismissed. It is
        // a mode once more - the Up arrow is the only way in - so Escape leaves
        // it, exactly as it leaves the drive picker.
        return vec![
            clickable("Enter", "Use this code", Priority::Essential, Action::Open),
            quit,
            hint("\u{2191}\u{2193}", "Recent codes", Priority::High),
            hint("Esc", "Back", Priority::High),
        ];
    }

    let mut v = Vec::with_capacity(8);
    {
        {
            if cx.has_results {
                v.push(clickable(
                    "Enter",
                    "Open",
                    Priority::Essential,
                    Action::Open,
                ));
            }
            v.push(quit);
            if cx.has_results {
                // One hint for both arrows, because they do one thing. The
                // pair that used to be here - `↓ results` to step into the
                // list and `↑ recent codes` to leave it - existed to explain a
                // mode that no longer exists.
                v.push(clickable(
                    "\u{2191}\u{2193}",
                    "Move",
                    Priority::High,
                    Action::Results,
                ));
            }
            if cx.has_text_selection {
                v.push(hint("Ctrl+C", "Copy", Priority::High));
            }
            // Ahead of `F1 help`, and `High` rather than `Normal`. Both are
            // deliberate: `fit` drops the lowest priority first and the
            // rightmost among equals, so at `Normal` this chip sat behind help
            // and was the first thing to go - which is how the viewer came to
            // be advertised nowhere at the shipped width.
            //
            // It outranks help because it is the only chip carrying *state*.
            // The others name a key; this one also answers "and what is it set
            // to", which is the question F2 leaves behind every time it is
            // pressed.
            v.push(hint("F2", viewer_label(cx.viewer), Priority::High));
            v.push(clickable("F1", "Help", Priority::Normal, Action::Help));
            v.push(clickable("F5", "Refresh", Priority::Low, Action::Refresh));
            if cx.has_text {
                v.push(hint("Esc", "Clear", Priority::Low));
            }
        }
    }
    v
}

/// How a renderer measures a chip.
///
/// A trait rather than a closure, so a terminal's measurement stays an integer
/// character count and a window's stays a font lookup, and neither has to
/// pretend to be the other. [`fit`] has no opinion about the unit; it only
/// requires that `width` and these answers agree about one.
pub trait Measure {
    /// One chip, with and without its label. Excludes the gap before it.
    fn chip(&self, hint: &Hint, labelled: bool) -> f32;
    /// The gap between two chips.
    fn gap(&self) -> f32;
}

/// Character columns, which is what a terminal has.
///
/// Every key and label here is ASCII or one of the four arrows, all of which
/// are single-width, so a character count is a column count. That is the
/// assumption the rest of this crate makes too; see `ui::input_line`.
pub struct Columns;

impl Measure for Columns {
    fn chip(&self, hint: &Hint, labelled: bool) -> f32 {
        let keys = hint.key.chars().count() + 2;
        let total = if labelled {
            keys + 1 + hint.label.chars().count()
        } else {
            keys
        };
        total as f32
    }

    fn gap(&self) -> f32 {
        2.0
    }
}

fn total<M: Measure>(hints: &[Hint], labelled: bool, m: &M) -> f32 {
    let chips: f32 = hints.iter().map(|h| m.chip(h, labelled)).sum();
    chips + m.gap() * hints.len().saturating_sub(1) as f32
}

/// Which hints survive `width`, and whether they keep their labels.
///
/// Drops the lowest-priority hint furthest to the right, and repeats, so what
/// survives a narrow window is the front of the line rather than an arbitrary
/// prefix of a sentence.
///
/// If the essentials alone still will not fit, the labels go and the keys
/// stay: somebody who cannot read `quit` can still read `Ctrl+Q` and try it.
/// Below even that there is no honest answer, and a clipped `Ctrl+Q` is the
/// best one available.
pub fn fit<M: Measure>(hints: &[Hint], width: f32, m: &M) -> (Vec<Hint>, bool) {
    let mut kept = hints.to_vec();

    while total(&kept, true, m) > width {
        let Some(at) = kept
            .iter()
            .enumerate()
            .filter(|(_, h)| h.priority != Priority::Essential)
            // Lowest priority first, then rightmost among equals.
            .max_by_key(|(i, h)| (h.priority, *i))
            .map(|(i, _)| i)
        else {
            break;
        };
        kept.remove(at);
    }

    let labelled = total(&kept, true, m) <= width;
    (kept, labelled)
}

/// The fitted line as plain text.
///
/// Not a rendering - a reading. What the tests below are about is which hints
/// survive a width and in what order, and that is answerable without knowing
/// what a chip is drawn with. A terminal and a window both add their own
/// styling to the same sequence.
pub fn plain(kept: &[Hint], labelled: bool) -> String {
    kept.iter()
        .map(|h| {
            if labelled {
                format!(" {}  {}", h.key, h.label)
            } else {
                format!(" {} ", h.key)
            }
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// [`fit`] in character columns, which is how the tests below are written and
/// how the terminal renderer asks.
pub fn fit_columns(hints: &[Hint], width: u16) -> (Vec<Hint>, bool) {
    fit(hints, f32::from(width), &Columns)
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The ordinary context: something typed, results on screen, nothing
    /// borrowing the body.
    fn ctx() -> Context {
        Context {
            picking_share: false,
            showing_recent: false,
            has_results: true,
            has_text: true,
            has_text_selection: false,
            compact: false,
            viewer: ViewerKind::Pdf,
        }
    }

    fn picking() -> Context {
        Context {
            picking_share: true,
            ..ctx()
        }
    }

    fn recent() -> Context {
        Context {
            showing_recent: true,
            has_text: false,
            has_results: false,
            ..ctx()
        }
    }

    /// Every context the bar is ever drawn in, in and out of the overlay.
    ///
    /// Eight of these collapsed to six when `Focus` did: the bar no longer has
    /// to answer "which pane has the keyboard", because the answer is always
    /// the search field.
    fn every_context() -> Vec<Context> {
        let mut all = Vec::new();
        for compact in [false, true] {
            for base in [ctx(), picking(), recent()] {
                all.push(Context { compact, ..base });
            }
        }
        all
    }

    fn rendered(kept: &[Hint], labelled: bool) -> String {
        plain(kept, labelled)
    }

    /// Every chip label, in every context and every viewer mode.
    ///
    /// A chip is a fragment and takes no stop, so it is held to the status
    /// rules - and it starts with a capital, because a chip is the head of
    /// what it says rather than the tail of something else. The bar used to
    /// read `Ctrl+Q quit   F2 viewer: pdf` beside a status line reading
    /// `Searching...`, which is three different conventions on one row.
    #[test]
    fn every_chip_keeps_the_house_style() {
        let mut labels = Vec::new();
        for viewer in ViewerKind::ALL {
            for cx in every_context() {
                for h in hints(Context { viewer, ..cx }) {
                    labels.push(h.label);
                    labels.push(h.key);
                }
            }
        }
        crate::view::style::check_all(
            "the hint bar",
            labels.iter().copied(),
            crate::view::style::Slot::Status,
        );
        for label in labels {
            assert!(
                crate::view::style::starts_capitalised(label),
                "the chip {label:?} does not start a sentence"
            );
        }
    }

    /// The F2 chip and the rest of the program have to name the same thing the
    /// same way.
    ///
    /// The chip is written out longhand because [`Hint::label`] is
    /// `&'static str` and this module is allocation-free by construction, so
    /// nothing makes the two agree except this.
    #[test]
    fn the_viewer_chip_spells_the_viewer_the_way_everything_else_does() {
        for viewer in ViewerKind::ALL {
            assert_eq!(
                viewer_label(viewer),
                format!("Viewer: {}", viewer.display()),
                "the chip and the toast would disagree about {viewer:?}"
            );
        }
    }

    /// The whole reason this module exists.
    ///
    /// Ctrl+Q is the only key that leaves, and it is not guessable - Ctrl+C
    /// copies and Esc stays. The sentence this replaced put it last in a
    /// hundred and ten characters, so on any terminal under about a hundred
    /// and twenty columns it was simply not on screen.
    #[test]
    fn the_way_out_is_on_screen_at_every_width_and_in_every_context() {
        for cx in every_context() {
            let all = hints(cx);
            // Whichever key it is. On the full screen that is `Ctrl+Q quit`;
            // in the overlay it is `Esc close`, because the overlay is
            // dismissed rather than quit and putting the key that takes the
            // whole index down in front of somebody in a hurry is the wrong
            // thing to offer. What must hold everywhere is that there is one,
            // that it is Essential, and that it therefore always fits.
            let exits: Vec<&Hint> = all
                .iter()
                .filter(|h| h.label == "Quit" || h.label == "Close")
                .collect();
            assert_eq!(
                exits.len(),
                1,
                "{cx:?} offers {} ways out; exactly one is the contract",
                exits.len()
            );
            let exit = *exits[0];
            assert_eq!(
                exit.priority,
                Priority::Essential,
                "{cx:?} can drop its own way out"
            );

            for width in 8u16..=200 {
                let (kept, labelled) = fit_columns(&all, width);
                assert!(
                    kept.contains(&exit),
                    "{cx:?} at width {width} dropped {:?}, its only way out",
                    exit.key
                );
                let line = rendered(&kept, labelled);
                assert!(
                    line.contains(exit.key),
                    "{cx:?} at width {width} drew {line:?} without {:?}",
                    exit.key
                );
            }
        }
    }

    /// And it fits, so the terminal never has to clip it.
    #[test]
    fn a_fitted_line_never_runs_past_the_width_it_was_given() {
        for cx in every_context() {
            for width in 20u16..=200 {
                let (kept, labelled) = fit_columns(&hints(cx), width);
                let drawn = rendered(&kept, labelled).chars().count();
                assert!(
                    drawn <= width as usize,
                    "{cx:?} at width {width} drew {drawn} columns"
                );
            }
        }
    }

    /// Hints go from the least important end, not off the right of a sentence.
    #[test]
    fn hints_are_given_up_least_important_first() {
        let all = hints(ctx());
        let full = total(&all, true, &Columns) as u16;

        let (kept, _) = fit_columns(&all, full);
        assert_eq!(kept.len(), all.len(), "everything fits at its own width");

        // One column short, and the cheapest thing on the line goes.
        let (kept, _) = fit_columns(&all, full - 1);
        let lost: Vec<_> = all.iter().filter(|h| !kept.contains(h)).collect();
        assert_eq!(
            lost.len(),
            1,
            "more than one hint was given up for one column"
        );
        assert_eq!(
            lost[0].priority,
            Priority::Low,
            "an important hint was given up while a cheap one stayed"
        );
    }

    /// Past the point where even the essentials fit with labels, the labels go
    /// and the keys stay.
    #[test]
    fn a_terminal_too_narrow_for_labels_still_shows_the_keys() {
        let all = hints(ctx());
        let (kept, labelled) = fit_columns(&all, 16);
        assert!(!labelled, "16 columns cannot hold a key and a label");
        assert!(kept.iter().all(|h| h.priority == Priority::Essential));
        assert!(rendered(&kept, labelled).contains("Ctrl+Q"));
    }

    /// A width nothing can be drawn in must not panic, and must not loop.
    #[test]
    fn a_width_of_nothing_is_survivable() {
        for cx in every_context() {
            let (kept, labelled) = fit_columns(&hints(cx), 0);
            assert!(kept.iter().all(|h| h.priority == Priority::Essential));
            let _ = rendered(&kept, labelled);
        }
    }

    /// Nothing to open, so nothing says Enter opens it. The key would raise
    /// "nothing to open", and a hint that teaches a disappointment is worse
    /// than no hint.
    #[test]
    fn an_empty_result_list_does_not_advertise_opening_one() {
        let cx = Context {
            has_results: false,
            ..ctx()
        };
        assert!(!hints(cx).iter().any(|h| h.key == "Enter"));
        assert!(hints(ctx()).iter().any(|h| h.key == "Enter"));
    }

    /// With nothing typed the list is the codes used before, and the bar
    /// describes that and nothing else.
    ///
    /// This used to also promise `Esc keep what you typed`, because recall was
    /// a mode laid over a half-typed code. Retiring the mode retired the draft,
    /// and a hint for a thing that no longer happens is worse than no hint.
    #[test]
    fn the_recent_list_describes_itself_and_nothing_else() {
        let all = hints(recent());
        let (kept, labelled) = fit_columns(&all, 120);
        let line = rendered(&kept, labelled);

        assert!(line.contains("Use this code"), "{line}");
        assert!(line.contains("Recent codes"), "{line}");
        // Lower-cased first, so that a label the case pass touches cannot
        // turn a negative assertion into one that passes by spelling.
        let folded = line.to_lowercase();
        assert!(
            !folded.contains("keep what you typed"),
            "the draft was retired with the mode: {line}"
        );
        assert!(!folded.contains("results"), "recall has no results: {line}");
    }

    /// The recent list is short, so its bar has to survive a narrow panel
    /// without losing the one key that uses a row.
    #[test]
    fn the_recent_list_still_names_enter_when_it_is_squeezed() {
        let all = hints(recent());
        for width in 40u16..=200 {
            let (kept, labelled) = fit_columns(&all, width);
            let line = rendered(&kept, labelled);
            assert!(
                line.contains("Enter"),
                "at width {width} the recent list stopped naming Enter: {line:?}"
            );
        }
    }

    /// Only one key leaves, so only one hint may say so - and which key it is
    /// depends on what "leaving" means where you are.
    ///
    /// On the full screen the way out is `Ctrl+Q`, and it has to be advertised
    /// because it is not guessable: `Ctrl+C` copies here and `Esc` deliberately
    /// stays. In the overlay the way out is `Esc`, because the overlay is
    /// dismissed rather than quit - and `Ctrl+Q` is left off on purpose, since
    /// it takes the whole index down with it.
    ///
    /// Two hints both claiming to be the way out is the failure this guards:
    /// one of them would be wrong, and the reader has no way to tell which.
    #[test]
    fn exactly_one_hint_is_the_way_out_and_it_is_the_right_one() {
        for cx in every_context() {
            let leaving: Vec<_> = hints(cx)
                .into_iter()
                .filter(|h| h.label == "Quit" || h.label == "Close")
                .collect();
            assert_eq!(leaving.len(), 1, "{cx:?} advertises {leaving:?}");

            let want = if cx.compact { "Esc" } else { "Ctrl+Q" };
            assert_eq!(
                leaving[0].key, want,
                "{cx:?} names {:?} as the way out",
                leaving[0].key
            );
        }
    }

    /// And the overlay does not offer the key that would take the index down
    /// with the window.
    #[test]
    fn the_overlay_does_not_advertise_quitting_the_whole_program() {
        for cx in every_context().into_iter().filter(|c| c.compact) {
            assert!(
                !hints(cx).iter().any(|h| h.key == "Ctrl+Q"),
                "{cx:?} puts quitting in front of somebody who wanted to close a window"
            );
        }
    }

    /// The two keys that cannot be undone are not one mis-click away.
    #[test]
    fn quitting_and_clearing_are_not_clickable() {
        for cx in every_context() {
            for h in hints(cx) {
                if h.key == "Ctrl+Q" || h.key == "Esc" {
                    assert_eq!(
                        h.action, None,
                        "{:?} would end a session or a code on one mis-click",
                        h.key
                    );
                }
            }
        }
    }
}
