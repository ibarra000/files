//! Fitting a string into a column that is narrower than it is.
//!
//! Two sentences of policy that both windows need and neither owns. This
//! lived in [`super::panel::row`] until the settings window wanted the same
//! elision for a drive path, and a settings module reaching into a panel
//! module for it would have been the wrong direction: the panel is a
//! consumer of this, not its home.
//!
//! Everything here takes a measuring function rather than a font, so the
//! policy - how much is dropped, and whether the ellipsis is paid for - is
//! checkable without a font atlas. The tests below use a font where every
//! character is one unit wide, which is what makes them about the rule
//! rather than about Segoe UI.

/// The one this program elides with, everywhere.
///
/// A single character rather than three full stops, which `view::style`
/// enforces in prose and which matters twice as much here: three stops in a
/// column this tight is three characters of the path thrown away to say the
/// same thing.
pub const ELLIPSIS: char = '\u{2026}';

/// Drops characters from the front until what is left fits, marking the cut
/// with a leading ellipsis.
///
/// Takes a measuring function rather than a font, so the policy - how much is
/// dropped, and whether the ellipsis is accounted for - can be checked without
/// a font atlas.
pub fn elide_left(text: &str, max_w: f32, measure: &impl Fn(&str) -> f32) -> String {
    if measure(text) <= max_w {
        return text.to_owned();
    }

    // One character at a time from the front. A path has at most a few hundred,
    // at most a handful of rows are ever on screen, and this only runs at all
    // for the ones that did not fit - so the simple loop is also the fast one.
    let mut start = 0;
    while start < text.len() {
        // `char_indices` rather than byte arithmetic: a share name can hold
        // characters that are not one byte long, and slicing inside one panics.
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

    // Not even the ellipsis fits. Returning it anyway would draw outside the
    // column; an empty string is at least honest about having no room.
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A font where every character is one unit wide, so the assertions below
    /// are about the policy rather than about Segoe UI's metrics.
    fn monospaced(text: &str) -> f32 {
        text.chars().count() as f32
    }

    #[test]
    fn a_folder_that_fits_is_left_alone() {
        let path = r"R:\11d\11-D-0704";
        assert_eq!(elide_left(path, 100.0, &monospaced), path);
        // Exactly the available width is still fitting.
        assert_eq!(
            elide_left(path, monospaced(path), &monospaced),
            path,
            "a folder was truncated to make room for nothing"
        );
    }

    /// The end of the path is what tells two rows apart, so the end is what
    /// survives.
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

    /// It must drop as little as it can get away with - an elision that
    /// overshoots throws away the very part it was keeping.
    #[test]
    fn no_more_is_dropped_than_has_to_be() {
        let path = r"R:\jobs\2024\11d\11-D-0704";
        for width in 4..=26 {
            let shown = elide_left(path, width as f32, &monospaced);
            assert!(monospaced(&shown) <= width as f32, "{width}: {shown:?}");
            if !shown.starts_with(ELLIPSIS) {
                continue;
            }
            // Putting one more character back must overflow, or the elision
            // was greedier than it needed to be.
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

    /// Slicing inside a character is a panic, and share names are not all
    /// ASCII.
    #[test]
    fn eliding_a_path_with_wide_characters_does_not_panic() {
        let path = "R:\\Zeichnungen\\Prüfung\\日本語のフォルダ\\11-D-0704";
        for width in 0..=40 {
            let shown = elide_left(path, width as f32, &monospaced);
            assert!(monospaced(&shown) <= width as f32, "{width}: {shown:?}");
        }
    }

    /// A column with no room draws nothing rather than spilling an ellipsis
    /// into the filename beside it.
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
