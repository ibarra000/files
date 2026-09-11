//! Bounded top-K selection.
//!
//! Only the retained rows are ever displayed, so sorting every match - as the
//! original implementation did - is wasted work that scales with the match
//! count rather than the result count.
//!
//! # Why a heap
//!
//! This was a fixed sorted `[Key; K]` array, which is the right structure at
//! K=15: the accept path is one fully-predicted `copy_within` of at most 112
//! bytes. That argument inverts as K grows, and the module said so - "a heap
//! only starts winning around K >= 64".
//!
//! The decisive cost is not the accept path but the *empty* one. A sweep of a
//! large index builds one selection per rayon chunk - hundreds of them - and
//! the overwhelming majority of chunks match nothing at all. An array pays to
//! fill K slots with `Key::MAX` for every one of those, then moves the whole
//! struct by value through the reduce tree. A `BinaryHeap` allocates nothing
//! until something is actually retained, so an empty chunk costs a pointer.
//!
//! The reject path - which is still where essentially every candidate goes -
//! is unchanged: one comparison against the heap root, which is the worst key
//! retained and therefore exactly the admission threshold.
//!
//! # The packed key
//!
//! The ranking key is packed into one `u64` so every comparison is a single
//! integer compare:
//!
//! ```text
//! bit  63      0 = the name matched, 1 = its folder did  (a name wins)
//! bits 62..48  match position within the name  (earlier wins)
//! bits 47..32  name length                     (shorter wins)
//! bits 31..0   entry index                     (directory order wins)
//! ```
//!
//! The top bit is what keeps a tree's results explicable. A code can match a
//! file's own name or the name of the folder it sits in, and "the file is
//! called `11-D-0704`" should always beat "the file is inside a folder called
//! `11-D-0704`" - otherwise one matching folder's entire contents outranks an
//! exactly-named file somewhere else, which is not what anyone scanning the
//! list expects. Putting it in the highest bit makes that ordering free.
//!
//! Including the entry index matters beyond tie-breaking. The original code
//! used `sort_by`, which is *stable*, so equal `(pos, len)` pairs kept
//! directory order. Making the key unique turns the ordering into a strict
//! total order, which means the unstable parallel merge below produces output
//! identical to the serial version instead of merely equivalent - no
//! run-to-run reshuffling of tied results.

use std::collections::BinaryHeap;

/// Number of results retained.
pub const K: usize = crate::config::MAX_RESULTS;

/// Packed ranking key. Lower compares better.
pub type Key = u64;

/// Largest match position the key can hold. Positions above this saturate,
/// which degrades ranking between two very deep matches and never
/// correctness - a name long enough to reach it is already unreadable.
pub const MAX_POS: u32 = 0x7FFF;

/// Set on a key whose *folder* matched rather than its own name.
const INHERITED: Key = 1 << 63;

/// Builds a ranking key for a name that matched in its own right.
///
/// Position and length saturate; both are bounded well below the saturation
/// point by the snapshot builder.
#[inline]
pub fn key(match_pos: u32, name_len: u32, index: u32) -> Key {
    ((match_pos.min(MAX_POS) as u64) << 48) | ((name_len.min(0xFFFF) as u64) << 32) | (index as u64)
}

/// Builds a ranking key for a file pulled in because its folder matched.
///
/// `match_pos` is the position within the *folder* name, so folders whose name
/// begins with the code still beat folders that merely contain it.
#[inline]
pub fn key_inherited(match_pos: u32, name_len: u32, index: u32) -> Key {
    key(match_pos, name_len, index) | INHERITED
}

/// True when this key was pulled in by its folder rather than its own name.
#[inline]
pub fn key_is_inherited(k: Key) -> bool {
    k & INHERITED != 0
}

#[inline]
pub fn key_index(k: Key) -> u32 {
    k as u32
}

#[inline]
pub fn key_pos(k: Key) -> u32 {
    ((k >> 48) & MAX_POS as u64) as u32
}

#[inline]
pub fn key_name_len(k: Key) -> u32 {
    ((k >> 32) & 0xFFFF) as u32
}

/// The best `K` keys seen.
///
/// A bounded max-heap, so the root is the *worst* key retained - which is
/// precisely the admission threshold a candidate must beat.
#[derive(Clone, Debug, Default)]
pub struct TopK {
    heap: BinaryHeap<Key>,
}

/// `push` indexes the root of a full heap, so a zero-sized selection would
/// have no threshold to compare against.
const _: () = assert!(K > 0);

