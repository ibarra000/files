//! Where a typed job code resolves to.
//!
//! Routing used to be six hardcoded regexes and a two-variant enum: one flat
//! share, one parent of per-job folders. It is now driven by a configured list
//! of mappings, each carrying its own ordered rules, so a new share is a
//! config edit rather than a rebuild.
//!
//! Everything here is pure and allocation-light: `classify` runs on every
//! keystroke, and every regex was compiled once when the configuration loaded.
//!
//! # Precedence
//!
//! Mappings are evaluated in configuration order, and rules within a mapping
//! in list order. Two terminations, and both matter:
//!
//! * **The first matching rule within a mapping wins.** Unconditional, not
//!   configurable. Without it `11-D-0704` would match both the "single letter
//!   in the middle" rule (giving `11d`) and the "two or more characters"
//!   rule (giving `11`), producing two targets in the same share.
//! * **A matching rule marked `stop` ends evaluation entirely.** This is what
//!   reproduces the CustomPro-before-jobs precedence. CustomPro codes contain
//!   dashes too (`P12345-001`, `PP987-A`), so without it such a code would
//!   also match the generic two-field job pattern and probe a nonexistent
//!   `R:\p12345` on every keystroke. Clearing `stop` is how a user asks for
//!   results merged across shares.

use std::path::{Path, PathBuf};

use regex::{Regex, RegexBuilder};
use smallvec::SmallVec;

use crate::util::winpath;

/// Identifies a mapping. Its position in the configured list.
///
/// Deliberately not the name: it is compared per result row and used as a map
/// key, where an integer beats a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MappingId(pub u16);

impl MappingId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// How a mapping is laid out on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingKind {
    /// One large directory holding every job's files directly. The only shape
    /// big enough to justify a persisted index and a background refresh.
    Flat,
    /// The whole tree beneath the path, walked and indexed.
    ///
    /// The answer to a share whose folder names no rule can predict: instead
    /// of deducing which directory a code lives in, every directory is read
    /// and the question becomes a search rather than a guess. Costs a
    /// background walk and a few hundred megabytes; buys never silently
    /// missing a file.
    Tree,
    /// The code resolves to a subfolder underneath the mapping's path.
    JobFolder,
}

impl MappingKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "flat" => Some(Self::Flat),
            "tree" | "recursive" => Some(Self::Tree),
            "job-folder" | "job_folder" | "jobfolder" => Some(Self::JobFolder),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Tree => "tree",
            Self::JobFolder => "job-folder",
        }
    }

    /// True when this mapping is indexed in the background rather than listed
    /// on demand.
    pub fn is_indexed(self) -> bool {
        matches!(self, Self::Flat | Self::Tree)
    }
}

/// How a resolved folder name is cased.
///
/// Not cosmetic: the job cache is keyed by path, so `R:\11D` and `R:\11d`
/// would be two cache entries and two round trips on a case-insensitive
/// share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseFold {
    #[default]
    Lower,
    Upper,
    Preserve,
}

impl CaseFold {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lower" => Some(Self::Lower),
            "upper" => Some(Self::Upper),
            "preserve" | "none" => Some(Self::Preserve),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Lower => "lower",
            Self::Upper => "upper",
            Self::Preserve => "preserve",
        }
    }

    /// Applies the fold.
    ///
    /// ASCII first, falling back to full Unicode only when the result is not
    /// ASCII - matching what the original `join_lower` did. The fallback is
    /// not theoretical: a case-insensitive `[A-Z]` also matches KELVIN SIGN
    /// and LATIN SMALL LETTER LONG S, so a pasted code can carry them in.
    pub fn apply(self, s: &str) -> String {
        match self {
            Self::Preserve => s.to_string(),
            Self::Lower => {
                let mut out = s.to_string();
                out.make_ascii_lowercase();
                if out.is_ascii() {
                    out
                } else {
                    s.to_lowercase()
                }
            }
            Self::Upper => {
                let mut out = s.to_string();
                out.make_ascii_uppercase();
                if out.is_ascii() {
                    out
                } else {
                    s.to_uppercase()
                }
            }
        }
    }
}

