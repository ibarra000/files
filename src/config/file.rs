//! Reading `config.toml`.
//!
//! Parsed with `toml_edit` in parse-only mode rather than `toml` plus a serde
//! derive. The reason is error quality: every value keeps a byte span, so a
//! mistake reports `config.toml:34:15` and echoes the offending text instead
//! of a shrug. The cross-field rules - `folder` only on a job mapping, a
//! template referencing only groups its own pattern provides - have to be
//! hand-written either way, and this keeps `syn` out of the build.
//!
//! # A bad configuration stops the program
//!
//! It does not fall back to defaults. Falling back would mean a mistyped path
//! silently searches the built-in location: the user types a code, sees "no
//! matches", and concludes the job has no files. There is no symptom to
//! notice. Refusing puts the error in front of the person who just edited the
//! file, which is the only moment anyone can act on it - and `--no-config`
//! gets them working again immediately.
//!
//! All errors are reported at once. Validation is offline and cheap; making
//! someone fix four typos in four runs is gratuitous.

use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::{ImDocument, Item, Value};

use crate::paths::{CaseFold, ConfigSource, Mapping, MappingId, MappingKind, Routes, Rule};
use crate::util::winpath;

/// The file written on first run, and the compiled-in fallback.
///
/// One source of truth: [`crate::config::default_routes`] parses this through
/// the ordinary loader, so what ships and what is written cannot drift.
pub const DEFAULT_CONFIG_TOML: &str = include_str!("../../assets/default_config.toml");

/// The only format version this build understands.
pub const CONFIG_VERSION: i64 = 1;

/// What went wrong, and exactly where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub path: PathBuf,
    /// One-based line and column, when the problem has a single location.
    pub loc: Option<(usize, usize)>,
    pub mapping: Option<String>,
    pub rule: Option<usize>,
    pub message: String,
    /// The offending text, echoed so a rule index rarely has to be counted.
    pub snippet: Option<String>,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        if let Some((line, col)) = self.loc {
            write!(f, ":{line}:{col}")?;
        }
        writeln!(f)?;
        write!(f, "  ")?;
        if let Some(name) = &self.mapping {
            write!(f, "mapping {name:?}")?;
            if let Some(rule) = self.rule {
                write!(f, ", rule {rule}")?;
            }
            write!(f, ": ")?;
        }
        write!(f, "{}", self.message)?;
        if let Some(snippet) = &self.snippet {
            write!(f, "\n    {snippet}")?;
        }
        Ok(())
    }
}

/// Formats a whole batch for the terminal.
pub fn report(errors: &[ConfigError]) -> String {
    let mut out = String::new();
    for e in errors {
        out.push_str("files: ");
        out.push_str(&e.to_string());
        out.push('\n');
    }
    out.push_str(&format!(
        "files: {} error{}; the configuration was not applied.\n",
        errors.len(),
        if errors.len() == 1 { "" } else { "s" }
    ));
    out.push_str(
        "try `files --check-config` after editing, or `files --no-config` to run on the \
         built-in defaults\n",
    );
    out
}

/// Collects errors while walking the document.
struct Ctx<'a> {
    path: PathBuf,
    text: &'a str,
    errors: Vec<ConfigError>,
}

impl Ctx<'_> {
    fn line_col(&self, offset: usize) -> (usize, usize) {
        let upto = &self.text[..offset.min(self.text.len())];
        let line = upto.matches('\n').count() + 1;
        let col = upto
            .rsplit('\n')
            .next()
            .map(|l| l.chars().count())
            .unwrap_or(0)
            + 1;
        (line, col)
    }

    fn loc(&self, span: Option<std::ops::Range<usize>>) -> Option<(usize, usize)> {
        span.map(|s| self.line_col(s.start))
    }

    fn err(
        &mut self,
        span: Option<std::ops::Range<usize>>,
        mapping: Option<&str>,
        rule: Option<usize>,
        message: impl Into<String>,
        snippet: Option<String>,
    ) {
        let loc = self.loc(span);
        self.errors.push(ConfigError {
            path: self.path.clone(),
            loc,
            mapping: mapping.map(str::to_string),
            rule,
            message: message.into(),
            snippet,
        });
    }
}

