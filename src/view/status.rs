//! The status line, rendered as a pure function of state and time.
//!
//! It is the only place the user learns that an index is stale, a drive is
//! unreachable or server-side filtering is off. Those conditions are hard to
//! reproduce on a development machine, so the exact text is pinned by tests.

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

/// Builds the status line text and tone.
///
/// The leading tone glyph or spinner is added by
/// [`crate::gui::panel::footer`]; adding it here too would draw it twice.
pub fn render(state: &AppState, wall: SystemTime) -> StatusLine {
    // A toast outranks everything: it answers a recent key press and expires
    // after `TOAST_LIFETIME`. It is drawn here because this band has a fixed
    // height, so toasts never resize the window.
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

    // An unreachable share explains every other oddity, so it comes next.
    // The failing share is named. No retry countdown is shown, so the panel
    // does not need a repaint every second while a drive is down.
    if let Some((id, Health::Unreachable { err, .. })) = state.index.unreachable() {
        return StatusLine {
            text: format!("{} \u{b7} F5", state.describe_drive_error(id, *err)),
            tone: Tone::Bad,
        };
    }

    // A missing viewer explains why `Enter` would appear to do nothing.
    // `F2` (switch viewer) is the fix.
    if state.viewer_missing(state.viewer)
        && let Some(app) = state.viewer.app(&state.settings.viewers)
    {
        // The program name keeps its configured spelling.
        return StatusLine {
            text: format!(
                "Cannot open \u{b7} {} is not on PATH \u{b7} F2 switches viewer",
                app.program
            ),
            tone: Tone::Warn,
        };
    }

    // One line for every kind of index work. Counts and progress figures are
    // on the Diagnostics page; what matters here is that the list is
    // incomplete, so a missing file may not really be missing.
    if state.index.activity != Activity::Idle {
        return StatusLine {
            text: "Indexing\u{2026}".into(),
            tone: Tone::Busy,
        };
    }

    // The selected file left the list and the cursor moved, so the next
    // `Enter` would open a different file.
    if state.selection_lost {
        return StatusLine {
            text: "File list changed \u{b7} check the selection before opening".into(),
            tone: Tone::Warn,
        };
    }

    // A partial live answer outranks the query phase, which can only say the
    // indexes are current. A live search in flight is shown only by the
    // spinner beside the count; it usually finishes in under a second.
    if let Some(live) = &state.live {
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
            text: "No drives set up \u{b7} Ctrl+, opens Settings".into(),
            tone: Tone::Warn,
        }),
        // Silent: these finish too quickly to read, and success is already
        // shown by an empty line.
        QueryPhase::LocalPending | QueryPhase::Verifying { .. } | QueryPhase::Verified { .. } => {
            None
        }
        QueryPhase::VerifyFailed { detail } => Some(StatusLine {
            text: format!("Showing cached results \u{b7} {detail}"),
            tone: Tone::Warn,
        }),
    };
    if let Some(line) = phase {
        return line;
    }

    // Otherwise, only an index problem is worth saying; a healthy index
    // leaves the line empty.
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

/// Returns a warning about the index, or `None` when it is healthy.
///
/// Index ages are shown in the `F5` drive list ([`crate::view::shares`]),
/// not here.
fn index_warning(state: &AppState, wall: SystemTime) -> Option<String> {
    // The panel uses the same predicate to reserve room for this line, so
    // the two must agree.
    if !state.has_standing_notice(wall) {
        return None;
    }

    let status = &state.index;
    if status.origin.is_none() {
        return Some("No file list yet".into());
    }

    // The share is named only when several are configured. The configured
    // name keeps its spelling; with no name, `view::sentence` capitalises the
    // fragment instead.
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

    // Name the share and the reason, so users refresh one share rather than
    // all of them.
    if state.settings.stale_notices
        && let Some((id, reason)) = status.stale_at(wall)
    {
        return Some(format!("{} \u{b7} F5", lead(id, reason.label())));
    }

    None
}