/// One compiled routing rule.
#[derive(Debug)]
pub struct Rule {
    re: Regex,
    /// Only meaningful for a job-folder mapping: a `Captures::expand`
    /// template building the subfolder name from the match.
    folder: Option<Box<str>>,
    /// When this rule matches, no later mapping is consulted.
    stop: bool,
    /// Position in the mapping's rule list, for error messages.
    index: u16,
    /// The pattern as written, for `--doctor`.
    source: Box<str>,
}

impl Rule {
    pub fn new(
        pattern: &str,
        folder: Option<&str>,
        stop: bool,
        index: u16,
    ) -> Result<Self, regex::Error> {
        let re = RegexBuilder::new(pattern)
            .case_insensitive(true)
            // A hand-edited pattern must not be able to allocate unboundedly
            // while the user is typing.
            .size_limit(1 << 20)
            .dfa_size_limit(1 << 20)
            .build()?;
        Ok(Self {
            re,
            folder: folder.map(Box::from),
            stop,
            index,
            source: Box::from(pattern),
        })
    }

    pub fn pattern(&self) -> &str {
        &self.source
    }

    pub fn folder(&self) -> Option<&str> {
        self.folder.as_deref()
    }

    pub fn stop(&self) -> bool {
        self.stop
    }

    pub fn index(&self) -> u16 {
        self.index
    }

    /// Whether this rule's pattern matches, without building a target.
    pub fn pattern_matches(&self, code: &str) -> bool {
        self.re.is_match(code)
    }

    /// Capture-group references a folder template makes that the pattern
    /// cannot satisfy.
    ///
    /// Checked when the configuration loads, because at runtime the failure is
    /// silent: `Captures::expand` writes nothing for an unknown group, the
    /// folder comes out empty, and the search would fall back to the mapping
    /// root - that is, the entire share.
    pub fn unsatisfiable_refs(&self) -> Vec<String> {
        let Some(template) = self.folder.as_deref() else {
            return Vec::new();
        };
        let names: Vec<Option<&str>> = self.re.capture_names().collect();
        template_refs(template)
            .into_iter()
            .filter(|r| match r {
                TemplateRef::Number(n) => *n >= self.re.captures_len(),
                TemplateRef::Name(name) => !names.iter().any(|n| n.is_some_and(|n| n == name)),
            })
            .map(|r| match r {
                TemplateRef::Number(n) => format!("${n}"),
                TemplateRef::Name(name) => format!("${name}"),
            })
            .collect()
    }
}

/// A capture reference inside a folder template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplateRef {
    Number(usize),
    Name(String),
}

/// Extracts the capture references a `Captures::expand` template makes.
///
/// Mirrors the regex crate's own syntax: `$1`, `${1}`, `$name`, `${name}`,
/// with `$$` an escaped dollar. Note `$1x` names the group `1x`, not group 1
/// followed by `x` - a trap worth catching at load rather than discovering as
/// an empty expansion.
fn template_refs(template: &str) -> Vec<TemplateRef> {
    let bytes = template.as_bytes();
    let mut refs = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'$' {
            i += 1; // escaped dollar
            continue;
        }
        let (name, next) = if bytes[i] == b'{' {
            let start = i + 1;
            match template[start..].find('}') {
                Some(off) => (&template[start..start + off], start + off + 1),
                None => break,
            }
        } else {
            let start = i;
            let mut end = i;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            (&template[start..end], end)
        };
        if !name.is_empty() {
            refs.push(match name.parse::<usize>() {
                Ok(n) => TemplateRef::Number(n),
                Err(_) => TemplateRef::Name(name.to_string()),
            });
        }
        i = next.max(i + 1);
    }
    refs
}

/// One configured share.
#[derive(Debug)]
pub struct Mapping {
    pub id: MappingId,
    pub name: Box<str>,
    pub path: PathBuf,
    pub kind: MappingKind,
    pub enabled: bool,
    pub case: CaseFold,
    pub rules: Box<[Rule]>,
}

