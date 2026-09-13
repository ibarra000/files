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

use std::ops::Range;
use std::time::{Instant, SystemTime};

use crate::app::state::{AppState, QueryPhase, Severity};
use crate::config::ViewerKind;
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
/// something is in flight - is added by [`crate::gui::overlay`], which is the one
/// place that knows the whole frame's clock. Putting it here as well is how the
/// line came to be drawn with two spinners on it.
pub fn render(state: &AppState, now: Instant, wall: SystemTime) -> StatusLine {
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
    if let Some((
        id,
        Health::Unreachable {
            err, next_retry_at, ..
        },
    )) = state.index.unreachable()
    {
        let retry = next_retry_at.saturating_duration_since(now);
        // `humanize::elapsed(ZERO)` is "0us", which reads as a stopwatch
        // rather than as a retry that is already due.
        let when = if retry.is_zero() {
            "retrying now".to_string()
        } else {
            format!("retrying in {}", humanize::elapsed(retry))
        };
        return StatusLine {
            text: format!(
                "{} · {when} · F5 to retry now",
                err.describe(&state.settings.routes.path_label(id))
            ),
            tone: Tone::Bad,
        };
    }

    // Second, because a drive nobody can reach outranks a viewer nobody can
    // launch - but ahead of everything else, because it is the explanation for
    // a keystroke that is about to appear to do nothing.
    //
    // `F2` is named rather than described: it is one press, it is reversible,
    // and it is the whole fix.
    if state.avwin_missing && state.viewer == ViewerKind::Avwin {
        return StatusLine {
            text: "avwin.exe is not on PATH, so Enter cannot open anything ·                    F2 switches to the built-in viewer"
                .into(),
            tone: Tone::Warn,
        };
    }

    // Browsing the codes used before. Carries the position, so stepping
    // through a long list does not feel bottomless - which is what the
    // terminal build's " History (2 of 3) " title said, and what nothing has
    // said since it became the empty state.
    if let Some(cursor) = state.history.cursor() {
        return StatusLine {
            text: format!(
                "Codes you used before · {} of {} · Enter to use it, Esc to go back",
                cursor + 1,
                state.history.len()
            ),
            tone: Tone::Normal,
        };
    }

    if let Activity::Scanning { seen } = state.index.activity {
        // Named, not bare. A rebuild the user can attribute is one they can
        // live with; an unexplained one appearing mid-search is what got
        // reported as "random indexing reloads".
        let why = match busy_scan_reason(state) {
            Some(reason) => format!(" ({})", reason.label()),
            None => String::new(),
        };
        return StatusLine {
            text: format!(
                "Building the file list{}{why} - {} files so far",
                busy_where(state),
                humanize::count(seen)
            ),
            tone: Tone::Busy,
        };
    }
    if state.index.activity == Activity::Queued {
        // Its own line rather than silence. A share waiting behind two walks
        // for three minutes while reporting nothing is exactly the
        // unexplained wait the other states here exist to replace.
        return StatusLine {
            text: format!(
                "{} waiting for a turn to be read...",
                shares_phrase(state.index.busy)
            ),
            tone: Tone::Busy,
        };
    }
    if let Activity::Walking {
        dirs,
        queued,
        files,
    } = state.index.activity
    {
        // Folders, not only files. A climbing file count says it is moving; a
        // folder count with a queue beside it also says roughly how much is
        // left, which over a walk lasting minutes is the difference between
        // progress and an unexplained wait.
        return StatusLine {
            text: format!(
                "Reading folders{} - {} read, {} to go, {} files",
                busy_where(state),
                humanize::count(dirs),
                humanize::count(queued),
                humanize::count(files)
            ),
            tone: Tone::Busy,
        };
    }
    if state.index.activity == Activity::LoadingDisk {
        return StatusLine {
            text: "Loading the saved file list...".into(),
            tone: Tone::Busy,
        };
    }
    if state.index.activity == Activity::Persisting {
        // Busy, and previously unlabelled: it drives the animation tick, so
        // the screen spun with nothing on it to explain why.
        return StatusLine {
            text: "Saving the file list...".into(),
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
            text: "The list changed - check the highlighted file before opening it".into(),
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
    let phase = match &state.phase {
        QueryPhase::Idle | QueryPhase::Local => None,
        QueryPhase::TooShort { need } => Some(StatusLine {
            text: format!("Keep typing - a code needs at least {need} characters"),
            tone: Tone::Normal,
        }),
        QueryPhase::NoShares => Some(StatusLine {
            text: "No shares are set up - run files --check-config".into(),
            tone: Tone::Warn,
        }),
        QueryPhase::LocalPending => Some(StatusLine {
            text: "Searching...".into(),
            tone: Tone::Busy,
        }),
        QueryPhase::Verifying { .. } => Some(StatusLine {
            text: "Checking the drive...".into(),
            tone: Tone::Busy,
        }),
        // Short, and without the match count or the round-trip time it used to
        // carry: the count is in the reserved slot to the left of this line,
        // and how many milliseconds the server took is a fact about the server.
        // What is worth a word is that somebody else has now confirmed what is
        // on screen.
        QueryPhase::Verified { by_stamp, .. } => Some(StatusLine {
            text: if *by_stamp {
                "Up to date"
            } else {
                "Checked just now"
            }
            .into(),
            tone: Tone::Good,
        }),
        QueryPhase::VerifyFailed { detail } => Some(StatusLine {
            text: format!("Showing the saved list - {detail}"),
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

/// Which share is busy, when saying so helps.
///
/// Named when it is the only one, so the ordinary case reads "walking jobs..."
/// rather than "walking 1 share...". Counted when several are, because three
/// sets of folder counters do not fit on one line and averaging them would be
/// fiction.
fn busy_where(state: &AppState) -> String {
    // Silent with one share configured: "building index custompro..." tells
    // somebody with a single share nothing they did not already know.
    if state.index.configured <= 1 {
        return String::new();
    }
    match state.index.busy_only {
        Some(id) => format!(" {}", state.settings.routes.label(id)),
        None if state.index.busy > 1 => format!(" {} shares", state.index.busy),
        None => String::new(),
    }
}

fn shares_phrase(n: usize) -> String {
    if n == 1 {
        "1 share".to_string()
    } else {
        format!("{n} shares")
    }
}

/// Why the share that is currently scanning started.
fn busy_scan_reason(state: &AppState) -> Option<crate::index::schedule::ScanReason> {
    state
        .index
        .busy_only
        .and_then(|id| state.status_of(id))
        .and_then(|s| s.last_scan_reason)
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
    let status = &state.index;
    if status.origin.is_none() {
        return Some("no file list yet".into());
    }

    // Named, because a degraded share is one somebody has to go and look at.
    // A name only when there are several: "jobs: no live updates" is useful
    // and "custompro: no live updates" when custompro is the only share is a
    // word nobody needed.
    let named = |id| {
        if status.configured > 1 {
            format!("{}: ", state.settings.routes.label(id))
        } else {
            String::new()
        }
    };

    if let Some((id, reason)) = status.degraded() {
        return Some(format!("{}{}", named(id), reason.label()));
    }

    // Which share to refresh, and why - not merely that something is stale.
    // Somebody told only that "an index is out of date" refreshes everything,
    // and everything is what a few hundred people must not all read at once.
    if state.settings.stale_notices
        && let Some((id, reason)) = status.stale_at(wall)
    {
        return Some(format!("{}{} · F5", named(id), reason.label()));
    }

    None
}

/// The two facts the footer reserves room for, so neither can be truncated.
///
/// Everything else on that line is prose, and prose is what the hint chips eat
/// into: it is laid out in whatever they left and ellipsised to fit. These two
/// are not prose. They are values somebody is *looking* for, and a value that
/// might not be there is not worth putting on screen.
///
/// `shown` is how many rows the renderer actually drew. That is the only
/// honest source for it: the state holds up to [`crate::config::MAX_RESULTS`]
/// and the panel has room for eight, so nothing in `view` can work it out.
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
/// Where in the result list the rows on screen are.
///
/// Empty when there is nothing to count, so the footer says nothing rather than
/// a zero, and when it all fits, because "1-4 of 4" is four words for a fact
/// the eye already has.
///
/// A *range*, not a count, because the list scrolls: "8 of 300" answers how
/// many were left out but not which eight, and somebody holding Down through
/// three hundred drawings wants to know how far they have got. This is the
/// terminal build's `showing 9-16 of 300` restored.
pub fn visible_range(state: &AppState, window: Range<usize>) -> String {
    let found = state.matched as usize;
    if found == 0 || state.hits.is_empty() || window.is_empty() {
        return String::new();
    }
    if window.len() >= found {
        return humanize::count(found);
    }
    format!(
        "{}-{} of {}",
        window.start + 1,
        window.end,
        humanize::count(found)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
                query: s.input.text().to_string(),
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

        let line = render(&s, now, EPOCH);
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

        let line = render(&s, now, EPOCH + Duration::from_secs(180)).text;
        for gone in ["Ready", "1,284,551", "updated", "3m", "match"] {
            assert!(!line.contains(gone), "{gone:?} is still there: {line:?}");
        }
    }

    #[test]
    fn a_short_query_says_how_many_characters_are_needed() {
        let now = Instant::now();
        let mut s = state_at(now);
        s.update(
            AppEvent::Key(KeyEvent::new(Key::Char('a'), Mods::NONE)),
            now,
        );
        assert_eq!(
            render(&s, now, EPOCH).text,
            "Keep typing - a code needs at least 3 characters"
        );
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
        let line = render(&s, now, EPOCH);
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
        let line = render(&s, now, EPOCH);
        assert_eq!(line.text, "No shares are set up - run files --check-config");
        assert_eq!(line.tone, Tone::Warn);
    }

    #[test]
    fn the_range_follows_the_window_down_the_list() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_code(&mut s, now);
        let many: Vec<_> = (0..300).map(|i| hit(&format!("a{i}.pdf"))).collect();
        give_results(&mut s, now, many, 300, 9_000);

        assert_eq!(
            visible_range(&s, s.visible_rows()),
            format!("1-{} of 300", crate::config::VISIBLE_ROWS)
        );

        // To the foot of the list, the way an arrow key does it.
        for _ in 0..299 {
            s.update(AppEvent::Key(KeyEvent::new(Key::Down, Mods::NONE)), now);
        }
        assert_eq!(
            visible_range(&s, s.visible_rows()),
            format!("{}-300 of 300", 300 - crate::config::VISIBLE_ROWS + 1)
        );
    }

    #[test]
    fn a_capped_result_list_says_which_of_them_is_on_screen() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_code(&mut s, now);
        let many: Vec<_> = (0..40).map(|i| hit(&format!("a{i}.pdf"))).collect();
        give_results(&mut s, now, many, 4321, 9_000);

        assert_eq!(
            visible_range(&s, s.visible_rows()),
            format!("1-{} of 4,321", crate::config::VISIBLE_ROWS)
        );
    }

    /// It all fits, so there is no range worth stating - only the total.
    #[test]
    fn a_result_list_that_fits_is_reported_as_a_count() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a"), hit("b")], 2, 9_000);

        assert_eq!(visible_range(&s, s.visible_rows()), "2");
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

        assert_eq!(render(&s, now, EPOCH).text, "");
        assert_eq!(visible_range(&s, s.visible_rows()), "");
    }

    /// The condition the previous implementation rendered as `0 / 0 files`.
    #[test]
    fn an_unreachable_drive_says_so_and_when_it_will_retry() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, |st| {
            st.health = Health::Unreachable {
                err: EnumError::Transient(53),
                since: now,
                attempt: 2,
                next_retry_at: now + Duration::from_secs(42),
            };
        });

        let line = render(&s, now, EPOCH);
        assert_eq!(line.tone, Tone::Bad);
        assert!(line.text.contains("unreachable"), "{}", line.text);
        assert!(line.text.contains("os error 53"), "{}", line.text);
        assert!(line.text.contains("retrying in"), "{}", line.text);
        assert!(
            line.text.contains("F5"),
            "the user needs a way out: {}",
            line.text
        );
    }

    #[test]
    fn a_running_scan_reports_progress() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, |st| {
            st.activity = Activity::Scanning { seen: 812_000 }
        });
        let line = render(&s, now, EPOCH);
        assert!(line.text.contains("Building the file list"));
        assert!(line.text.contains("812,000"));
        assert_eq!(line.tone, Tone::Busy);
    }

    /// The reported bug was not that the index rebuilds - it has to - but that
    /// a rebuild appeared mid-search with no explanation.
    #[test]
    fn a_running_scan_says_why_it_is_happening() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, |st| {
            st.activity = Activity::Scanning { seen: 1000 };
            st.last_scan_reason = Some(ScanReason::StampMoved);
        });
        let line = render(&s, now, EPOCH);
        assert!(
            line.text
                .contains("Building the file list (directory changed)"),
            "{}",
            line.text
        );
    }

    #[test]
    fn a_scan_with_no_recorded_reason_still_reads_cleanly() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, |st| {
            st.activity = Activity::Scanning { seen: 1000 };
            st.last_scan_reason = None;
        });
        let line = render(&s, now, EPOCH);
        assert!(
            line.text.contains("Building the file list"),
            "{}",
            line.text
        );
        assert!(
            !line.text.contains("()"),
            "no empty parentheses: {}",
            line.text
        );
    }

    /// `Persisting` counts as busy, so it drives the 100ms animation tick.
    /// Without a branch here the screen span with nothing to explain it.
    #[test]
    fn saving_the_index_is_visible() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, |st| st.activity = Activity::Persisting);
        assert_eq!(render(&s, now, EPOCH).text, "Saving the file list...");
    }

    #[test]
    fn a_freshly_built_index_does_not_mention_the_check() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(10, Duration::ZERO));
        assert!(
            !render(&s, now, EPOCH).text.contains("checked"),
            "the quiet case stays short"
        );
    }

    #[test]
    fn loading_the_disk_cache_is_visible() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, |st| st.activity = Activity::LoadingDisk);
        assert_eq!(
            render(&s, now, EPOCH).text,
            "Loading the saved file list..."
        );
    }

    #[test]
    fn verification_in_flight_says_so_and_keeps_the_counts_beside_it() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(100, Duration::ZERO));
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 100);
        s.update(AppEvent::Tick, now + crate::config::VERIFY_DEBOUNCE);

        let line = render(&s, now + crate::config::VERIFY_DEBOUNCE, EPOCH);
        assert!(line.text.contains("Checking the drive"));
        assert_eq!(line.tone, Tone::Busy);
        // The count is beside the line, not in it, so a round trip in flight
        // does not cost the number somebody is reading.
        assert_eq!(visible_range(&s, s.visible_rows()), "1");
    }

    #[test]
    fn a_stamp_proven_result_is_labelled_up_to_date() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(100, Duration::ZERO));
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 100);
        s.update(
            AppEvent::Verify(crate::app::event::VerifyMsg {
                epoch: s.query_epoch(),
                query: s.input.text().to_string(),
                elapsed: Duration::from_millis(2),
                outcome: crate::search::verify::VerifyOutcome::IndexAuthoritative { stamp: None },
            }),
            now,
        );

        let line = render(&s, now, EPOCH);
        assert!(line.text.starts_with("Up to date"), "{}", line.text);
        assert_eq!(line.tone, Tone::Good);
    }

    #[test]
    fn a_server_verified_result_is_labelled_verified() {
        let now = Instant::now();
        let mut s = state_at(now);
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 100);
        s.update(
            AppEvent::Verify(crate::app::event::VerifyMsg {
                epoch: s.query_epoch(),
                query: s.input.text().to_string(),
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
        // Short, and about the *check*: the count is in the reserved slot and
        // how many milliseconds the server took is a fact about the server.
        let line = render(&s, now, EPOCH);
        assert_eq!(line.text, "Checked just now");
        assert_eq!(line.tone, Tone::Good);
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

        let line = render(&s, now, EPOCH);
        assert!(
            line.text.contains("server filter disabled"),
            "{}",
            line.text
        );
        assert_eq!(line.tone, Tone::Warn);
        // And the results are still reachable beside it: a warning about the
        // index must not cost the count of what it found.
        assert_eq!(visible_range(&s, s.visible_rows()), "1");
    }

    #[test]
    fn a_missing_index_is_described_rather_than_faked() {
        let now = Instant::now();
        let s = state_at(now);
        assert!(render(&s, now, EPOCH).text.contains("no file list yet"));
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
        // Mapping 1 is the tree in the shipped table.
        publish(
            &mut s,
            MappingId(1),
            now,
            unreachable(EnumError::PathNotFound(3)),
        );

        let line = render(&s, now, EPOCH);
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
        let mut s = state_at(now);
        publish(
            &mut s,
            MappingId(0),
            now,
            unreachable(EnumError::PathNotFound(3)),
        );

        let line = render(&s, now, EPOCH);
        assert!(
            !line.text.starts_with(' '),
            "the reported 'random os error 03': {}",
            line.text
        );
        assert!(line.text.contains("os error 3"), "{}", line.text);
    }

    /// `humanize::elapsed(ZERO)` is "0us", which reads as a stopwatch rather
    /// than as a retry that is due.
    #[test]
    fn a_due_retry_says_so_rather_than_counting_down_to_zero() {
        let now = Instant::now();
        let mut s = state_at(now);
        publish(&mut s, MappingId(0), now, |st| {
            st.health = Health::Unreachable {
                err: EnumError::Transient(53),
                since: now,
                attempt: 1,
                next_retry_at: now,
            };
        });

        let line = render(&s, now, EPOCH);
        assert!(line.text.contains("retrying now"), "{}", line.text);
        assert!(!line.text.contains("0us"), "{}", line.text);
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

        let line = render(&s, now, EPOCH);
        assert!(line.text.contains("jobs: no live updates"), "{}", line.text);
    }

    #[test]
    fn a_share_queued_behind_a_walk_says_so() {
        let now = Instant::now();
        let mut s = state_with_two_shares(now);
        publish(&mut s, MappingId(1), now, |st| {
            st.activity = Activity::Queued;
        });
        let line = render(&s, now, EPOCH);
        assert!(
            line.text.contains("waiting for a turn to be read"),
            "a queued share must not look idle: {}",
            line.text
        );
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

        let line = render(&s, now, EPOCH);
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
        let line = render(&s, now, wall);
        assert!(line.text.contains("changes were missed"), "{}", line.text);
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

        let fresh = render(&s, now, EPOCH + Duration::from_secs(60));
        assert!(!fresh.text.contains("not refreshed"), "{}", fresh.text);

        let wall = EPOCH + crate::config::MAX_INDEX_AGE + Duration::from_secs(60);
        let old = render(&s, now, wall);
        assert!(old.text.contains("not refreshed recently"), "{}", old.text);
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
        let line = render(&s, now, wall);
        assert_eq!(
            line.text, "",
            "silent mode must not nag, about events or about age"
        );
        // The ages are still one keypress away, which is the whole bargain:
        // not shown, not hidden. See `view::shares`, which F5 opens.
        assert!(s.index.age(wall).is_some());
    }
}
