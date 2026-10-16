//! The text of one result row: the matched part of the filename and the
//! folder it is in.

use crate::search::matcher::Hit;

/// A filename split around the run that matched.
///
/// `matched` is empty when the row matched by folder or the offsets were
/// invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Name<'a> {
    pub before: &'a str,
    pub matched: &'a str,
    pub after: &'a str,
}

impl<'a> Name<'a> {
    /// Returns the whole name, unhighlighted.
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
/// `query_len` is the byte length of the folded query, which the matcher
/// guarantees equals the matched span in the original name. Out-of-range or
/// non-boundary offsets fall back to an unhighlighted name instead of
/// panicking.
pub fn highlight(hit: &Hit, query_len: usize) -> Name<'_> {
    let name: &str = &hit.name;

    // The folder matched, not the name, so there is no range to highlight.
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

/// Returns the folder part of a path, for the dimmed right-hand column.
///
/// A string split rather than `Path::parent`, so separators are shown exactly
/// as indexed.
pub fn folder(path: &str) -> &str {
    let Some(cut) = path.rfind(['\\', '/']) else {
        // A bare name (as produced by test sources) has no folder.
        return "";
    };
    // Root folders keep their trailing separator: `R:\` and `\`.
    let root = cut == 0 || path[..cut].ends_with(':');
    &path[..cut + usize::from(root)]
}

/// Returns the name of the enabled drive a hit came from, for the row badge.
///
/// Computed at paint time rather than stored on a [`Hit`]; it is a few string
/// comparisons per visible row. Disabled drives are skipped because they were
/// not searched.
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
            path: Arc::from(format!(r"R:\abc\AB-C-0704\{name}").as_str()),
            name: Arc::from(name),
            match_pos,
            index: 0,
        }
    }

    #[test]
    fn the_matched_run_is_the_part_the_query_found() {
        let h = hit("AAB-C-0704.pdf", 1);
        let name = highlight(&h, "AB-C-0704".len());
        assert_eq!(
            name,
            Name {
                before: "A",
                matched: "AB-C-0704",
                after: ".pdf"
            }
        );
        assert!(name.is_highlighted());
    }

    #[test]
    fn a_row_that_matched_by_folder_is_not_highlighted() {
        let mut h = hit("cover.pdf", 0);
        h.match_pos = u32::MAX;
        assert!(h.is_inherited());

        let name = highlight(&h, 9);
        assert_eq!(name.before, "cover.pdf");
        assert!(!name.is_highlighted());
    }

    #[test]
    fn an_impossible_range_falls_back_to_the_plain_name() {
        for (pos, len) in [(99, 3), (0, 99), (u32::MAX - 1, 4), (2, usize::MAX)] {
            let h = hit("short.pdf", pos);
            let name = highlight(&h, len);
            assert_eq!(name.before, "short.pdf", "pos {pos}, len {len}");
            assert!(!name.is_highlighted());
        }
    }

    #[test]
    fn a_range_that_splits_a_character_falls_back_rather_than_panicking() {
        // "é" is two bytes, so offset 1 is inside it and offset 2 is after it.
        let at_zero = hit("éAB-C.pdf", 0);
        let at_one = hit("éAB-C.pdf", 1);
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
        let h = hit("AB-C-0704.pdf", 0);
        assert!(!highlight(&h, 0).is_highlighted());
    }

    #[test]
    fn the_folder_is_everything_before_the_name() {
        assert_eq!(folder(r"R:\abc\AB-C-0704\a.pdf"), r"R:\abc\AB-C-0704");
        assert_eq!(
            folder(r"\\server\share\projects\a.pdf"),
            r"\\server\share\projects"
        );
        assert_eq!(folder("/mnt/projects/a.pdf"), "/mnt/projects");
    }

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
