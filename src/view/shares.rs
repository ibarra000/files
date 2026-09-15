//! The share list.
//!
//! Shown in place of the results when `F5` asks what to update. Deliberately
//! not an overlay, for the reason [`crate::history`] gives: this program has
//! no z-order and does not need one for a list.
//!
//! # Why this exists at all
//!
//! `F5` used to mean "re-read every share, now". With one person that is a
//! background hum. With a few hundred, all pressing it after the same email
//! goes round, it is several hundred simultaneous passes over a share of three
//! hundred thousand folders - and each pass is some nine hundred thousand
//! round trips. Almost every one of those presses wanted *one* share.
//!
//! So the question is asked rather than assumed. The list says how old each
//! share is and which one has a reason to be updated, `Enter` updates the one
//! highlighted, and `A` still updates everything for the times that is really
//! what was meant.

use std::time::SystemTime;

use crate::index::store::{Health, IndexStatus, StaleReason};
use crate::paths::Mapping;
use crate::util::humanize;
use crate::view::status::Tone;
use crate::view::{Block, Run};

pub struct Row<'a> {
    pub mapping: &'a Mapping,
    pub status: &'a IndexStatus,
    /// Why it wants updating, if it does. Age is judged by the caller, which
    /// is the only place that holds a wall clock.
    pub stale: Option<StaleReason>,
}

/// The width the name column is padded to.
///
/// Fixed rather than measured: the ages line up across rows, which is what
/// makes "which of these is out of date" answerable at a glance rather than by
/// reading every line.
pub fn age_of(row: &Row<'_>, now: SystemTime) -> String {
    match row.status.age(now) {
        Some(age) => humanize::age(age),
        // Not "unknown": the honest reading is that nothing has been read yet,
        // which is a different thing from an age nobody could work out.
        None => "not read yet".into(),
    }
}

/// Why this share should not be trusted, if it should not be.
fn why_not(share: &Row<'_>) -> Option<String> {
    match &share.status.health {
        Health::Unreachable { .. } => Some("unreachable".to_string()),
        Health::Degraded { reason, .. } => Some(reason.label().to_string()),
        Health::Ok => share.stale.map(|s| s.label().to_string()),
    }
}

/// One share, as it reads.
///
/// Three runs: the name, the age, and - only when there is one - why it is not
/// to be trusted. Keeping the reason in its own run is what lets a renderer
/// colour it without re-parsing the sentence.
pub fn row(share: &Row<'_>, now: SystemTime) -> Block {
    let mut out = vec![
        Run::body(share.mapping.name.to_string()),
        Run::dim(age_of(share, now)),
    ];
    if let Some(why) = why_not(share) {
        out.push(Run::toned(why, Tone::Warn));
    }
    out
}

// `title` and `empty_message` used to live here. Both described a bordered
// pane in a terminal: a caption inside the border, padded with a leading and a
// trailing space, and a message indented by two more. The drive list is drawn
// into the panel's body now, with no border and no caption - so both had no
// caller at all, and `empty_message` was two house-style violations (a leading
// pad and a backtick-quoted command) in a string nothing rendered.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::store::Origin;
    use crate::paths::{MappingId, MappingKind, RefreshPolicy};
    use std::time::Duration;

    const EPOCH: SystemTime = SystemTime::UNIX_EPOCH;

    fn mapping(name: &str) -> Mapping {
        Mapping {
            id: MappingId(0),
            name: name.into(),
            path: "R:\\".into(),
            kind: MappingKind::Tree,
            enabled: true,
            refresh: RefreshPolicy::Manual,
            depth: crate::config::DEFAULT_LIVE_DEPTH,
        }
    }

    fn status(entries: u32, built: Option<SystemTime>) -> IndexStatus {
        IndexStatus {
            origin: Some(Origin::Network),
            entries,
            built_at: built,
            ..IndexStatus::default()
        }
    }

    /// One row as plain text, which is what every assertion here is about.
    fn text(rows: &[Row<'_>], i: usize, now: SystemTime) -> String {
        crate::view::plain(&row(&rows[i], now))
    }

    #[test]
    fn every_share_gets_a_row() {
        let (a, b) = (mapping("jobs"), mapping("archive"));
        let (sa, sb) = (status(10, Some(EPOCH)), status(20, Some(EPOCH)));
        let shares = [
            Row {
                mapping: &a,
                status: &sa,
                stale: None,
            },
            Row {
                mapping: &b,
                status: &sb,
                stale: None,
            },
        ];
        assert_eq!(shares.len(), 2);
        assert!(!crate::view::plain(&row(&shares[0], EPOCH)).is_empty());
        assert!(!crate::view::plain(&row(&shares[1], EPOCH)).is_empty());
    }

    /// The number somebody is looking for when deciding what to update.
    #[test]
    fn a_row_shows_how_old_its_share_is() {
        let m = mapping("jobs");
        let s = status(10, Some(EPOCH));
        let shares = vec![Row {
            mapping: &m,
            status: &s,
            stale: None,
        }];
        let hour = EPOCH + Duration::from_secs(3600);
        assert!(
            text(&shares, 0, hour).contains("1h"),
            "{}",
            text(&shares, 0, hour)
        );
    }

    /// A share that has never been read says so, rather than reporting an age
    /// it does not have.
    #[test]
    fn a_share_that_was_never_read_says_so() {
        let m = mapping("jobs");
        let s = status(0, None);
        let shares = vec![Row {
            mapping: &m,
            status: &s,
            stale: None,
        }];
        assert!(text(&shares, 0, EPOCH).contains("not read yet"));
    }

    /// The reason replaces the file count: somebody scanning this list is
    /// looking for what needs doing, not for how big each share is.
    #[test]
    fn a_stale_share_shows_why_rather_than_its_size() {
        let m = mapping("jobs");
        let s = status(1_284_551, Some(EPOCH));
        let shares = vec![Row {
            mapping: &m,
            status: &s,
            stale: Some(StaleReason::EventsLost),
        }];
        let row = text(&shares, 0, EPOCH);
        assert!(row.contains("changes were missed"), "{row}");
        assert!(!row.contains("1,284,551"), "{row}");
    }

    /// Every row this module can produce, held to the house style.
    ///
    /// The drive name and the age come from the configuration and the clock,
    /// so what is being checked here is the shape of the line around them -
    /// which is the part this module writes.
    #[test]
    fn every_row_keeps_the_house_style() {
        let (a, b) = (mapping("jobs"), mapping("archive"));
        let (sa, sb) = (status(10, Some(EPOCH)), status(20, None));
        let rows = [
            Row {
                mapping: &a,
                status: &sa,
                stale: Some(StaleReason::Age),
            },
            Row {
                mapping: &b,
                status: &sb,
                stale: None,
            },
        ];
        // Each run on its own, not the joined block: these three are drawn
        // as columns, so there is no one line here to check.
        let lines: Vec<String> = rows
            .iter()
            .flat_map(|r| row(r, EPOCH + Duration::from_secs(9_000)))
            .map(|run| run.text.into_owned())
            .collect();
        crate::view::style::check_all(
            "the drive list",
            lines.iter().map(String::as_str),
            crate::view::style::Slot::Body,
        );
    }
}
