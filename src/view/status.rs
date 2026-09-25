//! The status line.
//!
//! Rendered as a pure function of state and time, so the exact text can be
//! asserted in tests. That matters more than usual here: the status line is
//! the only place the user learns that the index is stale, that the drive is
//! unreachable, or that server-side filtering has switched itself off, and
//! none of those conditions can be reproduced on a development machine.
//!
//! The previous implementation showed `0 / 0 files matched` for every one of
//! them.

use std::time::SystemTime;

use crate::app::state::{AppState, QueryPhase, Severity};
use crate::index::store::{Activity, Health};
use crate::util::humanize;

/// How urgent the line is, which drives its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Busy,
    Good,
    Warn,
    Bad,
}

/// One rendered status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    pub text: String,
    pub tone: Tone,
}

/// Builds the status line.
///
/// Words only. The mark that leads the line - a tone glyph, or a spinner while
/// something is in flight - is added by [`crate::gui::panel::footer`], which is the one
/// place that knows the whole frame's clock. Putting it here as well is how the
/// line came to be drawn with two spinners on it.
pub fn render(state: &AppState, wall: SystemTime) -> StatusLine {
    // Something that just happened outranks everything standing: it is the
    // answer to a key the user pressed a moment ago, and it goes away on its
    // own after `TOAST_LIFETIME`.
    //
    // This line is where they are drawn because it is the only band on the
    // panel whose height does not depend on what is in it. A toast row of its
    // own - which is what the terminal build had - would grow and shrink the
    // window twice per message.
    //
    // They were computed and thrown away for the whole of the rewrite. Every
    // one of "copied 9 characters", "the clipboard holds no text", "nothing to
    // open", "updating jobs...", "viewer: avwin (this session only)" and
    // "avwin.exe not found on PATH" was raised, given an expiry, kept on the
    // frame clock so the loop would wake to retire it - and never rendered.
    if let Some(toast) = &state.toast {
        return StatusLine {
            text: toast.text.clone(),
            tone: match toast.severity {
                Severity::Info => Tone::Normal,
                Severity::Warn => Tone::Warn,
                Severity::Error => Tone::Bad,
            },
        };
    }

    // A share that cannot be reached outranks everything else: it explains
    // every other oddity on screen.
    //
    // The share that actually failed is named. This used to hand
    // `EnumError::describe` the derived `custpro_path` whatever had gone
    // wrong, so a tree failure was reported against the flat share - and with
    // no flat mapping configured it rendered a leading space and an error code
    // attached to nothing, which is the reported "random os error 03".
    //
    // The countdown to the next automatic retry used to be on this line, and
    // it is gone with the rest of the panel's prose. It was a second thing
    // to read on the one line that has to land, it was the reason the panel
    // asked for a repaint once a second for as long as a drive stayed down,
    // and "F5" is the answer to the question it was answering anyway.
    if let Some((id, Health::Unreachable { err, .. })) = state.index.unreachable() {
        return StatusLine {
            text: format!("{} \u{b7} F5", state.describe_drive_error(id, *err)),
            tone: Tone::Bad,
        };
    }

    // Second, because a drive nobody can reach outranks a viewer nobody can
    // launch - but ahead of everything else, because it is the explanation for
    // a keystroke that is about to appear to do nothing.
    //
    // `F2` is named rather than described: it is one press, it is reversible,
    // and it is the whole fix.
    if state.avwin_missing && state.viewer.may_use_avwin() {
        return StatusLine {
            text: "Enter cannot open anything \u{b7} avwin.exe is not on PATH \u{b7} F2 uses the built-in viewer"
                .into(),
            tone: Tone::Warn,
        };
    }

    // One line for all five of them.
    //
    // There used to be five, and between them they carried a folder count, a
    // queue depth, a file count, a share name, and a reason in brackets -
    // rewriting themselves several times a second in the one place on the
    // panel reserved for things somebody has to act on. Every number in them
    // is either in the diagnostics - which report each drive's entry count,
    // its age and its resident size, at leisure and without rewriting
    // themselves - or is a progress figure for work in flight, which nothing
    // keeps once the work is done and which nobody was acting on. What is
    // left is the one fact that changes what to do: the list is not complete
    // yet, so a code that is missing may not really be missing.
    if state.index.activity != Activity::Idle {
        return StatusLine {
            text: "Indexing\u{2026}".into(),
            tone: Tone::Busy,
        };
    }

    // The file that was selected is not in the list any more, and the cursor
    // has been moved to whatever is now in its place. Worth interrupting the
    // ordinary line for: the next Enter opens something other than what the
    // user was looking at when they reached for it.
    //
    // Maintained since the selection was first tracked by path, asserted by
    // `tests/state_machine.rs` under the name "the user should be told the
    // list shifted", and until now read by nothing outside that test.
    if state.selection_lost {
        return StatusLine {
            text: "The list changed \u{b7} check the highlighted file before opening it".into(),
            tone: Tone::Warn,
        };
    }

    // The phase, where the phase is worth a sentence.
    //
    // `Idle`, `Local` and `Verified` are the quiet ones, and they yield to the
    // index below rather than returning here. Everything else is either
    // transient or something the user has to do about, and outranks an ambient
    // note about the index - "Keep typing" while a code is half-entered is
    // more use than "no file list yet", even when both are true.
    // Above the phase, because a partial live answer outranks a confirmed
    // local one: the phase can only say the indexes are current, and the live
    // share is the part of the answer that may be missing.
    if let Some(live) = &state.live {
        // "Asking jobs..." is gone. A live share answers in well under a
        // second, and a line that appears and disappears inside one is a
        // flicker rather than information - the spinner beside the count
        // already says something is in flight.
        if let Some((id, detail)) = live.failed.first() {
            return StatusLine {
                text: format!(
                    "{} did not answer \u{b7} {}",
                    state.settings.routes.label(*id),
                    detail
                ),
                tone: Tone::Warn,
            };
        }
        if let Some((id, skip)) = live.skipped.first() {
            return StatusLine {
                text: format!(
                    "{} was not searched \u{b7} {}",
                    state.settings.routes.label(*id),
                    skip.label()
                ),
                tone: Tone::Warn,
            };
        }
        if let Some((id, coverage)) = live.worst() {
            let reached = coverage.dirs_queried;
            let total = coverage.dirs_queried + coverage.dirs_skipped;
            return StatusLine {
                text: format!(
                    "Searched {reached} of {total} folders on {}",
                    state.settings.routes.label(id)
                ),
                tone: Tone::Warn,
            };
        }
    }

    let phase = match &state.phase {
        QueryPhase::Idle | QueryPhase::Local => None,

        QueryPhase::BadQuery { detail } => Some(StatusLine {
            text: crate::view::sentence(detail),
            tone: Tone::Warn,
        }),
        QueryPhase::NoShares => Some(StatusLine {
            text: "No drives are set up \u{b7} run files --check-config".into(),
            tone: Tone::Warn,
        }),
        // Silent, all three. A local search finishes in a few milliseconds
        // and a verification in a few hundred, so these were three lines
        // that appeared and vanished faster than they could be read - and
        // the last of them, "Up to date", is the line that says nothing is
        // wrong, which is what an empty status line already says.
        QueryPhase::LocalPending | QueryPhase::Verifying { .. } | QueryPhase::Verified { .. } => {
            None
        }
        QueryPhase::VerifyFailed { detail } => Some(StatusLine {
            text: format!("Showing the saved list \u{b7} {detail}"),
            tone: Tone::Warn,
        }),
    };
    if let Some(line) = phase {
        return line;
    }

    // Nothing is happening, so the only thing left worth saying is that the
    // index is wrong - and usually it is not, so the line is empty.
    //
    // An empty footer is the point of all this. The count sits to the left of
    // it and the keys to the right; the middle used to hold
    // `Ready · 1,284,551 files · updated 2m ago`, which was a number nobody was
    // looking for, in the one place the program has to say things people must
    // act on, rewriting itself once a second.
    match index_warning(state, wall) {
        Some(warning) => StatusLine {
            text: warning,
            tone: Tone::Warn,
        },
        None => StatusLine {
            text: String::new(),
            tone: Tone::Normal,
        },
    }
}

