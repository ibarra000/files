//! A short-lived cache of whether each configured drive path is a directory.
//!
//! The drive list warns on missing paths and redraws every frame. Each check
//! is a `stat`, which on an unreachable network drive blocks until it times
//! out, so answers are cached for [`TTL`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a cached answer stays valid. Short enough that a newly mapped
/// drive shows up promptly.
pub const TTL: Duration = Duration::from_secs(5);

/// Cached directory checks, keyed by path, with the time each was taken.
#[derive(Default, Clone)]
pub struct DirCache {
    seen: HashMap<PathBuf, (bool, Instant)>,
}

impl DirCache {
    /// Returns whether `path` is a directory, calling `probe` at most once per
    /// [`TTL`]. The probe and clock are injected so the cache is testable.
    pub fn is_dir(&mut self, path: &Path, now: Instant, probe: impl FnOnce(&Path) -> bool) -> bool {
        if let Some((answer, at)) = self.seen.get(path)
            && now.duration_since(*at) < TTL
        {
            return *answer;
        }
        let answer = probe(path);
        self.seen.insert(path.to_path_buf(), (answer, now));
        answer
    }

    /// Drops answers older than [`TTL`], so paths typed and abandoned in the
    /// add row do not accumulate.
    pub fn forget_stale(&mut self, now: Instant) {
        self.seen.retain(|_, (_, at)| now.duration_since(*at) < TTL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Returns a probe that counts its calls.
    fn counting(count: &Cell<usize>, answer: bool) -> impl FnOnce(&Path) -> bool + '_ {
        move |_| {
            count.set(count.get() + 1);
            answer
        }
    }

    #[test]
    fn asking_twice_inside_the_window_asks_the_disk_once() {
        let mut cache = DirCache::default();
        let now = Instant::now();
        let asked = Cell::new(0);
        let path = Path::new(r"R:\projects");

        assert!(cache.is_dir(path, now, counting(&asked, true)));
        assert!(cache.is_dir(path, now + TTL / 2, counting(&asked, false)));
        assert_eq!(asked.get(), 1, "the second call reached the disk");
    }

    #[test]
    fn an_answer_older_than_the_window_is_taken_again() {
        let mut cache = DirCache::default();
        let now = Instant::now();
        let asked = Cell::new(0);
        let path = Path::new(r"R:\projects");

        assert!(!cache.is_dir(path, now, counting(&asked, false)));
        assert!(cache.is_dir(path, now + TTL * 2, counting(&asked, true)));
        assert_eq!(asked.get(), 2);
    }

    #[test]
    fn two_paths_do_not_share_an_answer() {
        let mut cache = DirCache::default();
        let now = Instant::now();
        assert!(cache.is_dir(Path::new(r"R:\"), now, |_| true));
        assert!(!cache.is_dir(Path::new(r"V:\"), now, |_| false));
        assert!(cache.is_dir(Path::new(r"R:\"), now, |_| false));
    }

    #[test]
    fn paths_nobody_asks_about_are_forgotten() {
        let mut cache = DirCache::default();
        let now = Instant::now();
        for i in 0..100 {
            cache.is_dir(&PathBuf::from(format!("R:\\{i}")), now, |_| true);
        }
        assert_eq!(cache.seen.len(), 100);
        cache.forget_stale(now + TTL * 2);
        assert!(cache.seen.is_empty());
    }
}