/// Keys accepted at each level. Anything else is an error: a typo like
/// `enable = false` that is quietly ignored leaves someone searching a share
/// they believe they switched off.
const MAPPING_KEYS: &[&str] = &["name", "path", "kind", "enabled", "stop", "case", "rules"];
const RULE_KEYS: &[&str] = &["pattern", "folder", "stop"];
const SETTINGS_KEYS: &[&str] = &[
    "enum_strategy",
    "matcher",
    "server_filter",
    "persist",
    "cache_dir",
    "history",
    "viewer",
    "pdf_viewer",
];
const ROOT_KEYS: &[&str] = &["version", "mapping", "settings"];

/// Global options a config file may carry. Applied under the environment.
#[derive(Debug, Clone, Default)]
pub struct FileSettings {
    pub enum_strategy: Option<String>,
    pub matcher: Option<String>,
    pub server_filter: Option<bool>,
    pub persist: Option<bool>,
    pub cache_dir: Option<PathBuf>,
    pub history: Option<bool>,
    pub viewer: Option<String>,
    pub pdf_viewer: Option<PathBuf>,
}

/// A parsed configuration.
#[derive(Debug)]
pub struct ParsedConfig {
    pub routes: Routes,
    pub settings: FileSettings,
}

/// Parses configuration text.
///
/// `path` is used only for error messages, so this is fully testable without
/// a filesystem.
pub fn parse(
    text: &str,
    path: &Path,
    source: ConfigSource,
) -> Result<ParsedConfig, Vec<ConfigError>> {
    let doc: ImDocument<String> = match ImDocument::parse(text.to_string()) {
        Ok(d) => d,
        Err(e) => {
            let loc = e.span().map(|s| {
                let upto = &text[..s.start.min(text.len())];
                (
                    upto.matches('\n').count() + 1,
                    upto.rsplit('\n')
                        .next()
                        .map(|l| l.chars().count())
                        .unwrap_or(0)
                        + 1,
                )
            });
            return Err(vec![ConfigError {
                path: path.to_path_buf(),
                loc,
                mapping: None,
                rule: None,
                message: e.message().to_string(),
                snippet: None,
            }]);
        }
    };

    let mut ctx = Ctx {
        path: path.to_path_buf(),
        text,
        errors: Vec::new(),
    };

    for (key, item) in doc.iter() {
        if !ROOT_KEYS.contains(&key) {
            ctx.err(
                item.span(),
                None,
                None,
                format!(
                    "unknown key {key:?} (expected one of {})",
                    ROOT_KEYS.join(", ")
                ),
                None,
            );
        }
    }

    match doc.get("version").and_then(Item::as_integer) {
        Some(CONFIG_VERSION) => {}
        Some(other) => ctx.err(
            doc.get("version").and_then(Item::span),
            None,
            None,
            format!("unsupported version {other}; this build understands version {CONFIG_VERSION}"),
            None,
        ),
        None => ctx.err(
            None,
            None,
            None,
            format!("missing `version` (expected {CONFIG_VERSION})"),
            None,
        ),
    }

    let mappings = parse_mappings(&doc, &mut ctx);
    let settings = parse_settings(&doc, &mut ctx);

    if mappings.iter().filter(|m| m.enabled).count() == 0 {
        ctx.err(
            None,
            None,
            None,
            "no enabled mappings; nothing could ever be searched",
            None,
        );
    }

    // Only one flat mapping can be indexed so far: there is a single snapshot
    // slot, so a second one would be searched against the first one's listing
    // and quietly return another share's files. Refusing is the honest
    // response until the index is keyed per mapping.
    let flat: Vec<&str> = mappings
        .iter()
        .filter(|m| m.enabled && m.kind == MappingKind::Flat)
        .map(|m| m.name.as_ref())
        .collect();
    if flat.len() > 1 {
        ctx.err(
            None,
            None,
            None,
            format!(
                "{} enabled flat mappings ({}), but only one can be indexed in this build; \
                 disable all but one, or make the others kind = \"job-folder\"",
                flat.len(),
                flat.join(", ")
            ),
            None,
        );
    }

    if ctx.errors.is_empty() {
        Ok(ParsedConfig {
            routes: Routes::new(mappings, source),
            settings,
        })
    } else {
        Err(ctx.errors)
    }
}

