//! Which files are never shown, however well they match.
//!
//! A drawing share is not only drawings. Alongside them sit the files Windows
//! and its programs leave behind - `Thumbs.db`, `desktop.ini`, shortcuts,
//! installer leftovers - and a search for a job code returns those as readily
//! as it returns the drawing, because the index holds every name the share
//! does. What the person asked for is a drawing.
//!
//! So this is a blocklist rather than an allowlist, and that is the cautious
//! direction: an unusual but legitimate file type keeps working, and the cost
//! of a missing entry is noise rather than a file somebody cannot find.
//!
//! # Two halves, applied in two places, and why they cannot be one
//!
//! * **Extensions** are filtered at *search* time, in [`crate::search::matcher`].
//! * **The hidden and system attributes** are filtered at *index* time, in
//!   [`crate::index::enumerate`].
//!
//! That is forced rather than chosen. An extension is part of the name, and
//! the snapshot keeps every name; an attribute is not kept anywhere, because
//! [`crate::index::builder`] stores two arenas of bytes and nothing else. So
//! the attribute can only be acted on while the directory entry is still in
//! hand, which means a share shows its hidden files until it is next scanned.
//!
//! Filtering extensions at index time too would look tidier and would be a
//! trap: the on-disk cache already holds names written before this existed,
//! and the drawing tree is `refresh = "manual"`. A warm start would restore
//! the unfiltered index and keep showing `Thumbs.db` indefinitely, with no way
//! to find out why short of deleting the cache. Filtering as the results are
//! chosen is correct against every index that already exists.

/// What a fresh install hides.
///
/// Databases and scripts because they are never a drawing; `lnk` and `url`
/// because a shortcut is a pointer at the thing somebody actually wanted;
/// `ini`, `tmp` and `bak` because they are bookkeeping. `exe`, `dll` and the
/// shells are here for the same reason and one more: a share that has picked
/// up an executable named after a job code is not something to offer somebody
/// one keystroke away from opening.
///
/// `Thumbs.db` and `desktop.ini`, the two that prompted this, are covered by
/// `db` and `ini` without needing to be named.
pub const DEFAULT_HIDE_EXTENSIONS: &[&str] = &[
    "db", "js", "lnk", "ini", "tmp", "bak", "url", "exe", "dll", "bat", "cmd", "ps1", "vbs",
];

/// Whether a fresh install also hides what Windows marks hidden or system.
pub const DEFAULT_HIDE_SYSTEM_FILES: bool = true;

/// The files this configuration will not show.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hidden {
    /// Lowercased and dotted - `"db"` in the file becomes `".db"` here.
    ///
    /// Stored that way so the test is one `ends_with` rather than a split on
    /// the last dot: `Drawing.Final.db` has to be caught, and a file called
    /// plain `db` has to not be.
    suffixes: Box<[Box<str>]>,
    system: bool,
}

impl Hidden {
    pub fn new<S: AsRef<str>>(extensions: &[S], system: bool) -> Self {
        let mut suffixes: Vec<Box<str>> = extensions
            .iter()
            .map(|e| e.as_ref().trim().trim_start_matches('.'))
            // ASCII-only, and dropped rather than kept, because a non-ASCII
            // extension is one this type cannot answer consistently: `hides`
            // folds ASCII case on the original name while `hides_folded`
            // byte-compares against an arena `util::fold` has already folded
            // *including* Latin-1 and Cyrillic, so `.dÉ` would match one way
            // and not the other. The configuration file rejects these with a
            // message; this is the backstop for the environment variable,
            // which has nowhere to report to.
            .filter(|e| !e.is_empty() && e.is_ascii())
            .map(|e| format!(".{}", e.to_ascii_lowercase()).into_boxed_str())
            .collect();
        // A duplicate costs a comparison per candidate for nothing, and a
        // hand-edited list is exactly where one turns up.
        suffixes.sort_unstable();
        suffixes.dedup();
        Self {
            suffixes: suffixes.into_boxed_slice(),
            system,
        }
    }

