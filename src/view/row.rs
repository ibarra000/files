//! What one result row says.
//!
//! Two questions, neither of which needs a renderer to answer: which part of
//! the filename is the reason this row is here, and which folder it is in.
//! Both were previously tangled up in ratatui `Span`s in `ui::results`, and
//! both are the kind of thing that is wrong in a way nobody notices - a
//! highlight one byte out looks deliberate.

use crate::search::matcher::Hit;

/// A filename split around the run that matched.
///
/// `matched` is empty when there is nothing to highlight - either the row was
/// pulled in because its *folder* matched, or the offsets did not survive the
/// guard below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Name<'a> {
    pub before: &'a str,
    pub matched: &'a str,
    pub after: &'a str,
}

impl<'a> Name<'a> {
    /// The whole name, unhighlighted.
    fn plain(name: &'a str) -> Self {
        Self {
            before: name,
            matched: "",
            after: "",
        }
    }

    pub fn is_highlighted(&self) -> bool {
        !self.matched.is_empty()
    }
}

/// Splits a hit's name around the run the query matched.
///
/// `query_len` is in bytes of the *folded* query, which the matcher guarantees
/// is byte-length-identical to what it matched - so it indexes the original
/// name too. That guarantee is load-bearing and unenforceable from here, hence
/// the boundary checks: a pathological name must render wrong at worst, never
/// panic in the middle of a frame.
pub fn highlight(hit: &Hit, query_len: usize) -> Name<'_> {
    let name: &str = &hit.name;

    // Nothing in its own name matched - the folder did. Inventing a range here
    // would underline characters chosen at random, which reads as a bug in the
    // search rather than as an explanation of the row.
    if hit.is_inherited() || query_len == 0 {
        return Name::plain(name);
    }

    let start = hit.match_pos as usize;
    let Some(end) = start.checked_add(query_len) else {
        return Name::plain(name);
    };
    if end > name.len() || !name.is_char_boundary(start) || !name.is_char_boundary(end) {
        return Name::plain(name);
    }

    Name {
        before: &name[..start],
        matched: &name[start..end],
        after: &name[end..],
    }
}

/// The folder a hit is in, for the dimmed right-hand column.
///
/// A plain suffix strip rather than `Path::parent`, because these are UNC and
/// drive paths that came from the index as strings and go back out as strings;
/// round-tripping them through `Path` on the way to a text label would buy
/// nothing and would quietly normalise separators the user recognises.
pub fn folder(path: &str) -> &str {
    let Some(cut) = path.rfind(['\\', '/']) else {
        // A path that is nothing but a name - the fake sources in the tests
        // produce these - has no folder to show.
        return "";
    };
    // A file at the top of a drive is in `R:\`, not in `R:`, and one at the
    // root of a share is in `\`. Dropping the separator there turns a location
    // into a claim about which drive, so those two keep it.
    let root = cut == 0 || path[..cut].ends_with(':');
    &path[..cut + usize::from(root)]
}

/// Which configured drive a hit came off, for the badge at the end of a row.
///
/// The one thing worth keeping out of the deleted `crate::preview`, where it
/// was half of `locate` - the other half was [`folder`], which was already
/// here and which `locate` called.
///
/// Derived at paint time rather than stored on a [`Hit`]. There are a handful
/// of drives and at most [`crate::config::MAX_RESULTS`] rows, nearly all of
/// them culled before they are laid out, so this is a few string comparisons
/// per visible row against a list that is almost always two entries long.
///
/// `enabled` rather than `all`: a hit cannot have come off a drive that was
/// not searched, and naming a switched-off drive on a row would be a label
/// that contradicts the settings page.
pub fn drive_of<'a>(routes: &'a crate::paths::Routes, path: &str) -> Option<&'a str> {
    routes
        .enabled()
        .find(|m| crate::util::winpath::contains(&m.path, std::path::Path::new(path)))
        .map(|m| m.name.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn hit(name: &str, match_pos: u32) -> Hit {
        Hit {
            path: Arc::from(format!(r"R:\11d\11-D-0704\{name}").as_str()),
            name: Arc::from(name),
            match_pos,
            index: 0,
        }
    }

    #[test]
    fn the_matched_run_is_the_part_the_query_found() {
        let h = hit("A11-D-0704.pdf", 1);
        let name = highlight(&h, "11-D-0704".len());
        assert_eq!(
            name,
            Name {
                before: "A",
                matched: "11-D-0704",
                after: ".pdf"
            }
        );
        assert!(name.is_highlighted());
    }

    /// A row pulled in because its folder matched has nothing in its own name
    /// to underline. Highlighting the first nine characters of `cover.pdf`
    /// would be a confident lie.
    #[test]
    fn a_row_that_matched_by_folder_is_not_highlighted() {
        let mut h = hit("cover.pdf", 0);
        h.match_pos = u32::MAX;
        assert!(h.is_inherited());

        let name = highlight(&h, 9);
        assert_eq!(name.before, "cover.pdf");
        assert!(!name.is_highlighted());
    }

    /// Rendering must not panic on a name the matcher's guarantee does not
    /// hold for. Wrong at worst; never a torn frame.
    #[test]
    fn an_impossible_range_falls_back_to_the_plain_name() {
        for (pos, len) in [(99, 3), (0, 99), (u32::MAX - 1, 4), (2, usize::MAX)] {
            let h = hit("short.pdf", pos);
            let name = highlight(&h, len);
            assert_eq!(name.before, "short.pdf", "pos {pos}, len {len}");
            assert!(!name.is_highlighted());
        }
    }

    /// The offsets are bytes, and a name can hold characters that are not one
    /// byte long. Splitting inside one is a panic, not a rendering glitch.
    #[test]
    fn a_range_that_splits_a_character_falls_back_rather_than_panicking() {
        // "é" is two bytes, so offset 1 is inside it and offset 2 is after it.
        let at_zero = hit("é11-D.pdf", 0);
        let at_one = hit("é11-D.pdf", 1);
        assert_eq!(highlight(&at_zero, 2).matched, "é");
        assert!(
            !highlight(&at_zero, 1).is_highlighted(),
            "a run ending inside a character must not be taken"
        );
        assert!(
            !highlight(&at_one, 4).is_highlighted(),
            "nor one starting inside it"
        );
    }

    #[test]
    fn an_empty_query_highlights_nothing() {
        let h = hit("11-D-0704.pdf", 0);
        assert!(!highlight(&h, 0).is_highlighted());
    }

    #[test]
    fn the_folder_is_everything_before_the_name() {
        assert_eq!(folder(r"R:\11d\11-D-0704\a.pdf"), r"R:\11d\11-D-0704");
        assert_eq!(folder(r"\\server\share\jobs\a.pdf"), r"\\server\share\jobs");
        assert_eq!(folder("/mnt/jobs/a.pdf"), "/mnt/jobs");
    }

    /// A file at the root of a drive is at `R:\`, not at `R:`. The second
    /// claims it is on a drive; only the first says where.
    #[test]
    fn a_file_at_a_root_keeps_its_separator() {
        assert_eq!(folder(r"R:\a.pdf"), r"R:\");
        assert_eq!(folder(r"\a.pdf"), r"\");
    }

    #[test]
    fn a_bare_name_has_no_folder_to_show() {
        assert_eq!(folder("a.pdf"), "");
        assert_eq!(folder(""), "");
    }
}