impl Mapping {
    /// The directory `code` resolves to under this mapping, if any.
    ///
    /// Returns whether evaluation should stop here, so the caller can honour
    /// a rule's `stop` without needing the rule itself.
    fn resolve(&self, code: &str) -> Option<(Target, bool)> {
        // A mapping with no rules matches everything. That is the whole point
        // of an indexed share: there is nothing to deduce, because every file
        // under it is already known, so a code that matches nothing simply
        // returns no results rather than being declared unroutable. A
        // job-folder mapping with no rules could never resolve a directory
        // and is rejected at load time instead.
        if self.rules.is_empty() {
            return self.kind.is_indexed().then(|| {
                (
                    Target {
                        mapping: self.id,
                        kind: self.kind,
                        dir: self.path.clone(),
                    },
                    false,
                )
            });
        }
        for rule in self.rules.iter() {
            let Some(caps) = rule.re.captures(code) else {
                continue;
            };
            let dir = match self.kind {
                // Both are searched whole, so the code selects the mapping
                // rather than a directory within it.
                MappingKind::Flat | MappingKind::Tree => self.path.clone(),
                MappingKind::JobFolder => {
                    let template = rule.folder.as_deref()?;
                    let mut expanded = String::new();
                    caps.expand(template, &mut expanded);
                    let folder = self.case.apply(&expanded);
                    // Runtime backstop for the load-time template check: a
                    // group that legitimately captured nothing still yields an
                    // empty folder, which would search the whole share.
                    if !is_safe_folder(&folder) {
                        continue;
                    }
                    self.path.join(folder)
                }
            };
            return Some((
                Target {
                    mapping: self.id,
                    kind: self.kind,
                    dir,
                },
                rule.stop,
            ));
        }
        None
    }
}

/// True when an expanded folder name stays inside its mapping root.
///
/// Makes "a configuration can never point the search outside the directory it
/// names" a checkable property rather than a hope.
fn is_safe_folder(folder: &str) -> bool {
    if folder.is_empty() {
        return false;
    }
    if folder.starts_with(['\\', '/']) {
        return false;
    }
    if winpath::drive_letter_of(Path::new(folder)).is_some() {
        return false;
    }
    folder
        .split(['\\', '/'])
        .all(|c| !c.is_empty() && c != "." && c != "..")
}

/// Where a code resolves to.
///
/// `dir` is fully joined: the mapping root for a flat mapping, root plus
/// folder for a job folder. Callers no longer join roots themselves, which
/// removes the several places that each did it slightly differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub mapping: MappingId,
    pub kind: MappingKind,
    pub dir: PathBuf,
}

/// Two inline: the shipped configuration produces at most one, and a merged
/// configuration realistically produces two.
pub type TargetList = SmallVec<[Target; 2]>;

/// Where the routing table came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// No file was found; the compiled-in defaults are in effect.
    BuiltIn,
    /// Read from the default location.
    Default(PathBuf),
    /// Read from an explicit `--config` or `FILES_CONFIG`.
    Explicit(PathBuf),
}

impl ConfigSource {
    pub fn describe(&self) -> String {
        match self {
            Self::BuiltIn => "built-in defaults (no config file)".into(),
            Self::Default(p) | Self::Explicit(p) => p.display().to_string(),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::BuiltIn => None,
            Self::Default(p) | Self::Explicit(p) => Some(p),
        }
    }
}

/// The compiled routing table. Immutable once loaded, shared by every thread.
#[derive(Debug)]
pub struct Routes {
    mappings: Box<[Mapping]>,
    source: ConfigSource,
}

impl Routes {
    pub fn new(mappings: Vec<Mapping>, source: ConfigSource) -> Self {
        Self {
            mappings: mappings.into_boxed_slice(),
            source,
        }
    }

    pub fn source(&self) -> &ConfigSource {
        &self.source
    }

    pub fn all(&self) -> &[Mapping] {
        &self.mappings
    }

    pub fn enabled(&self) -> impl Iterator<Item = &Mapping> {
        self.mappings.iter().filter(|m| m.enabled)
    }

    pub fn get(&self, id: MappingId) -> Option<&Mapping> {
        self.mappings.get(id.index())
    }

    /// Short label for a result row or an error message.
    pub fn label(&self, id: MappingId) -> &str {
        self.get(id).map(|m| m.name.as_ref()).unwrap_or("?")
    }

    /// The directory a mapping points at.
    pub fn dir(&self, id: MappingId) -> Option<&Path> {
        self.get(id).map(|m| m.path.as_path())
    }

    /// Every enabled flat mapping, in configuration order.
    pub fn flat(&self) -> impl Iterator<Item = &Mapping> {
        self.enabled().filter(|m| m.kind == MappingKind::Flat)
    }

