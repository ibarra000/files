//! Short names for the codes somebody types every day.
//!
//! A job code is a thing to be read off a drawing rather than remembered,
//! which is fine for the one in front of you and poor for the six opened every
//! morning. An alias gives one of those a name its owner chose: `pw` finds
//! `11-D-0704`, on the second keystroke.
//!
//! # An alias expands, it does not match
//!
//! What an alias produces is a *query*, run through the ordinary matcher over
//! the ordinary index and ranked by the ordinary comparator. Nothing here
//! reaches into the search: there is no alias bit in the ranking key and no
//! second route a result can arrive by. That is deliberate, and it is what
//! makes this cheap to trust - the frozen oracle in `tests/matcher_parity.rs`
//! never has to learn that this module exists.
//!
//! It also means an alias may carry syntax. `ext:pdf 11-D-0704` is a perfectly
//! good expansion, because the thing being stored is a line to type and not a
//! code to look up.
//!
//! # The whole line, and exactly
//!
//! An alias fires only when the entire line is one, case aside. Not a prefix
//! and not a substring: either would let an alias hide the results for a
//! longer code that happens to contain it, which is a program quietly
//! searching for something other than what was typed. That is the failure the
//! rule engine was deleted for - see [`crate::paths`] - and it is not worth
//! reintroducing for the sake of saving a keystroke.
//!
//! The expansion is put on screen for the same reason. An alias is a shortcut
//! its owner wrote down, so watching it fire is confirmation; watching it fire
//! when it was not meant to is the only way anybody finds out.
//!
//! # A short name is not a short search
//!
//! [`crate::config::MIN_QUERY_LEN`] is three, and nothing here lowers it.
//! `code` is checked when the file is read, against the same
//! [`Query::check`](crate::search::query::Query::check) the matcher uses, so
//! the line that reaches the index is always long enough to be worth sweeping
//! for. A short *name* is the entire point; a short *search* is the thing the
//! minimum exists to prevent, and the two stop being the same question the
//! moment one expands into the other.

/// One configured alias.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    /// What is typed. ASCII, no whitespace, unique case-insensitively - all
    /// three enforced by the loader.
    pub name: Box<str>,
    /// The search line it stands for.
    pub code: Box<str>,
    /// What it is, for the person reading their own list a year later.
    pub note: Option<Box<str>>,
}

impl Alias {
    /// How the expansion is written wherever it is shown.
    ///
    /// One spelling, in one place, because the status line and the settings
    /// list saying it two different ways is how a user comes to believe they
    /// are two different things.
    pub fn describe(&self) -> String {
        format!("{} \u{b7} {}", self.name, self.code)
    }
}

/// The alias table. Immutable once loaded, shared by every thread.
///
/// A slice rather than a map. These lists are tens of entries, the lookup runs
/// once per keystroke against a string already in cache, and configuration
/// order is worth keeping: it is the order the user wrote and the order their
/// own list reads back in.
#[derive(Debug, Default)]
pub struct Aliases {
    entries: Box<[Alias]>,
}

impl Aliases {
    pub fn new(entries: Vec<Alias>) -> Self {
        Self {
            entries: entries.into_boxed_slice(),
        }
    }

    pub fn all(&self) -> &[Alias] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The alias this line is, if it is one.
    ///
    /// Trimmed, because a trailing space is a typist finishing a word and not
    /// a different alias. ASCII case folding, which is sound because the
    /// loader refuses a name that is not ASCII: a name whose case depends on
    /// the locale is a name that fires on one machine and not on the next.
    pub fn resolve(&self, line: &str) -> Option<&Alias> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        self.entries
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Aliases {
        Aliases::new(vec![
            Alias {
                name: "pw".into(),
                code: "11-D-0704".into(),
                note: Some("Powerwall bracket".into()),
            },
            Alias {
                name: "inv".into(),
                code: "ext:pdf inverter".into(),
                note: None,
            },
        ])
    }

    #[test]
    fn an_alias_resolves_to_the_line_it_stands_for() {
        assert_eq!(table().resolve("pw").unwrap().code.as_ref(), "11-D-0704");
    }

    #[test]
    fn an_alias_is_found_whatever_case_it_is_typed_in() {
        for typed in ["pw", "PW", "Pw"] {
            assert!(table().resolve(typed).is_some(), "{typed} should resolve");
        }
    }

    /// A trailing space is somebody finishing a word, not a different alias.
    #[test]
    fn surrounding_space_does_not_stop_an_alias_resolving() {
        assert!(table().resolve("  pw ").is_some());
    }

    /// The whole line or nothing. A prefix that expanded would make every
    /// longer code containing it unreachable.
    #[test]
    fn a_line_that_merely_starts_with_an_alias_is_not_one() {
        assert!(table().resolve("pwx").is_none());
        assert!(table().resolve("pw 11").is_none());
        assert!(table().resolve("xpw").is_none());
    }

    #[test]
    fn an_empty_line_resolves_to_nothing() {
        assert!(table().resolve("").is_none());
        assert!(table().resolve("   ").is_none());
    }

    #[test]
    fn an_empty_table_resolves_nothing_at_all() {
        let none = Aliases::default();
        assert!(none.is_empty());
        assert_eq!(none.len(), 0);
        assert!(none.resolve("pw").is_none());
    }

    /// An alias stores a line, not a code, so syntax survives it.
    #[test]
    fn an_alias_may_expand_to_a_line_carrying_syntax() {
        assert_eq!(
            table().resolve("inv").unwrap().code.as_ref(),
            "ext:pdf inverter"
        );
    }

    #[test]
    fn an_alias_describes_itself_the_one_way() {
        let aliases = table();
        let alias = aliases.resolve("pw").unwrap();
        assert_eq!(alias.describe(), "pw \u{b7} 11-D-0704");
    }

    #[test]
    fn the_table_keeps_the_order_it_was_configured_in() {
        let aliases = table();
        let names: Vec<&str> = aliases.all().iter().map(|a| a.name.as_ref()).collect();
        assert_eq!(names, ["pw", "inv"]);
    }
}
