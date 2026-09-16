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

use crate::paths::{ConfigSource, Mapping, MappingId, MappingKind, RefreshPolicy, Routes};
use crate::util::winpath;

/// The file written on first run, and the compiled-in fallback.
///
/// One source of truth: [`crate::config::default_routes`] parses this through
/// the ordinary loader, so what ships and what is written cannot drift.
pub const DEFAULT_CONFIG_TOML: &str = include_str!("../../assets/default_config.toml");

/// What to tell someone whose configuration predates the deletion of routing.
///
/// Its own constant because it is the one error message in this file that has
/// to be reachable from a test by name rather than by fragment: getting it
/// wrong means someone reads it, edits the file as instructed, and is refused
/// again.
const V1_MIGRATION: &str = concat!(
    "this is a version 1 configuration, written when a job code was matched against ",
    "patterns to work out which folder to look in. Both shares are indexed now, so there ",
    "is nothing left to match: delete every `[[mapping.rules]]` block and any `case` or ",
    "`stop` key, give each mapping `kind = \"flat\"` or `kind = \"tree\"`, and set ",
    "`version = 2`. Deleting the file also works - a fresh one is written on the next run.",
);

/// The only format version this build understands.
///
/// Bumped to 2 when routing was removed. A version 1 file is not merely
/// missing a key or two - every `[[mapping.rules]]` in it describes work the
/// program no longer does - so it gets one clear instruction rather than a
/// wall of "unknown key".
pub const CONFIG_VERSION: i64 = 2;

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
        "try `files-cli --check-config` after editing, or `--no-config` to run on the \
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
const MAPPING_KEYS: &[&str] = &["name", "path", "kind", "enabled", "refresh", "depth"];
pub(super) const SETTINGS_KEYS: &[&str] = &[
    "enum_strategy",
    "matcher",
    "server_filter",
    "persist",
    "max_concurrent_scans",
    "stale_notices",
    "auto_hide",
    "pdf_read_only",
    "live_updates",
    "cache_dir",
    "history",
    "hotkey",
    "viewer",
    "pdf_viewer",
    "theme",
    "hide_extensions",
    "hide_system_files",
];
const ROOT_KEYS: &[&str] = &["version", "mapping", "settings"];

/// Global options a config file may carry. Applied under the environment.
#[derive(Debug, Clone, Default)]
pub struct FileSettings {
    pub enum_strategy: Option<String>,
    pub matcher: Option<String>,
    pub server_filter: Option<bool>,
    pub persist: Option<bool>,
    pub max_concurrent_scans: Option<usize>,
    pub stale_notices: Option<bool>,
    pub auto_hide: Option<bool>,
    pub pdf_read_only: Option<bool>,
    pub live_updates: Option<bool>,
    pub cache_dir: Option<PathBuf>,
    pub history: Option<bool>,
    pub hotkey: Option<crate::hotkey::spec::HotkeySpec>,
    pub viewer: Option<String>,
    pub pdf_viewer: Option<PathBuf>,
    pub theme: Option<String>,
    pub hide_extensions: Option<Vec<String>>,
    pub hide_system_files: Option<bool>,
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

