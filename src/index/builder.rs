//! Streaming construction of a [`Snapshot`].
//!
//! Entries arrive from the enumerator as raw UTF-16 and are written straight
//! into the arenas: no `String` per entry, no `PathBuf` per entry, and no
//! separate lowercase copy. The previous implementation made four heap
//! allocations for every file, which at a million files is four million
//! allocations per refresh.
//!
//! # The offsets invariant
//!
//! `offsets` is seeded with `[0]` and every push appends the *end* offset.
//! That means `offsets.last() == arena.len()` holds continuously, the
//! terminal offset falls out for free, and the classic off-by-one at
//! `finish()` is structurally impossible.

use std::time::SystemTime;

use super::DirStamp;
use super::snapshot::{Arenas, SEP, Snapshot};
use crate::util::fold;

/// Arena addressing is 32-bit, so this is the hard ceiling on total name bytes.
pub const MAX_ARENA_BYTES: usize = u32::MAX as usize;

/// Names longer than this cannot be represented in the packed ranking key.
/// A Windows path component tops out at 255 UTF-16 units (<= 1020 UTF-8
/// bytes), so this is unreachable in practice and exists as a guard.
pub const MAX_NAME_BYTES: usize = u16::MAX as usize;

/// Builds a snapshot incrementally.
pub struct SnapshotBuilder {
    prefix: Box<str>,
    lower: Vec<u8>,
    orig: Vec<u8>,
    offsets: Vec<u32>,
    max_name_len: u32,
    truncated: bool,
    /// Reused across entries so the non-ASCII path allocates once, not once
    /// per filename.
    scratch: String,
}

impl SnapshotBuilder {
    pub fn new(prefix: &str) -> Self {
        Self::with_capacity(prefix, 0, 0)
    }

