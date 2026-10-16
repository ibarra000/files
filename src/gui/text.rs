//! Elision of strings that are wider than their column, shared by both windows.
//!
//! Functions take a measuring function rather than a font, so the policy is
//! testable without a font atlas.

/// The ellipsis character used for all elision.
///
/// A single character rather than three full stops, which saves width in
/// narrow columns.
pub const ELLIPSIS: char = '\u{2026}';

/// Drops characters from the front until the text fits, marking the cut with a
/// leading ellipsis.
///
/// Returns an empty string if not even the ellipsis fits.
pub fn elide_left(text: &str, max_w: f32, measure: &impl Fn(&str) -> f32) -> String {
    if measure(text) <= max_w {
        return text.to_owned();
    }

    // Linear scan is sufficient: paths are short, few rows are visible, and
    // this only runs for rows that overflow.
    let mut start = 0;
    while start < text.len() {
        // Step by character, not byte: slicing inside a multi-byte character
        // panics.
        let next = text[start..]
            .char_indices()
            .nth(1)
            .map_or(text.len(), |(offset, _)| start + offset);
        let candidate = format!("{ELLIPSIS}{}", &text[next..]);
        if measure(&candidate) <= max_w {
            return candidate;
        }
        start = next;
    }

    // Not even the ellipsis fits; drawing it would overflow the column.
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measures every character as one unit wide.
    fn monospaced(text: &str) -> f32 {
        text.chars().count() as f32
    }

    #[test]
    fn a_folder_that_fits_is_left_alone() {
        let path = r"R:\11d\11-D-0704";
        assert_eq!(elide_left(path, 100.0, &monospaced), path);
        // Exactly the available width still fits.
        assert_eq!(
            elide_left(path, monospaced(path), &monospaced),
            path,
            "a folder was truncated to make room for nothing"
        );
    }

    /// The tail distinguishes rows, so the tail is kept.
    #[test]
    fn a_long_folder_loses_its_front_and_keeps_its_tail() {
        let path = r"R:\jobs\2024\11d\11-D-0704";
        let shown = elide_left(path, 12.0, &monospaced);

        assert!(monospaced(&shown) <= 12.0, "{shown:?} does not fit");
        assert!(shown.starts_with(ELLIPSIS), "{shown:?} hides nothing");
        assert!(
            path.ends_with(shown.trim_start_matches(ELLIPSIS)),
            "{shown:?} is not a tail of the path"
        );
        assert!(shown.ends_with("11-D-0704"), "{shown:?} lost the job code");
    }

    #[test]
    fn no_more_is_dropped_than_has_to_be() {
        let path = r"R:\jobs\2024\11d\11-D-0704";
        for width in 4..=26 {
            let shown = elide_left(path, width as f32, &monospaced);
            assert!(monospaced(&shown) <= width as f32, "{width}: {shown:?}");
            if !shown.starts_with(ELLIPSIS) {
                continue;
            }
            // Restoring one more character must overflow.
            let tail = shown.chars().count() - 1;
            let total = path.chars().count();
            if tail >= total {
                continue;
            }
            let candidate: String = std::iter::once(ELLIPSIS)
                .chain(path.chars().skip(total - tail - 1))
                .collect();
            assert!(
                monospaced(&candidate) > width as f32,
                "{width}: {shown:?} dropped more than it had to"
            );
        }
    }

    #[test]
    fn eliding_a_path_with_wide_characters_does_not_panic() {
        let path = "R:\\Zeichnungen\\Prüfung\\日本語のフォルダ\\11-D-0704";
        for width in 0..=40 {
            let shown = elide_left(path, width as f32, &monospaced);
            assert!(monospaced(&shown) <= width as f32, "{width}: {shown:?}");
        }
    }

    #[test]
    fn a_column_too_narrow_for_anything_draws_nothing() {
        assert_eq!(elide_left(r"R:\jobs", 0.0, &monospaced), "");
        assert_eq!(elide_left(r"R:\jobs", 0.5, &monospaced), "");
    }

    #[test]
    fn an_empty_folder_stays_empty() {
        assert_eq!(elide_left("", 0.0, &monospaced), "");
    }
}
