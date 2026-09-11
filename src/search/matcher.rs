//! Substring matching over a [`Snapshot`].
//!
//! The previous implementation called `str::find` once per entry. That routes
//! through `core::str::pattern::StrSearcher`, which is the two-way algorithm
//! with no SIMD and recomputes its critical factorisation on every call - on
//! the order of 100-300 ms for a million entries, on the UI thread.
//!
//! Here the arena is contiguous, so a single `memchr::memmem` sweep covers a
//! whole chunk of entries at vectorised speed and byte hits are mapped back to
//! entry indices afterwards. Roughly 1-3 ms across eight cores for a 30 MB
//! arena, and sub-millisecond once it is warm in L3 - which it will be while
//! someone is typing.
//!
//! # Ranking parity
//!
//! Semantics are unchanged from the original: earliest match position wins,
//! then the shorter filename, and `matched` counts *entries* rather than hits.
//! `tests/matcher_parity.rs` asserts exact equality against a transcription of
//! the original `rank_matches`, including result order.
//!
//! # Why hits dedup for free
//!
//! `memmem` yields hits in strictly increasing byte order. On the first hit
//! inside an entry the cursor jumps past that entry's terminator, so each
//! entry contributes at most one candidate and the recorded position is
//! necessarily the leftmost - exactly what `str::find` returns.

use std::sync::Arc;

use memchr::memmem;
use rayon::prelude::*;

use crate::config::{MATCH_CHUNK_ENTRIES, MIN_QUERY_LEN, MatcherKind};
use crate::index::snapshot::Snapshot;
use crate::search::topk::{self, Key, TopK};
use crate::util::cancel::CancelToken;
use crate::util::fold;

/// A displayed result.
///
/// The full path is materialised here, not stored as an index. Holding an
/// index across a snapshot publish would silently open a *different* file, so
/// the ~15 allocations this costs buy structural immunity to that bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: Arc<str>,
    pub name: Arc<str>,
    /// Byte offset of the match within the name, for highlighting.
    pub match_pos: u32,
    pub index: u32,
}

/// Result of a completed match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOutcome {
    /// At most `topk::K`, already in display order.
    pub hits: Vec<Hit>,
    /// Number of entries that matched, which may greatly exceed `hits.len()`.
    pub matched: u32,
    /// Entries in the snapshot.
    pub total: u32,
    /// True when the sweep stopped early because the work was superseded.
    pub cancelled: bool,
    /// True when the slow Unicode fallback produced these results.
    pub unicode_fallback: bool,
}

/// Why a query was not run at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryReject {
    /// Below [`MIN_QUERY_LEN`].
    TooShort { need: usize },
    /// Contains a NUL byte.
    ///
    /// Rejected rather than stripped: stripping would let `a\0bc` match `abc`,
    /// which is silently wrong. This check is also what upholds the guarantee
    /// that a match can never span two arena entries.
    ContainsNul,
}

/// Runs a query against a snapshot.
pub fn search(
    snap: &Snapshot,
    query: &str,
    kind: MatcherKind,
    cancel: &CancelToken,
) -> Result<SearchOutcome, QueryReject> {
    if query.chars().count() < MIN_QUERY_LEN {
        return Err(QueryReject::TooShort {
            need: MIN_QUERY_LEN,
        });
    }
    let needle = fold::fold_query(query);
    if needle.contains(&0) {
        return Err(QueryReject::ContainsNul);
    }

    let total = snap.len() as u32;

    // O(1) rejection: no entry can contain a needle longer than the longest
    // name, so an over-long paste never touches the arena.
    if needle.len() as u32 > snap.max_name_len() || total == 0 {
        return Ok(SearchOutcome {
            total,
            ..Default::default()
        });
    }

    let (top, matched, cancelled) = match kind {
        MatcherKind::Simd => sweep(snap, &needle, cancel),
        MatcherKind::Naive => naive(snap, &needle, cancel),
    };

    // A non-ASCII query may have been folded conservatively (see
    // `util::fold`), so a zero-result fast path might be a false negative
    // rather than a real absence. Only then is the slow, fully Unicode-correct
    // comparison worth its cost.
    if matched == 0 && !cancelled && fold::needs_unicode_fallback(query) {
        let (top, matched) = unicode_fallback(snap, query, cancel);
        return Ok(SearchOutcome {
            hits: materialise(snap, &top.into_sorted()),
            matched,
            total,
            cancelled: cancel.is_cancelled(),
            unicode_fallback: true,
        });
    }

    Ok(SearchOutcome {
        hits: materialise(snap, &top.into_sorted()),
        matched,
        total,
        cancelled,
        unicode_fallback: false,
    })
}