    /// Repoints a mapping by name, for an environment or command-line
    /// override.
    ///
    /// Returns false when no such mapping exists, which the caller should
    /// treat as an error rather than ignore: silently dropping an override
    /// means searching a share the user believed they had redirected.
    pub fn set_path(&mut self, name: &str, path: PathBuf) -> bool {
        match self
            .mappings
            .iter_mut()
            .find(|m| m.name.eq_ignore_ascii_case(name))
        {
            Some(m) => {
                m.path = winpath::normalise_root(&path);
                true
            }
            None => false,
        }
    }

    /// The configured mapping names, for an error message.
    pub fn names(&self) -> Vec<&str> {
        self.mappings.iter().map(|m| m.name.as_ref()).collect()
    }

    /// Every share a query is searched against, in configuration order.
    ///
    /// Takes no code, and that *is* the change. Routing existed to work out
    /// which folder a code lived in before anything looked for it; an indexed
    /// share already knows where every file is, so the question became a
    /// search rather than a deduction and the answer stopped depending on what
    /// was typed.
    pub fn targets(&self) -> TargetList {
        self.enabled()
            .filter(|m| m.kind.is_indexed())
            .map(|m| Target {
                mapping: m.id,
                kind: m.kind,
                dir: m.path.clone(),
            })
            .collect()
    }

    /// Whether anything is searchable at all.
    pub fn any_indexed(&self) -> bool {
        self.enabled().any(|m| m.kind.is_indexed())
    }

    /// Every target `code` resolves to, in evaluation order.
    pub fn classify(&self, code: &str) -> TargetList {
        let mut targets = TargetList::new();
        for mapping in self.enabled() {
            let Some((target, stop)) = mapping.resolve(code) else {
                continue;
            };
            targets.push(target);
            if stop {
                break;
            }
        }
        targets
    }