    // A version 1 file is reported and nothing else is. Every `rules` block
    // and every `kind = "job-folder"` in it would otherwise produce its own
    // "unknown key" beneath the one message that matters, and a wall of errors
    // reads as a broken file rather than as an out-of-date one.
    if doc.get("version").and_then(Item::as_integer) == Some(1) {
        ctx.err(
            doc.get("version").and_then(Item::span),
            None,
            None,
            V1_MIGRATION,
            None,
        );
        return Err(ctx.errors);
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

    // Two mappings on the same directory are refused rather than merged. The
    // persisted index is keyed by a hash of the path (`persist::MappingKey`),
    // so both actors would write the same cache entry and each cold start
    // would restore whichever wrote last - and every result would appear
    // twice in one merged list.
    for (i, a) in mappings.iter().enumerate() {
        if !a.enabled || !a.kind.is_searched() {
            continue;
        }
        for b in mappings.iter().skip(i + 1) {
            if !b.enabled || !b.kind.is_searched() {
                continue;
            }
            if winpath::same_dir(&a.path, &b.path) {
                ctx.err(
                    None,
                    None,
                    None,
                    format!(
                        "mappings `{}` and `{}` both point at {}; \
                         indexing one directory twice returns every file twice",
                        a.name,
                        b.name,
                        a.path.display()
                    ),
                    None,
                );
                continue;
            }
            // A *tree* mapping containing another indexed mapping walks that
            // one's files as well as its own, so every hit inside appears
            // twice in one merged list and is walked twice on every pass.
            //
            // A flat parent is fine and stays legal: a flat mapping lists only
            // its own directory's entries, so a child mapping's files are not
            // in it to begin with.
            //
            // A *live* parent is fine too, and deliberately so - a walked tree
            // inside a live share is the configuration somebody actually wants,
            // because it indexes the part people work in and leaves the rest to
            // the server. The live pass is what keeps it from returning those
            // files a second time: it skips any directory that is the root of
            // another enabled mapping.
            let nested = if a.kind == MappingKind::Tree && winpath::contains(&a.path, &b.path) {
                Some((a, b))
            } else if b.kind == MappingKind::Tree && winpath::contains(&b.path, &a.path) {
                Some((b, a))
            } else {
                None
            };
            if let Some((outer, inner)) = nested {
                ctx.err(
                    None,
                    None,
                    None,
                    format!(
                        "mapping `{}` ({}) lies inside the walked tree `{}` ({}); \
                         every file under it would be indexed twice and shown twice",
                        inner.name,
                        inner.path.display(),
                        outer.name,
                        outer.path.display()
                    ),
                    None,
                );
            }
        }
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
                        format!("unknown kind {k:?} (expected \"flat\", \"tree\" or \"live\")"),
                        None,
                    );
                    MappingKind::Tree
                }
            },
            None => {
                ctx.err(
                    span.clone(),
                    Some(&label),
                    None,
                    "missing `kind` (expected \"flat\", \"tree\" or \"live\")",
                    None,
                );
                MappingKind::Tree
            }
        };

        let enabled = table.get("enabled").and_then(Item::as_bool).unwrap_or(true);

        // `depth` belongs to a live mapping and to nothing else, and `refresh`
        // to everything else. Each is refused where it does not apply rather
        // than ignored there, for the reason this file refuses an unknown key
        // at all: a setting that quietly does nothing leaves somebody believing
        // it took.
        let depth = match table.get("depth") {
            None => crate::config::DEFAULT_LIVE_DEPTH,
            Some(item) if !kind.is_live() => {
                ctx.err(
                    item.span(),
                    Some(&label),
                    None,
                    "`depth` applies only to a live mapping",
                    None,
                );
                crate::config::DEFAULT_LIVE_DEPTH
            }
            Some(item) => match item.as_integer() {
                Some(n) if (1..=i64::from(crate::config::MAX_LIVE_DEPTH)).contains(&n) => n as u16,
                _ => {
                    ctx.err(
                        item.span(),
                        Some(&label),
                        None,
                        format!(
                            "`depth` must be a whole number from 1 to {}",
                            crate::config::MAX_LIVE_DEPTH
                        ),
                        None,
                    );
                    crate::config::DEFAULT_LIVE_DEPTH
                }
            },
        };

        if kind.is_live()
            && let Some(item) = table.get("refresh")
        {
            ctx.err(
                item.span(),
                Some(&label),
                None,
                "`refresh` applies only to an indexed mapping; a live share is read when it is searched and at no other time",
                None,
            );
        }

        let refresh = match table.get("refresh") {
            None => RefreshPolicy::default_for(kind),
            Some(item) => match item.as_str().and_then(RefreshPolicy::parse) {
                Some(p) => p,
                None => {
                    ctx.err(
                        item.span(),
                        Some(&label),
                        None,
                        "`refresh` must be \"auto\" or \"manual\"",
                        None,
                    );
                    RefreshPolicy::default_for(kind)
                }
            },
        };
        mappings.push(Mapping {
            id: MappingId(index as u16),
            name: label.into(),
            path: winpath::normalise_root(Path::new(raw_path)),
            kind,
            enabled,
            depth,
            refresh,
        });
    }

    mappings
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
            "stale_notices" => out.stale_notices = value.and_then(Value::as_bool),
            "auto_hide" => out.auto_hide = value.and_then(Value::as_bool),
            "pdf_read_only" => out.pdf_read_only = value.and_then(Value::as_bool),
            "max_concurrent_scans" => {
                // Rejected rather than clamped. A zero here means "index
                // nothing, ever", which nobody writes on purpose, and this
                // file's policy is to say so rather than guess.
                match value.and_then(Value::as_integer) {
                    Some(n) if n >= 1 => out.max_concurrent_scans = Some(n as usize),
                    Some(n) => ctx.err(
                        item.span(),
                        None,
                        None,
                        format!("max_concurrent_scans must be at least 1, not {n}"),
                        None,
                    ),
                    None => {}
                }
            }
            "live_updates" => out.live_updates = value.and_then(Value::as_bool),
            "cache_dir" => out.cache_dir = value.and_then(Value::as_str).map(PathBuf::from),
            "history" => out.history = value.and_then(Value::as_bool),
            "hotkey" => {
                // Rejected here rather than ignored later, for the same reason
                // as `viewer` below and more sharply: a hotkey that silently
                // fell back to the default would look exactly like a working
                // one, right up until someone pressed the combination they
                // actually chose and nothing happened.
                if let Some(v) = value.and_then(Value::as_str) {
                    match crate::hotkey::spec::parse(v) {
                        Ok(spec) => out.hotkey = Some(spec),
                        Err(e) => ctx.err(item.span(), None, None, e.detail(), None),
                    }
                }
            }
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
                        format!("unknown viewer {v:?} (expected \"auto\", \"pdf\" or \"avwin\")"),
                        None,
                    );
                }
                out.viewer = raw.map(str::to_string);
            }
            "pdf_viewer" => out.pdf_viewer = value.and_then(Value::as_str).map(PathBuf::from),
            "hide_system_files" => out.hide_system_files = value.and_then(Value::as_bool),
            "hide_extensions" => {
                // One extension may be written bare rather than as an array
                // of one: hiding a single type is a common enough edit that
                // the brackets would only be ceremony.
                let list = match value {
                    Some(v) if v.is_str() => v.as_str().map(|s| vec![s.to_string()]),
                    Some(v) => v.as_array().map(|a| {
                        a.iter()
                            .filter_map(|e| e.as_str().map(str::to_string))
                            .collect()
                    }),
                    None => None,
                };
                // An extension, not a pattern and not a filename. Someone
                // reaching for `*.db` or `Thumbs.db` has written something
                // that would match nothing at all, and a filter that hides
                // nothing is worse than no filter: they would go on believing
                // the file was meant to be there.
                if let Some(v) = &list {
                    for entry in v {
                        let bare = entry.trim().trim_start_matches('.');
                        // Wildcards are checked before the dot, because
                        // `*.db` is the likeliest mistake and trips both -
                        // and being told to drop the star is more use than
                        // being told to drop the dot.
                        let bad = if bare.is_empty() {
                            Some("an extension cannot be blank")
                        } else if bare.contains(['*', '?']) {
                            Some("this is not a pattern; write the extension only")
                        } else if bare.contains('.') {
                            Some("write the extension only, as \"db\" or \".db\"")
                        } else if bare.contains(['/', '\\']) {
                            Some("an extension cannot contain a path")
                        } else if !bare.is_ascii() {
                            // `Hidden` drops these, because it cannot judge
                            // one consistently - see its `hides_folded`.
                            // Reported rather than dropped silently: a filter
                            // entry that is quietly ignored is a filter
                            // somebody believes is running.
                            Some("an extension must be ASCII")
                        } else {
                            None
                        };
                        if let Some(why) = bad {
                            ctx.err(
                                item.span(),
                                None,
                                None,
                                format!("hide_extensions entry {entry:?}: {why}"),
                                None,
                            );
                        }
                    }
                }
                out.hide_extensions = list;
            }
            "theme" => {
                let raw = value.and_then(Value::as_str);
                // Rejected here rather than ignored, for the same reason as
                // `viewer` above: a misspelling that fell back silently would
                // leave somebody staring at a panel that is still the colour
                // they were trying to change.
                if let Some(v) = raw
                    && crate::config::ThemeChoice::parse(v).is_none()
                {
                    ctx.err(
                        item.span(),
                        None,
                        None,
                        format!("unknown theme {v:?} (expected \"light\", \"dark\" or \"system\")"),
                        None,
                    );
                }
                out.theme = raw.map(str::to_string);
            }
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
version = 2

