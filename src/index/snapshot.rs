//! The immutable, arena-backed directory listing.
//!
//! # Shape
//!
//! All filenames live in two contiguous byte arenas addressed by a single
//! offsets table:
//!
//! ```text
//! offsets: [0, 12, 25, 31, ...]            (n + 1 entries)
//! lower:   b"report_a1.pdf\0drawing_b.dwg\0..."
//! orig:    b"Report_A1.PDF\0Drawing_B.dwg\0..."
//!          ^entry 0      ^NUL
//! ```
//!
//! Entry `i` occupies `offsets[i] .. offsets[i+1]`, of which the final byte is
//! always a NUL separator, so its name is `offsets[i] .. offsets[i+1] - 1`.
//!
//! Three properties do the work:
//!
//! * **The directory prefix is stored once.** The previous implementation
//!   copied the full path into every entry, duplicating `"V:\"` a million
//!   times. Paths are rebuilt only for the handful of rows actually displayed.
//! * **A single offsets table serves both arenas**, which is only sound
//!   because [`crate::util::fold`] is byte-length preserving.
//! * **Entries are NUL-separated.** NUL is illegal in a Windows filename, so a
//!   substring match can never span two entries - provided the needle itself
//!   contains no NUL. The matcher enforces that; see
//!   [`crate::search::matcher`].
//!
//! A snapshot is sealed at construction and shared as `Arc<Snapshot>`. Nothing
//! is ever deep-copied to hand it to a reader.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;
use std::time::SystemTime;

use memmap2::Mmap;

use super::DirStamp;

/// Byte separating entries in both arenas. Illegal in Windows filenames.
pub const SEP: u8 = 0;

/// Backing storage for the two arenas.
///
/// The mapped variant holds the `Mmap` together with byte *ranges* rather than
/// references, which is what keeps the type free of a self-referential borrow.
pub enum Arenas {
    Owned {
        lower: Box<[u8]>,
        orig: Box<[u8]>,
    },
    Mapped {
        /// Shared, because a tree index stores its filenames and its folder
        /// names as two snapshots inside one file. Mapping that file twice
        /// would work and would be a waste; more importantly, each mapping is
        /// an independent kernel object, so the two halves of one index could
        /// then be backed by different views of the same bytes.
        map: Arc<Mmap>,
        lower: Range<usize>,
        orig: Range<usize>,
    },
}

impl Arenas {
    #[inline]
    fn lower(&self) -> &[u8] {
        match self {
            Self::Owned { lower, .. } => lower,
            Self::Mapped { map, lower, .. } => &map[lower.clone()],
        }
    }

    #[inline]
    fn orig(&self) -> &[u8] {
        match self {
            Self::Owned { orig, .. } => orig,
            Self::Mapped { map, orig, .. } => &map[orig.clone()],
        }
    }
}

impl std::fmt::Debug for Arenas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Owned { lower, .. } => f
                .debug_struct("Owned")
                .field("bytes", &lower.len())
                .finish(),
            Self::Mapped { lower, .. } => f
                .debug_struct("Mapped")
                .field("bytes", &lower.len())
                .finish(),
        }
    }
}

/// An immutable listing of one directory's immediate child files.
#[derive(Debug)]
pub struct Snapshot {
    prefix: Box<str>,
    offsets: Box<[u32]>,
    arenas: Arenas,
    len: u32,
    max_name_len: u32,
    captured_at: SystemTime,
    volume_serial: u32,
    stamp: Option<DirStamp>,
    truncated: bool,
}