/// What is wrong with the index, if anything.
///
/// All that is left of a summary that used to lead every healthy status line
/// with `Ready · 1,284,551 files · updated 2m ago`. Three things were wrong
/// with that. It competed for width with the result count on the same line,
/// and lost, so it was usually an ellipsis. Its age ticked, so the line
/// rewrote itself once a second and then once a minute for as long as the
/// panel was open. And it was ambient: a number nobody was looking for, in the
/// one place the program has to say things people must act on.
///
/// The ages moved to `F5` when the drive picker was built - see
/// [`crate::view::shares`] - which is where somebody goes when they suspect
/// the list is out of date. What stays here is only what they cannot go and
/// look for, because they do not yet know to look.
///
/// `None` when the index is healthy, which is what makes a healthy footer
/// quiet.
fn index_warning(state: &AppState, wall: SystemTime) -> Option<String> {
    // Whether there is anything to say is asked of the state, because the
    // panel asks the same question before it decides whether to leave room for
    // this line - and a footer whose height and contents disagree is a message
    // drawn into a band of no height. This function owns the words; it does
    // not own the question.
    if !state.has_standing_notice(wall) {
        return None;
    }

    let status = &state.index;
    if status.origin.is_none() {
        return Some("No file list yet".into());
    }

    // Named, because a degraded share is one somebody has to go and look at.
    // A name only when there are several: "jobs: no live updates" is useful
    // and "custompro: no live updates" when custompro is the only share is a
    // word nobody needed.
    //
    // The name is not capitalised and the fragment after it is not either: a
    // drive is called whatever the configuration calls it, and a line that
    // leads with `Jobs` names a drive that does not exist. Where no name
    // leads, `view::sentence` capitalises the fragment instead - which is the
    // whole reason those fragments are written lowercase.
    let named = |id| {
        if status.configured > 1 {
            format!("{}: ", state.settings.routes.label(id))
        } else {
            String::new()
        }
    };
    let lead = |id, rest: &str| match named(id) {
        name if name.is_empty() => crate::view::sentence(rest),
        name => format!("{name}{rest}"),
    };

    if let Some((id, reason)) = status.degraded() {
        return Some(lead(id, reason.label()));
    }

    // Which share to refresh, and why - not merely that something is stale.
    // Somebody told only that "an index is out of date" refreshes everything,
    // and everything is what a few hundred people must not all read at once.
    if state.settings.stale_notices
        && let Some((id, reason)) = status.stale_at(wall)
    {
        return Some(format!("{} \u{b7} F5", lead(id, reason.label())));
    }

    None
}

