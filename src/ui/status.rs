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

use crate::app::state::{AppState, QueryPhase};
use crate::config::MIN_QUERY_LEN;
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
pub fn render(state: &AppState, now: Instant, wall: SystemTime) -> StatusLine {
    // A drive that cannot be reached outranks everything else: it explains
    // every other oddity on screen.
    if let Health::Unreachable {
        err, next_retry_at, ..
    } = &state.index.health
    {
        let retry = next_retry_at.saturating_duration_since(now);
        return StatusLine {
            text: format!(
                "{} · retrying in {} · F5 to retry now",
                err.describe(&state.settings.custpro_path.to_string_lossy()),
                humanize::elapsed(retry)
            ),
            tone: Tone::Bad,
        };
    }

    if let Activity::Scanning { seen } = state.index.activity {
        return StatusLine {
            text: format!(
                "{} building index... {} files so far",
                humanize::spinner(now.elapsed()),
                humanize::count(seen)
            ),
            tone: Tone::Busy,
        };
    }
    if state.index.activity == Activity::LoadingDisk {
        return StatusLine {
            text: "loading cached index...".into(),
            tone: Tone::Busy,
        };
    }

    match &state.phase {
        QueryPhase::Idle => StatusLine {
            text: format!("type a job code · {}", index_summary(state, wall)),
            tone: Tone::Normal,
        },
        QueryPhase::TooShort { need } => StatusLine {
            text: format!("type at least {need} characters"),
            tone: Tone::Normal,
        },
        QueryPhase::Unresolvable => StatusLine {
            text: "not a recognised job code".into(),
            tone: Tone::Warn,
        },
        QueryPhase::LocalPending => StatusLine {
            text: "searching...".into(),
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
        QueryPhase::Verifying { since } => StatusLine {
            text: format!(
                "{} {} · {} · checking the drive...",
                humanize::spinner(now.saturating_duration_since(*since)),
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
                    "up to date · {} · {}",
                    humanize::elapsed(*took),
                    matches_summary(state)
                )
            } else {
                format!(
                    "verified · {} · {}",
                    humanize::elapsed(*took),
                    matches_summary(state)
                )
            },
            tone: Tone::Good,
        },
        QueryPhase::VerifyFailed { detail } => StatusLine {
            text: format!(
                "{} · {} · {detail}",
                index_summary(state, wall),
                matches_summary(state)
            ),
            tone: Tone::Warn,
        },
    }
}

fn base_tone(state: &AppState) -> Tone {
    match &state.index.health {
        Health::Degraded { .. } => Tone::Warn,
        _ => Tone::Normal,
    }
}

/// Describes the index the results came from, including its age.
fn index_summary(state: &AppState, wall: SystemTime) -> String {
    let status = &state.index;
    let Some(origin) = status.origin else {
        return "no index yet".into();
    };
    let age = status
        .age(wall)
        .map(humanize::age)
        .unwrap_or_else(|| "unknown".into());
    let mut s = format!(
        "{} index · {age} · {} files",
        origin.label(),
        humanize::count(status.entries as usize)
    );
    if status.truncated {
        s.push_str(" (truncated)");
    }
    if let Health::Degraded { reason, .. } = &status.health {
        s.push_str(" · ");
        s.push_str(reason.label());
    }
    s
}

fn matches_summary(state: &AppState) -> String {
    if state.matched == 0 {
        return "no matches".into();
    }
    let shown = state.hits.len();
    if (state.matched as usize) > shown {
        format!(
            "{} matches (showing {shown})",
            humanize::count(state.matched as usize)
        )
    } else {
        format!(
            "{} match{}",
            state.matched,
            if state.matched == 1 { "" } else { "es" }
        )
    }
}

