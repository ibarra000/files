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

use std::time::{Duration, Instant, SystemTime};

use crate::app::state::{AppState, QueryPhase, Severity};
use crate::config::{MIN_QUERY_LEN, ViewerKind};
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

    match &state.phase {
        QueryPhase::Idle => StatusLine {
            text: format!("Ready · {}", index_summary(state, wall)),
            tone: Tone::Normal,
        },
        QueryPhase::TooShort { need } => StatusLine {
            text: format!("Keep typing - a code needs at least {need} characters"),
            tone: Tone::Normal,
        },
        QueryPhase::NoShares => StatusLine {
            text: "No shares are set up - run files --check-config".into(),
            tone: Tone::Warn,
        },
        QueryPhase::LocalPending => StatusLine {
            text: "Searching...".into(),
            tone: Tone::Busy,
        },
        QueryPhase::Local => StatusLine {
            text: format!(
                "{} · {}",
                index_summary(state, wall),
                matches_summary(state)
            ),
            tone: base_tone(state),
        },
        QueryPhase::Verifying { .. } => StatusLine {
            text: format!(
                "Checking the drive - {} · {}",
                index_summary(state, wall),
                matches_summary(state)
            ),
            tone: Tone::Busy,
        },
        QueryPhase::Verified { took, by_stamp } => StatusLine {
            text: if *by_stamp {
                // The directory has not changed, so the local index is
                // provably current - no query was needed at all.
                format!(
                    "Up to date · {} · checked in {}",
                    matches_summary(state),
                    humanize::elapsed(*took)
                )
            } else {
                format!(
                    "Checked just now · {} · {}",
                    matches_summary(state),
                    humanize::elapsed(*took)
                )
            },
            tone: Tone::Good,
        },
        QueryPhase::VerifyFailed { detail } => StatusLine {
            text: format!(
                "Showing the saved list - {detail} · {}",
                matches_summary(state)
            ),
            tone: Tone::Warn,
        },
    }
}

