//! User-defined short names that expand to a search line (`pw` -> `AB-C-0704`).
//!
//! An alias expands to an ordinary query, run through the normal matcher and
//! ranking; nothing in the search knows aliases exist, so the oracle in
//! `tests/matcher_parity.rs` is unaffected. The expansion may carry syntax
//! (`ext:pdf AB-C-0704`).
//!
//! An alias fires only when the whole line equals its name (ASCII case-
//! insensitive). Prefix or substring matching would hide results for longer
//! searches that contain the name. The expansion is shown on screen so an
//! unintended match is visible.
//!
//! Expansions are validated with
//! [`Query::check`](crate::search::query::Query::check), the same check the
//! matcher applies to a typed line.

/// Why a name or expansion cannot be used as an alias.
///
/// Shared by the config loader and the settings window so both reject the
/// same input with the same message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    NoName,
    NameIsNotAscii,
    NameHasASpace,
    DuplicateName,
    /// The `code` key, the search input the alias expands to, is empty.
    NoCode,
    /// The expansion is not a runnable search.
    UnsearchableCode(crate::search::query::QueryReject),
}

impl Invalid {
    /// Returns a lowercase error fragment for display beside the field or key.
    pub fn detail(&self) -> String {
        match self {
            Self::NoName => "an alias needs a name".into(),
            Self::NameIsNotAscii => "an alias name must be ASCII".into(),
            Self::NameHasASpace => "an alias name cannot contain a space".into(),
            Self::DuplicateName => "that name is already an alias".into(),
            Self::NoCode => "an alias needs a code, the search it expands to".into(),
            Self::UnsearchableCode(reject) => {
                format!("the expansion is not a valid search: {}", reject.detail())
            }
        }
    }
}

/// Checks whether `name` and the expansion `code` form a valid alias
/// alongside `existing`.
pub fn check(name: &str, code: &str, existing: &[Alias]) -> Result<(), Invalid> {
    let name = name.trim();
    let code = code.trim();

    if name.is_empty() {
        return Err(Invalid::NoName);
    }
    if !name.is_ascii() {
        return Err(Invalid::NameIsNotAscii);
    }
    if name.chars().any(char::is_whitespace) {
        return Err(Invalid::NameHasASpace);
    }
    if existing.iter().any(|a| a.name.eq_ignore_ascii_case(name)) {
        return Err(Invalid::DuplicateName);
    }
    if code.is_empty() {
        return Err(Invalid::NoCode);
    }
    crate::search::query::Query::parse(code)
        .check()
        .map_err(Invalid::UnsearchableCode)
}

/// One configured alias.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    /// The typed name: ASCII, no whitespace, unique case-insensitively
    /// (enforced by the loader).
    pub name: Box<str>,
    /// The search input the name expands to (the `code` key).
    pub code: Box<str>,
    /// Optional user description.
    pub note: Option<Box<str>>,
}

impl Alias {
    /// Returns the display form (`name · expansion`) used by the status line and
    /// the settings list.
    pub fn describe(&self) -> String {
        format!("{} \u{b7} {}", self.name, self.code)
    }
}

/// The alias table, immutable once loaded and shared across threads.
///
/// A slice rather than a map: lists are small, lookup runs once per keystroke,
/// and configuration order is preserved for display.
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

    /// Returns the alias whose name equals the trimmed line, if any.
    ///
    /// ASCII case folding is sufficient because the loader rejects non-ASCII
    /// names, which keeps matching locale-independent.
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
                code: "AB-C-0704".into(),
                note: Some("Project workbook".into()),
            },
            Alias {
                name: "inv".into(),
                code: "ext:pdf invoice".into(),
                note: None,
            },
        ])
    }

    #[test]
    fn an_alias_resolves_to_the_line_it_stands_for() {
        assert_eq!(table().resolve("pw").unwrap().code.as_ref(), "AB-C-0704");
    }

    #[test]
    fn an_alias_is_found_whatever_case_it_is_typed_in() {
        for typed in ["pw", "PW", "Pw"] {
            assert!(table().resolve(typed).is_some(), "{typed} should resolve");
        }
    }

    #[test]
    fn surrounding_space_does_not_stop_an_alias_resolving() {
        assert!(table().resolve("  pw ").is_some());
    }

    /// Only a whole-line match expands; prefixes and substrings do not.
    #[test]
    fn a_line_that_merely_starts_with_an_alias_is_not_one() {
        assert!(table().resolve("pwx").is_none());
        assert!(table().resolve("pw AB").is_none());
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

    #[test]
    fn an_alias_may_expand_to_a_line_carrying_syntax() {
        assert_eq!(
            table().resolve("inv").unwrap().code.as_ref(),
            "ext:pdf invoice"
        );
    }

    #[test]
    fn an_alias_describes_itself_the_one_way() {
        let aliases = table();
        let alias = aliases.resolve("pw").unwrap();
        assert_eq!(alias.describe(), "pw \u{b7} AB-C-0704");
    }

    #[test]
    fn the_table_keeps_the_order_it_was_configured_in() {
        let aliases = table();
        let names: Vec<&str> = aliases.all().iter().map(|a| a.name.as_ref()).collect();
        assert_eq!(names, ["pw", "inv"]);
    }
}