[[mapping]]
name = "jobs"
path = 'R:\'
kind = "tree"
"#;

    // --- live mappings ------------------------------------------------------

    const LIVE: &str = r#"
version = 2

[[mapping]]
name = "archive"
path = '\\nas\archive'
kind = "live"
"#;

    #[test]
    fn a_live_mapping_is_accepted_and_is_not_indexed() {
        let c = parse_ok(LIVE);
        let m = &c.routes.all()[0];
        assert_eq!(m.kind, MappingKind::Live);
        assert!(!m.kind.is_indexed());
        assert!(m.kind.is_searched(), "a live share is still searched");
        assert_eq!(m.depth, crate::config::DEFAULT_LIVE_DEPTH);
    }

    #[test]
    fn a_live_mapping_takes_a_depth() {
        let c = parse_ok(&format!("{LIVE}depth = 3\n"));
        assert_eq!(c.routes.all()[0].depth, 3);
    }

    /// A key quietly ignored is this file's stated failure mode: it leaves
    /// somebody believing a setting took.
    #[test]
    fn depth_on_an_indexed_mapping_is_refused_rather_than_ignored() {
        let errs = parse_err(&format!("{MINIMAL}depth = 2\n"));
        assert!(
            messages(&errs).contains("`depth` applies only to a live mapping"),
            "{}",
            messages(&errs)
        );
    }

    #[test]
    fn a_depth_outside_the_range_is_refused_with_the_limit_in_the_message() {
        for bad in ["0", "9", "\"deep\""] {
            let errs = parse_err(&format!("{LIVE}depth = {bad}\n"));
            assert!(
                messages(&errs).contains("`depth` must be a whole number from 1 to"),
                "{bad}: {}",
                messages(&errs)
            );
        }
    }

    /// There is no index for a timer to refresh, so the setting would do
    /// nothing at all.
    #[test]
    fn refresh_on_a_live_mapping_is_refused_because_nothing_reads_it_on_a_timer() {
        let errs = parse_err(&format!("{LIVE}refresh = \"auto\"\n"));
        assert!(
            messages(&errs).contains("`refresh` applies only to an indexed mapping"),
            "{}",
            messages(&errs)
        );
    }

    #[test]
    fn a_live_mapping_defaults_to_never_refreshing() {
        assert!(parse_ok(LIVE).routes.all()[0].refresh.is_manual());
    }

    /// Two mappings on one directory return every hit twice whichever route
    /// each of them is searched by.
    #[test]
    fn two_live_mappings_on_one_directory_are_refused_like_two_indexed_ones() {
        let errs = parse_err(
            r#"
version = 2

[[mapping]]
name = "a"
path = '\\nas\archive'
kind = "live"

[[mapping]]
name = "b"
path = '\\nas\archive'
kind = "live"
"#,
        );
        assert!(!errs.is_empty());
    }

    /// The walk indexes those files and the live query would find them again.
    #[test]
    fn a_live_mapping_inside_a_walked_tree_is_refused() {
        let errs = parse_err(
            r#"
version = 2

[[mapping]]
name = "jobs"
path = 'R:\'
kind = "tree"

[[mapping]]
name = "archive"
path = 'R:\archive'
kind = "live"
"#,
        );
        assert!(
            messages(&errs).contains("lies inside the walked tree"),
            "{}",
            messages(&errs)
        );
    }

    /// The useful configuration, and the reason the rule is asymmetric: index
    /// the subtree people work in, and ask the server about the rest.
    #[test]
    fn a_walked_tree_inside_a_live_share_is_accepted() {
        let c = parse_ok(
            r#"
version = 2

[[mapping]]
name = "archive"
path = 'R:\'
kind = "live"

[[mapping]]
name = "current"
path = 'R:\current'
kind = "tree"
"#,
        );
        assert_eq!(c.routes.all().len(), 2);
    }

    #[test]
    fn an_unknown_kind_names_all_three_it_could_have_been() {
        let errs = parse_err(
            r#"
version = 2

[[mapping]]
name = "jobs"
path = 'R:\'
kind = "nonsense"
"#,
        );
        let m = messages(&errs);
        assert!(m.contains("\"flat\""), "{m}");
        assert!(m.contains("\"tree\""), "{m}");
        assert!(m.contains("\"live\""), "{m}");
    }

    // --- hiding files -------------------------------------------------------

    #[test]
    fn hide_extensions_accepts_an_array() {
        let c = parse_ok(&format!(
            "{MINIMAL}
[settings]
hide_extensions = ['db', '.LNK']
"
        ));
        assert_eq!(
            c.settings.hide_extensions,
            Some(vec!["db".to_string(), ".LNK".to_string()])
        );
    }

    /// Hiding one type is a common enough edit that requiring an array of one
    /// would only be ceremony.
    #[test]
    fn hide_extensions_accepts_a_bare_string() {
        let c = parse_ok(&format!(
            "{MINIMAL}
[settings]
hide_extensions = 'db'
"
        ));
        assert_eq!(c.settings.hide_extensions, Some(vec!["db".to_string()]));
    }

    #[test]
    fn hide_extensions_may_be_emptied_to_hide_nothing() {
        let c = parse_ok(&format!(
            "{MINIMAL}
[settings]
hide_extensions = []
"
        ));
        assert_eq!(c.settings.hide_extensions, Some(Vec::new()));
    }

    /// A pattern or a whole filename would match nothing at all, and a filter
    /// that silently hides nothing is worse than no filter: whoever wrote it
    /// goes on believing the file was meant to be there.
    #[test]
    fn a_pattern_or_a_filename_is_rejected_rather_than_ignored() {
        for bad in ["'*.db'", "'Thumbs.db'", "'db/js'", "''"] {
            let errs = parse_err(&format!(
                "{MINIMAL}
[settings]
hide_extensions = [{bad}]
"
            ));
            let text = messages(&errs);
            assert!(
                text.contains("hide_extensions"),
                "{bad} was accepted or misreported: {text}"
            );
        }
    }

    /// Dropped by `Hidden` because it cannot judge one consistently, so it has
    /// to be reported here - a filter entry that is silently ignored is worse
    /// than one that is refused.
    #[test]
    fn a_non_ascii_extension_is_refused_rather_than_quietly_dropped() {
        let errs = parse_err(&format!(
            "{MINIMAL}\n[settings]\nhide_extensions = ['dé']\n"
        ));
        let text = messages(&errs);
        assert!(text.contains("ASCII"), "{text}");
    }

    #[test]
    fn hide_system_files_is_a_flag() {
        let c = parse_ok(&format!(
            "{MINIMAL}
[settings]
hide_system_files = false
"
        ));
        assert_eq!(c.settings.hide_system_files, Some(false));
    }

    /// Both keys have to be in `SETTINGS_KEYS` or the shipped file that
    /// mentions them stops the program at startup, which is what an unknown
    /// key means here.
    #[test]
    fn both_new_keys_are_known_to_the_allowlist() {
        for key in ["hide_extensions", "hide_system_files"] {
            assert!(
                SETTINGS_KEYS.contains(&key),
                "{key} would be an unknown key"
            );
        }
    }

    // --- the shipped default ------------------------------------------------

    /// The shipped file is the compiled-in default, so this is also the test
    /// that the block it documents actually parses and resolves.
    #[test]
    fn the_shipped_default_hides_the_files_that_prompted_it() {
        let c = builtin();
        let hidden = c.settings.hide_extensions.expect("nothing shipped");
        for ext in ["db", "js", "lnk"] {
            assert!(hidden.iter().any(|e| e == ext), "{ext} is not hidden");
        }
        assert_eq!(c.settings.hide_system_files, Some(true));
    }

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
    /// Every configured share is searched, and the results merge into one
    /// ranked list.
    ///
    /// This used to assert the opposite: a CustomPro code `stop`ped there, so
    /// the job share was never consulted. That existed because routing had to
    /// *choose* a folder, and choosing wrongly meant probing a path that did
    /// not exist on every keystroke. Both shares are indexed now, so there is
    /// nothing to choose between - a code that matches in both legitimately
    /// appears from both.
    fn the_shipped_default_searches_every_share() {
        let c = builtin();
        assert_eq!(c.routes.targets().len(), 2);
    }

    /// And every share is indexed, which is what makes the patterns
    /// unnecessary rather than merely absent.
    #[test]
    fn every_shipped_mapping_is_indexed() {
        for mapping in builtin().routes.all() {
            assert!(
                mapping.kind.is_indexed(),
                "{:?} is not indexed",
                mapping.name
            );
        }
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
        let t = c.routes.targets();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].dir, Path::new(r"R:\"));
    }

    #[test]
    fn a_mapping_is_enabled_unless_it_says_otherwise() {
        let c = parse_ok(MINIMAL);
        assert!(c.routes.all()[0].enabled);

        let text = MINIMAL.replace("kind = \"tree\"", "kind = \"tree\"\nenabled = false");
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
        let errs = parse_err("version = 2\n[[mapping]\nname = 'x'\n");
        let msg = messages(&errs);
        assert!(msg.contains(r"C:\cfg.toml:"), "{msg}");
    }
    #[test]
    fn a_duplicate_name_is_rejected() {
        let text = format!(
            "{MINIMAL}\n{}",
            MINIMAL.trim_start_matches("\nversion = 2\n")
        );
        let errs = parse_err(&text);
        assert!(messages(&errs).contains("duplicate mapping name"));
    }
    #[test]
    fn an_empty_path_is_rejected() {
        let text = MINIMAL.replace(r"path = 'R:\'", "path = ''");
        let errs = parse_err(&text);
        assert!(messages(&errs).contains("empty `path`"));
    }

    /// Every kind an indexed mapping can be, and each is searched whatever
    /// was typed.
    #[test]
    fn an_indexed_mapping_is_searched_for_every_query() {
        for kind in ["flat", "tree"] {
            let text = format!(
                "version = 2\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\nkind = \"{kind}\"\n"
            );
            let parsed = parse(&text, p(), ConfigSource::BuiltIn)
                .unwrap_or_else(|e| panic!("{kind} was rejected: {:?}", messages(&e)));
            let targets = parsed.routes.targets();
            assert_eq!(targets.len(), 1, "{kind} reached nothing");
            assert_eq!(targets[0].dir, std::path::Path::new("R:\\"));
        }
    }

    /// A configuration carrying the old patterns is refused rather than
    /// quietly ignoring them. Silently accepting keys that no longer do
    /// anything is how someone spends an afternoon editing a file that has no
    /// effect.
    #[test]
    fn a_leftover_rules_block_is_rejected() {
        let text = "version = 2\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\n\
                    kind = \"tree\"\n\n  [[mapping.rules]]\n  pattern = '^x$'\n";
        let errs = parse_err(text);
        assert!(messages(&errs).contains("unknown key \"rules\""));
    }

    /// And a version 1 file gets one instruction rather than a wall of
    /// "unknown key" for every pattern it carries.
    #[test]
    fn a_version_one_configuration_is_told_exactly_what_to_change() {
        let text = "version = 1\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\n\
                    kind = \"job-folder\"\n\n  [[mapping.rules]]\n  pattern = '^x$'\n\
                      folder = 'x'\n";
        let errs = parse_err(text);
        let msg = messages(&errs);
        assert!(msg.contains("version 1 configuration"), "{msg}");
        assert!(msg.contains("version = 2"), "{msg}");
        assert!(msg.contains("kind"), "{msg}");
        // And nothing else. Every `rules` block and every `job-folder` in the
        // file would otherwise report its own "unknown key" beneath the one
        // message that matters, and a wall of errors reads as a broken file
        // rather than as an out-of-date one.
        assert_eq!(errs.len(), 1, "{msg}");
    }

    #[test]
    fn an_unknown_key_is_rejected_rather_than_ignored() {
        // `enable` instead of `enabled` would otherwise leave a share the
        // user believes they turned off still being searched.
        let text = MINIMAL.replace("kind = \"tree\"", "kind = \"tree\"\nenable = false");
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

    /// The viewer key ships uncommented because F2 rewrites it in place, and
    /// it ships set to `auto`.
    ///
    /// A fresh install writes this file verbatim, so what is written here *is*
    /// the default a new machine gets - the `#[default]` on [`ViewerKind`]
    /// only covers a machine with no configuration file at all. The two have
    /// to agree, and this is what makes them.
    #[test]
    fn the_shipped_default_sets_a_viewer_the_writer_can_replace() {
        let c = builtin();
        assert_eq!(
            c.settings.viewer.as_deref(),
            Some(crate::config::ViewerKind::default().name()),
            "a fresh install would not get the default viewer"
        );
    }

    /// Silently defaulting every setting is the failure this file refuses
    /// everywhere else - and it also fed a panic in the config writer.
    #[test]
    fn a_settings_that_is_not_a_table_is_rejected() {
        for bad in ["settings = 3", "settings = \"pdf\"", "settings = []"] {
            // Root keys must precede the first table header, so this goes
            // beside `version` rather than after the mapping.
            let errs = parse_err(&MINIMAL.replace(
                "version = 2",
                &format!(
                    "version = 2
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

    /// Two flat shares are both indexed now.
    ///
    /// This replaces a test asserting the opposite. The store held one flat
    /// slot and one tree slot, so a second flat mapping would have been
    /// searched against the first one's listing - refusing was the honest
    /// answer while that was true. The store is keyed by mapping now, so the
    /// honest answer is to index both.
    /// Two flat shares are both indexed now.
    ///
    /// This replaces a test asserting the opposite. The store held one flat
    /// slot and one tree slot, so a second flat mapping would have been
    /// searched against the first one's listing - refusing was the honest
    /// answer while that was true. The store is keyed by mapping now, so the
    /// honest answer is to index both.
    #[test]
    fn two_enabled_flat_mappings_are_both_accepted() {
        let text = r#"
version = 2

[[mapping]]
name = "one"
path = 'V:\a'
kind = "flat"

[[mapping]]
name = "two"
path = 'W:\b'
kind = "flat"

[[mapping]]
name = "jobs"
path = 'R:\'
kind = "tree"
"#;
        let c = parse(text, p(), ConfigSource::BuiltIn)
            .unwrap_or_else(|e| panic!("rejected: {:?}", messages(&e)));
        assert_eq!(c.routes.flat().count(), 2, "both flat shares are indexed");
        assert_eq!(c.routes.targets().len(), 3, "and all three are searched");
    }

    /// Ten mappings, which is the configuration that prompted all of this.
    #[test]
    fn ten_mappings_are_all_accepted_and_all_searched() {
        let mut text = String::from("version = 2\n");
        for i in 0..10 {
            let kind = if i % 3 == 0 { "flat" } else { "tree" };
            text.push_str(&format!(
                "\n[[mapping]]\nname = \"share{i}\"\npath = 'X:\\\\share{i}'\nkind = \"{kind}\"\n"
            ));
        }
        let c = parse(&text, p(), ConfigSource::BuiltIn)
            .unwrap_or_else(|e| panic!("rejected: {:?}", messages(&e)));
        assert_eq!(c.routes.enabled().count(), 10);
        assert_eq!(
            c.routes.targets().len(),
            10,
            "every configured share is searched, not the first two"
        );
    }

    /// Two mappings on one directory share a cache key, so each cold start
    /// would restore whichever actor wrote last, and every hit would appear
    /// twice in one merged list.
    #[test]
    fn two_mappings_on_the_same_directory_are_refused() {
        let text = r#"
version = 2

[[mapping]]
name = "one"
path = 'V:\a'
kind = "tree"

[[mapping]]
name = "two"
path = 'V:\a\'
kind = "tree"
"#;
        let errs = parse_err(text);
        let msg = messages(&errs);
        assert!(msg.contains("one"), "{msg}");
        assert!(msg.contains("two"), "{msg}");
        assert!(msg.contains("twice"), "{msg}");
    }

    /// A tree that contains another mapping walks that one's files as well
    /// as its own, so every hit inside appears twice in one merged list.
    #[test]
    fn a_mapping_nested_inside_a_walked_tree_is_refused() {
        let text = r#"
version = 2

[[mapping]]
name = "jobs"
path = 'R:\'
kind = "tree"

[[mapping]]
name = "inner"
path = 'R:\11d'
kind = "tree"
"#;
        let errs = parse_err(text);
        let msg = messages(&errs);
        assert!(msg.contains("inner"), "{msg}");
        assert!(msg.contains("jobs"), "{msg}");
        assert!(msg.contains("twice"), "{msg}");
    }

    /// A *flat* parent lists only its own directory, so a child mapping's
    /// files were never in it and there is nothing to index twice.
    #[test]
    fn a_mapping_below_a_flat_share_is_allowed() {
        let text = r#"
version = 2

[[mapping]]
name = "top"
path = 'V:\docs'
kind = "flat"

[[mapping]]
name = "inner"
path = 'V:\docs\jobs'
kind = "tree"
"#;
        let c = parse(text, p(), ConfigSource::BuiltIn)
            .unwrap_or_else(|e| panic!("rejected: {:?}", messages(&e)));
        assert_eq!(c.routes.targets().len(), 2);
    }

    #[test]
    fn a_zero_walk_cap_is_refused_rather_than_clamped() {
        let text = format!("{MINIMAL}\n[settings]\nmax_concurrent_scans = 0\n");
        let errs = parse_err(&text);
        assert!(
            messages(&errs).contains("at least 1"),
            "{:?}",
            messages(&errs)
        );
    }

    #[test]
    fn a_disabled_second_flat_mapping_is_fine() {
        let text = r#"
version = 2

[[mapping]]
name = "custompro"
path = 'V:\Documents\custpro'
kind = "flat"

[[mapping]]
name = "archive"
path = 'W:\archive'
kind = "flat"
enabled = false
"#;
        let c = parse_ok(text);
        assert_eq!(c.routes.flat().count(), 1);
        assert_eq!(c.routes.all().len(), 2);
    }

    /// Several tree mappings are supported, and all of them are searched.
    #[test]
    fn several_tree_mappings_are_accepted() {
        let text = r#"
version = 2

[[mapping]]
name = "jobs"
path = 'R:\'
kind = "tree"

[[mapping]]
name = "old-jobs"
path = 'S:\archive'
kind = "tree"
"#;
        let c = parse_ok(text);
        assert_eq!(c.routes.enabled().count(), 2);
        // Both searched, in configuration order.
        let t = c.routes.targets();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].dir, Path::new(r"R:\"));
        assert_eq!(t[1].dir, Path::new(r"S:\archive"));
    }

    #[test]
    fn an_unknown_kind_is_rejected() {
        let text = MINIMAL.replace("kind = \"tree\"", "kind = \"folder\"");
        let errs = parse_err(&text);
        assert!(messages(&errs).contains("unknown kind"));
    }

    #[test]
    fn a_missing_or_wrong_version_is_rejected() {
        let errs = parse_err(&MINIMAL.replace("version = 2", "version = 9"));
        assert!(messages(&errs).contains("unsupported version 9"));

        let errs = parse_err(&MINIMAL.replace("version = 2\n", ""));
        assert!(messages(&errs).contains("missing `version`"));
    }

    #[test]
    fn a_file_with_no_mappings_is_rejected() {
        let errs = parse_err("version = 2\n");
        assert!(messages(&errs).contains("no `[[mapping]]`"));
    }

    /// Fixing four typos should take one run, not four.
    ///
    /// The one exception is a version 1 file, which stops at the migration
    /// message - see `a_version_one_configuration_is_told_exactly_what_to_change`.
    #[test]
    fn every_error_is_reported_at_once() {
        let text = "
version = 2

[[mapping]]
name = \"a\"
path = ''
kind = \"nonsense\"
enable = false
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