fn parse_mappings(doc: &ImDocument<String>, ctx: &mut Ctx<'_>) -> Vec<Mapping> {
    let Some(tables) = doc.get("mapping").and_then(Item::as_array_of_tables) else {
        ctx.err(None, None, None, "no `[[mapping]]` entries", None);
        return Vec::new();
    };

    let mut mappings: Vec<Mapping> = Vec::with_capacity(tables.len());
    let mut seen: Vec<String> = Vec::new();

    for (index, table) in tables.iter().enumerate() {
        let span = table.span();
        let name = table
            .get("name")
            .and_then(Item::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let label = if name.is_empty() {
            format!("#{index}")
        } else {
            name.clone()
        };

        if name.is_empty() {
            ctx.err(
                span.clone(),
                Some(&label),
                None,
                "missing or empty `name`",
                None,
            );
        } else if seen.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
            ctx.err(
                span.clone(),
                Some(&label),
                None,
                "duplicate mapping name",
                None,
            );
        }
        seen.push(name.clone());

        for (key, item) in table.iter() {
            if !MAPPING_KEYS.contains(&key) {
                ctx.err(
                    item.span(),
                    Some(&label),
                    None,
                    format!("unknown key {key:?}"),
                    None,
                );
            }
        }

        let raw_path = table
            .get("path")
            .and_then(Item::as_str)
            .unwrap_or("")
            .trim();
        if raw_path.is_empty() {
            ctx.err(
                span.clone(),
                Some(&label),
                None,
                "missing or empty `path`",
                None,
            );
        }

        let kind = match table.get("kind").and_then(Item::as_str) {
            Some(k) => match MappingKind::parse(k) {
                Some(k) => k,
                None => {
                    ctx.err(
                        table.get("kind").and_then(Item::span),
                        Some(&label),
                        None,
                        format!("unknown kind {k:?} (expected \"flat\" or \"job-folder\")"),
                        None,
                    );
                    MappingKind::JobFolder
                }
            },
            None => {
                ctx.err(
                    span.clone(),
                    Some(&label),
                    None,
                    "missing `kind` (expected \"flat\" or \"job-folder\")",
                    None,
                );
                MappingKind::JobFolder
            }
        };

        let case = match table.get("case").and_then(Item::as_str) {
            Some(c) => CaseFold::parse(c).unwrap_or_else(|| {
                ctx.err(
                    table.get("case").and_then(Item::span),
                    Some(&label),
                    None,
                    format!("unknown case {c:?} (expected \"lower\", \"upper\" or \"preserve\")"),
                    None,
                );
                CaseFold::Lower
            }),
            None => CaseFold::Lower,
        };

        let enabled = table.get("enabled").and_then(Item::as_bool).unwrap_or(true);
        let mapping_stop = table.get("stop").and_then(Item::as_bool).unwrap_or(false);

        let before = ctx.errors.len();
        let rules = parse_rules(table, kind, mapping_stop, &label, ctx);
        // An indexed mapping needs no rules: every file under it is already
        // known, so there is nothing to deduce and a code that matches nothing
        // simply returns nothing. A job-folder mapping is the opposite - its
        // rules are the only thing that can turn a code into a directory, so
        // without them it could never match anything at all.
        //
        // Only complain when the list is genuinely empty, not when every rule
        // was individually rejected: that would report the same mistake twice
        // and bury the message that matters.
        if rules.is_empty() && !kind.is_indexed() && ctx.errors.len() == before {
            ctx.err(
                span.clone(),
                Some(&label),
                None,
                "no `[[mapping.rules]]`; a job-folder mapping cannot resolve a \
                 code without them. An indexed mapping (kind = \"flat\" or \
                 kind = \"tree\") needs none.",
                None,
            );
        }

        mappings.push(Mapping {
            id: MappingId(index as u16),
            name: label.into(),
            path: winpath::normalise_root(Path::new(raw_path)),
            kind,
            enabled,
            case,
            rules: rules.into_boxed_slice(),
        });
    }

    mappings
}

