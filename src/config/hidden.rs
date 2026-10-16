//! The blocklist of files excluded from results, such as `Thumbs.db`,
//! `desktop.ini` and shortcuts.
//!
//! A blocklist keeps unusual but legitimate file types searchable; a missing
//! entry costs only noise.
//!
//! # Where each half is applied
//!
//! * **Extensions** are filtered at search time, in [`crate::search::matcher`].
//! * **Hidden and system attributes** are filtered at index time, in
//!   [`crate::index::enumerate`].
//!
//! Attributes are not stored in the snapshot ([`crate::index::builder`] keeps
//! only name arenas), so they can only be checked during enumeration; a share
//! shows its hidden files until it is next scanned. Extensions must stay a
//! search-time filter: a cached index, or a `refresh = "manual"` tree, may hold
//! names written before the filter applied and would otherwise never be
//! cleaned.

/// The extensions a fresh install hides.
///
/// Covers databases, scripts, shortcuts (`lnk`, `url`), bookkeeping files
/// (`ini`, `tmp`, `bak`) and executables, which should not be one keystroke
/// from opening. `db` and `ini` cover `Thumbs.db` and `desktop.ini`.
pub const DEFAULT_HIDE_EXTENSIONS: &[&str] = &[
    "db", "js", "lnk", "ini", "tmp", "bak", "url", "exe", "dll", "bat", "cmd", "ps1", "vbs",
];

/// Whether a fresh install also hides what Windows marks hidden or system.
pub const DEFAULT_HIDE_SYSTEM_FILES: bool = true;

/// The configured set of files excluded from results.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hidden {
    /// Lowercase suffixes with a leading dot (`"db"` becomes `".db"`).
    ///
    /// The dot makes a single `ends_with` correct: `Report.Final.db` matches
    /// and a file named plain `db` does not.
    suffixes: Box<[Box<str>]>,
    system: bool,
}

impl Hidden {
    pub fn new<S: AsRef<str>>(extensions: &[S], system: bool) -> Self {
        let mut suffixes: Vec<Box<str>> = extensions
            .iter()
            .map(|e| e.as_ref().trim().trim_start_matches('.'))
            // Non-ASCII extensions are dropped: `hides` folds only ASCII case,
            // while `util::fold` also folds Latin-1 and Cyrillic, so the two
            // checks would disagree. The config file rejects these with an
            // error; this is the backstop for the environment variable.
            .filter(|e| !e.is_empty() && e.is_ascii())
            .map(|e| format!(".{}", e.to_ascii_lowercase()).into_boxed_str())
            .collect();
        // Duplicates would add a wasted comparison per candidate.
        suffixes.sort_unstable();
        suffixes.dedup();
        Self {
            suffixes: suffixes.into_boxed_slice(),
            system,
        }
    }

    /// Returns a set that hides nothing, for benchmarks and parity tests.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.suffixes.is_empty()
    }

    /// Whether files with the hidden or system attribute are excluded.
    pub fn hides_system(&self) -> bool {
        self.system
    }

    /// The extensions being hidden, dotted and lowercased, for `--doctor`.
    pub fn suffixes(&self) -> impl Iterator<Item = &str> {
        self.suffixes.iter().map(|s| s.as_ref())
    }

    /// Whether a file with this name is excluded from results.
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

    /// Like [`Self::hides`], but on a name from the matcher's folded arena.
    ///
    /// Gives the same answer as `hides` because [`crate::util::fold`] preserves
    /// byte length and lowercases ASCII, and [`Self::new`] keeps only ASCII
    /// suffixes. Avoids a second arena lookup per candidate in the hot loop.
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
            "AB-C-0704.lnk",
            "tracking.js",
            "setup.exe",
        ] {
            assert!(h.hides(name), "{name} was still shown");
        }
    }

    #[test]
    fn a_document_is_never_hidden() {
        let h = shipped();
        for name in ["AB-C-0704.pdf", "AB-C-0704.tif", "AB-C-0704 rev B.PDF"] {
            assert!(!h.hides(name), "{name} disappeared");
        }
    }

    #[test]
    fn the_test_ignores_case_in_both_directions() {
        let h = Hidden::new(&["DB", ".LnK"], false);
        assert!(h.hides("Thumbs.db"));
        assert!(h.hides("THUMBS.DB"));
        assert!(h.hides("a.lnk"));
    }

    #[test]
    fn a_name_that_merely_contains_the_extension_is_kept() {
        let h = shipped();
        for name in ["db.pdf", "ab-db-0704.tif", "ini", "db", "bakery.pdf"] {
            assert!(!h.hides(name), "{name} disappeared");
        }
    }

    #[test]
    fn only_the_final_extension_counts() {
        let h = shipped();
        assert!(h.hides("Report.pdf.bak"));
        assert!(!h.hides("Report.bak.pdf"));
    }

    #[test]
    fn nothing_is_hidden_by_an_empty_list() {
        let h = Hidden::none();
        assert!(h.is_empty());
        assert!(!h.hides("Thumbs.db"));
        assert!(!h.hides_folded(b"thumbs.db"));
        assert!(!h.hides_system());
    }

    /// `hides_folded` on the folded name agrees with `hides` on the original.
    #[test]
    fn the_folded_test_agrees_with_the_plain_one() {
        let h = shipped();
        for name in [
            "Thumbs.db",
            "DESKTOP.INI",
            "AB-C-0704.pdf",
            "db.pdf",
            "Report.pdf.bak",
            // Non-ASCII names must agree too, though non-ASCII extensions are
            // never configured.
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

    /// Whitespace, leading dots, blanks and duplicates are normalised away.
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

    #[test]
    fn a_non_ascii_extension_is_dropped_rather_than_half_working() {
        let h = Hidden::new(&["d\u{e9}", "db"], false);
        assert_eq!(h.suffixes().collect::<Vec<_>>(), vec![".db"]);
        assert!(!h.hides("report.d\u{e9}"));
    }

    #[test]
    fn the_system_flag_is_carried_separately_from_the_list() {
        assert!(Hidden::new::<&str>(&[], true).hides_system());
        assert!(!Hidden::new(&["db"], false).hides_system());
    }
}
