//! Substring matching over a [`Snapshot`].
//!
//! The arena is contiguous, so one `memchr::memmem` sweep covers a whole chunk
//! of entries at SIMD speed, and byte hits are mapped back to entry indices
//! afterwards. Per-entry `str::find` is far slower because its two-way
//! searcher recomputes its factorisation on every call.
//!
//! # Ranking
//!
//! Earliest match position wins, then the shorter filename, and `matched`
//! counts entries rather than hits. `tests/matcher_parity.rs` asserts exact
//! equality, including result order, against a reference ranking.
//!
//! # Hidden files
//!
//! [`crate::config::hidden::Hidden`] drops a candidate when it becomes a hit,
//! before it enters the selection or is counted in `matched`, so the footer
//! count and the rows agree. Filtering after selection would be wrong: many
//! hidden matches could fill every slot and leave an unexplained empty list.
//!
//! # Hits dedup for free
//!
//! `memmem` yields hits in increasing byte order. On the first hit inside an
//! entry the cursor jumps past that entry's terminator, so each entry yields
//! at most one candidate and the recorded position is the leftmost, as with
//! `str::find`.

use std::sync::Arc;

use memchr::memmem;
use rayon::prelude::*;

use crate::config::hidden::Hidden;
use crate::config::{MATCH_CHUNK_ENTRIES, MAX_FILES_PER_FOLDER, MatcherKind};
use crate::index::snapshot::Snapshot;
use crate::index::tree::{TreeIndex, TreeSegment};
use crate::search::query::{Query, Sieve};
use crate::search::topk::{self, Key, TopK};
use crate::util::cancel::CancelToken;
use crate::util::fold;

/// A displayed result.
///
/// The full path is materialised rather than stored as an index, since an
/// index held across a snapshot publish would open a different file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: Arc<str>,
    pub name: Arc<str>,
    /// Byte offset of the match within the name, for highlighting.
    ///
    /// `u32::MAX` when the file was included because its folder matched; there
    /// is nothing in its own name to underline.
    pub match_pos: u32,
    pub index: u32,
}

impl Hit {
    /// True when this row is here because its folder matched, not its name.
    pub fn is_inherited(&self) -> bool {
        self.match_pos == u32::MAX
    }
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
///
/// Defined beside the parser, since every variant is a property of the
/// input; re-exported here for existing call sites.
pub use crate::search::query::QueryReject;

/// Runs a query against a snapshot.
pub fn search(
    snap: &Snapshot,
    query: &Query,
    kind: MatcherKind,
    hidden: &Hidden,
    cancel: &CancelToken,
) -> Result<SearchOutcome, QueryReject> {
    query.check()?;
    let needle = fold::fold_query(query.term());
    let sieve = Sieve::new(hidden, &needle, query);

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
        MatcherKind::Simd => sweep(snap, &needle, 0, &sieve, cancel),
        MatcherKind::Naive => naive(snap, &needle, &sieve, cancel),
    };