impl TopK {
    pub fn new() -> Self {
        // Deliberately does not reserve: most chunks of a large sweep retain
        // nothing, and the whole point of the heap is that those cost no
        // allocation at all.
        Self {
            heap: BinaryHeap::new(),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.heap.len() == K
    }

    /// The worst key currently retained, or `Key::MAX` when not yet full.
    ///
    /// Once full this is the admission threshold, and callers can use it to
    /// skip work for candidates that cannot possibly place.
    #[inline]
    pub fn threshold(&self) -> Key {
        if self.heap.len() == K {
            *self.heap.peek().expect("a full heap has a root")
        } else {
            Key::MAX
        }
    }

    /// Offers a candidate.
    #[inline]
    pub fn push(&mut self, k: Key) {
        if self.heap.len() < K {
            self.heap.push(k);
            return;
        }
        // Rejects the overwhelming majority in one comparison. Overwriting the
        // root sifts down once; `pop` followed by `push` would sift down and
        // then back up to reach the same place.
        let mut worst = self.heap.peek_mut().expect("K > 0 and the heap is full");
        if k < *worst {
            *worst = k;
        }
    }

    /// Folds another selection into this one, consuming it.
    ///
    /// By value because rayon's `reduce` already owns the right-hand side, and
    /// pushing the smaller side into the larger halves the work. That
    /// reordering is safe: the K smallest keys of a union do not depend on
    /// which side they arrived from, which is exactly the associativity the
    /// parallel sweep relies on.
    pub fn merge(&mut self, other: TopK) {
        let mut other = other;
        if other.heap.len() > self.heap.len() {
            std::mem::swap(self, &mut other);
        }
        for k in other.heap.into_vec() {
            self.push(k);
        }
    }

    /// The retained keys, best first.
    ///
    /// Consuming, because a heap's internal order is not display order and
    /// handing back a sorted slice would mean carrying a second copy of it for
    /// the whole sweep to serve the one caller that renders.
    pub fn into_sorted(self) -> Vec<Key> {
        self.heap.into_sorted_vec()
    }

    /// The retained entry indices, best first.
    pub fn into_indices(self) -> impl Iterator<Item = u32> {
        self.into_sorted().into_iter().map(key_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(keys: &[Key]) -> TopK {
        let mut t = TopK::new();
        for &k in keys {
            t.push(k);
        }
        t
    }

    /// Reference: sort everything, take K. This is what the original code did.
    fn reference(keys: &[Key]) -> Vec<Key> {
        let mut v = keys.to_vec();
        v.sort_unstable();
        v.truncate(K);
        v
    }

    #[test]
    fn packs_and_unpacks_each_field() {
        let k = key(7, 300, 123_456);
        assert_eq!(key_pos(k), 7);
        assert_eq!(key_name_len(k), 300);
        assert_eq!(key_index(k), 123_456);
    }

    #[test]
    fn orders_by_position_then_length_then_index() {
        assert!(key(0, 99, 99) < key(1, 0, 0), "earlier position wins");
        assert!(key(3, 10, 99) < key(3, 11, 0), "then shorter name wins");
        assert!(key(3, 10, 5) < key(3, 10, 6), "then directory order wins");
    }

    #[test]
    fn saturates_rather_than_overflowing_into_another_field() {
        let k = key(u32::MAX, u32::MAX, 42);
        assert_eq!(key_pos(k), MAX_POS);
        assert_eq!(key_name_len(k), 0xFFFF);
        assert_eq!(key_index(k), 42);
        assert!(
            !key_is_inherited(k),
            "saturating the position must not spill into the source bit"
        );
    }

    /// A file that matched by name beats every file pulled in by its folder,
    /// however good the folder's match was.
    #[test]
    fn a_name_of_its_own_outranks_an_inherited_folder_match() {
        let by_name = key(0xFFFF, 0xFFFF, u32::MAX);
        let by_folder = key_inherited(0, 0, 0);
        assert!(
            by_name < by_folder,
            "the worst possible name match should still win"
        );
        assert!(key_is_inherited(by_folder));
        assert!(!key_is_inherited(by_name));
    }

    /// The other fields still decide the order within each source.
    #[test]
    fn inherited_keys_are_ordered_among_themselves() {
        assert!(key_inherited(0, 10, 5) < key_inherited(1, 0, 0), "position");
        assert!(key_inherited(3, 10, 9) < key_inherited(3, 11, 0), "length");
        assert!(key_inherited(3, 10, 5) < key_inherited(3, 10, 6), "order");
    }

    #[test]
    fn the_source_bit_does_not_disturb_the_other_fields() {
        let k = key_inherited(7, 300, 123_456);
        assert_eq!(key_pos(k), 7);
        assert_eq!(key_name_len(k), 300);
        assert_eq!(key_index(k), 123_456);
    }

    #[test]
    fn keeps_the_best_k_in_sorted_order() {
        let keys: Vec<Key> = (0..K as u32 * 4).rev().map(|i| key(i, 0, i)).collect();
        let t = collect(&keys);
        assert_eq!(t.len(), K);
        let got = t.into_sorted();
        assert_eq!(got, reference(&keys));
        assert!(got.windows(2).all(|w| w[0] < w[1]), "output must be sorted");
    }

    #[test]
    fn holds_fewer_than_k_without_padding() {
        let keys: Vec<Key> = (0..3).map(|i| key(i, 0, i)).collect();
        let t = collect(&keys);
        assert_eq!(t.len(), 3);
        assert_eq!(t.into_sorted().len(), 3);
    }

    #[test]
    fn an_empty_selection_reports_no_threshold() {
        let t = TopK::new();
        assert!(t.is_empty());
        assert_eq!(t.threshold(), Key::MAX);
        assert!(t.into_sorted().is_empty());
    }

    #[test]
    fn threshold_becomes_the_worst_retained_key_once_full() {
        let keys: Vec<Key> = (0..K as u32 * 2).map(|i| key(i, 0, i)).collect();
        let t = collect(&keys);
        assert!(t.is_full());
        let threshold = t.threshold();
        assert_eq!(threshold, *t.into_sorted().last().unwrap());
    }

    #[test]
    fn insertion_order_does_not_affect_the_result() {
        let ascending: Vec<Key> = (0..K as u32 * 4).map(|i| key(i, 0, i)).collect();
        let descending: Vec<Key> = ascending.iter().rev().copied().collect();
        let mut shuffled = ascending.clone();
        shuffled.rotate_left(17);
        shuffled.swap(0, ascending.len() - 3);

        let want = reference(&ascending);
        for order in [&ascending, &descending, &shuffled] {
            assert_eq!(collect(order).into_sorted(), want);
        }
    }

    /// The property that makes parallel chunking correct: merging per-chunk
    /// selections yields the same answer as selecting over everything at once.
    #[test]
    fn merging_partitions_equals_selecting_over_the_whole_set() {
        let all: Vec<Key> = (0..1000u32).map(|i| key(i % 37, i % 11, i)).collect();

        for parts in [1usize, 2, 3, 7, 64] {
            let chunk = all.len().div_ceil(parts);
            let mut merged = TopK::new();
            for c in all.chunks(chunk) {
                merged.merge(collect(c));
            }
            assert_eq!(
                merged.into_sorted(),
                reference(&all),
                "partition into {parts} chunks diverged"
            );
        }
    }

    #[test]
    fn merging_is_order_independent() {
        let a = collect(&(0..40u32).map(|i| key(i, 0, i)).collect::<Vec<_>>());
        let b = collect(&(20..60u32).map(|i| key(i, 0, i)).collect::<Vec<_>>());

        let mut ab = a.clone();
        ab.merge(b.clone());
        let mut ba = b;
        ba.merge(a);
        assert_eq!(ab.into_sorted(), ba.into_sorted());
    }

    /// Merging the larger side into the smaller must give the same answer,
    /// since `merge` swaps them to do less work.
    #[test]
    fn merging_a_large_selection_into_a_small_one_is_symmetric() {
        let big = collect(&(0..K as u32 * 4).map(|i| key(i, 0, i)).collect::<Vec<_>>());
        let small = collect(&[key(0, 0, 9_999), key(1, 0, 9_998)]);

        let mut a = big.clone();
        a.merge(small.clone());
        let mut b = small;
        b.merge(big);
        assert_eq!(a.into_sorted(), b.into_sorted());
    }

    #[test]
    fn duplicate_keys_do_not_displace_distinct_better_ones() {
        let keys: Vec<Key> = std::iter::repeat_n(key(5, 5, 5), K * 4)
            .chain((0..3).map(|i| key(0, 0, i)))
            .collect();
        let t = collect(&keys);
        assert_eq!(
            &t.into_sorted()[..3],
            &[key(0, 0, 0), key(0, 0, 1), key(0, 0, 2)]
        );
    }
}