/// The facts the footer reserves room for, so neither can be truncated.
///
/// Everything else on that line is prose, and prose is what the buttons on
/// the right eat into: it is laid out in whatever they left and ellipsised
/// to fit. These are not prose. They are values somebody is *looking* for,
/// and a value that might not be there is not worth putting on screen.
///
/// # Why each of them is here
///
/// The count, because the panel had no way at all of saying there was more
/// than it was showing: two hundred and ninety-two results could sit in `hits`,
/// unreachable and unmentioned, behind a list of eight.
///
/// The viewer, because `F2` changes what `Enter` does and nothing on screen
/// said which way it had been changed. The chip that names it is real but it is
/// `Priority::Normal`, and at the shipped width it does not fit - so on its own
/// it would be an answer that is there until the moment somebody needs it.
/// How many the code found.
///
/// Empty when there is nothing to count, so the footer says nothing rather
/// than a zero.
///
/// This used to be a *range* - "9-16 of 300" - and the argument for it was
/// sound while it held: the panel drew a twelve-row window over the result
/// list, the other 288 were unreachable, and somebody holding Down through
/// three hundred drawings had nothing else to tell them how far they had got.
///
/// The content band is a scroller now. Every result is reachable, the
/// scrollbar says how far down them you are, and a range printed beside it
/// would be a second, worse answer to a question the bar already answers. So
/// what is left is the fact the range was wrapped around: how many there are.
///
/// With its noun, because a bare "300" at the foot of a list reads as a page
/// number, a code, or anything but a count.
pub fn found(state: &AppState) -> String {
    let found = state.matched as usize;
    if found == 0 || state.hits.is_empty() {
        return String::new();
    }
    let unit = if found == 1 { "item" } else { "items" };
    format!("{} {unit}", humanize::count(found))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use std::time::Instant;

    use crate::app::event::{AppEvent, IndexMsg};
    use crate::app::key::{Key, KeyEvent, Mods};
    use crate::app::state::AppState;
    use crate::config::Settings;
    use crate::index::errors::EnumError;
    use crate::index::schedule::ScanReason;
    use crate::index::store::StaleReason;
    use crate::index::store::{DegradeReason, IndexStatus, Origin};
    use crate::paths::{MappingId, MappingKind};
    use crate::search::matcher::Hit;
    use std::sync::Arc;

    const EPOCH: SystemTime = SystemTime::UNIX_EPOCH;

    /// One share, which is what the assertions below are written against.
    /// Several-share rendering has its own tests at the end of this module.
    fn state_at(now: Instant) -> AppState {
        AppState::new(
            Settings::for_mapping("custompro", crate::config::CUSTPRO_PATH, MappingKind::Flat),
            now,
        )
    }

    /// The same, in developer mode - which is what puts the drive's path and
    /// the operating system's code on the line.
    ///
    /// Several tests below are *about* that detail reaching the screen, so
    /// they have to ask for it. What the ordinary user sees is asserted
    /// separately, by `a_drive_failure_is_a_sentence_until_somebody_asks`.
    fn dev_state_at(now: Instant) -> AppState {
        let mut s = state_at(now);
        s.settings.dev_mode = true;
        s
    }

    /// Two shares, for the rendering that only exists when there are several.
    fn state_with_two_shares(now: Instant) -> AppState {
        AppState::new(Settings::default(), now)
    }

    fn publish(s: &mut AppState, id: MappingId, now: Instant, f: impl FnOnce(&mut IndexStatus)) {
        let mut status = IndexStatus::default();
        f(&mut status);
        s.update(
            AppEvent::Index(IndexMsg::Status {
                id,
                status: Arc::new(status),
            }),
            now,
        );
    }

    fn with_index(s: &mut AppState, now: Instant, f: impl FnOnce(&mut IndexStatus)) {
        let mut status = IndexStatus::default();
        f(&mut status);
        s.update(
            AppEvent::Index(IndexMsg::Status {
                id: MappingId(0),
                status: Arc::new(status),
            }),
            now,
        );
    }

    fn healthy_status(entries: u32, age: Duration) -> impl FnOnce(&mut IndexStatus) {
        move |st: &mut IndexStatus| {
            st.origin = Some(Origin::Network);
            st.entries = entries;
            st.built_at = Some(EPOCH);
            let _ = age;
        }
    }

    fn type_code(s: &mut AppState, now: Instant) {
        for c in "11-D-0704".chars() {
            s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)), now);
        }
    }

    fn hit(name: &str) -> Hit {
        Hit {
            path: Arc::from(format!("V:\\{name}").as_str()),
            name: Arc::from(name),
            match_pos: 0,
            index: 0,
        }
    }

    fn give_results(s: &mut AppState, now: Instant, hits: Vec<Hit>, matched: u32, total: u32) {
        s.update(
            AppEvent::Search(crate::app::event::SearchMsg {
                epoch: s.query_epoch(),
                query: crate::search::query::Query::parse(s.input.text()),
                elapsed: Duration::from_micros(300),
                result: Ok(crate::search::matcher::SearchOutcome {
                    hits,
                    matched,
                    total,
                    cancelled: false,
                    unicode_fallback: false,
                }),
            }),
            now,
        );
    }

    #[test]
    fn an_idle_app_with_a_healthy_index_says_nothing_at_all() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(1_284_551, Duration::ZERO));

        let line = render(&s, EPOCH);
        assert_eq!(line.text, "", "a healthy footer is a quiet one");
        assert_eq!(line.tone, Tone::Normal);
    }

    /// The counts and the age did not move somewhere shorter - they left.
    ///
    /// They were ambient: a number nobody was looking for, sitting in the one
    /// place the program has to say things people must act on, rewriting
    /// itself once a second as the age ticked. The ages are in the drive
    /// picker, which is where somebody goes when they suspect the list is out
    /// of date; how many matched is in the reserved slot beside this line.
    #[test]
    fn the_file_count_and_the_index_age_are_not_on_this_line() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(1_284_551, Duration::ZERO));
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 1_284_551);

        let line = render(&s, EPOCH + Duration::from_secs(180)).text;
        for gone in ["Ready", "1,284,551", "updated", "3m", "match"] {
            assert!(!line.contains(gone), "{gone:?} is still there: {line:?}");
        }
    }

    /// One character is a search now, so the line that used to say "keep
    /// typing" says what it says for any other search in flight. The old
    /// line, and the phase behind it, are gone: there is no such thing as a
    /// query that is too short any more, only one with nothing on it.
    #[test]
    fn one_character_is_searched_for_rather_than_refused() {
        let now = Instant::now();
        let mut s = state_at(now);
        s.update(
            AppEvent::Key(KeyEvent::new(Key::Char('a'), Mods::NONE)),
            now,
        );
        let line = render(&s, EPOCH).text;
        assert!(!line.contains("Keep typing"), "{line:?}");
    }

    #[test]
    /// An unusual query is searched for rather than refused.
    ///
    /// This used to assert "not a recognised job code": a pattern had to
    /// recognise a string before anything would look anywhere, so an
    /// unfamiliar code was declined rather than searched. With both shares
    /// indexed there is nothing to recognise - an odd query simply finds
    /// nothing, which is a different and far more honest answer, because
    /// "I will not look" and "I looked and it is not there" were previously
    /// indistinguishable.
    fn an_unusual_query_is_searched_for_rather_than_refused() {
        let now = Instant::now();
        let mut s = state_at(now);
        for c in "!!!".chars() {
            s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)), now);
        }
        let line = render(&s, EPOCH);
        assert_ne!(line.text, "not a recognised job code");
        assert_eq!(s.phase, QueryPhase::LocalPending);
    }

    /// The refusal still exists for a configuration made only of patterns,
    /// where declining is the honest answer.
    /// With no share enabled, the status line says so rather than searching
    /// nothing and reporting no matches.
    ///
    /// This used to read "not a recognised job code": a pattern had to match
    /// before anything would look anywhere. Both shares are indexed now, so
    /// the only way a query reaches nothing is a configuration with nothing
    /// in it.
    #[test]
    fn a_configuration_with_no_enabled_share_says_so() {
        let now = Instant::now();
        // Built directly rather than parsed: the config layer refuses a file
        // with no enabled mapping in it, which is why this phase is a
        // defensive branch rather than something a user can reach by editing
        // the file.
        let routes = crate::paths::Routes::new(Vec::new(), crate::paths::ConfigSource::BuiltIn);
        let settings = Settings::with_routes(std::sync::Arc::new(routes), |s| s);

        let mut s = AppState::new(settings, now);
        for c in "11-D-0704".chars() {
            s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)), now);
        }
        let line = render(&s, EPOCH);
        assert_eq!(
            line.text,
            "No drives are set up \u{b7} run files --check-config"
        );
        assert_eq!(line.tone, Tone::Warn);
    }

    #[test]
    fn the_count_does_not_move_as_the_list_is_walked() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_code(&mut s, now);
        let many: Vec<_> = (0..300).map(|i| hit(&format!("a{i}.pdf"))).collect();
        give_results(&mut s, now, many, 300, 9_000);

        assert_eq!(found(&s), "300 items");

        // To the foot of the list, the way an arrow key does it. This used to
        // read "295-300 of 300" by the end, because the panel drew a window
        // over the list and the range said where the window was. The band
        // scrolls now and the scrollbar says that, so the number beside it is
        // a number rather than a second opinion about the same thing.
        for _ in 0..299 {
            s.update(AppEvent::Key(KeyEvent::new(Key::Down, Mods::NONE)), now);
        }
        assert_eq!(found(&s), "300 items");
    }

    /// What the index found, not what came back: the search is capped at
    /// [`crate::config::MAX_RESULTS`], and the difference is the whole reason
    /// there is a number in the footer at all.
    #[test]
    fn a_capped_result_list_says_how_many_it_really_found() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_code(&mut s, now);
        let many: Vec<_> = (0..40).map(|i| hit(&format!("a{i}.pdf"))).collect();
        give_results(&mut s, now, many, 4321, 9_000);

        assert_eq!(found(&s), "4,321 items");
    }

    #[test]
    fn a_short_result_list_is_reported_as_a_count() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a"), hit("b")], 2, 9_000);

        assert_eq!(found(&s), "2 items");
    }

    #[test]
    /// Nothing found is said by the body, at length, with what to try next -
    /// see [`crate::view::empty`]. Repeating it here as a count of zero would
    /// be the same news twice.
    fn no_matches_is_left_to_the_body_to_explain() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_code(&mut s, now);
        give_results(&mut s, now, vec![], 0, 9_000);

        assert_eq!(render(&s, EPOCH).text, "");
        assert_eq!(found(&s), "");
    }

    /// The condition the previous implementation rendered as `0 / 0 files`.
    #[test]
    fn an_unreachable_drive_says_so_and_what_to_press() {
        let now = Instant::now();
        let mut s = dev_state_at(now);
        with_index(&mut s, now, |st| {
            st.health = Health::Unreachable {
                err: EnumError::Transient(53),
                since: now,
                attempt: 2,
                next_retry_at: now + Duration::from_secs(42),
            };
        });

        let line = render(&s, EPOCH);
        assert_eq!(line.tone, Tone::Bad);
        assert!(line.text.contains("unreachable"), "{}", line.text);
        assert!(line.text.contains("os error 53"), "{}", line.text);
        assert!(
            line.text.contains("F5"),
            "the user needs a way out: {}",
            line.text
        );
        // The countdown to the next automatic retry is deliberately not
        // here. It was a second thing to read on the one line that has to
        // land, and it was the reason the panel woke once a second for as
        // long as a drive stayed down.
        assert!(!line.text.contains("retrying"), "{}", line.text);
    }

    /// Every kind of index work says the same five characters.
    ///
    /// There used to be five tests here and five branches behind them, and
    /// between them they asserted a folder count, a queue depth, a file
    /// count, a share name and a reason in brackets - all of it rewriting
    /// itself several times a second in the one place on the panel reserved
    /// for things somebody has to act on. Every one of those numbers is in
    /// the diagnostics now. What the panel says is the fact that changes
    /// what to do: the list is not finished, so a code that is missing may
    /// not really be missing.
    #[test]
    fn every_kind_of_indexing_reads_the_same() {
        let now = Instant::now();
        for activity in [
            Activity::Scanning { seen: 812_000 },
            Activity::Queued,
            Activity::Walking {
                dirs: 100,
                queued: 20,
                files: 4000,
            },
            Activity::LoadingDisk,
            Activity::Persisting,
        ] {
            let named = format!("{activity:?}");
            let mut s = state_at(now);
            with_index(&mut s, now, |st| st.activity = activity);
            let line = render(&s, EPOCH);
            assert_eq!(line.text, "Indexing\u{2026}", "{named}");
            assert_eq!(line.tone, Tone::Busy, "{named}");
        }
    }

    /// And none of them carries a number, which is the point.
    #[test]
    fn indexing_never_puts_a_count_on_the_status_line() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, |st| {
            st.activity = Activity::Scanning { seen: 812_000 };
            st.last_scan_reason = Some(ScanReason::StampMoved);
        });
        let line = render(&s, EPOCH);
        assert!(!line.text.contains("812"), "{}", line.text);
        assert!(!line.text.contains('('), "{}", line.text);
    }

    /// A healthy index says nothing at all.
    ///
    /// This used to assert that the line did not contain "checked", against a
    /// line that reads "Checked just now" - a case-sensitive negative that
    /// passed on the capital and tested nothing for as long as it existed.
    /// What it was reaching for is that the footer is *empty* when all is
    /// well, which is the whole argument of `index_warning`, so it says that.
    #[test]
    fn a_freshly_built_index_says_nothing_at_all() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(10, Duration::ZERO));
        let line = render(&s, EPOCH);
        assert_eq!(line.text, "", "the quiet case is quiet");
    }

    /// A search in flight says nothing, and neither does one that came back
    /// clean.
    ///
    /// Three lines used to live here - "Searching...", "Checking the
    /// drive...", and "Up to date" or "Checked just now". A local search
    /// finishes in a few milliseconds and a verification in a few hundred,
    /// so the first two appeared and vanished faster than they could be
    /// read; the third is the line that says nothing is wrong, which is
    /// exactly what an empty status line already says.
    ///
    /// What is still on screen while a search is in flight is the spinner
    /// beside the count, which does not rewrite itself and does not occupy
    /// the line reserved for things that matter.
    #[test]
    fn a_search_in_flight_and_a_search_confirmed_both_say_nothing() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(100, Duration::ZERO));
        type_code(&mut s, now);
        assert_eq!(render(&s, EPOCH).text, "", "a search was announced");

        give_results(&mut s, now, vec![hit("a.pdf")], 1, 100);
        s.update(AppEvent::Tick, now + crate::config::VERIFY_DEBOUNCE);
        assert_eq!(render(&s, EPOCH).text, "", "a verification was announced");
        // The count is beside the line, not in it, so a round trip in
        // flight never costs the number somebody is reading.
        assert_eq!(found(&s), "1 item");

        s.update(
            AppEvent::Verify(crate::app::event::VerifyMsg {
                epoch: s.query_epoch(),
                query: crate::search::query::Query::parse(s.input.text()),
                elapsed: Duration::from_millis(42),
                outcome: crate::search::verify::VerifyOutcome::Server {
                    hits: vec![hit("a.pdf")],
                    matched: 1,
                    capped: false,
                    audit: crate::search::verify::AuditVerdict::Consistent,
                },
            }),
            now,
        );
        assert_eq!(render(&s, EPOCH).text, "", "good news was reported");
    }

    #[test]
    fn a_degraded_index_is_flagged_without_hiding_the_results() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, |st| {
            st.origin = Some(Origin::Network);
            st.entries = 10;
            st.built_at = Some(EPOCH);
            st.health = Health::Degraded {
                reason: DegradeReason::ServerFilterDisabled,
                since: now,
            };
        });
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 10);

        let line = render(&s, EPOCH);
        assert!(
            line.text.contains("Server filter disabled"),
            "{}",
            line.text
        );
        assert_eq!(line.tone, Tone::Warn);
        // And the results are still reachable beside it: a warning about the
        // index must not cost the count of what it found.
        assert_eq!(found(&s), "1 item");
    }

    #[test]
    fn a_missing_index_is_described_rather_than_faked() {
        let now = Instant::now();
        let s = state_at(now);
        assert!(render(&s, EPOCH).text.contains("No file list yet"));
    }
    // --- naming the share that actually failed --------------------------
    //
    // These cover the reported "random os error 03". `EnumError::describe`
    // was handed the derived `custpro_path` whatever had gone wrong, so a
    // tree failure named the flat share - and with no flat mapping enabled it
    // rendered a leading space and an error code attached to nothing.

    fn unreachable(err: EnumError) -> impl FnOnce(&mut IndexStatus) {
        move |st: &mut IndexStatus| {
            st.health = Health::Unreachable {
                err,
                since: Instant::now(),
                attempt: 1,
                next_retry_at: Instant::now() + Duration::from_secs(30),
            };
        }
    }

    #[test]
    fn an_unreachable_tree_names_the_tree_not_the_flat_share() {
        let now = Instant::now();
        let mut s = state_with_two_shares(now);
        s.settings.dev_mode = true;
        // Mapping 1 is the tree in the shipped table.
        publish(
            &mut s,
            MappingId(1),
            now,
            unreachable(EnumError::PathNotFound(3)),
        );

        let line = render(&s, EPOCH);
        assert!(
            line.text.contains("R:"),
            "the failing share must be named: {}",
            line.text
        );
        assert!(
            !line.text.contains("custpro"),
            "naming the wrong share is the bug: {}",
            line.text
        );
        assert!(line.text.contains("os error 3"), "{}", line.text);
    }

    /// The literal reported symptom: a message with nothing in front of it.
    #[test]
    fn an_error_never_renders_with_an_empty_target() {
        let now = Instant::now();
        let mut s = dev_state_at(now);
        publish(
            &mut s,
            MappingId(0),
            now,
            unreachable(EnumError::PathNotFound(3)),
        );

        let line = render(&s, EPOCH);
        assert!(
            !line.text.starts_with(' '),
            "the reported 'random os error 03': {}",
            line.text
        );
        assert!(line.text.contains("os error 3"), "{}", line.text);
    }

    /// The line reads the same however close the next retry is.
    ///
    /// It used to carry a countdown, which needed a special case at zero -
    /// `humanize::elapsed(ZERO)` is "0us", and a drive reported as "retrying
    /// in 0us" reads as a stopwatch rather than as a retry that is due. Both
    /// the countdown and its special case are gone: it was a second thing to
    /// read on the one line that has to land, and it was the reason the
    /// panel asked for a repaint once a second for as long as a drive stayed
    /// down.
    #[test]
    fn the_unreachable_line_does_not_change_as_the_retry_approaches() {
        let now = Instant::now();
        let mut far = state_at(now);
        let mut due = state_at(now);
        for (s, at) in [(&mut far, now + Duration::from_secs(42)), (&mut due, now)] {
            publish(s, MappingId(0), now, move |st| {
                st.health = Health::Unreachable {
                    err: EnumError::Transient(53),
                    since: now,
                    attempt: 1,
                    next_retry_at: at,
                };
            });
        }
        assert_eq!(render(&far, EPOCH).text, render(&due, EPOCH).text);
        assert!(!render(&due, EPOCH).text.contains("0us"));
    }

    // --- several shares -------------------------------------------------

    #[test]
    fn a_degraded_share_is_named_when_there_are_several() {
        let now = Instant::now();
        let mut s = state_with_two_shares(now);
        publish(&mut s, MappingId(0), now, |st| {
            st.origin = Some(Origin::Network);
            st.entries = 10;
            st.built_at = Some(EPOCH);
        });
        publish(&mut s, MappingId(1), now, |st| {
            st.origin = Some(Origin::Network);
            st.entries = 10;
            st.built_at = Some(EPOCH);
            st.health = Health::Degraded {
                reason: DegradeReason::LiveUpdatesUnavailable,
                since: now,
            };
        });
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 20);

        let line = render(&s, EPOCH);
        assert!(line.text.contains("jobs: no live updates"), "{}", line.text);
    }

    /// A share waiting behind another share's walk is still indexing, and
    /// must not look idle.
    ///
    /// It used to have a line of its own naming how many shares were
    /// waiting. Which share is queued behind which is in the diagnostics;
    /// what the panel has to say is that the list is not finished.
    #[test]
    fn a_share_queued_behind_a_walk_does_not_look_idle() {
        let now = Instant::now();
        let mut s = state_with_two_shares(now);
        publish(&mut s, MappingId(1), now, |st| {
            st.activity = Activity::Queued;
        });
        let line = render(&s, EPOCH);
        assert_eq!(line.text, "Indexing\u{2026}", "{}", line.text);
        assert_eq!(line.tone, Tone::Busy);
    }
    // --- which share to refresh ------------------------------------------
    //
    // The whole point of naming it: somebody told only that "an index is out
    // of date" refreshes everything, and everything is what a few hundred
    // people must not all read off one server at once.

    fn ready(entries: u32) -> impl FnOnce(&mut IndexStatus) {
        move |st: &mut IndexStatus| {
            st.origin = Some(Origin::Network);
            st.entries = entries;
            st.built_at = Some(EPOCH);
        }
    }

    fn typed_with_results(s: &mut AppState, now: Instant, total: u32) {
        type_code(s, now);
        give_results(s, now, vec![hit("a.pdf")], 1, total);
    }

    #[test]
    fn a_share_that_missed_changes_is_named_and_says_what_to_press() {
        let now = Instant::now();
        let mut s = state_with_two_shares(now);
        publish(&mut s, MappingId(0), now, ready(10));
        publish(&mut s, MappingId(1), now, |st| {
            ready(10)(st);
            st.stale = Some(StaleReason::EventsLost);
        });
        typed_with_results(&mut s, now, 20);

        let line = render(&s, EPOCH);
        assert!(line.text.contains("jobs"), "{}", line.text);
        assert!(line.text.contains("changes were missed"), "{}", line.text);
        assert!(line.text.contains("F5"), "{}", line.text);
    }

    /// A reported reason outranks mere age: "changes were missed" says the
    /// list is incomplete, which is worth acting on however recently it was
    /// built.
    #[test]
    fn a_reported_reason_outranks_age() {
        let now = Instant::now();
        let mut s = state_at(now);
        publish(&mut s, MappingId(0), now, |st| {
            ready(10)(st);
            st.stale = Some(StaleReason::EventsLost);
        });
        typed_with_results(&mut s, now, 10);

        // Old enough that age alone would also have fired.
        let wall = EPOCH + crate::config::MAX_INDEX_AGE + Duration::from_secs(60);
        let line = render(&s, wall);
        assert!(line.text.contains("Changes were missed"), "{}", line.text);
        assert!(
            !line.text.contains("not refreshed recently"),
            "the weaker reason should not be shown as well: {}",
            line.text
        );
    }

    /// Age is judged when the line is drawn, not stored: an on-demand share's
    /// actor may not wake for hours, and a flag it never set would be a
    /// promise the status line could not keep.
    #[test]
    fn an_index_nobody_has_refreshed_says_so_on_its_own() {
        let now = Instant::now();
        let mut s = state_at(now);
        publish(&mut s, MappingId(0), now, ready(10));
        typed_with_results(&mut s, now, 10);

        let fresh = render(&s, EPOCH + Duration::from_secs(60));
        assert!(
            !fresh.text.to_lowercase().contains("not refreshed"),
            "{}",
            fresh.text
        );

        let wall = EPOCH + crate::config::MAX_INDEX_AGE + Duration::from_secs(60);
        let old = render(&s, wall);
        assert!(old.text.contains("Not refreshed recently"), "{}", old.text);
    }

    /// Silent mode, for somebody handed the tool who does not need current
    /// data and should not be nagged about it.
    #[test]
    fn stale_notices_can_be_turned_off() {
        let now = Instant::now();
        let quiet = Settings {
            stale_notices: false,
            ..Settings::for_mapping("custompro", crate::config::CUSTPRO_PATH, MappingKind::Flat)
        };
        let mut s = AppState::new(quiet, now);
        publish(&mut s, MappingId(0), now, |st| {
            ready(10)(st);
            st.stale = Some(StaleReason::EventsLost);
        });
        typed_with_results(&mut s, now, 10);

        let wall = EPOCH + crate::config::MAX_INDEX_AGE + Duration::from_secs(60);
        let line = render(&s, wall);
        assert_eq!(
            line.text, "",
            "silent mode must not nag, about events or about age"
        );
        // The ages are still one keypress away, which is the whole bargain:
        // not shown, not hidden. See `view::shares`, which F5 opens.
        assert!(s.index.age(wall).is_some());
    }

    // --- the house style --------------------------------------------------

    /// Every line this function can produce, held to the house style.
    ///
    /// A battery rather than a check bolted onto each test above, because the
    /// thing being guarded is that they are consistent *with each other* - and
    /// no test of one line can see that. This footer once alternated between
    /// `Searching...`, `Building the file list - 812,000 files so far` and
    /// `nothing to open` within a few seconds of each other, each of them
    /// perfectly reasonable on its own.
    ///
    /// A line that this cannot reach is a line nothing here is checking, so a
    /// new branch in `render` belongs in this list.
    fn every_line() -> Vec<String> {
        let now = Instant::now();
        let wall_old = EPOCH + crate::config::MAX_INDEX_AGE + Duration::from_secs(60);
        let mut out = Vec::new();
        let mut say = |s: &AppState, wall| out.push(render(s, wall).text);

        // Nothing happening, and nothing wrong.
        let mut quiet = state_at(now);
        with_index(&mut quiet, now, healthy_status(10, Duration::ZERO));
        say(&quiet, EPOCH);

        // A drive that cannot be reached, which outranks everything.
        let mut down = state_at(now);
        with_index(&mut down, now, unreachable(EnumError::Transient(53)));
        say(&down, EPOCH);

        // The viewer that cannot be launched.
        let mut no_viewer = state_at(now);
        no_viewer.avwin_missing = true;
        no_viewer.viewer = crate::config::ViewerKind::Avwin;
        say(&no_viewer, EPOCH);

        // Every activity, on one drive and on several.
        for several in [false, true] {
            for activity in [
                Activity::Scanning { seen: 812_000 },
                Activity::Queued,
                Activity::Walking {
                    dirs: 9_000,
                    queued: 400,
                    files: 812_000,
                },
                Activity::LoadingDisk,
                Activity::Persisting,
            ] {
                let mut s = if several {
                    state_with_two_shares(now)
                } else {
                    state_at(now)
                };
                let reason = Some(ScanReason::StampMoved);
                publish(&mut s, MappingId(0), now, |st| {
                    st.activity = activity;
                    st.last_scan_reason = reason;
                });
                say(&s, EPOCH);
            }
        }

        // The list moved under the selection.
        let mut shifted = state_at(now);
        with_index(&mut shifted, now, healthy_status(10, Duration::ZERO));
        shifted.selection_lost = true;
        say(&shifted, EPOCH);

        // Half a code typed, and a code with nowhere to look.
        let mut short = state_at(now);
        short.update(
            AppEvent::Key(KeyEvent::new(Key::Char('1'), Mods::NONE)),
            now,
        );
        say(&short, EPOCH);

        let routes = crate::paths::Routes::new(Vec::new(), crate::paths::ConfigSource::BuiltIn);
        let mut none = AppState::new(
            Settings::with_routes(std::sync::Arc::new(routes), |s| s),
            now,
        );
        type_code(&mut none, now);
        say(&none, EPOCH);

        // Every phase the search goes through.
        let mut searching = state_at(now);
        with_index(&mut searching, now, healthy_status(10, Duration::ZERO));
        type_code(&mut searching, now);
        for phase in [
            QueryPhase::LocalPending,
            QueryPhase::Verifying { since: now },
            QueryPhase::Verified {
                took: Duration::from_millis(12),
                by_stamp: true,
            },
            QueryPhase::Verified {
                took: Duration::from_millis(12),
                by_stamp: false,
            },
            QueryPhase::VerifyFailed {
                detail: "the drive stopped answering".into(),
            },
        ] {
            searching.phase = phase;
            say(&searching, EPOCH);
        }

        // The codes used before. Opening is what commits a code to the list,
        // so the code is opened, the notice that raised is cleared - it would
        // otherwise outrank the line being asked for - and then Up.
        let mut recalling = state_at(now);
        with_index(&mut recalling, now, healthy_status(10, Duration::ZERO));
        typed_with_results(&mut recalling, now, 10);
        recalling.update(AppEvent::Key(KeyEvent::new(Key::Enter, Mods::NONE)), now);
        recalling.update(AppEvent::Key(KeyEvent::new(Key::Esc, Mods::NONE)), now);
        recalling.toast = None;
        recalling.update(AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)), now);
        assert!(
            recalling.history.cursor().is_some(),
            "the recall line was never reached, so nothing here checks it"
        );
        say(&recalling, EPOCH);

        // Nothing to report but the index itself: no list, degraded, stale -
        // each with one drive, where the fragment is capitalised, and with
        // several, where the drive's own name leads instead.
        let bare = state_at(now);
        say(&bare, EPOCH);

        for several in [false, true] {
            for reason in [
                DegradeReason::LiveUpdatesUnavailable,
                DegradeReason::PartiallyUnreadable,
            ] {
                let mut s = if several {
                    state_with_two_shares(now)
                } else {
                    state_at(now)
                };
                publish(&mut s, MappingId(0), now, move |st| {
                    ready(10)(st);
                    st.health = Health::Degraded { reason, since: now };
                });
                typed_with_results(&mut s, now, 10);
                say(&s, EPOCH);
            }
            for stale in [
                StaleReason::EventsLost,
                StaleReason::NoLiveUpdates,
                StaleReason::Age,
            ] {
                let mut s = if several {
                    state_with_two_shares(now)
                } else {
                    state_at(now)
                };
                publish(&mut s, MappingId(0), now, move |st| {
                    ready(10)(st);
                    st.stale = Some(stale);
                });
                typed_with_results(&mut s, now, 10);
                say(&s, wall_old);
            }
        }

        out
    }

    #[test]
    fn every_status_line_keeps_the_house_style() {
        let lines = every_line();
        crate::view::style::check_all(
            "the status line",
            lines.iter().map(String::as_str),
            crate::view::style::Slot::Status,
        );
    }

    /// A line starts the way a line starts.
    ///
    /// Except where a drive's own name leads it: a drive is called whatever
    /// the configuration calls it, and `Jobs: no live updates` names a drive
    /// nobody configured. So the rule is applied to the lines this module
    /// writes end to end, and the named ones are checked for the shape they
    /// do have.
    ///
    /// Two shapes now, not one. `jobs: no live updates` is the older; the
    /// other is `custompro unreachable`, which is what a drive failure reads
    /// as outside developer mode - it used to lead with the drive's *path*,
    /// and a path starts with a capital letter by accident rather than by
    /// design.
    #[test]
    fn every_status_line_starts_the_way_a_line_should() {
        let configured: Vec<String> = ["custompro", "jobs"]
            .iter()
            .map(|n| format!("{n} "))
            .collect();

        for line in every_line() {
            if configured.iter().any(|name| line.starts_with(name)) {
                continue;
            }
            if let Some((name, rest)) = line.split_once(": ") {
                // A name leading the line, which happens only with several
                // drives configured. Everything after it stays lowercase.
                if !name.contains(' ') {
                    assert!(
                        !rest.is_empty(),
                        "{line:?} names a drive and then says nothing"
                    );
                    continue;
                }
            }
            assert!(
                crate::view::style::starts_capitalised(&line),
                "{line:?} does not start a sentence"
            );
        }
    }
}