fn base_tone(state: &AppState) -> Tone {
    match state.index.degraded() {
        Some(_) => Tone::Warn,
        None => Tone::Normal,
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

/// Describes the index the results came from, including its age.
fn index_summary(state: &AppState, wall: SystemTime) -> String {
    let status = &state.index;
    let Some(_origin) = status.origin else {
        return "no file list yet".into();
    };
    // The *oldest* share's age, not the newest. Taking the newest would let
    // one freshly rebuilt share vouch for nine stale ones, and the age is the
    // number somebody checks before trusting a result.
    let age = status
        .age(wall)
        .map(humanize::age)
        .unwrap_or_else(|| "unknown".into());
    // One corpus, one number: somebody searching four shares is not asked to
    // add up four counts. The label names the shape only when there is one
    // share, because "files · updated" describing ten of them would be a claim
    // about all of them that no single origin supports.
    let mut s = if status.configured > 1 {
        format!(
            "{} files across {} shares · updated {age}",
            humanize::count(status.entries as usize),
            status.configured
        )
    } else {
        format!(
            "{} files · updated {age}",
            humanize::count(status.entries as usize)
        )
    };
    if status.truncated {
        s.push_str(" (partial)");
    }
    // A total that is quietly short reads as a complete answer, which is the
    // same silent-omission failure the walked index exists to prevent.
    if status.ready < status.configured {
        s.push_str(&format!(
            " · {} of {} indexed",
            status.ready, status.configured
        ));
    }
    // How long ago the listing was last *proven* current, which is a
    // different and usually much smaller number than its age. Shown only when
    // it differs, so the quiet case stays short.
    if let (Some(age), Some(confirmed)) = (status.age(wall), status.confirmed_age(wall))
        && confirmed + Duration::from_secs(30) < age
    {
        s.push_str(" · checked ");
        s.push_str(&humanize::age(confirmed));
    }
    // Named, because a degraded share is one somebody has to go and look at.
    if let Some((id, reason)) = status.degraded() {
        s.push_str(" · ");
        if status.configured > 1 {
            s.push_str(state.settings.routes.label(id));
            s.push_str(": ");
        }
        s.push_str(reason.label());
    }
    // Which share to refresh, and why - not merely that something is stale.
    // Somebody told only that "an index is out of date" refreshes everything,
    // and everything is what a few hundred people must not all read at once.
    if state.settings.stale_notices
        && let Some((id, reason)) = status.stale_at(wall)
    {
        s.push_str(" · ");
        if status.configured > 1 {
            s.push_str(state.settings.routes.label(id));
            s.push_str(": ");
        }
        s.push_str(reason.label());
        s.push_str(" · F5");
    }
    s
}

fn matches_summary(state: &AppState) -> String {
    if state.matched == 0 {
        return "no matches".into();
    }
    // How many were *found*. How many are on screen is a different number and
    // is said in a different place - see [`result_count`] - because this one
    // lives in the prose that the hint chips truncate, and a count that can be
    // ellipsised away is not a count anybody can rely on.
    //
    // It used to read "4,321 matches (showing 15)", where fifteen was the
    // length of `hits` rather than the eight rows the panel has room for. The
    // parenthesis was a claim about the screen made by something that cannot
    // see it.
    format!(
        "{} match{}",
        humanize::count(state.matched as usize),
        if state.matched == 1 { "" } else { "es" }
    )
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
pub fn footer_facts(state: &AppState, shown: usize) -> String {
    let viewer = match state.viewer {
        ViewerKind::Pdf => "F2: pdf",
        ViewerKind::Avwin => "F2: avwin",
    };
    match result_count(state, shown) {
        Some(count) => format!("{count}  ·  {viewer}"),
        None => viewer.to_string(),
    }
}

/// How many results were found, and how many of them fit.
///
/// `None` when there is nothing to count, so the footer says nothing rather
/// than a zero.
fn result_count(state: &AppState, shown: usize) -> Option<String> {
    let found = state.matched as usize;
    if found == 0 || state.hits.is_empty() {
        return None;
    }
    if shown < found {
        Some(format!("{shown} of {}", humanize::count(found)))
    } else {
        Some(humanize::count(found))
    }
}

/// Describes the minimum query length, for the empty state.
pub fn min_query_hint() -> String {
    format!("type at least {MIN_QUERY_LEN} characters")
}

/// Age formatting shared with the results pane.
pub fn age_of(state: &AppState, wall: SystemTime) -> Option<Duration> {
    state.index.age(wall)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn an_idle_app_invites_input_and_describes_the_index() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(1_284_551, Duration::ZERO));

        let line = render(&s, now, EPOCH);
        assert!(line.text.starts_with("Ready"));
        assert!(line.text.contains("1,284,551 files"));
        assert_eq!(line.tone, Tone::Normal);
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
    fn results_report_the_index_age_and_the_match_count() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(1_284_551, Duration::ZERO));
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 1_284_551);

        let line = render(&s, now, EPOCH + Duration::from_secs(180));
        assert!(line.text.contains("files · updated"));
        assert!(
            line.text.contains("3m"),
            "the age must be visible: {}",
            line.text
        );
        assert!(line.text.contains("1 match"));
    }

    #[test]
    fn a_capped_result_list_says_how_many_are_hidden() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a"), hit("b")], 4321, 9_000);

        let line = render(&s, now, EPOCH);
        assert!(line.text.contains("4,321 matches"), "{}", line.text);
    }

    #[test]
    fn no_matches_is_stated_plainly() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_code(&mut s, now);
        give_results(&mut s, now, vec![], 0, 9_000);
        assert!(render(&s, now, EPOCH).text.contains("no matches"));
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

    /// The index age is the first thing a user checks before trusting a
    /// result. It must describe the data, not the last time we asked about it.
    #[test]
    fn a_confirmed_index_reports_the_data_age_and_the_check_separately() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, |st| {
            st.origin = Some(Origin::Network);
            st.entries = 10;
            st.built_at = Some(EPOCH);
            st.confirmed_at = Some(EPOCH + Duration::from_secs(3600));
        });
        let line = render(&s, now, EPOCH + Duration::from_secs(3660));
        assert!(
            line.text.contains("1h"),
            "the data really is an hour old: {}",
            line.text
        );
        assert!(
            line.text.contains("checked"),
            "and it was proven current a minute ago: {}",
            line.text
        );
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
    fn verification_in_flight_shows_a_spinner_and_keeps_the_counts() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(100, Duration::ZERO));
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 100);
        s.update(AppEvent::Tick, now + crate::config::VERIFY_DEBOUNCE);

        let line = render(&s, now + crate::config::VERIFY_DEBOUNCE, EPOCH);
        assert!(line.text.contains("Checking the drive"));
        assert!(line.text.contains("1 match"));
        assert_eq!(line.tone, Tone::Busy);
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
        let line = render(&s, now, EPOCH);
        assert!(
            line.text.starts_with("Checked just now · 1 match · 42ms"),
            "{}",
            line.text
        );
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
        assert!(line.text.contains("1 match"));
        assert_eq!(line.tone, Tone::Warn);
    }

    #[test]
    fn a_truncated_index_says_so() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, |st| {
            st.origin = Some(Origin::DiskCache);
            st.entries = 1000;
            st.built_at = Some(EPOCH);
            st.truncated = true;
        });
        assert!(render(&s, now, EPOCH).text.contains("(partial)"));
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
    fn the_summary_sums_every_share_and_ages_from_the_oldest() {
        let now = Instant::now();
        let mut s = state_with_two_shares(now);
        publish(&mut s, MappingId(0), now, |st| {
            st.origin = Some(Origin::Network);
            st.entries = 1_000_000;
            st.built_at = Some(EPOCH);
        });
        publish(&mut s, MappingId(1), now, |st| {
            st.origin = Some(Origin::Network);
            st.entries = 284_551;
            st.built_at = Some(EPOCH + Duration::from_secs(3600));
        });
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 1_284_551);

        let line = render(&s, now, EPOCH + Duration::from_secs(3660));
        assert!(line.text.contains("2 shares"), "{}", line.text);
        assert!(line.text.contains("1,284,551 files"), "{}", line.text);
        assert!(
            line.text.contains("1h"),
            "the oldest share sets the age: {}",
            line.text
        );
    }

    /// A total that is quietly short reads as a complete answer.
    #[test]
    fn a_short_index_says_how_many_shares_have_answered() {
        let now = Instant::now();
        let mut s = state_with_two_shares(now);
        publish(&mut s, MappingId(0), now, |st| {
            st.origin = Some(Origin::Network);
            st.entries = 10;
            st.built_at = Some(EPOCH);
        });
        type_code(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 10);

        let line = render(&s, now, EPOCH);
        assert!(line.text.contains("1 of 2 indexed"), "{}", line.text);
    }

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
    fn stale_notices_can_be_turned_off_without_hiding_the_age() {
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
        assert!(
            !line.text.contains("changes were missed"),
            "silent mode must not nag: {}",
            line.text
        );
        assert!(
            !line.text.contains("not refreshed"),
            "nor about age: {}",
            line.text
        );
        assert!(
            line.text.contains("updated"),
            "but the age itself is still there: {}",
            line.text
        );
    }
}