impl Snapshot {
    /// Assembles a snapshot from already-validated parts.
    ///
    /// Prefer [`crate::index::builder::SnapshotBuilder`]; this exists for the
    /// persistence loader, which validates the same invariants against
    /// untrusted file bytes before calling in.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts(
        prefix: Box<str>,
        offsets: Box<[u32]>,
        arenas: Arenas,
        max_name_len: u32,
        captured_at: SystemTime,
        volume_serial: u32,
        stamp: Option<DirStamp>,
        truncated: bool,
    ) -> Self {
        let len = (offsets.len() - 1) as u32;
        Self {
            prefix,
            offsets,
            arenas,
            len,
            max_name_len,
            captured_at,
            volume_serial,
            stamp,
            truncated,
        }
    }

    /// Assembles a snapshot from *untrusted* parts, validating first.
    ///
    /// The persistence loader uses this: the bytes come from a file, so every
    /// structural invariant is proven before any of them is indexed.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_from_parts(
        prefix: Box<str>,
        offsets: Box<[u32]>,
        arenas: Arenas,
        max_name_len: u32,
        captured_at: SystemTime,
        volume_serial: u32,
        stamp: Option<DirStamp>,
        truncated: bool,
    ) -> Result<Self, &'static str> {
        if offsets.is_empty() {
            return Err("offsets must contain at least the initial zero");
        }
        let s = Self::from_parts(
            prefix,
            offsets,
            arenas,
            max_name_len,
            captured_at,
            volume_serial,
            stamp,
            truncated,
        );
        s.check_invariants()?;
        Ok(s)
    }

    /// An empty listing for `prefix`.
    pub fn empty(prefix: &str) -> Self {
        Self::from_parts(
            prefix.into(),
            vec![0u32].into_boxed_slice(),
            Arenas::Owned {
                lower: Box::new([]),
                orig: Box::new([]),
            },
            0,
            SystemTime::UNIX_EPOCH,
            0,
            None,
            false,
        )
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// `n + 1` byte offsets into both arenas.
    #[inline]
    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }

    /// The folded arena, including NUL separators. This is what the matcher
    /// scans.
    #[inline]
    pub fn lower(&self) -> &[u8] {
        self.arenas.lower()
    }

    /// The original-case arena, including NUL separators.
    #[inline]
    pub fn orig(&self) -> &[u8] {
        self.arenas.orig()
    }

    /// Length in bytes of entry `i`'s name, excluding the separator.
    #[inline]
    pub fn name_len(&self, i: u32) -> u32 {
        let i = i as usize;
        self.offsets[i + 1] - self.offsets[i] - 1
    }

    /// Folded name bytes of entry `i`, excluding the separator.
    #[inline]
    pub fn name_lower(&self, i: u32) -> &[u8] {
        let r = self.name_range(i);
        &self.arenas.lower()[r]
    }

    /// Original-case name bytes of entry `i`, excluding the separator.
    #[inline]
    pub fn name_orig(&self, i: u32) -> &[u8] {
        let r = self.name_range(i);
        &self.arenas.orig()[r]
    }

    #[inline]
    fn name_range(&self, i: u32) -> Range<usize> {
        let i = i as usize;
        let start = self.offsets[i] as usize;
        let end = self.offsets[i + 1] as usize - 1;
        start..end
    }

    /// Entry `i`'s name as text.
    ///
    /// Deliberately lossy: for a mapped snapshot these bytes come from a file
    /// that another process could in principle have altered, so this must
    /// never be `from_utf8_unchecked`.
    pub fn display_name(&self, i: u32) -> Cow<'_, str> {
        String::from_utf8_lossy(self.name_orig(i))
    }

    /// Full path to entry `i`, allocated on demand.
    ///
    /// Called only for the rows actually shown, never per entry.
    pub fn full_path(&self, i: u32) -> String {
        let name = self.display_name(i);
        let mut s = String::with_capacity(self.prefix.len() + name.len() + 1);
        s.push_str(&self.prefix);
        if !self.prefix.is_empty() && !self.prefix.ends_with(['\\', '/']) {
            s.push('\\');
        }
        s.push_str(&name);
        s
    }

    /// Longest name in the snapshot. Lets the matcher reject an over-long
    /// query in O(1) instead of scanning the whole arena.
    #[inline]
    pub fn max_name_len(&self) -> u32 {
        self.max_name_len
    }

    #[inline]
    pub fn captured_at(&self) -> SystemTime {
        self.captured_at
    }

    #[inline]
    pub fn volume_serial(&self) -> u32 {
        self.volume_serial
    }

    /// Directory timestamp at capture time, when the source could supply one.
    /// An unchanged stamp proves the listing is still current.
    #[inline]
    pub fn stamp(&self) -> Option<DirStamp> {
        self.stamp
    }

    /// True when the source directory held more entries than the arena could
    /// address. The UI must say so rather than silently showing a prefix.
    #[inline]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Total bytes held by the arenas and the offsets table.
    pub fn memory_bytes(&self) -> usize {
        self.arenas.lower().len() + self.arenas.orig().len() + self.offsets.len() * 4
    }

    /// Verifies every structural invariant.
    ///
    /// Cheap enough (a single pass over the offsets) to run against untrusted
    /// persisted bytes at load time, which converts every subsequent indexing
    /// operation from "trusted" to "proven in bounds".
    pub fn check_invariants(&self) -> Result<(), &'static str> {
        if self.offsets.is_empty() {
            return Err("offsets must contain at least the initial zero");
        }
        if self.offsets[0] != 0 {
            return Err("offsets must start at zero");
        }
        let lower = self.arenas.lower();
        let orig = self.arenas.orig();
        if lower.len() != orig.len() {
            return Err("arenas must be the same length");
        }
        if *self.offsets.last().unwrap() as usize != lower.len() {
            return Err("final offset must equal the arena length");
        }
        if self.len as usize != self.offsets.len() - 1 {
            return Err("len must equal offsets.len() - 1");
        }
        let mut max_seen = 0u32;
        for w in self.offsets.windows(2) {
            // Every entry carries at least its separator, so offsets are
            // strictly increasing.
            if w[1] <= w[0] {
                return Err("offsets must be strictly increasing");
            }
            max_seen = max_seen.max(w[1] - w[0] - 1);
        }
        if max_seen != self.max_name_len {
            return Err("max_name_len does not match the offsets table");
        }
        Ok(())
    }

    /// Samples separator placement.
    ///
    /// Checking every entry would touch every page and defeat lazy mapping;
    /// a missing separator can only produce a wrong result, never unsafety,
    /// because all indexing goes through the validated offsets.
    pub fn sample_separators(&self, stride: usize) -> Result<(), &'static str> {
        if self.len == 0 {
            return Ok(());
        }
        let lower = self.arenas.lower();
        let stride = stride.max(1);
        let last = self.len - 1;
        let mut i = 0u32;
        loop {
            let end = self.offsets[i as usize + 1] as usize - 1;
            if lower[end] != SEP {
                return Err("entry is not NUL terminated");
            }
            if i == last {
                break;
            }
            i = (i + stride as u32).min(last);
        }
        Ok(())
    }

    /// Iterates original-case names. Used by tests and diagnostics.
    pub fn iter_names(&self) -> impl Iterator<Item = Cow<'_, str>> + '_ {
        (0..self.len).map(move |i| self.display_name(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::builder::SnapshotBuilder;

    fn snap(names: &[&str]) -> Snapshot {
        let mut b = SnapshotBuilder::new("V:\\");
        for n in names {
            assert!(b.push_str(n));
        }
        b.finish(SystemTime::UNIX_EPOCH, 0, None)
    }

    #[test]
    fn empty_snapshot_is_structurally_valid() {
        let s = Snapshot::empty("V:\\");
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
        assert_eq!(s.offsets(), &[0]);
        s.check_invariants().unwrap();
    }

    #[test]
    fn round_trips_names_in_order() {
        let names = ["Report_A1.PDF", "drawing_b.dwg", "x"];
        let s = snap(&names);
        assert_eq!(s.len(), 3);
        let got: Vec<String> = s.iter_names().map(|c| c.into_owned()).collect();
        assert_eq!(got, names);
    }

    #[test]
    fn folded_and_original_arenas_share_one_offsets_table() {
        let s = snap(["Report_A1.PDF", "MiXeD.txt"].as_slice());
        assert_eq!(s.name_lower(0), b"report_a1.pdf");
        assert_eq!(s.name_orig(0), b"Report_A1.PDF");
        assert_eq!(s.name_lower(1), b"mixed.txt");
        assert_eq!(s.name_orig(1), b"MiXeD.txt");
        assert_eq!(s.lower().len(), s.orig().len());
    }

    #[test]
    fn every_entry_is_nul_terminated() {
        let s = snap(["a", "bb", "ccc"].as_slice());
        for i in 0..s.len() as u32 {
            let end = s.offsets()[i as usize + 1] as usize - 1;
            assert_eq!(s.lower()[end], SEP);
            assert_eq!(s.orig()[end], SEP);
        }
        s.sample_separators(1).unwrap();
    }

    #[test]
    fn name_len_excludes_the_separator() {
        let s = snap(["abcd", "ef"].as_slice());
        assert_eq!(s.name_len(0), 4);
        assert_eq!(s.name_len(1), 2);
        assert_eq!(s.max_name_len(), 4);
    }

    #[test]
    fn offsets_are_strictly_increasing_even_for_empty_names() {
        // An empty name still occupies its separator byte, which is what keeps
        // the offsets strictly increasing and the galloping cursor in the
        // matcher monotone.
        let mut b = SnapshotBuilder::new("V:\\");
        assert!(b.push_str(""));
        assert!(b.push_str("a"));
        let s = b.finish(SystemTime::UNIX_EPOCH, 0, None);
        assert_eq!(s.name_len(0), 0);
        assert!(s.offsets().windows(2).all(|w| w[1] > w[0]));
        s.check_invariants().unwrap();
    }

    #[test]
    fn full_path_joins_the_prefix_once() {
        let s = snap(["Report.pdf"].as_slice());
        assert_eq!(s.full_path(0), "V:\\Report.pdf");
    }

    #[test]
    fn full_path_inserts_a_separator_when_the_prefix_lacks_one() {
        let mut b = SnapshotBuilder::new("R:\\ab1234");
        b.push_str("f.txt");
        let s = b.finish(SystemTime::UNIX_EPOCH, 0, None);
        assert_eq!(s.full_path(0), "R:\\ab1234\\f.txt");
    }

    #[test]
    fn the_prefix_is_stored_once_not_per_entry() {
        let s = snap(["a.txt", "b.txt", "c.txt"].as_slice());
        // Arena holds only names plus separators: 5+1 three times.
        assert_eq!(s.lower().len(), 18);
        assert!(!s.lower().windows(3).any(|w| w == b"V:\\"));
    }

    #[test]
    fn display_name_is_lossy_rather_than_panicking_on_bad_utf8() {
        let mut b = SnapshotBuilder::new("V:\\");
        b.push_raw_for_test(&[0xFF, 0xFE, b'a'], &[0xFF, 0xFE, b'a']);
        let s = b.finish(SystemTime::UNIX_EPOCH, 0, None);
        assert!(s.display_name(0).contains('\u{FFFD}'));
    }

    #[test]
    fn non_ascii_names_survive_the_round_trip() {
        let s = snap(["Écoles-Été.pdf", "ПРИВЕТ.txt"].as_slice());
        assert_eq!(s.display_name(0), "Écoles-Été.pdf");
        assert_eq!(s.display_name(1), "ПРИВЕТ.txt");
        assert_eq!(s.name_lower(0), "écoles-été.pdf".as_bytes());
        s.check_invariants().unwrap();
    }

    /// Validation of untrusted parts, which is what the persistence loader
    /// relies on before it indexes a single byte of a mapped file.
    #[test]
    fn try_from_parts_rejects_corrupt_input() {
        let good = snap(["aa", "bb"].as_slice());
        let arenas = || Arenas::Owned {
            lower: good.lower().to_vec().into_boxed_slice(),
            orig: good.orig().to_vec().into_boxed_slice(),
        };
        let build = |offsets: Vec<u32>, max_name_len: u32| {
            Snapshot::try_from_parts(
                good.prefix().into(),
                offsets.into_boxed_slice(),
                arenas(),
                max_name_len,
                SystemTime::UNIX_EPOCH,
                0,
                None,
                false,
            )
        };

        assert!(
            build(vec![0, 3, 6], 2).is_ok(),
            "the honest table must pass"
        );
        assert!(
            build(vec![0, 3, 3], 2).is_err(),
            "must reject non-increasing offsets"
        );
        assert!(
            build(vec![1, 3, 6], 2).is_err(),
            "must reject a nonzero first offset"
        );
        assert!(
            build(vec![0, 3, 9], 2).is_err(),
            "must reject a final offset past the arena"
        );
        assert!(build(vec![], 0).is_err(), "must reject an empty table");
        assert!(
            build(vec![0, 3, 6], 99).is_err(),
            "must reject a bogus max_name_len"
        );
    }

    #[test]
    fn memory_is_bounded_by_names_plus_offsets() {
        let s = snap(["abc", "de"].as_slice());
        // (3+1) + (2+1) = 7 bytes per arena, plus 3 offsets of 4 bytes.
        assert_eq!(s.memory_bytes(), 7 + 7 + 12);
    }
}
