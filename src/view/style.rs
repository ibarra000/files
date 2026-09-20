//! The house style, as something a test can run.
//!
//! The rules are argued for in [`crate::view`]'s module note. This is the
//! machine-readable half of them, so that a line breaking one is a red test
//! rather than a thing somebody notices in a screenshot six months later.
//!
//! # Why a checker rather than a list of expected strings
//!
//! The exact wording is already pinned, test by test, all over this crate, and
//! that is the right way to pin wording: a test that says what a line should
//! say is readable and is worth arguing with. What those tests cannot do is
//! notice a line that says a perfectly reasonable thing in the wrong shape -
//! and the shape is what went wrong. Every one of these rules was broken by a
//! string somebody wrote carefully.
//!
//! The checker knows nothing about meaning. It is deliberately mechanical, so
//! that a new line either passes or names the rule it broke.

/// Which slot a line is written for.
///
/// Not which module it lives in: toasts are written in
/// [`crate::app::state`] and rendered in the status line's own 13 pt run, so
/// they are status text and are held to the status rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The status line and the toasts that share it. One line, ellipsised
    /// to whatever the footer's two buttons left, so it is a fragment and
    /// takes no stop.
    Status,
    /// The results pane, the empty-state blocks and the windows. Room for
    /// whole sentences, so it has them.
    Body,
}

/// Which rule a line broke, if it broke one.
///
/// A `Vec` rather than the first failure, because a line that breaks two rules
/// should say so once rather than over two runs of the suite.
pub fn violations(text: &str, slot: Slot) -> Vec<String> {
    let mut out = Vec::new();

    // The one that was actually shipped: `src/view/status.rs` carried a line
    // with twenty literal spaces in the middle of it, which the footer
    // ellipsised and a screen reader read out in full.
    if text.contains("  ") {
        out.push("a run of two spaces - spacing is the renderer's business".to_string());
    }
    if text.contains("...") {
        out.push("three periods - the ellipsis is \u{2026}".to_string());
    }
    // Spaced, so the hyphen inside `11-D-0704` and the one inside
    // `--check-config` are untouched. It is only the connector that is banned.
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

/// Whether a line begins the way a line should.
///
/// Separate from [`violations`] because it has an exception the checker cannot
/// see: a line may legitimately begin with a program, a switch or a drive name
/// read out of the configuration, and those keep their own spelling. Applied
/// only where the caller knows the whole corpus is prose.
pub fn starts_capitalised(text: &str) -> bool {
    match text.chars().next() {
        Some(first) => !first.is_lowercase(),
        // An empty status line is the healthy one, and it starts with nothing.
        None => true,
    }
}

/// Asserts a whole corpus at once, naming every line that failed.
///
/// One panic listing everything rather than one per run: a case pass touches a
/// hundred strings, and finding out about them one `cargo test` at a time is
/// how a rule stops being applied.
///
/// Not behind `cfg(test)`, because the toasts are written in
/// [`crate::app::state`] and driven from `tests/`, which compiles against the
/// library as an outside caller would. Ten lines in the binary is the price of
/// the rule being checked where the strings actually are.
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
        let bad = "avwin.exe is not on PATH \u{b7}          F2 switches viewer";
        assert!(!violations(bad, Slot::Status).is_empty());
    }

    #[test]
    fn three_periods_are_caught_and_one_ellipsis_is_not() {
        assert!(!violations("Searching...", Slot::Status).is_empty());
        assert!(violations("Searching\u{2026}", Slot::Status).is_empty());
    }

    /// The hyphen inside a job code and inside a command-line switch is not a
    /// connector, and a rule that could not tell them apart would be a rule
    /// nobody could follow.
    #[test]
    fn only_the_spaced_hyphen_is_a_connector() {
        assert!(violations("Nothing matched 11-D-0704", Slot::Status).is_empty());
        assert!(violations("Run files --check-config", Slot::Status).is_empty());
        assert!(!violations("Keep typing - a code needs 3 characters", Slot::Status).is_empty());
    }

    /// The two slots differ in exactly one rule, and it is this one.
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