/// Parallel SIMD sweep.
fn sweep(snap: &Snapshot, needle: &[u8], cancel: &CancelToken) -> (TopK, u32, bool) {
    let offsets = snap.offsets();
    let arena = snap.lower();
    let n = snap.len();
    let chunks = n.div_ceil(MATCH_CHUNK_ENTRIES);

    // Below a chunk's worth of entries the rayon fan-out costs more than the
    // scan it parallelises.
    if chunks <= 1 {
        let (top, matched) = scan_chunk(arena, offsets, needle, 0, n);
        return (top, matched, cancel.is_cancelled());
    }

    let finder_needle = needle.to_vec();
    let (top, matched, cancelled) = (0..chunks)
        .into_par_iter()
        .map(|c| {
            // One check per chunk. At roughly 160 us of work per chunk this
            // bounds cancellation latency well below perception, without
            // putting a branch inside the vectorised scan.
            if cancel.is_cancelled() {
                return (TopK::new(), 0u32, true);
            }
            let lo = c * MATCH_CHUNK_ENTRIES;
            let hi = ((c + 1) * MATCH_CHUNK_ENTRIES).min(n);
            let (top, matched) = scan_chunk(arena, offsets, &finder_needle, lo, hi);
            (top, matched, false)
        })
        .reduce(
            || (TopK::new(), 0u32, false),
            |(mut a, ca, xa), (b, cb, xb)| {
                a.merge(b);
                (a, ca + cb, xa || xb)
            },
        );

    (top, matched, cancelled)
}

/// Sweeps entries `lo..hi`, whose names occupy one contiguous arena range.
fn scan_chunk(arena: &[u8], offsets: &[u32], needle: &[u8], lo: usize, hi: usize) -> (TopK, u32) {
    let mut top = TopK::new();
    let mut matched = 0u32;
    if lo >= hi {
        return (top, matched);
    }

    // Chunk boundaries are snapped to entry boundaries, so this slice holds
    // entries lo..hi whole. Splitting mid-entry would both destroy a match at
    // the seam and compute a nonsensical position for the fragment.
    let byte_lo = offsets[lo] as usize;
    let byte_hi = offsets[hi] as usize;
    let slice = &arena[byte_lo..byte_hi];

    let finder = memmem::Finder::new(needle);
    let mut cursor = 0usize;
    let mut entry = lo;

    while let Some(rel) = finder.find(&slice[cursor..]) {
        let abs = (byte_lo + cursor + rel) as u32;
        entry = advance_to(offsets, entry, abs);

        let start = offsets[entry];
        let end = offsets[entry + 1];
        let pos = abs - start;
        let name_len = end - start - 1;

        top.push(topk::key(pos, name_len, entry as u32));
        matched += 1;

        // Jump past this entry entirely. This is what makes `matched` count
        // entries rather than hits, and what guarantees `pos` is the leftmost
        // occurrence.
        cursor = end as usize - byte_lo;
        entry += 1;
        if cursor >= slice.len() || entry >= hi {
            break;
        }
    }

    (top, matched)
}

/// Maps a byte offset to its entry index, starting from `entry`.
///
/// Both the hit offsets and the entry cursor advance monotonically, so this
/// gallops forward from where it left off instead of binary searching the
/// whole offsets array from the root: O(1) when hits are dense, O(log gap)
/// when they are sparse, and every probe lands in cache lines already being
/// walked.
#[inline]
fn advance_to(offsets: &[u32], entry: usize, abs: u32) -> usize {
    if offsets[entry + 1] > abs {
        return entry;
    }
    let last = offsets.len() - 1;
    let mut step = 1usize;
    while entry + step + 1 < last && offsets[entry + step + 1] <= abs {
        step <<= 1;
    }
    let hi = (entry + step + 1).min(last);
    entry + offsets[entry + 1..=hi].partition_point(|&o| o <= abs)
}

/// Straightforward per-entry search, retained so a field regression in the
/// SIMD path is a flag flip rather than a rebuild.
fn naive(snap: &Snapshot, needle: &[u8], cancel: &CancelToken) -> (TopK, u32, bool) {
    let mut top = TopK::new();
    let mut matched = 0u32;
    for i in 0..snap.len() as u32 {
        if i % 4096 == 0 && cancel.is_cancelled() {
            return (top, matched, true);
        }
        let name = snap.name_lower(i);
        if let Some(pos) = memmem::find(name, needle) {
            top.push(topk::key(pos as u32, name.len() as u32, i));
            matched += 1;
        }
    }
    (top, matched, false)
}

/// Fully Unicode-correct comparison, for the rare query the length-preserving
/// fold cannot represent.
fn unicode_fallback(snap: &Snapshot, query: &str, cancel: &CancelToken) -> (TopK, u32) {
    let mut top = TopK::new();
    let mut matched = 0u32;
    let needle = query.to_lowercase();
    for i in 0..snap.len() as u32 {
        if i % 4096 == 0 && cancel.is_cancelled() {
            break;
        }
        let name = String::from_utf8_lossy(snap.name_orig(i));
        if let Some(pos) = name.to_lowercase().find(&needle) {
            top.push(topk::key(pos as u32, name.len() as u32, i));
            matched += 1;
        }
    }
    (top, matched)
}