/// The key hints along the bottom.
pub fn help_line(viewer_missing: bool) -> String {
    // No "quit" hint, because there is no key that quits: the window's close
    // button ends the program, as it does for every other application.
    let mut s = String::from(
        "Enter open · Up history · Down results · Shift+arrows select · Ctrl+C copy · F5 refresh · Esc clear",
    );
    if viewer_missing {
        s.push_str("  ·  WARNING: avwin.exe not found on PATH");
    }
    s
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
    use crate::app::state::AppState;
    use crate::config::Settings;
    use crate::index::errors::EnumError;
    use crate::index::store::{DegradeReason, FlatStatus, Origin};
    use crate::search::matcher::Hit;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Arc;

    const EPOCH: SystemTime = SystemTime::UNIX_EPOCH;

    fn state_at(now: Instant) -> AppState {
        AppState::new(Settings::default(), now)
    }

    fn with_index(s: &mut AppState, now: Instant, f: impl FnOnce(&mut FlatStatus)) {
        let mut status = FlatStatus::default();
        f(&mut status);
        s.update(AppEvent::Index(IndexMsg::Status(Arc::new(status))), now);
    }

    fn healthy_status(entries: u32, age: Duration) -> impl FnOnce(&mut FlatStatus) {
        move |st: &mut FlatStatus| {
            st.origin = Some(Origin::Network);
            st.entries = entries;
            st.built_at = Some(EPOCH);
            let _ = age;
        }
    }

    fn type_code(s: &mut AppState, now: Instant) {
        for c in "11-D-0704".chars() {
            s.update(
                AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                now,
            );
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
                query: s.input.clone(),
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
        assert!(line.text.starts_with("type a job code"));
        assert!(line.text.contains("1,284,551 files"));
        assert_eq!(line.tone, Tone::Normal);
    }

    #[test]
    fn a_short_query_says_how_many_characters_are_needed() {
        let now = Instant::now();
        let mut s = state_at(now);
        s.update(
            AppEvent::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            now,
        );
        assert_eq!(render(&s, now, EPOCH).text, "type at least 3 characters");
    }

    #[test]
    fn an_unrecognised_code_is_called_out() {
        let now = Instant::now();
        let mut s = state_at(now);
        for c in "!!!".chars() {
            s.update(
                AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                now,
            );
        }
        let line = render(&s, now, EPOCH);
        assert_eq!(line.text, "not a recognised job code");
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
        assert!(line.text.contains("live index"));
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
        assert!(
            line.text.contains("4,321 matches (showing 2)"),
            "{}",
            line.text
        );
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
        assert!(line.text.contains("building index"));
        assert!(line.text.contains("812,000"));
        assert_eq!(line.tone, Tone::Busy);
    }

    #[test]
    fn loading_the_disk_cache_is_visible() {
        let now = Instant::now();
        let mut s = state_at(now);
        with_index(&mut s, now, |st| st.activity = Activity::LoadingDisk);
        assert_eq!(render(&s, now, EPOCH).text, "loading cached index...");
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
        assert!(line.text.contains("checking the drive"));
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
                query: s.input.clone(),
                elapsed: Duration::from_millis(2),
                outcome: crate::search::verify::VerifyOutcome::IndexAuthoritative { stamp: None },
            }),
            now,
        );

        let line = render(&s, now, EPOCH);
        assert!(line.text.starts_with("up to date"), "{}", line.text);
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
                query: s.input.clone(),
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
        assert!(line.text.starts_with("verified · 42ms"), "{}", line.text);
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
        assert!(render(&s, now, EPOCH).text.contains("truncated"));
    }

    #[test]
    fn a_missing_index_is_described_rather_than_faked() {
        let now = Instant::now();
        let s = state_at(now);
        assert!(render(&s, now, EPOCH).text.contains("no index yet"));
    }

    #[test]
    fn the_help_line_warns_about_a_missing_viewer() {
        assert!(!help_line(false).contains("WARNING"));
        assert!(help_line(true).contains("avwin.exe"));
    }

    #[test]
    fn the_help_line_never_offers_a_key_that_quits() {
        // There is no such key any more. Advertising one would send someone
        // pressing Ctrl+C expecting to exit, which now copies instead.
        let line = help_line(false);
        assert!(!line.contains("quit"), "{line}");
        assert!(line.contains("Esc clear"), "{line}");
        assert!(line.contains("Ctrl+C copy"), "{line}");
        assert!(line.contains("Up history"), "{line}");
    }
}