    /// Hides nothing. For benchmarks and for the parity tests, which compare
    /// two matchers rather than two configurations.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.suffixes.is_empty()
    }

    /// Whether the hidden and system attributes are to be honoured.
    pub fn hides_system(&self) -> bool {
        self.system
    }

    /// The extensions being hidden, dotted and lowercased, for `--doctor`.
    pub fn suffixes(&self) -> impl Iterator<Item = &str> {
        self.suffixes.iter().map(|s| s.as_ref())
    }

    /// Whether a file with this name should be kept out of the results.
    pub fn hides(&self, name: &str) -> bool {
        let bytes = name.as_bytes();
        self.suffixes.iter().any(|suffix| {
            let suffix = suffix.as_bytes();
            bytes
                .len()
                .checked_sub(suffix.len())
                .is_some_and(|at| bytes[at..].eq_ignore_ascii_case(suffix))
        })
    }

    /// The same question asked of the matcher's folded arena.
    ///
    /// Exact rather than an approximation, and that rests on two things: the
    /// contract [`crate::util::fold`] states in its module note - folding is
    /// byte-length preserving and lowercases ASCII - and the fact that every
    /// suffix here *is* ASCII, which [`Self::new`] enforces rather than
    /// assumes. Given both, comparing an already-lowercased suffix against
    /// already-folded bytes gives the same answer as [`Self::hides`] would on
    /// the original name, without the second arena lookup that would cost, per
    /// candidate, in the hot loop.
    pub fn hides_folded(&self, name: &[u8]) -> bool {
        self.suffixes
            .iter()
            .any(|suffix| name.ends_with(suffix.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shipped() -> Hidden {
        Hidden::new(DEFAULT_HIDE_EXTENSIONS, DEFAULT_HIDE_SYSTEM_FILES)
    }

    #[test]
    fn the_files_that_prompted_this_are_hidden() {
        let h = shipped();
        for name in [
            "Thumbs.db",
            "desktop.ini",
            "11-D-0704.lnk",
            "tracking.js",
            "setup.exe",
        ] {
            assert!(h.hides(name), "{name} was still shown");
        }
    }

    #[test]
    fn a_drawing_is_never_hidden() {
        let h = shipped();
        for name in ["11-D-0704.pdf", "11-D-0704.dwg", "11-D-0704 rev B.PDF"] {
            assert!(!h.hides(name), "{name} disappeared");
        }
    }

    /// Windows filenames are not case sensitive and neither is this. A share
    /// that writes `THUMBS.DB` is the same share.
    #[test]
    fn the_test_ignores_case_in_both_directions() {
        let h = Hidden::new(&["DB", ".LnK"], false);
        assert!(h.hides("Thumbs.db"));
        assert!(h.hides("THUMBS.DB"));
        assert!(h.hides("a.lnk"));
    }

    /// The extension is the end of the name, not a substring of it. A folder
    /// of `db`-prefixed drawings would otherwise vanish.
    #[test]
    fn a_name_that_merely_contains_the_extension_is_kept() {
        let h = shipped();
        for name in ["db.pdf", "11-db-0704.dwg", "ini", "db", "bakery.pdf"] {
            assert!(!h.hides(name), "{name} disappeared");
        }
    }

    /// The last extension wins, not the first.
    #[test]
    fn only_the_final_extension_counts() {
        let h = shipped();
        assert!(h.hides("Drawing.pdf.bak"));
        assert!(!h.hides("Drawing.bak.pdf"));
    }

    #[test]
    fn nothing_is_hidden_by_an_empty_list() {
        let h = Hidden::none();
        assert!(h.is_empty());
        assert!(!h.hides("Thumbs.db"));
        assert!(!h.hides_folded(b"thumbs.db"));
        assert!(!h.hides_system());
    }

    /// The two answers have to agree, because one decides what the user sees
    /// and the other is what actually runs. The folded form is the name as
    /// `util::fold` would have written it into the arena.
    #[test]
    fn the_folded_test_agrees_with_the_plain_one() {
        let h = shipped();
        for name in [
            "Thumbs.db",
            "DESKTOP.INI",
            "11-D-0704.pdf",
            "db.pdf",
            "Drawing.pdf.bak",
            // Non-ASCII names still have to be judged the same way by both,
            // even though a non-ASCII *extension* can never be configured.
            "Caf\u{e9}.db",
            "CAF\u{c9}.pdf",
            "x",
            "",
        ] {
            let folded = name.to_ascii_lowercase();
            assert_eq!(
                h.hides(name),
                h.hides_folded(folded.as_bytes()),
                "{name} was judged two different ways"
            );
        }
    }

    /// Whitespace and a leading dot are both things a hand-edited config will
    /// contain, and neither should change what is hidden.
    #[test]
    fn the_list_is_normalised_however_it_was_written() {
        let h = Hidden::new(&[" .DB ", "db", "", "  ", ".lnk"], false);
        let suffixes: Vec<&str> = h.suffixes().collect();
        assert_eq!(
            suffixes,
            vec![".db", ".lnk"],
            "duplicates or blanks survived"
        );
        assert!(h.hides("Thumbs.db"));
    }

    /// Dropped rather than kept, because this type cannot answer for one
    /// consistently: `hides` folds ASCII case on the original name while
    /// `hides_folded` compares against an arena `util::fold` has already
    /// folded including Latin-1, so the two would disagree. The configuration
    /// file refuses these outright; this is what the environment variable,
    /// which has nowhere to report to, falls back on.
    #[test]
    fn a_non_ascii_extension_is_dropped_rather_than_half_working() {
        let h = Hidden::new(&["d\u{e9}", "db"], false);
        assert_eq!(h.suffixes().collect::<Vec<_>>(), vec![".db"]);
        assert!(!h.hides("drawing.d\u{e9}"));
    }

    #[test]
    fn the_system_flag_is_carried_separately_from_the_list() {
        assert!(Hidden::new::<&str>(&[], true).hides_system());
        assert!(!Hidden::new(&["db"], false).hides_system());
    }
}
