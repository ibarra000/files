//! The directory index: enumeration, snapshots, persistence, and the actor
//! that keeps them fresh.

pub mod actor;
pub mod builder;
pub mod enumerate;
pub mod errors;
pub mod fake_source;
pub mod jobs;
pub mod log;
pub mod persist;
pub mod schedule;
pub mod snapshot;
pub mod std_enum;
pub mod store;

#[cfg(windows)]
pub mod volume;

#[cfg(windows)]
pub mod win_enum;

#[cfg(windows)]
pub mod win_util;

pub use errors::EnumError;
pub use snapshot::Snapshot;
pub use store::{Activity, Health, IndexStore, Origin};

/// How a [`DirStamp`] was obtained.
///
/// Two probe paths exist and they do not populate the same fields: the handle
/// query returns both timestamps, the attribute fallback only the write time.
/// Recording which one produced a stamp is what stops a source that
/// intermittently falls back from looking like a directory that changes on
/// every probe - the comparison would flip between `(lw, ct)` and `(lw, lw)`
/// forever, and every flip costs a full enumeration.
///
/// A kind change is therefore reported as "changed" exactly once; the
/// following scan records the new kind and the comparison settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampKind {
    /// `LastWriteTime` and `ChangeTime`, read from a directory handle.
    Full,
    /// `LastWriteTime` only, with `change_time` carrying the same value.
    WriteOnly,
}

impl StampKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "write+change",
            Self::WriteOnly => "write-only",
        }
    }
}

/// A directory's modification timestamps.
///
/// NTFS updates these whenever an entry is added to or removed from the
/// directory, which makes a stamp comparison a proxy for "has this listing
/// changed" that costs about three round trips instead of a full
/// enumeration - roughly a 700x saving on a million-entry share. An unchanged
/// stamp also proves the local index is still authoritative, which lets the
/// server-side verification be skipped entirely.
///
/// Some NAS firmware does not propagate directory mtime on entry churn, and
/// some shares will not answer the probe at all. Neither case is allowed to
/// turn into a per-minute rescan: see [`schedule::StampHealth`] for the
/// fallback, and `--bench --allow-write` for whether the stamp actually moves
/// on the real share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirStamp {
    pub last_write: i64,
    pub change_time: i64,
    pub kind: StampKind,
}

impl DirStamp {
    /// Both timestamps, from a directory handle.
    pub const fn new(last_write: i64, change_time: i64) -> Self {
        Self {
            last_write,
            change_time,
            kind: StampKind::Full,
        }
    }

    /// Write time only, from the attribute fallback. `change_time` carries the
    /// same value so the struct stays comparable, and `kind` records that the
    /// second field is not independent.
    pub const fn write_only(last_write: i64) -> Self {
        Self {
            last_write,
            change_time: last_write,
            kind: StampKind::WriteOnly,
        }
    }

    /// True when the two stamps describe the same directory state.
    ///
    /// Stamps of differing kinds are never equal, because their second field
    /// means different things.
    pub fn matches(self, other: Self) -> bool {
        self == other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_compare_by_both_timestamps() {
        let a = DirStamp::new(100, 200);
        assert_eq!(a, DirStamp::new(100, 200));
        assert_ne!(a, DirStamp::new(101, 200));
        assert_ne!(a, DirStamp::new(100, 201));
    }

    /// The bug this prevents: a source that sometimes answers the handle query
    /// and sometimes falls back would otherwise look like a directory changing
    /// on every single probe, costing a full enumeration each time.
    #[test]
    fn a_full_stamp_never_equals_a_write_only_stamp_with_the_same_numbers() {
        assert_ne!(DirStamp::new(100, 100), DirStamp::write_only(100));
    }

    #[test]
    fn a_write_only_stamp_carries_the_write_time_in_both_fields() {
        let s = DirStamp::write_only(42);
        assert_eq!(s.last_write, 42);
        assert_eq!(s.change_time, 42);
        assert_eq!(s.kind, StampKind::WriteOnly);
    }

    #[test]
    fn write_only_stamps_still_detect_a_change() {
        assert!(DirStamp::write_only(1).matches(DirStamp::write_only(1)));
        assert!(!DirStamp::write_only(1).matches(DirStamp::write_only(2)));
    }
}
