//! Whether a configured drive is actually there, asked no more than it has
//! to be.
//!
//! The drive list draws a warning on any row whose path is not a directory,
//! which is the one configuration error with no other symptom: a search over
//! a drive that is not mapped finds nothing, and a code with no files looks
//! exactly like a job with no files.
//!
//! Asking is a `stat`. On a mapped network drive that is a round trip, and
//! on an *unreachable* one it is a round trip that has to time out - the
//! expensive case being exactly the case the warning exists for. The drive
//! list is redrawn every frame the settings window is open, so with three
//! drives configured and one of them down, the page was issuing sixty
//! blocking network calls a second against a share that was not answering.
//!
//! So the answer is kept for a few seconds. Long enough that redrawing costs
//! nothing, short enough that plugging a drive in and looking at the window
//! shows the change without anybody having to know there is a cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long an answer is kept.
///
/// Five seconds. The thing being cached changes when somebody maps a drive
/// or plugs one in, which is a deliberate act followed by looking at this
/// window - so the only requirement is that the wait afterwards is shorter
/// than the time it takes to look.
pub const TTL: Duration = Duration::from_secs(5);

/// What was asked, when.
#[derive(Default, Clone)]
pub struct DirCache {
    seen: HashMap<PathBuf, (bool, Instant)>,
}

impl DirCache {
    /// Whether `path` is a directory, asking `probe` at most once per
    /// [`TTL`].
    ///
    /// Takes the probe as an argument rather than calling `Path::is_dir`
    /// directly, so the caching rule can be tested without a filesystem and
    /// without a clock that has to be waited on.
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

    /// Drops answers nobody has asked for since `TTL` ago.
    ///
    /// Without this the map grows by one entry for every path that was ever
    /// typed into the add row and then edited, and this window can be left
    /// open for hours.
    pub fn forget_stale(&mut self, now: Instant) {
        self.seen.retain(|_, (_, at)| now.duration_since(*at) < TTL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A probe that counts how many times it was asked.
    fn counting(count: &Cell<usize>, answer: bool) -> impl FnOnce(&Path) -> bool + '_ {
        move |_| {
            count.set(count.get() + 1);
            answer
        }
    }

    /// The whole point: a redraw does not cost a round trip.
    #[test]
    fn asking_twice_inside_the_window_asks_the_disk_once() {
        let mut cache = DirCache::default();
        let now = Instant::now();
        let asked = Cell::new(0);
        let path = Path::new(r"R:\jobs");

        assert!(cache.is_dir(path, now, counting(&asked, true)));
        assert!(cache.is_dir(path, now + TTL / 2, counting(&asked, false)));
        assert_eq!(asked.get(), 1, "the second call reached the disk");
    }

    /// And after the window it does, so plugging a drive in shows up.
    #[test]
    fn an_answer_older_than_the_window_is_taken_again() {
        let mut cache = DirCache::default();
        let now = Instant::now();
        let asked = Cell::new(0);
        let path = Path::new(r"R:\jobs");

        assert!(!cache.is_dir(path, now, counting(&asked, false)));
        assert!(cache.is_dir(path, now + TTL * 2, counting(&asked, true)));
        assert_eq!(asked.get(), 2);
    }

    /// Two drives are two questions.
    #[test]
    fn two_paths_do_not_share_an_answer() {
        let mut cache = DirCache::default();
        let now = Instant::now();
        assert!(cache.is_dir(Path::new(r"R:\"), now, |_| true));
        assert!(!cache.is_dir(Path::new(r"V:\"), now, |_| false));
        assert!(cache.is_dir(Path::new(r"R:\"), now, |_| false));
    }

    /// A window left open for hours does not accumulate a row per keystroke.
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