fn parse_rules(
    table: &toml_edit::Table,
    kind: MappingKind,
    mapping_stop: bool,
    label: &str,
    ctx: &mut Ctx<'_>,
) -> Vec<Rule> {
    let Some(entries) = table.get("rules").and_then(Item::as_array_of_tables) else {
        return Vec::new();
    };

    let mut rules = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        for (key, item) in entry.iter() {
            if !RULE_KEYS.contains(&key) {
                ctx.err(
                    item.span(),
                    Some(label),
                    Some(index),
                    format!("unknown key {key:?}"),
                    None,
                );
            }
        }

        let Some(pattern) = entry.get("pattern").and_then(Item::as_str) else {
            ctx.err(
                entry.span(),
                Some(label),
                Some(index),
                "missing `pattern`",
                None,
            );
            continue;
        };

        let folder = entry.get("folder").and_then(Item::as_str);
        match (kind, folder) {
            (MappingKind::Flat, Some(_)) => {
                ctx.err(
                    entry.get("folder").and_then(Item::span),
                    Some(label),
                    Some(index),
                    "`folder` is only meaningful on a job-folder mapping (is `kind` wrong?)",
                    None,
                );
            }
            (MappingKind::JobFolder, None) => {
                ctx.err(
                    entry.span(),
                    Some(label),
                    Some(index),
                    "missing `folder`; without it the code would resolve to the share root",
                    None,
                );
                continue;
            }
            _ => {}
        }

        let stop = entry
            .get("stop")
            .and_then(Item::as_bool)
            .unwrap_or(mapping_stop);

        match Rule::new(pattern, folder, stop, index as u16) {
            Ok(rule) => {
                // An unsatisfiable reference expands to nothing, the folder
                // comes out empty, and the search would fall back to the
                // entire share. Silent at runtime, so it is caught here.
                let missing = rule.unsatisfiable_refs();
                if !missing.is_empty() {
                    ctx.err(
                        entry.get("folder").and_then(Item::span),
                        Some(label),
                        Some(index),
                        format!(
                            "`folder` references {} which the pattern does not capture",
                            missing.join(", ")
                        ),
                        folder.map(str::to_string),
                    );
                }
                rules.push(rule);
            }
            Err(e) => ctx.err(
                entry.get("pattern").and_then(Item::span),
                Some(label),
                Some(index),
                format!("pattern does not compile: {e}"),
                Some(pattern.to_string()),
            ),
        }
    }
    rules
}

fn parse_settings(doc: &ImDocument<String>, ctx: &mut Ctx<'_>) -> FileSettings {
    let mut out = FileSettings::default();
    let Some(item) = doc.get("settings") else {
        return out;
    };
    // `settings = 3`, or `[[settings]]` for `[settings]`, used to fall through
    // here and default *every* setting with nothing said. That is the failure
    // this file refuses everywhere else: the user writes a configuration, it is
    // silently ignored, and there is no symptom to notice.
    let Some(table) = item.as_table() else {
        ctx.err(
            item.span(),
            None,
            None,
            "`settings` must be a table written as [settings]",
            None,
        );
        return out;
    };
    for (key, item) in table.iter() {
        if !SETTINGS_KEYS.contains(&key) {
            ctx.err(
                item.span(),
                None,
                None,
                format!("unknown setting {key:?}"),
                None,
            );
            continue;
        }
        let value = item.as_value();
        match key {
            "enum_strategy" => {
                out.enum_strategy = value.and_then(Value::as_str).map(str::to_string)
            }
            "matcher" => out.matcher = value.and_then(Value::as_str).map(str::to_string),
            "server_filter" => out.server_filter = value.and_then(Value::as_bool),
            "persist" => out.persist = value.and_then(Value::as_bool),
            "cache_dir" => out.cache_dir = value.and_then(Value::as_str).map(PathBuf::from),
            "history" => out.history = value.and_then(Value::as_bool),
            "viewer" => {
                let raw = value.and_then(Value::as_str);
                // Rejected here rather than ignored later. A misspelt viewer
                // that silently fell back would leave someone pressing Enter
                // and getting the wrong program with nothing to explain it.
                if let Some(v) = raw
                    && crate::config::ViewerKind::parse(v).is_none()
                {
                    ctx.err(
                        item.span(),
                        None,
                        None,
                        format!("unknown viewer {v:?} (expected \"pdf\" or \"avwin\")"),
                        None,
                    );
                }
                out.viewer = raw.map(str::to_string);
            }
            "pdf_viewer" => out.pdf_viewer = value.and_then(Value::as_str).map(PathBuf::from),
            _ => {}
        }
    }
    out
}

// --- locating and creating the file ---------------------------------------

/// `%APPDATA%\files\config.toml`.
///
/// Roaming, unlike the index cache in `%LOCALAPPDATA%`: settings should follow
/// the user between machines, a memory-mapped index should not.
pub fn default_config_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let p = PathBuf::from(appdata);
    if p.as_os_str().is_empty() {
        return None;
    }
    Some(p.join("files").join("config.toml"))
}

