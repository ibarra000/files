//! A mechanical checker for the house style in [`crate::view`].
//!
//! Wording is pinned by per-module tests; this catches lines whose shape
//! breaks a rule, and names the rule.

/// The slot a line is written for, which decides the rules it follows.
///
/// Toasts are written in [`crate::app::state`] but rendered in the status
/// line, so they are status text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The status line and its toasts: one ellipsised fragment, no full stop.
    Status,
    /// The results pane, empty-state blocks and windows: whole sentences.
    Body,
}

/// Returns every house-style rule the line breaks.
pub fn violations(text: &str, slot: Slot) -> Vec<String> {
    let mut out = Vec::new();

    if text.contains("  ") {
        out.push("a run of two spaces - spacing is the renderer's business".to_string());
    }
    if text.contains("...") {
        out.push("three periods - the ellipsis is \u{2026}".to_string());
    }
    // Spaced, so hyphens in `AB-C-0704` and `--check-config` pass.
    if text.contains(" - ") {
        out.push("\" - \" as a connector - independent facts join with \" \u{b7} \"".to_string());
    }
    if text != text.trim() {
        out.push("leading or trailing space".to_string());
    }
    if slot == Slot::Status && text.ends_with('.') {
        out.push("a full stop - the status slot is a fragment".to_string());
    }

    out
}

/// Returns whether the line does not start with a lowercase letter.
///
/// Kept out of [`violations`] because lines may begin with a program, switch
/// or drive name; apply it only to corpora known to be prose.
pub fn starts_capitalised(text: &str) -> bool {
    match text.chars().next() {
        Some(first) => !first.is_lowercase(),
        // An empty status line is the healthy state.
        None => true,
    }
}

/// Asserts the house style over a corpus, listing every failing line in one
/// panic.
///
/// Not behind `cfg(test)` because integration tests in `tests/` check the
/// toasts from [`crate::app::state`] through the library.
pub fn check_all<'a>(what: &str, lines: impl IntoIterator<Item = &'a str>, slot: Slot) {
    let mut bad = Vec::new();
    for line in lines {
        for problem in violations(line, slot) {
            bad.push(format!("  {line:?}\n      {problem}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{what} breaks the house style:\n{}",
        bad.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_that_pads_itself_is_caught() {
        let bad = "sketch.exe is not on PATH \u{b7}          F2 switches viewer";
        assert!(!violations(bad, Slot::Status).is_empty());
    }

    #[test]
    fn three_periods_are_caught_and_one_ellipsis_is_not() {
        assert!(!violations("Searching...", Slot::Status).is_empty());
        assert!(violations("Searching\u{2026}", Slot::Status).is_empty());
    }

    #[test]
    fn only_the_spaced_hyphen_is_a_connector() {
        assert!(violations("Nothing matched AB-C-0704", Slot::Status).is_empty());
        assert!(violations("Run files --check-config", Slot::Status).is_empty());
        assert!(!violations("Keep typing - a search needs 3 characters", Slot::Status).is_empty());
    }

    #[test]
    fn the_full_stop_is_the_only_thing_the_two_slots_disagree_about() {
        let line = "Nothing matched.";
        assert!(!violations(line, Slot::Status).is_empty());
        assert!(violations(line, Slot::Body).is_empty());
    }

    #[test]
    fn an_empty_status_line_is_healthy() {
        assert!(violations("", Slot::Status).is_empty());
        assert!(starts_capitalised(""));
    }
}