    /// Whether `code` resolves anywhere, without building paths.
    pub fn resolves(&self, code: &str) -> bool {
        self.enabled().any(|m| m.resolve(code).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, folder: Option<&str>, stop: bool) -> Rule {
        Rule::new(pattern, folder, stop, 0).unwrap()
    }

    fn job_mapping(id: u16, name: &str, path: &str, rules: Vec<Rule>) -> Mapping {
        Mapping {
            id: MappingId(id),
            name: name.into(),
            path: PathBuf::from(path),
            kind: MappingKind::JobFolder,
            enabled: true,
            case: CaseFold::Lower,
            rules: rules.into_boxed_slice(),
        }
    }

    fn flat_mapping(id: u16, name: &str, path: &str, rules: Vec<Rule>) -> Mapping {
        Mapping {
            id: MappingId(id),
            name: name.into(),
            path: PathBuf::from(path),
            kind: MappingKind::Flat,
            enabled: true,
            case: CaseFold::Lower,
            rules: rules.into_boxed_slice(),
        }
    }

    // --- template references ------------------------------------------------

    #[test]
    fn finds_numeric_and_named_template_references() {
        assert_eq!(
            template_refs("${1}${2}"),
            vec![TemplateRef::Number(1), TemplateRef::Number(2)]
        );
        assert_eq!(template_refs("$1"), vec![TemplateRef::Number(1)]);
        assert_eq!(
            template_refs("${job}-x"),
            vec![TemplateRef::Name("job".into())]
        );
    }

    /// The trap the shipped config avoids by always using braces: `$1x` is the
    /// group named `1x`, not group 1 followed by `x`.
    #[test]
    fn a_bare_reference_swallows_following_word_characters() {
        assert_eq!(template_refs("$1x"), vec![TemplateRef::Name("1x".into())]);
        assert_eq!(
            template_refs("${1}x"),
            vec![TemplateRef::Number(1)],
            "braces stop the reference"
        );
    }

    #[test]
    fn an_escaped_dollar_is_not_a_reference() {
        assert_eq!(template_refs("$$1"), Vec::new());
        assert_eq!(template_refs("a$$b"), Vec::new());
    }

    #[test]
    fn a_template_with_no_references_is_empty() {
        assert_eq!(template_refs("literal"), Vec::new());
        assert_eq!(template_refs(""), Vec::new());
    }

    /// The load-time check that stops a silent empty expansion.
    #[test]
    fn detects_references_the_pattern_cannot_satisfy() {
        let r = rule(r"^([A-Z]+)-([0-9]+)$", Some("${1}${2}"), false);
        assert!(r.unsatisfiable_refs().is_empty());

        let r = rule(r"^([A-Z]+)-([0-9]+)$", Some("${3}"), false);
        assert_eq!(r.unsatisfiable_refs(), vec!["$3"]);

        let r = rule(r"^([A-Z]+)$", Some("$1x"), false);
        assert_eq!(r.unsatisfiable_refs(), vec!["$1x"]);
    }

    #[test]
    fn a_named_group_the_pattern_declares_is_satisfiable() {
        let r = rule(r"^(?<job>[A-Z]+)$", Some("${job}"), false);
        assert!(r.unsatisfiable_refs().is_empty());

        let r = rule(r"^(?<job>[A-Z]+)$", Some("${other}"), false);
        assert_eq!(r.unsatisfiable_refs(), vec!["$other"]);
    }

    // --- folder safety ------------------------------------------------------

    #[test]
    fn accepts_ordinary_folder_names() {
        assert!(is_safe_folder("11d"));
        assert!(is_safe_folder("ab12"));
        assert!(is_safe_folder(r"nested\folder"));
    }

    /// A configuration must never be able to point the search outside the
    /// directory it names.
    #[test]
    fn rejects_folders_that_escape_the_mapping_root() {
        assert!(!is_safe_folder(""));
        assert!(!is_safe_folder(".."));
        assert!(!is_safe_folder(r"..\..\windows"));
        assert!(!is_safe_folder(r"a\..\b"));
        assert!(!is_safe_folder("."));
        assert!(!is_safe_folder(r"\absolute"));
        assert!(!is_safe_folder("/absolute"));
        assert!(!is_safe_folder(r"C:\elsewhere"));
        assert!(!is_safe_folder(r"a\\b"));
    }

    // --- case folding -------------------------------------------------------

    #[test]
    fn folds_case_as_configured() {
        assert_eq!(CaseFold::Lower.apply("AbC12"), "abc12");
        assert_eq!(CaseFold::Upper.apply("AbC12"), "ABC12");
        assert_eq!(CaseFold::Preserve.apply("AbC12"), "AbC12");
    }

    #[test]
    fn case_folding_handles_non_ascii() {
        assert_eq!(CaseFold::Lower.apply("ÉCOLE"), "école");
        assert_eq!(CaseFold::Upper.apply("école"), "ÉCOLE");
    }

    // --- routing ------------------------------------------------------------

    fn two_mappings() -> Routes {
        Routes::new(
            vec![
                flat_mapping(
                    0,
                    "custompro",
                    r"V:\Documents\custpro",
                    vec![rule("^PP", None, true), rule("^P[0-9]+", None, true)],
                ),
                job_mapping(
                    1,
                    "jobs",
                    r"R:\",
                    vec![
                        rule(
                            r"^([A-Z0-9]+)-([A-Z])-([A-Z0-9]+)$",
                            Some("${1}${2}"),
                            false,
                        ),
                        rule(
                            r"^([A-Z0-9]+)-([A-Z0-9]{2,})-([A-Z0-9]+)$",
                            Some("${1}"),
                            false,
                        ),
                        rule(r"^([A-Z0-9]+)-([A-Z0-9]+)$", Some("${1}"), false),
                    ],
                ),
            ],
            ConfigSource::BuiltIn,
        )
    }

    #[test]
    fn a_job_code_resolves_to_a_subfolder() {
        let t = two_mappings().classify("11-D-0704");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].kind, MappingKind::JobFolder);
        assert_eq!(t[0].dir, PathBuf::from(r"R:\11d"));
    }

    #[test]
    fn a_custompro_code_resolves_to_the_flat_directory() {
        let t = two_mappings().classify("PP987");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].kind, MappingKind::Flat);
        assert_eq!(t[0].dir, PathBuf::from(r"V:\Documents\custpro"));
    }

    /// The precedence bug the `stop` flag exists to prevent. Without it this
    /// code would also match the two-field job rule and probe a nonexistent
    /// `R:\p12345` on every keystroke.
    #[test]
    fn a_dashed_custompro_code_produces_exactly_one_target() {
        for code in ["P12345-001", "PP987-A", "P1-D-0704", "P99-XY-1"] {
            let t = two_mappings().classify(code);
            assert_eq!(t.len(), 1, "{code} routed to {t:?}");
            assert_eq!(t[0].kind, MappingKind::Flat, "{code}");
        }
    }

    #[test]
    fn clearing_stop_merges_across_shares() {
        let routes = Routes::new(
            vec![
                flat_mapping(
                    0,
                    "custompro",
                    r"V:\Documents\custpro",
                    vec![rule("^P[0-9]+", None, false)],
                ),
                job_mapping(
                    1,
                    "jobs",
                    r"R:\",
                    vec![rule(r"^([A-Z0-9]+)-([A-Z0-9]+)$", Some("${1}"), false)],
                ),
            ],
            ConfigSource::BuiltIn,
        );
        let t = routes.classify("P12345-001");
        assert_eq!(t.len(), 2, "{t:?}");
        assert_eq!(t[0].kind, MappingKind::Flat);
        assert_eq!(t[1].dir, PathBuf::from(r"R:\p12345"));
    }

    /// Unconditional, and not governed by `stop`: two rules in one mapping
    /// must never both produce a target.
    #[test]
    fn the_first_matching_rule_in_a_mapping_wins() {
        let t = two_mappings().classify("11-D-0704");
        assert_eq!(t.len(), 1);
        assert_eq!(
            t[0].dir,
            PathBuf::from(r"R:\11d"),
            "the more specific rule listed first should win"
        );
    }

    #[test]
    fn an_unroutable_code_produces_nothing() {
        assert!(two_mappings().classify("!!!").is_empty());
        assert!(!two_mappings().resolves("!!!"));
        assert!(two_mappings().resolves("11-D-0704"));
    }

    #[test]
    fn a_disabled_mapping_is_skipped() {
        let mut routes = two_mappings();
        let mut mappings: Vec<Mapping> = routes.mappings.into_vec();
        mappings[0].enabled = false;
        routes = Routes::new(mappings, ConfigSource::BuiltIn);

        // The CustomPro rules no longer claim it, so the job rules see it.
        let t = routes.classify("P12345-001");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].kind, MappingKind::JobFolder);
    }

    #[test]
    fn configuration_order_determines_evaluation_order() {
        let routes = Routes::new(
            vec![
                job_mapping(
                    0,
                    "first",
                    r"R:\",
                    vec![rule(r"^([A-Z0-9]+)-([A-Z0-9]+)$", Some("${1}"), false)],
                ),
                job_mapping(
                    1,
                    "second",
                    r"S:\",
                    vec![rule(r"^([A-Z0-9]+)-([A-Z0-9]+)$", Some("${1}"), false)],
                ),
            ],
            ConfigSource::BuiltIn,
        );
        let t = routes.classify("AB12-0704");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].mapping, MappingId(0));
        assert_eq!(t[1].mapping, MappingId(1));
    }

    #[test]
    fn a_rule_whose_folder_expands_unsafely_does_not_match() {
        let routes = Routes::new(
            vec![job_mapping(
                0,
                "jobs",
                r"R:\",
                vec![rule(r"^(\.\.)-([A-Z0-9]+)$", Some("${1}"), false)],
            )],
            ConfigSource::BuiltIn,
        );
        assert!(
            routes.classify("..-0704").is_empty(),
            "an escaping folder must not resolve"
        );
    }

    #[test]
    fn partial_input_resolves_to_the_same_folder_as_it_is_typed() {
        // What lets prefetch de-duplication collapse a burst of keystrokes.
        let routes = two_mappings();
        let a = routes.classify("11-D-0");
        let b = routes.classify("11-D-0704");
        assert_eq!(a[0].dir, b[0].dir);
    }

    #[test]
    fn labels_and_directories_are_reachable_by_id() {
        let routes = two_mappings();
        assert_eq!(routes.label(MappingId(0)), "custompro");
        assert_eq!(routes.dir(MappingId(1)), Some(Path::new(r"R:\")));
        assert_eq!(routes.label(MappingId(99)), "?");
    }

    #[test]
    fn flat_mappings_are_listed_in_order() {
        let routes = two_mappings();
        let flat: Vec<&str> = routes.flat().map(|m| m.name.as_ref()).collect();
        assert_eq!(flat, vec!["custompro"]);
    }

    #[test]
    fn a_bad_pattern_is_rejected_at_construction() {
        assert!(Rule::new("^P[0-9+", None, false, 0).is_err());
    }
}