/// Writes the shipped default to `path` if nothing is there yet.
///
/// `create_new` so two instances starting together cannot interleave: the
/// loser simply reads what the winner wrote. Failure is not fatal - a
/// read-only profile means no file, and the built-in defaults are identical
/// anyway.
pub fn write_default_if_absent(path: &Path) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(DEFAULT_CONFIG_TOML.as_bytes())?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// Loads configuration from `path`.
pub fn load_file(path: &Path, explicit: bool) -> Result<ParsedConfig, Vec<ConfigError>> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        vec![ConfigError {
            path: path.to_path_buf(),
            loc: None,
            mapping: None,
            rule: None,
            message: format!("could not be read: {e}"),
            snippet: None,
        }]
    })?;
    let source = if explicit {
        ConfigSource::Explicit(path.to_path_buf())
    } else {
        ConfigSource::Default(path.to_path_buf())
    };
    parse(&text, path, source)
}

/// The compiled-in defaults, parsed through the ordinary loader.
pub fn builtin() -> ParsedConfig {
    parse(
        DEFAULT_CONFIG_TOML,
        Path::new("<built-in>"),
        ConfigSource::BuiltIn,
    )
    .expect("the shipped default configuration must parse")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> &'static Path {
        Path::new(r"C:\cfg.toml")
    }

    fn parse_ok(text: &str) -> ParsedConfig {
        match parse(text, p(), ConfigSource::BuiltIn) {
            Ok(c) => c,
            Err(errs) => panic!("expected success, got:\n{}", report(&errs)),
        }
    }

    fn parse_err(text: &str) -> Vec<ConfigError> {
        match parse(text, p(), ConfigSource::BuiltIn) {
            Ok(_) => panic!("expected a rejection"),
            Err(e) => e,
        }
    }

    fn messages(errs: &[ConfigError]) -> String {
        errs.iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    const MINIMAL: &str = r#"
version = 1

[[mapping]]
name = "jobs"
path = 'R:\'
kind = "job-folder"

  [[mapping.rules]]
  pattern = '^([A-Z0-9]+)-([A-Z0-9]+)$'
  folder  = '${1}'
"#;

    // --- the shipped default ------------------------------------------------

    #[test]
    fn the_shipped_default_parses() {
        let c = builtin();
        assert_eq!(c.routes.enabled().count(), 2);
        assert_eq!(c.routes.flat().count(), 1);
    }

    #[test]
    fn the_shipped_default_points_at_the_corrected_directory() {
        let c = builtin();
        let flat = c.routes.flat().next().unwrap();
        assert_eq!(flat.path, Path::new(r"V:\Documents\custpro"));
    }

    /// The precedence that keeps a dashed CustomPro code out of the job share.
    #[test]
    fn the_shipped_default_stops_on_custompro() {
        let c = builtin();
        assert_eq!(c.routes.classify("P12345-001").len(), 1);
    }

    /// Guards the one silent way to break this file: writing a pattern as a
    /// basic string, where `\d` becomes `d` and still parses.
    #[test]
    fn the_shipped_default_uses_literal_strings_for_paths_and_patterns() {
        for line in DEFAULT_CONFIG_TOML.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            for key in ["path", "pattern", "folder"] {
                if let Some(rest) = line.strip_prefix(key) {
                    let rest = rest.trim_start().trim_start_matches('=').trim_start();
                    assert!(
                        rest.starts_with('\''),
                        "{key} must use a literal string: {line}"
                    );
                }
            }
        }
    }

    // --- acceptance ----------------------------------------------------------

    #[test]
    fn parses_a_minimal_configuration() {
        let c = parse_ok(MINIMAL);
        assert_eq!(c.routes.enabled().count(), 1);
        let t = c.routes.classify("AB12-0704");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].dir, Path::new(r"R:\ab12"));
    }

    #[test]
    fn a_mapping_is_enabled_unless_it_says_otherwise() {
        let c = parse_ok(MINIMAL);
        assert!(c.routes.all()[0].enabled);

        let text = MINIMAL.replace(
            "kind = \"job-folder\"",
            "kind = \"job-folder\"\nenabled = false",
        );
        match parse(&text, p(), ConfigSource::BuiltIn) {
            Ok(_) => panic!("a config with nothing enabled should be rejected"),
            Err(e) => assert!(
                messages(&e).contains("no enabled mappings"),
                "{}",
                messages(&e)
            ),
        }
    }

    #[test]
    fn reads_the_global_settings_table() {
        let text = format!("{MINIMAL}\n[settings]\nserver_filter = true\npersist = false\n");
        let c = parse_ok(&text);
        assert_eq!(c.settings.server_filter, Some(true));
        assert_eq!(c.settings.persist, Some(false));
        assert_eq!(c.settings.matcher, None);
    }

    #[test]
    fn a_path_is_normalised_on_the_way_in() {
        let text = MINIMAL.replace(r"path = 'R:\'", r"path = 'R:\jobs\'");
        let c = parse_ok(&text);
        assert_eq!(c.routes.all()[0].path, Path::new(r"R:\jobs"));
    }

    // --- rejection -----------------------------------------------------------

    #[test]
    fn a_syntax_error_reports_a_line_and_column() {
        let errs = parse_err("version = 1\n[[mapping]\nname = 'x'\n");
        let msg = messages(&errs);
        assert!(msg.contains(r"C:\cfg.toml:"), "{msg}");
    }

    #[test]
    fn an_uncompilable_pattern_names_the_mapping_the_rule_and_the_reason() {
        let text = MINIMAL.replace(r"'^([A-Z0-9]+)-([A-Z0-9]+)$'", "'^P[0-9+'");
        let errs = parse_err(&text);
        let msg = messages(&errs);
        assert!(msg.contains(r"C:\cfg.toml:"), "{msg}");
        assert!(msg.contains("mapping \"jobs\""), "{msg}");
        assert!(msg.contains("rule 0"), "{msg}");
        assert!(msg.contains("^P[0-9+"), "{msg}");
    }

    #[test]
    fn a_duplicate_name_is_rejected() {
        let text = format!(
            "{MINIMAL}\n{}",
            MINIMAL.trim_start_matches("\nversion = 1\n")
        );
        let errs = parse_err(&text);
        assert!(messages(&errs).contains("duplicate mapping name"));
    }

    #[test]
    fn folder_on_a_flat_mapping_is_rejected() {
        let text = MINIMAL.replace("kind = \"job-folder\"", "kind = \"flat\"");
        let errs = parse_err(&text);
        assert!(messages(&errs).contains("only meaningful on a job-folder"));
    }

    #[test]
    fn a_job_mapping_without_a_folder_is_rejected() {
        let text = MINIMAL.replace("  folder  = '${1}'\n", "");
        let errs = parse_err(&text);
        let msg = messages(&errs);
        assert!(msg.contains("missing `folder`"), "{msg}");
        assert!(msg.contains("share root"), "{msg}");
    }

    /// The silent one: an unsatisfiable reference expands to nothing and the
    /// search falls back to the whole share.
    #[test]
    fn a_folder_referencing_a_missing_group_is_rejected() {
        let text = MINIMAL.replace("folder  = '${1}'", "folder  = '${3}'");
        let errs = parse_err(&text);
        let msg = messages(&errs);
        assert!(msg.contains("$3"), "{msg}");
        assert!(msg.contains("does not capture"), "{msg}");
    }

    #[test]
    fn a_bare_reference_that_swallows_text_is_rejected() {
        let text = MINIMAL.replace("folder  = '${1}'", "folder  = '$1x'");
        let errs = parse_err(&text);
        assert!(messages(&errs).contains("$1x"));
    }

    #[test]
    fn an_empty_path_is_rejected() {
        let text = MINIMAL.replace(r"path = 'R:\'", "path = ''");
        let errs = parse_err(&text);
        assert!(messages(&errs).contains("empty `path`"));
    }

    /// A job-folder mapping's rules are the only thing that can turn a code
    /// into a directory, so without them it can never match anything.
    #[test]
    fn a_job_folder_mapping_with_no_rules_is_rejected() {
        let text =
            "version = 1\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\nkind = \"job-folder\"\n";
        let errs = parse_err(text);
        assert!(messages(&errs).contains("no `[[mapping.rules]]`"));
    }

    /// An indexed mapping is the opposite: every file under it is already
    /// known, so there is nothing for a rule to deduce.
    #[test]
    fn an_indexed_mapping_needs_no_rules() {
        for kind in ["flat", "tree"] {
            let text = format!(
                "version = 1\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\nkind = \"{kind}\"\n"
            );
            let parsed = parse(&text, p(), ConfigSource::BuiltIn)
                .unwrap_or_else(|e| panic!("{kind} was rejected: {:?}", messages(&e)));
            assert_eq!(parsed.routes.enabled().count(), 1);
        }
    }

    /// And it matches every query, rather than being declared unroutable.
    #[test]
    fn a_rule_less_indexed_mapping_matches_anything() {
        let text = "version = 1\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\nkind = \"tree\"\n";
        let parsed = parse(text, p(), ConfigSource::BuiltIn).expect("valid");
        for code in ["11-D-0704", "anything at all", "zzz"] {
            let targets = parsed.routes.classify(code);
            assert_eq!(targets.len(), 1, "{code:?} reached nothing");
            assert_eq!(targets[0].kind, MappingKind::Tree);
            assert_eq!(targets[0].dir, std::path::Path::new("R:\\"));
        }
    }

    #[test]
    fn an_unknown_key_is_rejected_rather_than_ignored() {
        // `enable` instead of `enabled` would otherwise leave a share the
        // user believes they turned off still being searched.
        let text = MINIMAL.replace(
            "kind = \"job-folder\"",
            "kind = \"job-folder\"\nenable = false",
        );
        let errs = parse_err(&text);
        assert!(messages(&errs).contains("unknown key \"enable\""));
    }

    #[test]
    fn reads_the_viewer_and_its_override_from_the_settings_table() {
        let text = format!(
            "{MINIMAL}\n[settings]\nviewer = \"avwin\"\npdf_viewer = 'C:\\tools\\sumatra.exe'\n"
        );
        let c = parse_ok(&text);
        assert_eq!(c.settings.viewer.as_deref(), Some("avwin"));
        assert_eq!(
            c.settings.pdf_viewer.as_deref(),
            Some(Path::new(r"C:\tools\sumatra.exe"))
        );
    }

    /// A misspelt viewer that fell back silently would leave someone pressing
    /// Enter and getting the wrong program, with nothing on screen to say so.
    #[test]
    fn an_unknown_viewer_value_is_rejected_with_its_text() {
        let text = format!("{MINIMAL}\n[settings]\nviewer = \"notepad\"\n");
        let errs = parse_err(&text);
        let msg = messages(&errs);
        assert!(msg.contains("notepad"), "{msg}");
        assert!(
            msg.contains("avwin"),
            "the message must say what is allowed: {msg}"
        );
    }

    /// The viewer key ships uncommented because F2 rewrites it in place.
    #[test]
    fn the_shipped_default_sets_a_viewer_the_writer_can_replace() {
        let c = builtin();
        assert_eq!(c.settings.viewer.as_deref(), Some("pdf"));
    }

    /// Silently defaulting every setting is the failure this file refuses
    /// everywhere else - and it also fed a panic in the config writer.
    #[test]
    fn a_settings_that_is_not_a_table_is_rejected() {
        for bad in ["settings = 3", "settings = \"pdf\"", "settings = []"] {
            // Root keys must precede the first table header, so this goes
            // beside `version` rather than after the mapping.
            let errs = parse_err(&MINIMAL.replace(
                "version = 1",
                &format!(
                    "version = 1
{bad}"
                ),
            ));
            assert!(
                messages(&errs).contains("must be a table"),
                "{bad}: {}",
                messages(&errs)
            );
        }
    }

    #[test]
    fn an_unknown_setting_is_rejected() {
        let text = format!("{MINIMAL}\n[settings]\nserver_fitler = true\n");
        let errs = parse_err(&text);
        assert!(messages(&errs).contains("unknown setting"));
    }

    /// A second flat mapping would be searched against the first one's
    /// snapshot and return another share's files. Refused rather than risked.
    #[test]
    fn a_second_enabled_flat_mapping_is_rejected_by_name() {
        let text = r#"
version = 1

[[mapping]]
name = "custompro"
path = 'V:\Documents\custpro'
kind = "flat"
  [[mapping.rules]]
  pattern = '^P[0-9]+'

[[mapping]]
name = "archive"
path = 'W:\archive'
kind = "flat"
  [[mapping.rules]]
  pattern = '^AR[0-9]+'
"#;
        let errs = parse_err(text);
        let msg = messages(&errs);
        assert!(msg.contains("only one can be indexed"), "{msg}");
        assert!(msg.contains("custompro"), "{msg}");
        assert!(msg.contains("archive"), "{msg}");
        assert!(
            msg.contains("job-folder"),
            "the message should say what to do: {msg}"
        );
    }

    #[test]
    fn a_disabled_second_flat_mapping_is_fine() {
        let text = r#"
version = 1

[[mapping]]
name = "custompro"
path = 'V:\Documents\custpro'
kind = "flat"
  [[mapping.rules]]
  pattern = '^P[0-9]+'

[[mapping]]
name = "archive"
path = 'W:\archive'
kind = "flat"
enabled = false
  [[mapping.rules]]
  pattern = '^AR[0-9]+'
"#;
        let c = parse_ok(text);
        assert_eq!(c.routes.flat().count(), 1);
        assert_eq!(c.routes.all().len(), 2);
    }

    /// Several job-folder mappings are supported today.
    #[test]
    fn several_job_folder_mappings_are_accepted() {
        let text = r#"
version = 1

[[mapping]]
name = "jobs"
path = 'R:\'
kind = "job-folder"
  [[mapping.rules]]
  pattern = '^([A-Z0-9]+)-([A-Z0-9]+)$'
  folder  = '${1}'

[[mapping]]
name = "old-jobs"
path = 'S:\archive'
kind = "job-folder"
  [[mapping.rules]]
  pattern = '^([A-Z0-9]+)-([A-Z0-9]+)$'
  folder  = '${1}'
"#;
        let c = parse_ok(text);
        assert_eq!(c.routes.enabled().count(), 2);
        // Both match, in configuration order.
        let t = c.routes.classify("AB12-0704");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].dir, Path::new(r"R:\ab12"));
        assert_eq!(t[1].dir, Path::new(r"S:\archive\ab12"));
    }

    #[test]
    fn an_unknown_kind_is_rejected() {
        let text = MINIMAL.replace("kind = \"job-folder\"", "kind = \"folder\"");
        let errs = parse_err(&text);
        assert!(messages(&errs).contains("unknown kind"));
    }

    #[test]
    fn a_missing_or_wrong_version_is_rejected() {
        let errs = parse_err(&MINIMAL.replace("version = 1", "version = 9"));
        assert!(messages(&errs).contains("unsupported version 9"));

        let errs = parse_err(&MINIMAL.replace("version = 1\n", ""));
        assert!(messages(&errs).contains("missing `version`"));
    }

    #[test]
    fn a_file_with_no_mappings_is_rejected() {
        let errs = parse_err("version = 1\n");
        assert!(messages(&errs).contains("no `[[mapping]]`"));
    }

    /// Fixing four typos should take one run, not four.
    #[test]
    fn every_error_is_reported_at_once() {
        let text = "
version = 1

[[mapping]]
name = \"a\"
path = ''
kind = \"nonsense\"

  [[mapping.rules]]
  pattern = '^P[0-9+'
";
        let errs = parse_err(text);
        assert!(
            errs.len() >= 3,
            "expected several errors, got {}",
            messages(&errs)
        );
    }

    #[test]
    fn the_report_tells_the_user_how_to_recover() {
        let errs = parse_err("version = 1\n");
        let r = report(&errs);
        assert!(r.contains("--check-config"), "{r}");
        assert!(r.contains("--no-config"), "{r}");
        assert!(r.contains("was not applied"), "{r}");
    }

    // --- first-run write -----------------------------------------------------

    #[test]
    fn writes_the_default_once_and_then_leaves_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        assert!(write_default_if_absent(&path).unwrap());
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, DEFAULT_CONFIG_TOML);

        std::fs::write(&path, "version = 1\n# edited\n").unwrap();
        assert!(!write_default_if_absent(&path).unwrap());
        assert!(std::fs::read_to_string(&path).unwrap().contains("edited"));
    }

    #[test]
    fn the_file_that_is_written_is_the_file_that_is_compiled_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_default_if_absent(&path).unwrap();

        let from_disk = load_file(&path, false).expect("the written default must load");
        let from_memory = builtin();
        assert_eq!(
            from_disk.routes.names(),
            from_memory.routes.names(),
            "the written and compiled-in defaults must not drift"
        );
    }

    #[test]
    fn a_missing_explicit_file_is_an_error() {
        let errs = load_file(Path::new(r"C:\definitely-not-here-8812.toml"), true).unwrap_err();
        assert!(messages(&errs).contains("could not be read"));
    }
}