    // A non-ASCII query may have been folded conservatively (see
    // `util::fold`), so an empty fast-path result may be a false negative.
    // Only then is the slow, fully Unicode-correct comparison run.
    if matched == 0 && !cancelled && fold::needs_unicode_fallback(query.term()) {
        let (top, matched) = unicode_fallback(snap, query, &sieve, cancel);
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

/// Runs a query against a walked tree.
///
/// Two passes per segment, because a search term often names a folder:
///
/// 1. the filenames, swept as a flat listing is;
/// 2. the folder names, a much smaller arena, whose matches include the
///    files inside them.
///
/// A file that matched by its own name always outranks one included by its
/// folder; see [`topk`]'s key layout.
pub fn search_tree(
    index: &TreeIndex,
    query: &Query,
    hidden: &Hidden,
    cancel: &CancelToken,
) -> Result<SearchOutcome, QueryReject> {
    query.check()?;
    let needle = fold::fold_query(query.term());
    let sieve = Sieve::new(hidden, &needle, query);

    let total = index.len() as u32;
    if total == 0 {
        return Ok(SearchOutcome::default());
    }

    let mut top = TopK::new();
    let mut matched = 0u32;
    let mut cancelled = false;

    for (s, segment) in index.segments().iter().enumerate() {
        if cancel.is_cancelled() {
            cancelled = true;
            break;
        }
        let base = index.base(s);

        let (names, n, stopped) = sweep(segment.files(), &needle, base, &sieve, cancel);
        top.merge(names);
        matched = matched.saturating_add(n);
        cancelled |= stopped;

        let (folders, f) = sweep_folders(segment, &needle, base, &sieve);
        top.merge(folders);
        matched = matched.saturating_add(f);
    }

    Ok(SearchOutcome {
        hits: materialise_tree(index, &top.into_sorted()),
        matched,
        total,
        cancelled,
        unicode_fallback: false,
    })
}

/// Judges a query without consulting any index.
///
/// Both rejections are properties of the input, so they are decided once per
/// search rather than per share; with nothing indexed, per-share checks
/// would report "no matches" instead of the real reason.
pub fn check_query(query: &Query) -> Result<(), QueryReject> {
    query.check()
}

/// Folds every share's results into one ranked list.
///
/// One concat, sort and truncate instead of folding [`merge`] N-1 times.
/// Equivalent because each input is already the `MAX_RESULTS` smallest of its
/// index under the same comparator, and taking the K smallest is associative.
pub fn merge_all(parts: Vec<SearchOutcome>) -> SearchOutcome {
    // One or zero parts are already in order and need no re-sort.
    if parts.len() <= 1 {
        return parts.into_iter().next().unwrap_or_default();
    }

    let mut out = SearchOutcome {
        hits: Vec::with_capacity(parts.iter().map(|p| p.hits.len()).sum()),
        ..SearchOutcome::default()
    };
    for part in parts {
        out.hits.extend(part.hits);
        out.matched = out.matched.saturating_add(part.matched);
        out.total = out.total.saturating_add(part.total);
        out.cancelled |= part.cancelled;
        out.unicode_fallback |= part.unicode_fallback;
    }
    sort_hits(&mut out.hits);
    out.hits.truncate(crate::config::MAX_RESULTS);
    out
}

/// The cross-index ranking, over materialised rows because packed ordinals
/// are positions within different arenas and are not comparable.
fn sort_hits(hits: &mut [Hit]) {
    hits.sort_by(|a, b| {
        a.is_inherited()
            .cmp(&b.is_inherited())
            .then(a.match_pos.cmp(&b.match_pos))
            .then(a.name.len().cmp(&b.name.len()))
            .then_with(|| a.path.cmp(&b.path))
    });
}

/// Folds two searches into one list ranked by match quality, not by share.
pub fn merge(flat: SearchOutcome, tree: SearchOutcome) -> SearchOutcome {
    let mut hits = flat.hits;
    hits.extend(tree.hits);
    // Same ranking as `sort_hits`; packed ordinals are not comparable across
    // indexes.
    hits.sort_by(|a, b| {
        a.is_inherited()
            .cmp(&b.is_inherited())
            .then(a.match_pos.cmp(&b.match_pos))
            .then(a.name.len().cmp(&b.name.len()))
            .then_with(|| a.path.cmp(&b.path))
    });
    hits.truncate(crate::config::MAX_RESULTS);

    SearchOutcome {
        hits,
        matched: flat.matched.saturating_add(tree.matched),
        total: flat.total.saturating_add(tree.total),
        cancelled: flat.cancelled || tree.cancelled,
        unicode_fallback: flat.unicode_fallback || tree.unicode_fallback,
    }
}

/// Matches folder names and includes the files inside them.
///
/// Not chunked across rayon: the folder arena is about a fiftieth the size of
/// the filename arena, so the fan-out would cost more than the scan.
fn sweep_folders(segment: &TreeSegment, needle: &[u8], base: u32, sieve: &Sieve) -> (TopK, u32) {
    let mut top = TopK::new();
    let mut matched = 0u32;

    let dirs = segment.dirs();
    let offsets = dirs.offsets();
    let arena = dirs.lower();
    if dirs.is_empty() || needle.len() as u32 > dirs.max_name_len() {
        return (top, matched);
    }

    let finder = memmem::Finder::new(needle);
    let mut cursor = 0usize;
    let mut dir = 0usize;

    while let Some(rel) = finder.find(&arena[cursor..]) {
        let abs = (cursor + rel) as u32;
        dir = advance_to(offsets, dir, abs);

        let start = offsets[dir];
        let end = offsets[dir + 1];
        let name_len = end - start - 1;

        // The mode is judged against the folder, since the folder matched. A
        // refused folder contributes nothing, but the cursor still moves past it.
        let Some(pos) = sieve.admits_folder(&arena[start as usize..end as usize - 1], abs - start)
        else {
            cursor = end as usize;
            dir += 1;
            if cursor >= arena.len() || dir >= dirs.len() {
                break;
            }
            continue;
        };

        // Every file in the folder, bounded so one large folder cannot fill
        // every slot and hide other matching folders.
        let files = segment.files_of(dir as u32);
        let mut shown = 0u32;
        let mut admitted = 0u32;
        // The cap counts admitted files, not inspected ones. Capping first
        // would let non-matching files exhaust the cap under `ext:pdf`.
        for i in files.start..files.end {
            if !sieve.admits_inherited(segment.files().name_lower(i)) {
                continue;
            }
            admitted += 1;
            if shown < MAX_FILES_PER_FOLDER as u32 {
                top.push(topk::key_inherited(pos, name_len, base + i));
                shown += 1;
            }
        }
        // Files in matching folders that passed the sieve. The footer shows
        // this as "1-12 of {matched}", so it must not count filtered files.
        matched = matched.saturating_add(admitted);

        cursor = end as usize;
        dir += 1;
        if cursor >= arena.len() || dir >= dirs.len() {
            break;
        }
    }

    (top, matched)
}

/// Turns retained keys into hits, for a tree.
///
/// Re-sorted by path because a parallel walk assigns ordinals in arrival
/// order; without this, ties would rank differently between walks and break
/// the strict total order `topk` documents.
fn materialise_tree(index: &TreeIndex, keys: &[Key]) -> Vec<Hit> {
    let mut hits: Vec<(Key, Hit)> = keys
        .iter()
        .filter_map(|&k| {
            let ordinal = topk::key_index(k);
            let (s, local) = index.locate(ordinal)?;
            let segment = &index.segments()[s];
            Some((
                k,
                Hit {
                    path: Arc::from(index.full_path(ordinal)?.as_str()),
                    name: Arc::from(segment.files().display_name(local).as_ref()),
                    // An inherited hit matched in the folder name, so there is nothing
                    // in this row to underline.
                    match_pos: if topk::key_is_inherited(k) {
                        u32::MAX
                    } else {
                        topk::key_pos(k)
                    },
                    index: ordinal,
                },
            ))
        })
        .collect();

    hits.sort_by(|(ka, a), (kb, b)| {
        topk::key_is_inherited(*ka)
            .cmp(&topk::key_is_inherited(*kb))
            .then(topk::key_pos(*ka).cmp(&topk::key_pos(*kb)))
            .then(a.name.len().cmp(&b.name.len()))
            .then_with(|| a.path.cmp(&b.path))
    });
    hits.into_iter().map(|(_, h)| h).collect::<Vec<Hit>>()
}

/// Parallel SIMD sweep.
fn sweep(
    snap: &Snapshot,
    needle: &[u8],
    base: u32,
    sieve: &Sieve,
    cancel: &CancelToken,
) -> (TopK, u32, bool) {
    let offsets = snap.offsets();
    let arena = snap.lower();
    let n = snap.len();
    let chunks = n.div_ceil(MATCH_CHUNK_ENTRIES);

    // Below a chunk's worth of entries the rayon fan-out costs more than the
    // scan it parallelises.
    if chunks <= 1 {
        let (top, matched) = scan_chunk(arena, offsets, needle, 0, n, base, sieve);
        return (top, matched, cancel.is_cancelled());
    }

    let finder_needle = needle.to_vec();
    let (top, matched, cancelled) = (0..chunks)
        .into_par_iter()
        .map(|c| {
            // One check per chunk. At roughly 160 us of work per chunk this
            // bounds cancellation latency without a branch in the vectorised scan.
            if cancel.is_cancelled() {
                return (TopK::new(), 0u32, true);
            }
            let lo = c * MATCH_CHUNK_ENTRIES;
            let hi = ((c + 1) * MATCH_CHUNK_ENTRIES).min(n);
            let (top, matched) = scan_chunk(arena, offsets, &finder_needle, lo, hi, base, sieve);
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
///
/// `base` is added to every entry index, so a tree segment contributes
/// global ordinals while a flat listing passes zero.
fn scan_chunk(
    arena: &[u8],
    offsets: &[u32],
    needle: &[u8],
    lo: usize,
    hi: usize,
    base: u32,
    sieve: &Sieve,
) -> (TopK, u32) {
    let mut top = TopK::new();
    let mut matched = 0u32;
    if lo >= hi {
        return (top, matched);
    }

    // Chunk boundaries are snapped to entry boundaries, so this slice holds
    // entries lo..hi whole. Splitting mid-entry would lose a match at the seam
    // and compute a wrong position for the fragment.
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

        // Judged on the folded bytes, which gives the same answer as the
        // original name (see `Hidden::hides_folded`). Skipping `matched` as well
        // as the push keeps the count consistent with the rows.
        //
        // A refused row never reaches `TopK`, so it never occupies a slot or
        // raises the admission threshold. Filtering after selection would return
        // only the survivors among the best K.
        if let Some(at) = sieve.admits(&arena[start as usize..end as usize - 1], pos) {
            top.push(topk::key(at, name_len, base + entry as u32));
            matched += 1;
        }

        // Jump past this entry entirely. This makes `matched` count entries
        // rather than hits and guarantees `pos` is the leftmost occurrence.
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
/// Hit offsets and the entry cursor both advance monotonically, so this
/// gallops forward instead of binary searching from the root: O(1) for dense
/// hits, O(log gap) for sparse ones.
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

/// Straightforward per-entry search, kept so a regression in the SIMD path
/// can be bypassed with a configuration flag.
fn naive(snap: &Snapshot, needle: &[u8], sieve: &Sieve, cancel: &CancelToken) -> (TopK, u32, bool) {
    let mut top = TopK::new();
    let mut matched = 0u32;
    for i in 0..snap.len() as u32 {
        if i % 4096 == 0 && cancel.is_cancelled() {
            return (top, matched, true);
        }
        let name = snap.name_lower(i);
        if let Some(pos) = memmem::find(name, needle)
            && let Some(at) = sieve.admits(name, pos as u32)
        {
            top.push(topk::key(at, name.len() as u32, i));
            matched += 1;
        }
    }
    (top, matched, false)
}

/// Fully Unicode-correct comparison, for the rare query the length-preserving
/// fold cannot represent.
fn unicode_fallback(
    snap: &Snapshot,
    query: &Query,
    sieve: &Sieve,
    cancel: &CancelToken,
) -> (TopK, u32) {
    let mut top = TopK::new();
    let mut matched = 0u32;
    let needle = query.term().to_lowercase();
    for i in 0..snap.len() as u32 {
        if i % 4096 == 0 && cancel.is_cancelled() {
            break;
        }
        let name = String::from_utf8_lossy(snap.name_orig(i));
        let lower = name.to_lowercase();
        if let Some(pos) = lower.find(&needle)
            && let Some(at) = sieve.admits_lossy(&name, &lower, &needle, pos as u32)
        {
            top.push(topk::key(at, name.len() as u32, i));
            matched += 1;
        }
    }
    (top, matched)
}

/// Turns retained keys into displayable hits.
///
/// Takes already-sorted keys because the heap's internal order is not display
/// order.
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
        search(
            s,
            &Query::contains(q),
            MatcherKind::Simd,
            &Hidden::none(),
            &CancelToken::never(),
        )
        .unwrap()
    }

    fn names(o: &SearchOutcome) -> Vec<String> {
        o.hits.iter().map(|h| h.name.to_string()).collect()
    }

    fn hiding(exts: &[&str]) -> Hidden {
        Hidden::new(exts, false)
    }

    fn run_hiding(s: &Snapshot, q: &str, hidden: &Hidden) -> SearchOutcome {
        search(
            s,
            &Query::contains(q),
            MatcherKind::Simd,
            hidden,
            &CancelToken::never(),
        )
        .unwrap()
    }

    // --- narrowing the query ------------------------------------------------

    fn ask(s: &Snapshot, line: &str) -> SearchOutcome {
        search(
            s,
            &Query::parse(line),
            MatcherKind::Simd,
            &Hidden::none(),
            &CancelToken::never(),
        )
        .unwrap()
    }

    #[test]
    fn a_trailing_star_keeps_only_the_names_that_start_with_the_term() {
        let s = snap(&["0704-plan.pdf", "project-0704.pdf", "0704.pdf"]);
        assert_eq!(names(&ask(&s, "0704*")), ["0704.pdf", "0704-plan.pdf"]);
    }

    /// The sweep records only the leftmost occurrence per entry; the trailing
    /// form must still admit a stem that ends in the search term.
    #[test]
    fn a_leading_star_finds_a_name_whose_first_occurrence_is_not_the_last() {
        let s = snap(&["0704-rev-0704.pdf", "0704-plan.pdf"]);
        assert_eq!(names(&ask(&s, "*0704")), ["0704-rev-0704.pdf"]);
    }

    #[test]
    fn a_type_filter_keeps_only_that_type() {
        let s = snap(&["AB-C-0704.pdf", "AB-C-0704.tif", "AB-C-0704.docx"]);
        assert_eq!(names(&ask(&s, "AB-C-0704 ext:pdf")), ["AB-C-0704.pdf"]);
        let both = ask(&s, "AB-C-0704 ext:pdf,tif");
        assert_eq!(both.matched, 2);
    }

    /// Four hundred refused names outrank the kept ones. The filter must run
    /// before selection, or only a handful of rows would survive.
    #[test]
    fn a_refused_row_never_occupies_a_slot_in_the_selection() {
        let mut names_in: Vec<String> = (0..400).map(|i| format!("0704-{i:04}.tif")).collect();
        names_in.extend((0..400).map(|i| format!("zz-0704-{i:04}.pdf")));
        let refs: Vec<&str> = names_in.iter().map(String::as_str).collect();
        let s = snap(&refs);

        let out = ask(&s, "0704 ext:pdf");
        assert_eq!(out.matched, 400, "every PDF matched");
        assert_eq!(out.hits.len(), crate::config::MAX_RESULTS);
        assert!(
            out.hits.iter().all(|h| h.name.ends_with(".pdf")),
            "a refused row reached the selection"
        );
    }

    /// `view::status::found` renders this as the footer count, so it must not
    /// include filtered rows.
    #[test]
    fn the_match_count_counts_what_survived_the_filter() {
        let s = snap(&["a-0704.pdf", "b-0704.tif", "c-0704.tif"]);
        assert_eq!(ask(&s, "0704").matched, 3);
        assert_eq!(ask(&s, "0704 ext:pdf").matched, 1);
    }

    /// The total is unchanged by the filter. `view::empty` renders it as
    /// "Searched N files".
    #[test]
    fn the_total_is_the_whole_index_whatever_the_filter() {
        let s = snap(&["a-0704.pdf", "b-0704.tif", "unrelated.txt"]);
        assert_eq!(ask(&s, "0704 ext:pdf").total, 3);
    }

    #[test]
    fn a_narrowed_query_still_honours_the_hidden_list() {
        let s = snap(&["0704.pdf", "0704.db"]);
        let out = search(
            &s,
            &Query::parse("0704*"),
            MatcherKind::Simd,
            &hiding(&["db"]),
            &CancelToken::never(),
        )
        .unwrap();
        assert_eq!(names(&out), ["0704.pdf"]);
    }

    #[test]
    fn the_two_matchers_agree_under_every_mode_and_filter() {
        let s = snap(&[
            "0704.pdf",
            "0704-plan.tif",
            "plan-0704.pdf",
            "0704-rev-0704.tif",
        ]);
        for line in ["0704", "0704*", "*0704", "0704 ext:pdf", "*0704 ext:tif"] {
            let q = Query::parse(line);
            let simd = search(
                &s,
                &q,
                MatcherKind::Simd,
                &Hidden::none(),
                &CancelToken::never(),
            )
            .unwrap();
            let naive = search(
                &s,
                &q,
                MatcherKind::Naive,
                &Hidden::none(),
                &CancelToken::never(),
            )
            .unwrap();
            assert_eq!(simd, naive, "{line:?}");
        }
    }

    // --- hidden files -------------------------------------------------------

    #[test]
    fn a_hidden_extension_is_left_out_of_the_results() {
        let s = snap(&["AB-C-0704.pdf", "AB-C-0704.db", "AB-C-0704.lnk"]);
        assert_eq!(
            names(&run_hiding(&s, "AB-C-0704", &hiding(&["db", "lnk"]))),
            vec!["AB-C-0704.pdf"]
        );
    }

    /// The footer count and the rows on screen agree.
    #[test]
    fn a_hidden_file_is_not_counted_as_a_match_either() {
        let s = snap(&["AB-C-0704.pdf", "AB-C-0704.db", "AB-C-0704.lnk"]);
        let o = run_hiding(&s, "AB-C-0704", &hiding(&["db", "lnk"]));
        assert_eq!(o.matched, 1, "the footer would disagree with the list");
        assert_eq!(o.total, 3, "the index still holds all three");
    }

    /// Hidden files are filtered during selection, so they cannot fill the cap.
    #[test]
    fn a_wanted_file_is_found_behind_a_crowd_of_hidden_ones() {
        let mut owned: Vec<String> = (0..500).map(|i| format!("AB-C-0704 ({i}).db")).collect();
        owned.push("AB-C-0704.pdf".to_string());
        let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        let s = snap(&refs);

        let o = run_hiding(&s, "AB-C-0704", &hiding(&["db"]));
        assert_eq!(names(&o), vec!["AB-C-0704.pdf"]);
        assert_eq!(o.matched, 1);
    }

    /// Case does not bypass the filter, in either direction.
    #[test]
    fn the_filter_ignores_case_the_way_windows_does() {
        let s = snap(&["Thumbs.DB", "THUMBS.db", "thumbs.pdf"]);
        assert_eq!(
            names(&run_hiding(&s, "thumbs", &hiding(&["DB"]))),
            vec!["thumbs.pdf"]
        );
    }

    /// Both matchers apply the filter, since `--bench` switches between them.
    #[test]
    fn both_matchers_hide_the_same_files() {
        let s = snap(&["AB-C-0704.pdf", "AB-C-0704.db"]);
        let hidden = hiding(&["db"]);
        let simd = search(
            &s,
            &Query::contains("AB-C"),
            MatcherKind::Simd,
            &hidden,
            &CancelToken::never(),
        )
        .unwrap();
        let naive = search(
            &s,
            &Query::contains("AB-C"),
            MatcherKind::Naive,
            &hidden,
            &CancelToken::never(),
        )
        .unwrap();
        assert_eq!(names(&simd), names(&naive));
        assert_eq!(simd.matched, naive.matched);
    }

    /// The non-ASCII slow path reads the original arena rather than the folded
    /// one, so it is filtered separately.
    #[test]
    fn the_unicode_fallback_hides_the_same_files() {
        let s = snap(&["Cafe\u{301}.pdf", "Cafe\u{301}.db"]);
        let o = run_hiding(&s, "Caf\u{e9}", &hiding(&["db"]));
        for name in names(&o) {
            assert!(
                !name.ends_with(".db"),
                "{name} came back through the fallback"
            );
        }
    }

    #[test]
    fn nothing_is_hidden_when_the_list_is_empty() {
        let s = snap(&["AB-C-0704.pdf", "AB-C-0704.db"]);
        assert_eq!(run_hiding(&s, "AB-C", &Hidden::none()).matched, 2);
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
        let n = topk::K * 2;
        let names_v: Vec<String> = (0..n).map(|i| format!("abc{i:05}")).collect();
        let refs: Vec<&str> = names_v.iter().map(|s| s.as_str()).collect();
        let s = snap(&refs);
        let o = run(&s, "abc");
        assert_eq!(o.hits.len(), topk::K, "retained is capped");
        assert_eq!(o.matched as usize, n, "but the match count is the truth");
        assert_eq!(o.total as usize, n);
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
            search(
                &s,
                &Query::contains(""),
                MatcherKind::Simd,
                &Hidden::none(),
                &CancelToken::never()
            ),
            Err(QueryReject::Empty)
        );
    }

    #[test]
    fn rejects_rather_than_strips_an_embedded_nul() {
        let s = snap(&["abcd"]);
        assert_eq!(
            search(
                &s,
                &Query::contains("ab\0cd"),
                MatcherKind::Simd,
                &Hidden::none(),
                &CancelToken::never()
            ),
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
            .map(|i| format!("item_{i:04}_report.pdf"))
            .collect();
        let refs: Vec<&str> = names_v.iter().map(|s| s.as_str()).collect();
        let s = snap(&refs);
        for q in ["item", "report", "_012", "0999", "zzz", ".pdf"] {
            let a = search(
                &s,
                &Query::contains(q),
                MatcherKind::Simd,
                &Hidden::none(),
                &CancelToken::never(),
            )
            .unwrap();
            let b = search(
                &s,
                &Query::contains(q),
                MatcherKind::Naive,
                &Hidden::none(),
                &CancelToken::never(),
            )
            .unwrap();
            assert_eq!(a.hits, b.hits, "hits diverged for {q:?}");
            assert_eq!(a.matched, b.matched, "count diverged for {q:?}");
        }
    }

    #[test]
    fn works_across_many_rayon_chunks() {
        // More than one chunk, so the parallel merge is exercised.
        let n = MATCH_CHUNK_ENTRIES * 3 + 17;
        let names_v: Vec<String> = (0..n).map(|i| format!("f{i:07}.dat")).collect();
        let refs: Vec<&str> = names_v.iter().map(|s| s.as_str()).collect();
        let s = snap(&refs);

        let o = run(&s, "f000");
        let naive_o = search(
            &s,
            &Query::contains("f000"),
            MatcherKind::Naive,
            &Hidden::none(),
            &CancelToken::never(),
        )
        .unwrap();
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

        let o = search(
            &s,
            &Query::contains("f00"),
            MatcherKind::Simd,
            &Hidden::none(),
            &token,
        )
        .unwrap();
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

#[cfg(test)]
mod tree_tests {
    use super::*;
    use crate::index::tree::{SegmentBuilder, TreeIndex, TreeSegment};
    use std::sync::Arc;

    fn segment(dirs: &[(&str, &[&str])]) -> Arc<TreeSegment> {
        let mut b = SegmentBuilder::new();
        for (rel, files) in dirs {
            let owned: Vec<String> = files.iter().map(|f| f.to_string()).collect();
            assert!(b.push_dir(rel, &owned));
        }
        let s = b.seal();
        s.check_invariants().unwrap();
        Arc::new(s)
    }

    fn index(dirs: &[(&str, &[&str])]) -> TreeIndex {
        TreeIndex::empty("R:\\").appended(segment(dirs))
    }

    fn run(index: &TreeIndex, q: &str) -> SearchOutcome {
        search_tree(
            index,
            &Query::contains(q),
            &Hidden::none(),
            &CancelToken::never(),
        )
        .unwrap()
    }

    fn paths(o: &SearchOutcome) -> Vec<String> {
        o.hits.iter().map(|h| h.path.to_string()).collect()
    }

    fn run_hiding(index: &TreeIndex, q: &str, exts: &[&str]) -> SearchOutcome {
        search_tree(
            index,
            &Query::contains(q),
            &Hidden::new(exts, false),
            &CancelToken::never(),
        )
        .unwrap()
    }

    // --- narrowing the query ------------------------------------------------

    fn ask(index: &TreeIndex, line: &str) -> SearchOutcome {
        search_tree(
            index,
            &Query::parse(line),
            &Hidden::none(),
            &CancelToken::never(),
        )
        .unwrap()
    }

    /// A folder of five thousand scans and ten PDFs contributes all ten under
    /// `ext:pdf`, because the cap counts admitted files.
    #[test]
    fn the_folder_cap_counts_the_files_it_admitted_not_the_ones_it_looked_at() {
        let mut files: Vec<String> = (0..MAX_FILES_PER_FOLDER + 20)
            .map(|i| format!("scan-{i:03}.tif"))
            .collect();
        files.push("the-one.pdf".into());
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let ix = index(&[("project-0704", &refs)]);

        let out = ask(&ix, "0704 ext:pdf");
        assert_eq!(
            paths(&out),
            ["R:\\project-0704\\the-one.pdf"],
            "the PDF was crowded out by scans the filter had already refused"
        );
        assert_eq!(out.matched, 1);
    }

    /// The file's own name did not match; the folder did. The mode is not
    /// applied to the file.
    #[test]
    fn a_mode_applies_to_the_folder_that_matched_and_not_to_its_files() {
        let ix = index(&[("0704-project", &["anything at all.pdf"])]);
        assert_eq!(paths(&ask(&ix, "0704*")).len(), 1);
        // The same folder, asked for as a trailing match, is not a match.
        assert!(paths(&ask(&ix, "*0704")).is_empty());
    }

    #[test]
    fn a_folder_match_still_honours_the_type_filter_on_what_it_brings_in() {
        let ix = index(&[("project-0704", &["a.pdf", "b.tif", "c.pdf"])]);
        let out = ask(&ix, "0704 ext:pdf");
        assert_eq!(
            paths(&out),
            ["R:\\project-0704\\a.pdf", "R:\\project-0704\\c.pdf"]
        );
        assert_eq!(out.matched, 2);
    }

    #[test]
    fn a_folder_holding_nothing_of_the_asked_for_type_contributes_nothing() {
        let ix = index(&[("project-0704", &["a.tif", "b.tif"])]);
        let out = ask(&ix, "0704 ext:pdf");
        assert!(paths(&out).is_empty());
        assert_eq!(out.matched, 0);
    }

    /// A folder has no extension, so the whole of its name is its stem.
    #[test]
    fn a_folder_name_is_never_split_on_its_dots() {
        let ix = index(&[("11.5 revisions", &["plan.pdf"])]);
        assert_eq!(paths(&ask(&ix, "*revisions")).len(), 1);
    }

    #[test]
    fn a_hidden_file_is_left_out_when_its_own_name_matched() {
        let ix = index(&[("projects", &["AB-C-0704.pdf", "AB-C-0704.db"])]);
        assert_eq!(
            paths(&run_hiding(&ix, "AB-C-0704", &["db"])),
            vec!["R:\\projects\\AB-C-0704.pdf"]
        );
    }

    /// The folder pass is a separate route into the results and must also hide
    /// files such as `Thumbs.db`.
    #[test]
    fn a_hidden_file_pulled_in_by_its_folder_is_left_out_too() {
        let ix = index(&[("AB-C-0704", &["sheet 1.pdf", "Thumbs.db", "desktop.ini"])]);
        assert_eq!(
            paths(&run_hiding(&ix, "AB-C-0704", &["db", "ini"])),
            vec!["R:\\AB-C-0704\\sheet 1.pdf"]
        );
    }

    /// A folder is judged by its own name, never by the hidden-extension filter.
    #[test]
    fn the_filter_applies_to_files_rather_than_the_folders_holding_them() {
        let ix = index(&[("AB-C-0704.db", &["sheet 1.pdf"])]);
        assert_eq!(
            paths(&run_hiding(&ix, "AB-C-0704", &["db"])),
            vec!["R:\\AB-C-0704.db\\sheet 1.pdf"]
        );
    }

    /// A file is reachable through a folder whose name matches the search term.
    #[test]
    fn finds_a_file_in_a_folder_no_rule_would_have_guessed() {
        let ix = index(&[(
            "archive\\2019\\odd name",
            &["AB-3-0704 survey.pdf", "unrelated.txt"],
        )]);
        assert_eq!(
            paths(&run(&ix, "AB-3-0704")),
            vec!["R:\\archive\\2019\\odd name\\AB-3-0704 survey.pdf"]
        );
    }

    /// Matching a folder brings its contents with it.
    #[test]
    fn a_folder_match_brings_in_the_files_inside_it() {
        let ix = index(&[
            ("abc\\0704", &["quote.pdf", "photo.pdf"]),
            ("ab12", &["unrelated.pdf"]),
        ]);
        let got = paths(&run(&ix, "0704"));
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.contains(&"R:\\abc\\0704\\quote.pdf".to_string()));
        assert!(got.contains(&"R:\\abc\\0704\\photo.pdf".to_string()));
    }

    /// A file named after the search term beats a file inside a folder named after it.
    #[test]
    fn a_file_named_for_the_term_outranks_the_folder_named_for_it() {
        let ix = index(&[
            ("abc\\0704", &["aaa.pdf", "bbb.pdf"]),
            ("elsewhere", &["0704 summary.pdf"]),
        ]);
        assert_eq!(
            paths(&run(&ix, "0704"))[0],
            "R:\\elsewhere\\0704 summary.pdf"
        );
    }

    /// One large folder must not fill every slot and hide the others.
    #[test]
    fn one_huge_folder_cannot_crowd_out_every_other_match() {
        let many: Vec<String> = (0..500).map(|i| format!("f{i:04}.pdf")).collect();
        let many: Vec<&str> = many.iter().map(|s| s.as_str()).collect();
        let ix = index(&[("0704 big", &many), ("0704 small", &["only.pdf"])]);

        let got = paths(&run(&ix, "0704"));
        assert!(
            got.iter().any(|p| p.ends_with("0704 small\\only.pdf")),
            "the small folder was crowded out of {} hits",
            got.len()
        );
        assert!(
            got.len() <= crate::config::MAX_FILES_PER_FOLDER + 1,
            "the big folder contributed {} rows",
            got.len()
        );
    }

    /// `matched` counts every file the search reaches, even when only some are
    /// retained.
    #[test]
    fn the_match_count_is_not_capped_by_what_is_shown() {
        let many: Vec<String> = (0..500).map(|i| format!("f{i:04}.pdf")).collect();
        let many: Vec<&str> = many.iter().map(|s| s.as_str()).collect();
        let ix = index(&[("0704 big", &many)]);
        assert_eq!(run(&ix, "0704").matched, 500);
    }

    #[test]
    fn results_span_segments() {
        let ix = TreeIndex::empty("R:\\")
            .appended(segment(&[("a", &["0704 one.pdf"])]))
            .appended(segment(&[("b", &["0704 two.pdf"])]));

        let got = paths(&run(&ix, "0704"));
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.contains(&"R:\\a\\0704 one.pdf".to_string()));
        assert!(got.contains(&"R:\\b\\0704 two.pdf".to_string()));
    }

    /// The same files split across segments differently rank identically, which
    /// depends on the re-sort in `materialise_tree`.
    #[test]
    fn ranking_does_not_depend_on_how_the_walk_was_segmented() {
        let whole = index(&[
            ("a", &["0704 one.pdf", "0704 two.pdf"]),
            ("b", &["0704 three.pdf"]),
        ]);
        let split = TreeIndex::empty("R:\\")
            .appended(segment(&[("a", &["0704 one.pdf", "0704 two.pdf"])]))
            .appended(segment(&[("b", &["0704 three.pdf"])]));

        assert_eq!(paths(&run(&whole, "0704")), paths(&run(&split, "0704")));
    }

    #[test]
    fn an_empty_folder_that_matches_contributes_nothing() {
        let ix = index(&[("0704 empty", &[]), ("other", &["keep.pdf"])]);
        assert!(paths(&run(&ix, "0704")).is_empty());
    }

    #[test]
    fn an_empty_index_matches_nothing_without_panicking() {
        let ix = TreeIndex::empty("R:\\");
        let o = run(&ix, "0704");
        assert!(o.hits.is_empty());
        assert_eq!(o.total, 0);
    }

    #[test]
    fn a_short_query_is_rejected_rather_than_run() {
        let ix = index(&[("a", &["one.pdf"])]);
        assert!(matches!(
            search_tree(
                &ix,
                &Query::contains(""),
                &Hidden::none(),
                &CancelToken::never()
            ),
            Err(QueryReject::Empty)
        ));
    }

    #[test]
    fn matching_is_case_insensitive_over_both_names_and_folders() {
        let ix = index(&[("ABC\\0704", &["QUOTE.pdf"])]);
        assert_eq!(run(&ix, "abc").hits.len(), 1, "the folder");
        assert_eq!(run(&ix, "quote").hits.len(), 1, "the file");
    }

    /// A file included by its folder has no position to underline.
    #[test]
    fn an_inherited_hit_carries_no_highlight() {
        let ix = index(&[("0704", &["quote.pdf"])]);
        assert_eq!(run(&ix, "0704").hits[0].match_pos, u32::MAX);
    }
    // --- merging N shares -----------------------------------------------

    fn outcome(names: &[(&str, u32)]) -> SearchOutcome {
        let hits: Vec<Hit> = names
            .iter()
            .enumerate()
            .map(|(i, (name, pos))| Hit {
                path: format!("X:\\{name}").into(),
                name: (*name).into(),
                match_pos: *pos,
                index: i as u32,
            })
            .collect();
        SearchOutcome {
            matched: hits.len() as u32,
            total: hits.len() as u32,
            hits,
            cancelled: false,
            unicode_fallback: false,
        }
    }

    #[test]
    fn merge_all_sums_the_counts_of_every_share() {
        let out = merge_all(vec![outcome(&[("a", 0)]), outcome(&[("b", 0), ("c", 0)])]);
        assert_eq!(out.matched, 3);
        assert_eq!(out.total, 3);
        assert_eq!(out.hits.len(), 3);
    }

    #[test]
    fn merge_all_of_nothing_is_an_empty_answer() {
        let out = merge_all(Vec::new());
        assert_eq!(out.hits.len(), 0);
        assert_eq!(out.matched, 0);
        assert!(!out.cancelled);
    }

    #[test]
    fn merge_all_of_one_share_returns_it_untouched() {
        let one = outcome(&[("b", 1), ("a", 0)]);
        let before = one.hits.clone();
        assert_eq!(merge_all(vec![one]).hits, before);
    }

    #[test]
    fn a_cancelled_share_cancels_the_merged_answer() {
        let mut a = outcome(&[("a", 0)]);
        a.cancelled = true;
        let out = merge_all(vec![a, outcome(&[("b", 0)])]);
        assert!(out.cancelled, "a partial answer must not look complete");
    }

    /// Taking the K smallest is associative, so one concat-and-sort equals
    /// folding the pairwise merge.
    #[test]
    fn merge_all_agrees_with_folding_the_pairwise_merge() {
        let parts = vec![
            outcome(&[("delta", 3), ("alpha", 0)]),
            outcome(&[("echo", 1), ("bravo", 2)]),
            outcome(&[("charlie", 0), ("foxtrot", 4)]),
        ];

        let folded = parts
            .clone()
            .into_iter()
            .reduce(merge)
            .expect("three parts");
        let at_once = merge_all(parts);

        let names = |o: &SearchOutcome| -> Vec<String> {
            o.hits.iter().map(|h| h.name.to_string()).collect()
        };
        assert_eq!(names(&at_once), names(&folded));
        assert_eq!(at_once.matched, folded.matched);
        assert_eq!(at_once.total, folded.total);
    }

    /// The merged list is capped however many shares contributed.
    #[test]
    fn merge_all_caps_the_merged_list() {
        let big: Vec<(&str, u32)> = vec![("a", 0); crate::config::MAX_RESULTS];
        let parts = vec![outcome(&big), outcome(&big), outcome(&big)];
        assert_eq!(merge_all(parts).hits.len(), crate::config::MAX_RESULTS);
    }

    // --- the query is judged once, not per share -------------------------

    #[test]
    fn check_query_rejects_an_empty_query_before_any_index_is_consulted() {
        assert!(matches!(
            check_query(&Query::contains("")),
            Err(QueryReject::Empty)
        ));
        assert!(check_query(&Query::contains("a")).is_ok());
    }

    /// With nothing indexed, an empty query reports that there is nothing to
    /// search for rather than "no matches".
    #[test]
    fn an_empty_query_is_rejected_even_with_no_shares_indexed() {
        assert!(check_query(&Query::contains("")).is_err());
        assert_eq!(merge_all(Vec::new()).hits.len(), 0);
    }
}