/// Turns retained keys into displayable hits.
///
/// Takes the already-sorted keys rather than the selection itself: the heap's
/// internal order is not display order, so the sort has to happen somewhere,
/// and doing it once at the boundary keeps `TopK` free of a second copy.
fn materialise(snap: &Snapshot, keys: &[Key]) -> Vec<Hit> {
    keys.iter()
        .map(|&k: &Key| {
            let index = topk::key_index(k);
            Hit {
                path: Arc::from(snap.full_path(index).as_str()),
                name: Arc::from(snap.display_name(index).as_ref()),
                match_pos: topk::key_pos(k),
                index,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::builder::SnapshotBuilder;
    use std::time::SystemTime;

    fn snap(names: &[&str]) -> Snapshot {
        let mut b = SnapshotBuilder::new("V:\\");
        for n in names {
            b.push_str(n);
        }
        b.finish(SystemTime::UNIX_EPOCH, 0, None)
    }

    fn run(s: &Snapshot, q: &str) -> SearchOutcome {
        search(s, q, MatcherKind::Simd, &CancelToken::never()).unwrap()
    }

    fn names(o: &SearchOutcome) -> Vec<String> {
        o.hits.iter().map(|h| h.name.to_string()).collect()
    }

    #[test]
    fn finds_a_substring_case_insensitively() {
        let s = snap(&["Report_ABC123.pdf", "other.txt"]);
        let o = run(&s, "abc123");
        assert_eq!(names(&o), vec!["Report_ABC123.pdf"]);
        assert_eq!(o.matched, 1);
        assert_eq!(o.total, 2);
    }

    #[test]
    fn ranks_earlier_matches_first() {
        let s = snap(&["xxabc.txt", "abc.txt", "xabc.txt"]);
        assert_eq!(
            names(&run(&s, "abc")),
            vec!["abc.txt", "xabc.txt", "xxabc.txt"]
        );
    }

    #[test]
    fn breaks_position_ties_by_shorter_name() {
        let s = snap(&["abc_long_name.txt", "abc.txt", "abc_mid.txt"]);
        assert_eq!(
            names(&run(&s, "abc")),
            vec!["abc.txt", "abc_mid.txt", "abc_long_name.txt"]
        );
    }

    #[test]
    fn breaks_remaining_ties_by_directory_order() {
        let s = snap(&["abc1", "abc2", "abc3"]);
        assert_eq!(names(&run(&s, "abc")), vec!["abc1", "abc2", "abc3"]);
    }

    #[test]
    fn counts_entries_not_occurrences() {
        // Three occurrences of "aa" inside one entry must count once.
        let s = snap(&["aaaaaa", "zzz"]);
        let o = run(&s, "aaa");
        assert_eq!(o.matched, 1, "one entry matched, however many occurrences");
        assert_eq!(o.hits.len(), 1);
    }

    #[test]
    fn reports_the_leftmost_occurrence() {
        let s = snap(&["zzabczzabc"]);
        assert_eq!(run(&s, "abc").hits[0].match_pos, 2);
    }

    #[test]
    fn caps_results_but_not_the_match_count() {
        let names_v: Vec<String> = (0..100).map(|i| format!("abc{i:03}")).collect();
        let refs: Vec<&str> = names_v.iter().map(|s| s.as_str()).collect();
        let s = snap(&refs);
        let o = run(&s, "abc");
        assert_eq!(o.hits.len(), topk::K);
        assert_eq!(o.matched, 100);
        assert_eq!(o.total, 100);
    }

    #[test]
    fn a_match_can_never_span_two_entries() {
        // "cd" would appear across the boundary of "abc" and "def" were the
        // entries not separated.
        let s = snap(&["abc", "def"]);
        assert_eq!(run(&s, "cde").matched, 0);
    }

    #[test]
    fn rejects_a_query_shorter_than_the_minimum() {
        let s = snap(&["abc"]);
        assert_eq!(
            search(&s, "ab", MatcherKind::Simd, &CancelToken::never()),
            Err(QueryReject::TooShort {
                need: MIN_QUERY_LEN
            })
        );
    }

    #[test]
    fn rejects_rather_than_strips_an_embedded_nul() {
        let s = snap(&["abcd"]);
        assert_eq!(
            search(&s, "ab\0cd", MatcherKind::Simd, &CancelToken::never()),
            Err(QueryReject::ContainsNul)
        );
    }

    #[test]
    fn a_query_longer_than_any_name_short_circuits() {
        let s = snap(&["ab.txt"]);
        let o = run(&s, &"x".repeat(500));
        assert_eq!(o.matched, 0);
        assert_eq!(o.total, 1);
    }

    #[test]
    fn searching_an_empty_snapshot_is_not_an_error() {
        let s = Snapshot::empty("V:\\");
        let o = run(&s, "abc");
        assert_eq!(o.total, 0);
        assert_eq!(o.matched, 0);
    }

    #[test]
    fn hits_carry_a_usable_full_path() {
        let s = snap(&["Report.pdf"]);
        assert_eq!(&*run(&s, "report").hits[0].path, "V:\\Report.pdf");
    }

    #[test]
    fn simd_and_naive_agree() {
        let names_v: Vec<String> = (0..5000)
            .map(|i| format!("job_{i:04}_report.pdf"))
            .collect();
        let refs: Vec<&str> = names_v.iter().map(|s| s.as_str()).collect();
        let s = snap(&refs);
        for q in ["job", "report", "_012", "0999", "zzz", ".pdf"] {
            let a = search(&s, q, MatcherKind::Simd, &CancelToken::never()).unwrap();
            let b = search(&s, q, MatcherKind::Naive, &CancelToken::never()).unwrap();
            assert_eq!(a.hits, b.hits, "hits diverged for {q:?}");
            assert_eq!(a.matched, b.matched, "count diverged for {q:?}");
        }
    }

    #[test]
    fn works_across_many_rayon_chunks() {
        // Comfortably more than one chunk, so the parallel merge is exercised.
        let n = MATCH_CHUNK_ENTRIES * 3 + 17;
        let names_v: Vec<String> = (0..n).map(|i| format!("f{i:07}.dat")).collect();
        let refs: Vec<&str> = names_v.iter().map(|s| s.as_str()).collect();
        let s = snap(&refs);

        let o = run(&s, "f000");
        let naive_o = search(&s, "f000", MatcherKind::Naive, &CancelToken::never()).unwrap();
        assert_eq!(o.matched, naive_o.matched);
        assert_eq!(o.hits, naive_o.hits);
        assert_eq!(o.total, n as u32);
    }

    #[test]
    fn a_cancelled_search_reports_itself() {
        let n = MATCH_CHUNK_ENTRIES * 4;
        let names_v: Vec<String> = (0..n).map(|i| format!("f{i:07}.dat")).collect();
        let refs: Vec<&str> = names_v.iter().map(|s| s.as_str()).collect();
        let s = snap(&refs);

        let epoch = crate::util::cancel::Epoch::new();
        let token = epoch.token(epoch.current());
        epoch.bump(); // supersede before the sweep starts

        let o = search(&s, "f00", MatcherKind::Simd, &token).unwrap();
        assert!(o.cancelled);
    }

    #[test]
    fn falls_back_to_full_unicode_for_a_query_the_fold_cannot_represent() {
        // U+212A KELVIN SIGN lowercases to 'k', a length change the
        // byte-length-preserving fold declines to apply.
        let s = snap(&["kelvin_report.pdf"]);
        let o = run(&s, "\u{212A}elvin");
        assert_eq!(o.matched, 1, "the slow path should rescue this");
        assert!(o.unicode_fallback);
    }

    #[test]
    fn ascii_queries_never_take_the_unicode_fallback() {
        let s = snap(&["nothing_here.txt"]);
        let o = run(&s, "zzz");
        assert_eq!(o.matched, 0);
        assert!(!o.unicode_fallback);
    }

    #[test]
    fn matches_ordinary_accented_names_on_the_fast_path() {
        let s = snap(&["Écoles-Été.pdf"]);
        let o = run(&s, "été");
        assert_eq!(o.matched, 1);
        assert!(!o.unicode_fallback, "length-preserving fold should suffice");
    }

    #[test]
    fn advance_to_lands_on_the_right_entry() {
        let s = snap(&["aaa", "bbbb", "c", "dd"]);
        let offsets = s.offsets();
        // Entry starts: 0, 4, 9, 11 (each name plus its separator).
        for (byte, want) in [
            (0u32, 0usize),
            (3, 0),
            (4, 1),
            (8, 1),
            (9, 2),
            (11, 3),
            (12, 3),
        ] {
            assert_eq!(advance_to(offsets, 0, byte), want, "byte {byte}");
        }
    }

    #[test]
    fn advance_to_is_monotone_from_a_warm_cursor() {
        let names_v: Vec<String> = (0..1000).map(|i| format!("n{i:04}")).collect();
        let refs: Vec<&str> = names_v.iter().map(|s| s.as_str()).collect();
        let s = snap(&refs);
        let offsets = s.offsets();
        let mut entry = 0usize;
        for i in 0..s.len() {
            let byte = offsets[i];
            entry = advance_to(offsets, entry, byte);
            assert_eq!(entry, i);
        }
    }
}
