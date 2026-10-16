//! The share list, shown in place of the results when `F5` asks what to
//! update.
//!
//! `Enter` updates only the highlighted share. There is intentionally no
//! "update all" key: a full pass is hundreds of thousands of network round
//! trips, and many machines doing it at once overloads the file server.

use std::time::SystemTime;

use crate::index::store::{Health, IndexStatus, StaleReason};
use crate::paths::Mapping;
use crate::util::humanize;
use crate::view::status::Tone;
use crate::view::{Block, Run};

pub struct Row<'a> {
    pub mapping: &'a Mapping,
    pub status: &'a IndexStatus,
    /// Why the share wants updating, if it does. The caller judges age, since
    /// it holds the wall clock.
    pub stale: Option<StaleReason>,
}

/// Returns the share's age as text, or `not read yet`.
pub fn age_of(row: &Row<'_>, now: SystemTime) -> String {
    match row.status.age(now) {
        Some(age) => humanize::age(age),
        None => "not read yet".into(),
    }
}

/// Returns why the share should not be trusted, if anything.
fn why_not(share: &Row<'_>) -> Option<String> {
    match &share.status.health {
        Health::Unreachable { .. } => Some("unreachable".to_string()),
        Health::Degraded { reason, .. } => Some(reason.label().to_string()),
        Health::Ok => share.stale.map(|s| s.label().to_string()),
    }
}

/// Returns one share's row: name, age and, if any, a warning-toned reason.
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

    /// Returns one row as plain text.
    fn text(rows: &[Row<'_>], i: usize, now: SystemTime) -> String {
        crate::view::plain(&row(&rows[i], now))
    }

    #[test]
    fn every_share_gets_a_row() {
        let (a, b) = (mapping("projects"), mapping("archive"));
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

    #[test]
    fn a_row_shows_how_old_its_share_is() {
        let m = mapping("projects");
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

    #[test]
    fn a_share_that_was_never_read_says_so() {
        let m = mapping("projects");
        let s = status(0, None);
        let shares = vec![Row {
            mapping: &m,
            status: &s,
            stale: None,
        }];
        assert!(text(&shares, 0, EPOCH).contains("not read yet"));
    }

    #[test]
    fn a_stale_share_shows_why_rather_than_its_size() {
        let m = mapping("projects");
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

    #[test]
    fn every_row_keeps_the_house_style() {
        let (a, b) = (mapping("projects"), mapping("archive"));
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
        // Runs are checked individually because they are drawn as columns.
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
