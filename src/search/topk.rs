//! Bounded top-K selection.
//!
//! Only 15 rows are ever displayed, so sorting every match - as the previous
//! implementation did - is wasted work that scales with the match count rather
//! than the result count. A fixed sorted array beats a `BinaryHeap` decisively
//! at this size: the reject path is one comparison against a value already in
//! L1, and the accept path is a single fully-predicted `copy_within` of at
//! most 112 bytes. A heap only starts winning around K >= 64.
//!
//! # The packed key
//!
//! The ranking key is packed into one `u64` so every comparison is a single
//! integer compare:
//!
//! ```text
//! bits 63..48  match position within the name  (earlier wins)
//! bits 47..32  name length                     (shorter wins)
//! bits 31..0   entry index                     (directory order wins)
//! ```
//!
//! Including the entry index matters beyond tie-breaking. The original code
//! used `sort_by`, which is *stable*, so equal `(pos, len)` pairs kept
//! directory order. Making the key unique turns the ordering into a strict
//! total order, which means the unstable parallel merge below produces output
//! identical to the serial version instead of merely equivalent - no
//! run-to-run reshuffling of tied results.

/// Number of results retained.
pub const K: usize = crate::config::MAX_RESULTS;

/// Packed ranking key. Lower compares better.
pub type Key = u64;

/// Builds a ranking key. Position and length saturate; both are bounded well
/// below the saturation point by the snapshot builder.
#[inline]
pub fn key(match_pos: u32, name_len: u32, index: u32) -> Key {
    ((match_pos.min(0xFFFF) as u64) << 48) | ((name_len.min(0xFFFF) as u64) << 32) | (index as u64)
}

#[inline]
pub fn key_index(k: Key) -> u32 {
    k as u32
}

#[inline]
pub fn key_pos(k: Key) -> u32 {
    (k >> 48) as u32
}

#[inline]
pub fn key_name_len(k: Key) -> u32 {
    ((k >> 32) & 0xFFFF) as u32
}

/// The best `K` keys seen, kept sorted ascending.
///
/// Because it is always sorted, the final result needs no sort at all.
#[derive(Clone, Copy, Debug)]
pub struct TopK {
    buf: [Key; K],
    len: usize,
}

impl Default for TopK {
    fn default() -> Self {
        Self::new()
    }
}

impl TopK {
    pub const fn new() -> Self {
        Self {
            buf: [Key::MAX; K],
            len: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.len == K
    }

    /// The worst key currently retained, or `Key::MAX` when not yet full.
    ///
    /// Once full this is the admission threshold, and callers can use it to
    /// skip work for candidates that cannot possibly place.
    #[inline]
    pub fn threshold(&self) -> Key {
        if self.len == K {
            self.buf[K - 1]
        } else {
            Key::MAX
        }
    }

    /// Offers a candidate.
    #[inline]
    pub fn push(&mut self, k: Key) {
        // Rejects the overwhelming majority in one comparison.
        if self.len == K && k >= self.buf[K - 1] {
            return;
        }
        let at = self.buf[..self.len].partition_point(|&x| x < k);
        let end = (self.len + 1).min(K);
        if at < end - 1 {
            self.buf.copy_within(at..end - 1, at + 1);
        }
        self.buf[at] = k;
        self.len = end;
    }

    /// Merges another selection into this one.
    pub fn merge(&mut self, other: &TopK) {
        for &k in &other.buf[..other.len] {
            self.push(k);
        }
    }

    /// The retained keys, best first.
    #[inline]
    pub fn keys(&self) -> &[Key] {
        &self.buf[..self.len]
    }

    /// The retained entry indices, best first.
    pub fn indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.keys().iter().copied().map(key_index)
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
        assert_eq!(key_pos(k), 0xFFFF);
        assert_eq!(key_name_len(k), 0xFFFF);
        assert_eq!(key_index(k), 42);
    }

    #[test]
    fn keeps_the_best_k_in_sorted_order() {
        let keys: Vec<Key> = (0..100).rev().map(|i| key(i, 0, i)).collect();
        let t = collect(&keys);
        assert_eq!(t.len(), K);
        assert_eq!(t.keys(), reference(&keys).as_slice());
        assert!(
            t.keys().windows(2).all(|w| w[0] < w[1]),
            "output must be sorted"
        );
    }

    #[test]
    fn holds_fewer_than_k_without_padding() {
        let keys: Vec<Key> = (0..3).map(|i| key(i, 0, i)).collect();
        let t = collect(&keys);
        assert_eq!(t.len(), 3);
        assert_eq!(t.keys().len(), 3);
    }

    #[test]
    fn an_empty_selection_reports_no_threshold() {
        let t = TopK::new();
        assert!(t.is_empty());
        assert_eq!(t.threshold(), Key::MAX);
        assert!(t.keys().is_empty());
    }

    #[test]
    fn threshold_becomes_the_worst_retained_key_once_full() {
        let keys: Vec<Key> = (0..K as u32 * 2).map(|i| key(i, 0, i)).collect();
        let t = collect(&keys);
        assert!(t.is_full());
        assert_eq!(t.threshold(), *t.keys().last().unwrap());
    }

    #[test]
    fn insertion_order_does_not_affect_the_result() {
        let ascending: Vec<Key> = (0..60).map(|i| key(i, 0, i)).collect();
        let descending: Vec<Key> = ascending.iter().rev().copied().collect();
        let mut shuffled = ascending.clone();
        shuffled.rotate_left(17);
        shuffled.swap(0, 40);

        let want = reference(&ascending);
        for order in [&ascending, &descending, &shuffled] {
            assert_eq!(collect(order).keys(), want.as_slice());
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
                merged.merge(&collect(c));
            }
            assert_eq!(
                merged.keys(),
                reference(&all).as_slice(),
                "partition into {parts} chunks diverged"
            );
        }
    }

    #[test]
    fn merging_is_order_independent() {
        let a = collect(&(0..40u32).map(|i| key(i, 0, i)).collect::<Vec<_>>());
        let b = collect(&(20..60u32).map(|i| key(i, 0, i)).collect::<Vec<_>>());

        let mut ab = a;
        ab.merge(&b);
        let mut ba = b;
        ba.merge(&a);
        assert_eq!(ab.keys(), ba.keys());
    }

    #[test]
    fn duplicate_keys_do_not_displace_distinct_better_ones() {
        let keys: Vec<Key> = std::iter::repeat_n(key(5, 5, 5), 50)
            .chain((0..3).map(|i| key(0, 0, i)))
            .collect();
        let t = collect(&keys);
        assert_eq!(&t.keys()[..3], &[key(0, 0, 0), key(0, 0, 1), key(0, 0, 2)]);
    }
}