/// Returns the match count with its noun ("300 items"), reserved its own
/// space in the footer so it is never truncated.
///
/// Empty when there is nothing to count. This is the number the index found,
/// which may exceed the hits returned.
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

    /// A state with one share. Several-share tests are at the end.
    fn state_at(now: Instant) -> AppState {
        AppState::new(
            Settings::for_mapping(
                "shared",
                crate::config::FLAT_FIXTURE_PATH,
                MappingKind::Flat,
            ),
            now,
        )
    }

    /// The same in developer mode, which adds the drive path and OS error
    /// code. The non-developer wording is covered by
    /// `a_drive_failure_is_a_sentence_until_somebody_asks` in
    /// `tests/state_machine.rs`.
    fn dev_state_at(now: Instant) -> AppState {
        let mut s = state_at(now);
        s.settings.dev_mode = true;
        s
    }

    /// A state with two shares.
    fn state_with_two_shares(now: Instant) -> AppState {
        AppState::new(Settings::fixture(), now)
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

    fn type_search(s: &mut AppState, now: Instant) {
        for c in "AB-C-0704".chars() {
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

    #[test]
    fn the_file_count_and_the_index_age_are_not_on_this_line() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(1_284_551, Duration::ZERO));
        type_search(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 1_284_551);

        let line = render(&s, EPOCH + Duration::from_secs(180)).text;
        for gone in ["Ready", "1,284,551", "updated", "3m", "match"] {
            assert!(!line.contains(gone), "{gone:?} is still there: {line:?}");
        }
    }

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
    fn an_unusual_query_is_searched_for_rather_than_refused() {
        let now = Instant::now();
        let mut s = state_at(now);
        for c in "!!!".chars() {
            s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)), now);
        }
        let line = render(&s, EPOCH);
        assert_ne!(line.text, "not a recognised search");
        assert_eq!(s.phase, QueryPhase::LocalPending);
    }

    #[test]
    fn a_configuration_with_no_enabled_share_says_so() {
        let now = Instant::now();
        // The shipped template enables no drives, so this is the first-run state.
        let routes = crate::paths::Routes::new(Vec::new(), crate::paths::ConfigSource::BuiltIn);
        let settings = Settings::with_routes(std::sync::Arc::new(routes), |s| s);

        let mut s = AppState::new(settings, now);
        for c in "AB-C-0704".chars() {
            s.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)), now);
        }
        let line = render(&s, EPOCH);
        assert_eq!(line.text, "No drives set up \u{b7} Ctrl+, opens Settings");
        assert_eq!(line.tone, Tone::Warn);
    }

    #[test]
    fn the_count_does_not_move_as_the_list_is_walked() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_search(&mut s, now);
        let many: Vec<_> = (0..300).map(|i| hit(&format!("a{i}.pdf"))).collect();
        give_results(&mut s, now, many, 300, 9_000);

        assert_eq!(found(&s), "300 items");

        // Walk to the foot of the list; the count must not change.
        for _ in 0..299 {
            s.update(AppEvent::Key(KeyEvent::new(Key::Down, Mods::NONE)), now);
        }
        assert_eq!(found(&s), "300 items");
    }

    /// The count is what the index found, not the hits returned, which are
    /// capped at [`crate::config::MAX_RESULTS`].
    #[test]
    fn a_capped_result_list_says_how_many_it_really_found() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_search(&mut s, now);
        let many: Vec<_> = (0..40).map(|i| hit(&format!("a{i}.pdf"))).collect();
        give_results(&mut s, now, many, 4321, 9_000);

        assert_eq!(found(&s), "4,321 items");
    }

    #[test]
    fn a_short_result_list_is_reported_as_a_count() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_search(&mut s, now);
        give_results(&mut s, now, vec![hit("a"), hit("b")], 2, 9_000);

        assert_eq!(found(&s), "2 items");
    }

    #[test]
    /// No matches is reported by [`crate::view::empty`], not as a zero count.
    fn no_matches_is_left_to_the_body_to_explain() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(9_000, Duration::ZERO));
        type_search(&mut s, now);
        give_results(&mut s, now, vec![], 0, 9_000);

        assert_eq!(render(&s, EPOCH).text, "");
        assert_eq!(found(&s), "");
    }

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
        // No retry countdown, so the panel need not repaint every second.
        assert!(!line.text.contains("retrying"), "{}", line.text);
    }

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

    #[test]
    fn a_freshly_built_index_says_nothing_at_all() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(10, Duration::ZERO));
        let line = render(&s, EPOCH);
        assert_eq!(line.text, "", "the quiet case is quiet");
    }

    #[test]
    fn a_search_in_flight_and_a_search_confirmed_both_say_nothing() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, healthy_status(100, Duration::ZERO));
        type_search(&mut s, now);
        assert_eq!(render(&s, EPOCH).text, "", "a search was announced");

        give_results(&mut s, now, vec![hit("a.pdf")], 1, 100);
        s.update(AppEvent::Tick, now + crate::config::VERIFY_DEBOUNCE);
        assert_eq!(render(&s, EPOCH).text, "", "a verification was announced");
        // The count is separate from the line, so it survives a verification.
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
        type_search(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 10);

        let line = render(&s, EPOCH);
        assert!(
            line.text.contains("Server filter disabled"),
            "{}",
            line.text
        );
        assert_eq!(line.tone, Tone::Warn);
        // The warning must not hide the count.
        assert_eq!(found(&s), "1 item");
    }

    #[test]
    fn a_missing_index_is_described_rather_than_faked() {
        let now = Instant::now();
        let s = state_at(now);
        assert!(render(&s, EPOCH).text.contains("No file list yet"));
    }
    // --- naming the share that actually failed --------------------------

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
            !line.text.contains(r"V:\shared"),
            "naming the wrong share is the bug: {}",
            line.text
        );
        assert!(line.text.contains("os error 3"), "{}", line.text);
    }

    /// An error message must always name its target.
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
        type_search(&mut s, now);
        give_results(&mut s, now, vec![hit("a.pdf")], 1, 20);

        let line = render(&s, EPOCH);
        assert!(
            line.text.contains("projects: no live updates"),
            "{}",
            line.text
        );
    }

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

    fn ready(entries: u32) -> impl FnOnce(&mut IndexStatus) {
        move |st: &mut IndexStatus| {
            st.origin = Some(Origin::Network);
            st.entries = entries;
            st.built_at = Some(EPOCH);
        }
    }

    fn typed_with_results(s: &mut AppState, now: Instant, total: u32) {
        type_search(s, now);
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
        assert!(line.text.contains("projects"), "{}", line.text);
        assert!(line.text.contains("changes were missed"), "{}", line.text);
        assert!(line.text.contains("F5"), "{}", line.text);
    }

    /// "Changes were missed" outranks age, since it means the list is
    /// incomplete.
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

    /// Age is judged at render time, since an on-demand share's actor may not
    /// wake for hours to set a flag.
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

    #[test]
    fn stale_notices_can_be_turned_off() {
        let now = Instant::now();
        let quiet = Settings {
            stale_notices: false,
            ..Settings::for_mapping(
                "shared",
                crate::config::FLAT_FIXTURE_PATH,
                MappingKind::Flat,
            )
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
        // Ages remain available in the `F5` drive list (`view::shares`).
        assert!(s.index.age(wall).is_some());
    }

    // --- the house style --------------------------------------------------

    /// Returns every line `render` can produce, for the house-style checks.
    ///
    /// A new branch in `render` must be added here or it goes unchecked.
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
        no_viewer.settings.viewers = std::sync::Arc::new(crate::config::Viewers::new(vec![
            crate::config::ViewerApp::new("sketch", "sketch.exe", &["tif"]),
        ]));
        no_viewer.set_viewers_missing(Box::new([true]));
        no_viewer.viewer = crate::config::ViewerKind::App(0);
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

        // Half a search typed, and a search with nowhere to look.
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
        type_search(&mut none, now);
        say(&none, EPOCH);

        // Every phase the search goes through.
        let mut searching = state_at(now);
        with_index(&mut searching, now, healthy_status(10, Duration::ZERO));
        type_search(&mut searching, now);
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

        // Recall history: opening commits the search, the resulting toast is
        // cleared so it does not outrank the recall line, then Up.
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

    /// Lines start capitalised, except those led by a configured drive name
    /// (`projects: no live updates`, `shared unreachable`), which keep the
    /// configured spelling.
    #[test]
    fn every_status_line_starts_the_way_a_line_should() {
        let configured: Vec<String> = ["shared", "projects"]
            .iter()
            .map(|n| format!("{n} "))
            .collect();

        for line in every_line() {
            if configured.iter().any(|name| line.starts_with(name)) {
                continue;
            }
            if let Some((name, rest)) = line.split_once(": ") {
                // A drive name leads only when several are configured.
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