    /// Pre-sizes the arenas.
    ///
    /// The hints come from the previous run's persisted header, so from the
    /// second run onward the reservation is essentially exact and the arenas
    /// never grow.
    pub fn with_capacity(prefix: &str, hint_entries: usize, hint_avg_name: usize) -> Self {
        let arena_hint = hint_entries.saturating_mul(hint_avg_name.saturating_add(1));
        let arena_hint = arena_hint.min(MAX_ARENA_BYTES);
        let mut offsets = Vec::with_capacity(hint_entries + 1);
        offsets.push(0u32);
        Self {
            prefix: prefix.into(),
            lower: Vec::with_capacity(arena_hint),
            orig: Vec::with_capacity(arena_hint),
            offsets,
            max_name_len: 0,
            truncated: false,
            scratch: String::new(),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn arena_bytes(&self) -> usize {
        self.orig.len()
    }

    /// True once a push has been refused for want of arena space.
    #[inline]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Appends a name given as UTF-16, as delivered by the Windows
    /// enumerators.
    ///
    /// Returns `false` when the entry does not fit, in which case the caller
    /// should stop enumerating; the snapshot will report itself truncated.
    pub fn push_wide(&mut self, wide: &[u16]) -> bool {
        // Fast path: every unit is ASCII, so UTF-8 length equals unit count
        // and both arenas can be written in one pass with no intermediate
        // buffer. This is essentially every real filename.
        if wide.iter().all(|&u| u < 0x80) {
            if !self.reserve_for(wide.len()) {
                return false;
            }
            for &u in wide {
                let b = u as u8;
                self.orig.push(b);
                self.lower.push(b.to_ascii_lowercase());
            }
            self.close_entry(wide.len());
            return true;
        }

        // General path: decode UTF-16 to UTF-8 exactly once, then fold that
        // UTF-8. Folding the UTF-16 separately is how the two arenas drift
        // apart, so it is deliberately not an option here.
        self.scratch.clear();
        for r in char::decode_utf16(wide.iter().copied()) {
            // Unpaired surrogates are legal in Windows filenames. U+FFFD is
            // three bytes and identical in both arenas, so the equal-length
            // invariant survives.
            self.scratch.push(r.unwrap_or(char::REPLACEMENT_CHARACTER));
        }
        let n = self.scratch.len();
        if !self.reserve_for(n) {
            return false;
        }
        let taken = std::mem::take(&mut self.scratch);
        self.orig.extend_from_slice(taken.as_bytes());
        fold::fold_into(&taken, &mut self.lower);
        self.scratch = taken;
        self.close_entry(n);
        true
    }

    /// Appends a name given as UTF-8.
    pub fn push_str(&mut self, name: &str) -> bool {
        let n = name.len();
        if !self.reserve_for(n) {
            return false;
        }
        self.orig.extend_from_slice(name.as_bytes());
        fold::fold_into(name, &mut self.lower);
        self.close_entry(n);
        true
    }

    /// Copies an already-folded entry verbatim from another snapshot.
    ///
    /// Two memcpys and no re-folding, so a rebuild cannot introduce fold
    /// drift between the source and the copy.
    pub fn push_raw(&mut self, orig: &[u8], lower: &[u8]) -> bool {
        debug_assert_eq!(orig.len(), lower.len(), "arenas must stay in lockstep");
        let n = orig.len();
        if orig.len() != lower.len() || !self.reserve_for(n) {
            return false;
        }
        self.orig.extend_from_slice(orig);
        self.lower.extend_from_slice(lower);
        self.close_entry(n);
        true
    }

    /// Test-only escape hatch for constructing entries whose bytes are not
    /// valid UTF-8, to prove the display path degrades rather than panics.
    #[cfg(test)]
    pub fn push_raw_for_test(&mut self, orig: &[u8], lower: &[u8]) -> bool {
        self.push_raw(orig, lower)
    }

    /// Checks that a name of `n` bytes plus its separator still fits.
    fn reserve_for(&mut self, n: usize) -> bool {
        if n > MAX_NAME_BYTES {
            // Skipping one absurd name is better than truncating the listing.
            return false;
        }
        let needed = self.orig.len().saturating_add(n).saturating_add(1);
        if needed > MAX_ARENA_BYTES {
            self.truncated = true;
            return false;
        }
        true
    }

    fn close_entry(&mut self, name_len: usize) {
        self.orig.push(SEP);
        self.lower.push(SEP);
        debug_assert_eq!(self.orig.len(), self.lower.len());
        self.offsets.push(self.orig.len() as u32);
        self.max_name_len = self.max_name_len.max(name_len as u32);
        debug_assert_eq!(
            self.offsets.last().copied(),
            Some(self.orig.len() as u32),
            "offsets must track the arena length after every push"
        );
    }

    /// Seals the builder.
    pub fn finish(
        self,
        captured_at: SystemTime,
        volume_serial: u32,
        stamp: Option<DirStamp>,
    ) -> Snapshot {
        Snapshot::from_parts(
            self.prefix,
            self.offsets.into_boxed_slice(),
            Arenas::Owned {
                lower: self.lower.into_boxed_slice(),
                orig: self.orig.into_boxed_slice(),
            },
            self.max_name_len,
            captured_at,
            volume_serial,
            stamp,
            self.truncated,
        )
    }

    /// Consumes the builder and returns the raw sections, for the persistence
    /// writer, which streams them straight to disk.
    pub fn into_sections(self) -> (Box<str>, Vec<u32>, Vec<u8>, Vec<u8>, u32, bool) {
        (
            self.prefix,
            self.offsets,
            self.lower,
            self.orig,
            self.max_name_len,
            self.truncated,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn offsets_track_the_arena_after_every_push() {
        let mut b = SnapshotBuilder::new("V:\\");
        for name in ["a", "bb", "ccc"] {
            b.push_str(name);
            assert_eq!(b.offsets.last().copied(), Some(b.orig.len() as u32));
        }
        assert_eq!(b.offsets, vec![0, 2, 5, 9]);
    }

    #[test]
    fn wide_and_str_pushes_agree() {
        let names = ["Report_A1.PDF", "drawing.dwg", "MiXeD"];

        let mut a = SnapshotBuilder::new("V:\\");
        for n in names {
            a.push_str(n);
        }
        let a = a.finish(SystemTime::UNIX_EPOCH, 0, None);

        let mut w = SnapshotBuilder::new("V:\\");
        for n in names {
            w.push_wide(&utf16(n));
        }
        let w = w.finish(SystemTime::UNIX_EPOCH, 0, None);

        assert_eq!(a.offsets(), w.offsets());
        assert_eq!(a.lower(), w.lower());
        assert_eq!(a.orig(), w.orig());
    }

    #[test]
    fn wide_and_str_agree_on_non_ascii_too() {
        let names = ["Écoles-Été.pdf", "ПРИВЕТ.txt", "ΣΙΓΜΑ", "\u{212A}elvin"];

        let mut a = SnapshotBuilder::new("V:\\");
        let mut w = SnapshotBuilder::new("V:\\");
        for n in names {
            a.push_str(n);
            w.push_wide(&utf16(n));
        }
        let a = a.finish(SystemTime::UNIX_EPOCH, 0, None);
        let w = w.finish(SystemTime::UNIX_EPOCH, 0, None);

        assert_eq!(a.lower(), w.lower(), "folded arenas must match");
        assert_eq!(a.orig(), w.orig(), "original arenas must match");
    }

    #[test]
    fn unpaired_surrogates_become_replacement_chars_without_breaking_lengths() {
        let mut b = SnapshotBuilder::new("V:\\");
        // 0xD800 alone is an unpaired high surrogate.
        assert!(b.push_wide(&[0x0041, 0xD800, 0x0042]));
        let s = b.finish(SystemTime::UNIX_EPOCH, 0, None);
        s.check_invariants().unwrap();
        assert_eq!(s.display_name(0), "A\u{FFFD}B");
        assert_eq!(s.lower().len(), s.orig().len());
    }

    #[test]
    fn arenas_stay_equal_length_across_mixed_pushes() {
        let mut b = SnapshotBuilder::new("V:\\");
        b.push_str("ASCII.txt");
        b.push_wide(&utf16("Écoles.pdf"));
        b.push_str("");
        b.push_wide(&utf16("\u{1E9E}harp"));
        let s = b.finish(SystemTime::UNIX_EPOCH, 0, None);
        assert_eq!(s.lower().len(), s.orig().len());
        s.check_invariants().unwrap();
    }

    #[test]
    fn push_raw_copies_without_refolding() {
        let mut src = SnapshotBuilder::new("V:\\");
        src.push_str("MiXeD.TXT");
        let src = src.finish(SystemTime::UNIX_EPOCH, 0, None);

        let mut dst = SnapshotBuilder::new("V:\\");
        assert!(dst.push_raw(src.name_orig(0), src.name_lower(0)));
        let dst = dst.finish(SystemTime::UNIX_EPOCH, 0, None);

        assert_eq!(dst.name_orig(0), b"MiXeD.TXT");
        assert_eq!(dst.name_lower(0), b"mixed.txt");
    }

    #[test]
    fn tracks_the_longest_name() {
        let mut b = SnapshotBuilder::new("V:\\");
        b.push_str("ab");
        b.push_str("abcdef");
        b.push_str("abc");
        let s = b.finish(SystemTime::UNIX_EPOCH, 0, None);
        assert_eq!(s.max_name_len(), 6);
    }

    #[test]
    fn rejects_an_absurdly_long_name_without_truncating_the_listing() {
        let mut b = SnapshotBuilder::new("V:\\");
        let huge = "x".repeat(MAX_NAME_BYTES + 1);
        assert!(!b.push_str(&huge));
        assert!(
            !b.truncated(),
            "skipping one name is not a truncated listing"
        );
        assert!(b.push_str("ok.txt"));
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn capacity_hint_does_not_change_the_result() {
        let names = ["one.txt", "two.txt", "three.txt"];
        let mut a = SnapshotBuilder::with_capacity("V:\\", 0, 0);
        let mut b = SnapshotBuilder::with_capacity("V:\\", 3, 8);
        for n in names {
            a.push_str(n);
            b.push_str(n);
        }
        let a = a.finish(SystemTime::UNIX_EPOCH, 0, None);
        let b = b.finish(SystemTime::UNIX_EPOCH, 0, None);
        assert_eq!(a.offsets(), b.offsets());
        assert_eq!(a.lower(), b.lower());
    }

    #[test]
    fn an_empty_builder_finishes_into_an_empty_snapshot() {
        let s = SnapshotBuilder::new("V:\\").finish(SystemTime::UNIX_EPOCH, 0, None);
        assert!(s.is_empty());
        s.check_invariants().unwrap();
    }
}
