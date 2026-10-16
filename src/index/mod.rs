//! The directory index: enumeration, snapshots, persistence, and the actor
//! that keeps them fresh.

pub mod actor;
pub mod builder;
pub mod enumerate;
pub mod errors;
pub mod fake_source;
pub mod log;
pub mod permit;
pub mod persist;
pub mod schedule;
pub mod snapshot;
pub mod std_enum;
pub mod store;
pub mod tree;
pub mod walk;
pub mod watch;

#[cfg(windows)]
pub mod volume;

#[cfg(windows)]
pub mod win_enum;

#[cfg(windows)]
pub mod win_util;

#[cfg(windows)]
pub mod win_watch;

pub use errors::EnumError;
pub use snapshot::Snapshot;
pub use store::{Activity, Health, IndexStore, Origin};

/// How a [`DirStamp`] was obtained.
///
/// The handle query returns both timestamps; the attribute fallback returns
/// only the write time. Recording the kind keeps a source that intermittently
/// falls back from comparing `(lw, ct)` against `(lw, lw)` and forcing a full
/// enumeration on every probe. A kind change reports "changed" once, then the
/// next scan records the new kind.
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
/// NTFS updates these when an entry is added or removed, so comparing stamps
/// detects a changed listing in about three round trips instead of a full
/// enumeration. An unchanged stamp also means the local index is still
/// authoritative and server-side verification can be skipped.
///
/// Some NAS firmware does not update directory mtime on entry churn, and some
/// shares do not answer the probe. See [`schedule::StampHealth`] for the
/// fallback, and `--bench --allow-write` to test whether a share's stamp moves.
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

    /// Write time only, from the attribute fallback. `change_time` repeats the
    /// write time and `kind` marks it as not independent.
    pub const fn write_only(last_write: i64) -> Self {
        Self {
            last_write,
            change_time: last_write,
            kind: StampKind::WriteOnly,
        }
    }

    /// Returns true when both stamps describe the same directory state.
    ///
    /// Stamps of differing kinds are never equal.
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

    /// A source that alternates between the handle query and the fallback must
    /// not look like a directory that changes on every probe.
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
