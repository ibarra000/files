//! Parsing and validation of `config.toml`.
//!
//! Uses `toml_edit` in parse-only mode instead of a serde derive so every value
//! keeps a byte span and errors can report `config.toml:34:15` with the
//! offending text. This also keeps `syn` out of the build.
//!
//! An invalid file stops the program instead of falling back to defaults, so a
//! mistyped path cannot silently search the wrong location. `--no-config` runs
//! on the built-in defaults. All errors are collected and reported together.

use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::{ImDocument, Item, Value};

use crate::alias::{Alias, Aliases};
use crate::config::{ViewerApp, ViewerKind, Viewers};
use crate::paths::{ConfigSource, Mapping, MappingId, MappingKind, RefreshPolicy, Routes};
use crate::util::winpath;

/// The file written on first run, and the compiled-in fallback.
///
/// [`crate::config::default_routes`] parses this through the ordinary loader,
/// so the shipped defaults and the written file cannot drift.
pub const DEFAULT_CONFIG_TOML: &str = include_str!("../../assets/default_config.toml");

/// The error message for a version 1 configuration.
///
/// A named constant so tests can verify that following its instructions
/// produces a file that loads.
const V1_MIGRATION: &str = concat!(
    "version 1 configurations are no longer supported. Delete every `[[mapping.rules]]` ",
    "block and any `case` or `stop` key, give each mapping `kind = \"flat\"` or ",
    "`kind = \"tree\"`, and set `version = 3`. Alternatively, delete the file and a new ",
    "one is written on the next run.",
);

/// The only format version this build understands.
///
/// A version 1 file gets the single `V1_MIGRATION` message. A version 2 file
/// is upgraded by [`super::migrate`].
pub const CONFIG_VERSION: i64 = 3;

/// What went wrong, and exactly where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub path: PathBuf,
    /// One-based line and column, when the problem has a single location.
    pub loc: Option<(usize, usize)>,
    /// The entry kind and name the problem is in, such as `("mapping", "jobs")`
    /// or `("alias", "pw")`.
    pub entry: Option<(&'static str, String)>,
    pub rule: Option<usize>,
    pub message: String,
    /// The offending text, echoed in the report.
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
        if let Some((kind, name)) = &self.entry {
            write!(f, "{kind} {name:?}")?;
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

/// Formats a batch of errors for the terminal.
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
        entry: Option<(&'static str, &str)>,
        rule: Option<usize>,
        message: impl Into<String>,
        snippet: Option<String>,
    ) {
        let loc = self.loc(span);
        self.errors.push(ConfigError {
            path: self.path.clone(),
            loc,
            entry: entry.map(|(k, n)| (k, n.to_string())),
            rule,
            message: message.into(),
            snippet,
        });
    }
}

/// Returns the item as a boolean, or records an error at its span.
///
/// A quoted value such as `enabled = "false"` is an error rather than treated
/// as absent, which would silently keep the default.
fn bool_at(ctx: &mut Ctx<'_>, key: &str, item: &Item) -> Option<bool> {
    if let Some(b) = item.as_bool() {
        return Some(b);
    }
    ctx.err(
        item.span(),
        None,
        None,
        format!("{key} must be true or false, written without quotes"),
        None,
    );
    None
}

/// Keys accepted at each level. Any other key is an error so typos are not
/// silently ignored.
pub(super) const MAPPING_KEYS: &[&str] = &["name", "path", "kind", "enabled", "refresh", "depth"];
pub(super) const SETTINGS_KEYS: &[&str] = &[
    "enum_strategy",
    "matcher",
    "server_filter",
    "persist",
    "max_concurrent_scans",
    "stale_notices",
    "dev_mode",
    "hide_on_blur",
    "hide_after_opening",
    "hide_on_escape",
    "pdf_read_only",
    "live_updates",
    "cache_dir",
    "index_log",
    "history",
    "hotkey",
    "viewer",
    "pdf_viewer",
    "update_from",
    "theme",
    "backdrop",
    "result_layout",
    "dock",
    "columns",
    "check_for_updates",
    "hide_extensions",
    "hide_system_files",
];
const ALIAS_KEYS: &[&str] = &["name", "code", "note"];
const VIEWER_KEYS: &[&str] = &["name", "program", "extensions"];
pub(super) const ROOT_KEYS: &[&str] = &["version", "mapping", "settings", "alias", "viewer"];

/// Global options from the `[settings]` table. Environment variables take
/// precedence.
#[derive(Debug, Clone, Default)]
pub struct FileSettings {
    pub enum_strategy: Option<String>,
    pub matcher: Option<String>,
    pub server_filter: Option<bool>,
    pub persist: Option<bool>,
    pub max_concurrent_scans: Option<usize>,
    pub stale_notices: Option<bool>,
    pub dev_mode: Option<bool>,
    pub hide_on_blur: Option<bool>,
    pub hide_after_opening: Option<bool>,
    pub hide_on_escape: Option<bool>,
    pub pdf_read_only: Option<bool>,
    pub live_updates: Option<bool>,
    pub cache_dir: Option<PathBuf>,
    pub index_log: Option<PathBuf>,
    pub history: Option<bool>,
    pub hotkey: Option<crate::hotkey::spec::HotkeySpec>,
    pub viewer: Option<String>,
    pub pdf_viewer: Option<PathBuf>,
    pub update_from: Option<PathBuf>,
    pub theme: Option<String>,
    pub backdrop: Option<String>,
    pub result_layout: Option<String>,
    pub dock: Option<String>,
    pub columns: Option<usize>,
    pub check_for_updates: Option<bool>,
    pub hide_extensions: Option<Vec<String>>,
    pub hide_system_files: Option<bool>,
}

/// A parsed configuration.
#[derive(Debug)]
pub struct ParsedConfig {
    pub routes: Routes,
    pub settings: FileSettings,
    pub aliases: Aliases,
    pub viewers: Viewers,
}

/// Parses configuration text.
///
/// `path` is used only in error messages; no filesystem access occurs.
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
                entry: None,
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

    // A version 1 file reports only the migration message; its obsolete keys
    // would otherwise each add an "unknown key" error.
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
    let aliases = parse_aliases(&doc, &mut ctx);
    let viewers = Viewers::new(parse_viewers(&doc, &mut ctx));
    check_viewer_setting(&doc, &settings, &viewers, &mut ctx);

    // Set-level mapping rules, shared with the settings window so both reject
    // the same configurations.
    for conflict in crate::paths::conflicts(&mappings) {
        ctx.err(None, None, None, conflict.detail(), None);
    }

    if ctx.errors.is_empty() {
        Ok(ParsedConfig {
            routes: Routes::new(mappings, source),
            settings,
            aliases: Aliases::new(aliases),
            viewers,
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
                Some(("mapping", &label)),
                None,
                "missing or empty `name`",
                None,
            );
        } else if seen.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
            ctx.err(
                span.clone(),
                Some(("mapping", &label)),
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
                    Some(("mapping", &label)),
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
                Some(("mapping", &label)),
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
                        Some(("mapping", &label)),
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
                    Some(("mapping", &label)),
                    None,
                    "missing `kind` (expected \"flat\", \"tree\" or \"live\")",
                    None,
                );
                MappingKind::Tree
            }
        };

        let enabled = match table.get("enabled") {
            Some(item) => bool_at(ctx, "enabled", item).unwrap_or(true),
            None => true,
        };

        // `depth` applies only to live mappings and `refresh` only to indexed
        // ones; each is an error where it does not apply.
        let depth = match table.get("depth") {
            None => crate::config::DEFAULT_LIVE_DEPTH,
            Some(item) if !kind.is_live() => {
                ctx.err(
                    item.span(),
                    Some(("mapping", &label)),
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
                        Some(("mapping", &label)),
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
                Some(("mapping", &label)),
                None,
                "`refresh` applies only to an indexed mapping; a live mapping is read only when searched",
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
                        Some(("mapping", &label)),
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

/// Reads the optional `[[alias]]` tables.
///
/// Validation ensures every alias that loads will work when typed.
fn parse_aliases(doc: &ImDocument<String>, ctx: &mut Ctx<'_>) -> Vec<Alias> {
    let Some(tables) = doc.get("alias").and_then(Item::as_array_of_tables) else {
        return Vec::new();
    };

    let mut aliases: Vec<Alias> = Vec::with_capacity(tables.len());

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

        for (key, item) in table.iter() {
            if !ALIAS_KEYS.contains(&key) {
                ctx.err(
                    item.span(),
                    Some(("alias", &label)),
                    None,
                    format!("unknown key {key:?}"),
                    None,
                );
            }
        }

        let code = table
            .get("code")
            .and_then(Item::as_str)
            .unwrap_or("")
            .trim()
            .to_string();

        // Shared with the settings window so both apply the same rules.
        if let Err(problem) = crate::alias::check(&name, &code, &aliases) {
            // Point at the offending key when there is one.
            let at = match problem {
                crate::alias::Invalid::NoCode | crate::alias::Invalid::UnsearchableCode(_) => {
                    table.get("code").and_then(Item::span)
                }
                _ => table.get("name").and_then(Item::span),
            };
            ctx.err(
                at.or(span.clone()),
                Some(("alias", &label)),
                None,
                problem.detail(),
                (!code.is_empty()).then(|| code.clone()),
            );
        }

        let note = table
            .get("note")
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(Into::into);

        aliases.push(Alias {
            name: name.into(),
            code: code.into(),
            note,
        });
    }

    aliases
}

/// Reads a single string or an array of strings.
fn string_or_list(value: &Value) -> Option<Vec<String>> {
    if let Some(s) = value.as_str() {
        return Some(vec![s.to_string()]);
    }
    value.as_array().map(|a| {
        a.iter()
            .filter_map(|e| e.as_str().map(str::to_string))
            .collect()
    })
}

/// Returns why `entry` is not a bare file extension, or `None` if it is one.
///
/// Patterns (`*.db`) and filenames (`Thumbs.db`) would match nothing, so they
/// are rejected. Used by `hide_extensions` and a viewer's `extensions`.
fn extension_problem(entry: &str) -> Option<&'static str> {
    let bare = entry.trim().trim_start_matches('.');
    // Wildcards are checked before dots so `*.db` gets the more useful message.
    if bare.is_empty() {
        Some("an extension cannot be blank")
    } else if bare.contains(['*', '?']) {
        Some("this is not a pattern; write the extension only")
    } else if bare.contains('.') {
        Some("write the extension only, as \"db\" or \".db\"")
    } else if bare.contains(['/', '\\']) {
        Some("an extension cannot contain a path")
    } else if !bare.is_ascii() {
        // `Hidden` cannot match non-ASCII extensions consistently (see its
        // `hides_folded`), so they are reported instead of silently dropped.
        Some("an extension must be ASCII")
    } else {
        None
    }
}

/// Reads the optional `[[viewer]]` tables.
///
/// With none, Enter offers only the two built-in modes.
fn parse_viewers(doc: &ImDocument<String>, ctx: &mut Ctx<'_>) -> Vec<ViewerApp> {
    let Some(tables) = doc.get("viewer").and_then(Item::as_array_of_tables) else {
        if let Some(item) = doc.get("viewer") {
            ctx.err(
                item.span(),
                None,
                None,
                "`viewer` must be written as [[viewer]] tables",
                None,
            );
        }
        return Vec::new();
    };

    let mut viewers: Vec<ViewerApp> = Vec::with_capacity(tables.len());

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

        for (key, item) in table.iter() {
            if !VIEWER_KEYS.contains(&key) {
                ctx.err(
                    item.span(),
                    Some(("viewer", &label)),
                    None,
                    format!("unknown key {key:?}"),
                    None,
                );
            }
        }

        let name_problem = if name.is_empty() {
            Some("a viewer needs a name".to_string())
        } else if ViewerKind::is_reserved(&name) {
            Some(format!(
                "{name:?} is one of the built-in modes; give the viewer another name"
            ))
        } else if viewers.iter().any(|v| v.name.eq_ignore_ascii_case(&name)) {
            Some("another viewer already has this name".to_string())
        } else {
            None
        };
        if let Some(problem) = name_problem {
            let at = table.get("name").and_then(Item::span).or(span.clone());
            ctx.err(at, Some(("viewer", &label)), None, problem, None);
        }

        let program = table
            .get("program")
            .and_then(Item::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        // Existence is not checked: the file roams between machines, and a
        // missing program is reported by the panel and `--doctor` instead.
        if program.is_empty() {
            ctx.err(
                table.get("program").and_then(Item::span).or(span.clone()),
                Some(("viewer", &label)),
                None,
                "a viewer needs a program: a name on PATH, or a full path",
                None,
            );
        }

        let extensions = viewer_extensions(table, &label, ctx);
        let refs: Vec<&str> = extensions.iter().map(String::as_str).collect();
        viewers.push(ViewerApp::new(&name, &program, &refs));
    }

    viewers
}

/// Reads a viewer's `extensions`, reporting each invalid entry.
fn viewer_extensions(table: &toml_edit::Table, label: &str, ctx: &mut Ctx<'_>) -> Vec<String> {
    let Some(item) = table.get("extensions") else {
        return Vec::new();
    };
    let Some(list) = item.as_value().and_then(string_or_list) else {
        ctx.err(
            item.span(),
            Some(("viewer", label)),
            None,
            "extensions must be a list of file types, as [\"dwg\"]",
            None,
        );
        return Vec::new();
    };
    // `string_or_list` drops non-string entries, so report them here.
    if let Some(array) = item.as_value().and_then(Value::as_array)
        && array.iter().any(|e| !e.is_str())
    {
        ctx.err(
            item.span(),
            Some(("viewer", label)),
            None,
            "every extensions entry must be a quoted file type, as \"dwg\"",
            None,
        );
    }

    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        let bare = entry.trim().trim_start_matches('.');
        let why = extension_problem(&entry).or_else(|| {
            // Claiming PDFs would disable PDF mode; `pdf_viewer` chooses the
            // program for PDFs instead.
            bare.eq_ignore_ascii_case("pdf").then_some(
                "PDF mode combines PDFs itself; use pdf_viewer to choose the program that opens them",
            )
        });
        match why {
            Some(why) => ctx.err(
                item.span(),
                Some(("viewer", label)),
                None,
                format!("extensions entry {entry:?}: {why}"),
                None,
            ),
            None => out.push(entry),
        }
    }
    out
}

/// Checks `[settings] viewer` against the modes this file makes available.
///
/// An unknown name is an error so a misspelling cannot silently fall back.
fn check_viewer_setting(
    doc: &ImDocument<String>,
    settings: &FileSettings,
    viewers: &Viewers,
    ctx: &mut Ctx<'_>,
) {
    let Some(v) = settings.viewer.as_deref() else {
        return;
    };
    if ViewerKind::parse(v, viewers).is_some() {
        return;
    }
    let expected = if viewers.is_empty() {
        "expected \"auto\" or \"pdf\", or the name of a [[viewer]] added to this file".to_string()
    } else {
        let names: Vec<String> = viewers.names().iter().map(|n| format!("{n:?}")).collect();
        format!(
            "expected \"auto\", \"pdf\" or one of the [[viewer]] names: {}",
            names.join(", ")
        )
    };
    let span = doc
        .get("settings")
        .and_then(|s| s.get("viewer"))
        .and_then(Item::span);
    ctx.err(
        span,
        None,
        None,
        format!("unknown viewer {v:?} ({expected})"),
        None,
    );
}

fn parse_settings(doc: &ImDocument<String>, ctx: &mut Ctx<'_>) -> FileSettings {
    let mut out = FileSettings::default();
    let Some(item) = doc.get("settings") else {
        return out;
    };
    // A non-table `settings` (such as `settings = 3` or `[[settings]]`) is an
    // error, not a silent fall back to every default.
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
            "server_filter" => out.server_filter = bool_at(ctx, key, item),
            "persist" => out.persist = bool_at(ctx, key, item),
            "stale_notices" => out.stale_notices = bool_at(ctx, key, item),
            "dev_mode" => out.dev_mode = bool_at(ctx, key, item),
            "hide_on_blur" => out.hide_on_blur = bool_at(ctx, key, item),
            "hide_after_opening" => out.hide_after_opening = bool_at(ctx, key, item),
            "hide_on_escape" => out.hide_on_escape = bool_at(ctx, key, item),
            "pdf_read_only" => out.pdf_read_only = bool_at(ctx, key, item),
            "max_concurrent_scans" => {
                // Zero would index nothing, so it is an error rather than
                // clamped.
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
            "live_updates" => out.live_updates = bool_at(ctx, key, item),
            "check_for_updates" => out.check_for_updates = bool_at(ctx, key, item),
            "cache_dir" => out.cache_dir = value.and_then(Value::as_str).map(PathBuf::from),
            "index_log" => out.index_log = value.and_then(Value::as_str).map(PathBuf::from),
            "history" => out.history = bool_at(ctx, key, item),
            "hotkey" => {
                // An invalid hotkey is an error; a silent fall back to the
                // default would look like a working hotkey.
                if let Some(v) = value.and_then(Value::as_str) {
                    match crate::hotkey::spec::parse(v) {
                        Ok(spec) => out.hotkey = Some(spec),
                        Err(e) => ctx.err(item.span(), None, None, e.detail(), None),
                    }
                }
            }
            // Validated in `check_viewer_setting` after `[[viewer]]` is read,
            // since valid names depend on the viewers the file defines.
            "viewer" => out.viewer = value.and_then(Value::as_str).map(str::to_string),
            "pdf_viewer" => out.pdf_viewer = value.and_then(Value::as_str).map(PathBuf::from),
            "update_from" => out.update_from = value.and_then(Value::as_str).map(PathBuf::from),
            "hide_system_files" => out.hide_system_files = bool_at(ctx, key, item),
            "hide_extensions" => {
                let list = value.and_then(string_or_list);
                if let Some(v) = &list {
                    for entry in v {
                        if let Some(why) = extension_problem(entry) {
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
                // Unknown values are errors, as for `viewer`.
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
            "backdrop" => {
                let raw = value.and_then(Value::as_str);
                // Unknown values are errors, as for `theme`.
                if let Some(v) = raw
                    && crate::gui::window::Material::parse(v).is_none()
                {
                    ctx.err(
                        item.span(),
                        None,
                        None,
                        format!(
                            "unknown backdrop {v:?} (expected \"acrylic\", \"mica\", \"tabbed\" or \"none\")"
                        ),
                        None,
                    );
                }
                out.backdrop = raw.map(str::to_string);
            }
            "result_layout" => {
                let raw = value.and_then(Value::as_str);
                // Unknown values are errors, as for `theme`.
                if let Some(v) = raw
                    && crate::config::ResultLayout::parse(v).is_none()
                {
                    ctx.err(
                        item.span(),
                        None,
                        None,
                        format!(
                            "unknown result_layout {v:?} (expected \"compact\" or \"detailed\")"
                        ),
                        None,
                    );
                }
                out.result_layout = raw.map(str::to_string);
            }
            "dock" => {
                let raw = value.and_then(Value::as_str);
                if let Some(v) = raw
                    && crate::config::Dock::parse(v).is_none()
                {
                    ctx.err(
                        item.span(),
                        None,
                        None,
                        format!("unknown dock {v:?} (expected \"free\", \"top\" or \"bottom\")"),
                        None,
                    );
                }
                out.dock = raw.map(str::to_string);
            }
            "columns" => {
                let max = crate::config::MAX_COLUMNS;
                match value.and_then(Value::as_integer) {
                    Some(n) if (1..=max as i64).contains(&n) => out.columns = Some(n as usize),
                    Some(n) => ctx.err(
                        item.span(),
                        None,
                        None,
                        format!("columns must be from 1 to {max}, not {n}"),
                        None,
                    ),
                    None => ctx.err(
                        item.span(),
                        None,
                        None,
                        format!("columns must be a number from 1 to {max}"),
                        None,
                    ),
                }
            }
            _ => {}
        }
    }
    out
}

// --- locating and creating the file ---------------------------------------

/// Returns `%APPDATA%\files\config.toml`.
///
/// Settings roam with the profile; the index cache stays in `%LOCALAPPDATA%`.
pub fn default_config_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let p = PathBuf::from(appdata);
    if p.as_os_str().is_empty() {
        return None;
    }
    Some(p.join("files").join("config.toml"))
}

/// Writes the shipped default to `path` if no file exists there.
///
/// Uses `create_new` so two instances starting together cannot interleave.
/// Failure is not fatal, since the built-in defaults are identical.
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
            entry: None,
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

/// The result of upgrading an outdated configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migrated {
    /// The original format version.
    pub from: i64,
    /// Where the original file was copied.
    pub backup: PathBuf,
}

impl Migrated {
    /// The user-facing notice that the file was rewritten.
    pub fn detail(&self) -> String {
        format!(
            "Settings upgraded from an older version \u{b7} the original is saved at {}",
            self.backup.display()
        )
    }
}

/// Loads `path`, upgrading it first if its version is outdated.
///
/// The upgrade is done by [`super::migrate`]. The original is copied to a
/// backup beside it before anything is written.
pub fn load_migrating(
    path: &Path,
    explicit: bool,
) -> Result<(ParsedConfig, Option<Migrated>), Vec<ConfigError>> {
    let first = load_file(path, explicit);
    // Only an outdated version triggers a rewrite; other errors are reported
    // as they are.
    let Err(errors) = first else {
        return first.map(|parsed| (parsed, None));
    };
    let Some(from) = outdated_version(path) else {
        return Err(errors);
    };

    let text = std::fs::read_to_string(path).map_err(|e| vec![unreadable(path, &e)])?;
    let migrated = match super::migrate::to_current(&text) {
        Ok(migrated) => migrated,
        // Report the original errors, which are actionable, rather than the
        // migration's.
        Err(_) => return Err(errors),
    };

    let source = if explicit {
        ConfigSource::Explicit(path.to_path_buf())
    } else {
        ConfigSource::Default(path.to_path_buf())
    };
    // The migrated text must parse before it replaces the file, as in
    // `write::save`.
    let parsed = parse(&migrated, path, source)?;

    let backup = backup_path(path, from);
    std::fs::copy(path, &backup).map_err(|e| vec![unwritable(&backup, &e.to_string())])?;
    super::write::replace(path, &migrated).map_err(|e| vec![unwritable(path, &e)])?;

    Ok((parsed, Some(Migrated { from, backup })))
}

/// Returns the file's declared version if it is older than [`CONFIG_VERSION`].
///
/// Read directly from the file rather than inferred from error messages.
fn outdated_version(path: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: toml_edit::ImDocument<String> = toml_edit::ImDocument::parse(text).ok()?;
    let version = doc.get("version").and_then(Item::as_integer)?;
    (version < CONFIG_VERSION).then_some(version)
}

/// The backup path for the original file, beside it as `<stem>.v<from>.bak`.
fn backup_path(path: &Path, from: i64) -> PathBuf {
    let mut name = path
        .file_stem()
        .unwrap_or_else(|| std::ffi::OsStr::new("config"))
        .to_os_string();
    name.push(format!(".v{from}.bak"));
    path.with_file_name(name)
}

fn unreadable(path: &Path, e: &std::io::Error) -> ConfigError {
    ConfigError {
        path: path.to_path_buf(),
        loc: None,
        entry: None,
        rule: None,
        message: format!("could not be read: {e}"),
        snippet: None,
    }
}

fn unwritable(path: &Path, detail: &str) -> ConfigError {
    ConfigError {
        path: path.to_path_buf(),
        loc: None,
        entry: None,
        rule: None,
        message: format!("could not be brought up to date: {detail}"),
        snippet: None,
    }
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
version = 3

[[mapping]]
name = "jobs"
path = 'R:\'
kind = "tree"
"#;

    // --- live mappings ------------------------------------------------------

    const LIVE: &str = r#"
version = 3

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

    #[test]
    fn two_live_mappings_on_one_directory_are_refused_like_two_indexed_ones() {
        let errs = parse_err(
            r#"
version = 3

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

    /// The tree walk would index files the live query also finds.
    #[test]
    fn a_live_mapping_inside_a_walked_tree_is_refused() {
        let errs = parse_err(
            r#"
version = 3

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

    /// The reverse nesting is allowed: index a busy subtree, query the rest live.
    #[test]
    fn a_walked_tree_inside_a_live_share_is_accepted() {
        let c = parse_ok(
            r#"
version = 3

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
version = 3

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

    /// The shipped file mentions these keys, so it would fail to load without them.
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

    /// Also verifies that the documented block in the shipped file parses.
    #[test]
    fn the_shipped_default_hides_the_files_that_prompted_it() {
        let c = builtin();
        let hidden = c.settings.hide_extensions.expect("nothing shipped");
        for ext in ["db", "js", "lnk"] {
            assert!(hidden.iter().any(|e| e == ext), "{ext} is not hidden");
        }
        assert_eq!(c.settings.hide_system_files, Some(true));
    }

    /// The template's example mappings are all disabled.
    #[test]
    fn the_shipped_default_parses_with_no_drives_enabled() {
        let c = builtin();
        assert_eq!(c.routes.enabled().count(), 0);
        assert!(c.routes.targets().is_empty());
        let kinds: Vec<_> = c.routes.all().iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            vec![MappingKind::Flat, MappingKind::Tree, MappingKind::Live]
        );
    }

    /// The shipped file documents every accepted key, mostly as commented-out
    /// examples.
    #[test]
    fn every_setting_the_loader_accepts_is_one_the_shipped_file_mentions() {
        for key in SETTINGS_KEYS {
            assert!(
                DEFAULT_CONFIG_TOML.contains(key),
                "{key} is accepted and undocumented"
            );
        }
    }

    /// A basic string would silently turn `\d` into `d` and still parse.
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
        let c = parse_ok(&text);
        assert!(!c.routes.all()[0].enabled);
        assert_eq!(
            c.routes.enabled().count(),
            0,
            "no enabled drives is a valid configuration"
        );
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
        let errs = parse_err("version = 3\n[[mapping]\nname = 'x'\n");
        let msg = messages(&errs);
        assert!(msg.contains(r"C:\cfg.toml:"), "{msg}");
    }
    #[test]
    fn a_duplicate_name_is_rejected() {
        let text = format!(
            "{MINIMAL}\n{}",
            MINIMAL.trim_start_matches("\nversion = 3\n")
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

    #[test]
    fn an_indexed_mapping_is_searched_for_every_query() {
        for kind in ["flat", "tree"] {
            let text = format!(
                "version = 3\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\nkind = \"{kind}\"\n"
            );
            let parsed = parse(&text, p(), ConfigSource::BuiltIn)
                .unwrap_or_else(|e| panic!("{kind} was rejected: {:?}", messages(&e)));
            let targets = parsed.routes.targets();
            assert_eq!(targets.len(), 1, "{kind} reached nothing");
            assert_eq!(targets[0].dir, std::path::Path::new("R:\\"));
        }
    }

    #[test]
    fn a_leftover_rules_block_is_rejected() {
        let text = "version = 3\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\n\
                    kind = \"tree\"\n\n  [[mapping.rules]]\n  pattern = '^x$'\n";
        let errs = parse_err(text);
        assert!(messages(&errs).contains("unknown key \"rules\""));
    }

    #[test]
    fn a_version_one_configuration_is_told_exactly_what_to_change() {
        let text = "version = 1\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\n\
                    kind = \"job-folder\"\n\n  [[mapping.rules]]\n  pattern = '^x$'\n\
                      folder = 'x'\n";
        let errs = parse_err(text);
        let msg = messages(&errs);
        assert!(msg.contains("version 1 configuration"), "{msg}");
        assert!(msg.contains("version = 3"), "{msg}");
        assert!(msg.contains("kind"), "{msg}");
        // No "unknown key" errors for the obsolete keys.
        assert_eq!(errs.len(), 1, "{msg}");
    }

    #[test]
    fn an_unknown_key_is_rejected_rather_than_ignored() {
        let text = MINIMAL.replace("kind = \"tree\"", "kind = \"tree\"\nenable = false");
        let errs = parse_err(&text);
        assert!(messages(&errs).contains("unknown key \"enable\""));
    }

    #[test]
    fn reads_the_viewer_and_its_override_from_the_settings_table() {
        let text = format!(
            "{MINIMAL}\n[settings]\nviewer = \"avwin\"\npdf_viewer = 'C:\\tools\\sumatra.exe'\n\n[[viewer]]\nname = \"avwin\"\nprogram = 'avwin.exe'\nextensions = [\"dwg\"]\n"
        );
        let c = parse_ok(&text);
        assert_eq!(c.settings.viewer.as_deref(), Some("avwin"));
        assert_eq!(
            c.settings.pdf_viewer.as_deref(),
            Some(Path::new(r"C:\tools\sumatra.exe"))
        );
    }

    #[test]
    fn an_unknown_viewer_value_is_rejected_with_its_text() {
        let text = format!("{MINIMAL}\n[settings]\nviewer = \"notepad\"\n");
        let errs = parse_err(&text);
        let msg = messages(&errs);
        assert!(msg.contains("notepad"), "{msg}");
        assert!(
            msg.contains("\"auto\"") && msg.contains("[[viewer]]"),
            "the message must say what is allowed: {msg}"
        );
    }

    /// avwin is valid only when a `[[viewer]]` defines it.
    #[test]
    fn avwin_is_not_a_viewer_until_the_file_adds_it() {
        let errs = parse_err(&format!("{MINIMAL}\n[settings]\nviewer = \"avwin\"\n"));
        assert!(messages(&errs).contains("unknown viewer \"avwin\""));
    }

    #[test]
    fn an_unknown_viewer_lists_the_ones_that_are_configured() {
        let text = format!(
            "{MINIMAL}\n[settings]\nviewer = \"avwn\"\n\n[[viewer]]\nname = \"avwin\"\nprogram = 'avwin.exe'\nextensions = [\"dwg\"]\n"
        );
        let msg = messages(&parse_err(&text));
        assert!(msg.contains("\"avwin\""), "{msg}");
    }

    #[test]
    fn a_viewer_table_is_read_in_order() {
        let text = format!(
            "{MINIMAL}\n[[viewer]]\nname = \"avwin\"\nprogram = 'avwin.exe'\nextensions = [\"dwg\"]\n\n[[viewer]]\nname = \"Notes\"\nprogram = 'C:\\Windows\\notepad.exe'\nextensions = '.TXT'\n"
        );
        let c = parse_ok(&text);
        let all = c.viewers.all();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name.as_ref(), "avwin");
        assert_eq!(all[0].program.as_ref(), "avwin.exe");
        assert_eq!(all[0].extensions.as_ref(), [Box::from("dwg")]);
        assert_eq!(all[1].name.as_ref(), "Notes");
        assert_eq!(all[1].program.as_ref(), r"C:\Windows\notepad.exe");
        // A bare string is a list of one; the dot and case are normalised.
        assert_eq!(all[1].extensions.as_ref(), [Box::from("txt")]);
    }

    #[test]
    fn a_viewer_needs_a_name_and_a_program() {
        let msg = messages(&parse_err(&format!(
            "{MINIMAL}\n[[viewer]]\nextensions = [\"dwg\"]\n"
        )));
        assert!(msg.contains("needs a name"), "{msg}");
        assert!(msg.contains("needs a program"), "{msg}");
    }

    #[test]
    fn a_viewer_name_must_be_unique_and_not_a_built_in_mode() {
        let twice = format!(
            "{MINIMAL}\n[[viewer]]\nname = \"avwin\"\nprogram = 'avwin.exe'\nextensions = [\"dwg\"]\n\n[[viewer]]\nname = \"AVWIN\"\nprogram = 'x.exe'\n"
        );
        assert!(messages(&parse_err(&twice)).contains("already has this name"));

        for reserved in ["auto", "PDF", "merge"] {
            let text = format!("{MINIMAL}\n[[viewer]]\nname = \"{reserved}\"\nprogram = 'x.exe'\n");
            let msg = messages(&parse_err(&text));
            assert!(msg.contains("built-in modes"), "{reserved}: {msg}");
        }
    }

    #[test]
    fn a_viewer_extension_is_held_to_the_same_rules_as_a_hidden_one() {
        for (bad, why) in [
            ("*.dwg", "not a pattern"),
            ("a.dwg", "extension only"),
            ("", "blank"),
            ("pdf", "pdf_viewer"),
            (".PDF", "pdf_viewer"),
        ] {
            let text = format!(
                "{MINIMAL}\n[[viewer]]\nname = \"x\"\nprogram = 'x.exe'\nextensions = [\"{bad}\"]\n"
            );
            let msg = messages(&parse_err(&text));
            assert!(msg.contains(why), "{bad:?}: {msg}");
        }
        let not_a_list =
            format!("{MINIMAL}\n[[viewer]]\nname = \"x\"\nprogram = 'x.exe'\nextensions = 3\n");
        assert!(messages(&parse_err(&not_a_list)).contains("list of file types"));
        let mixed = format!(
            "{MINIMAL}\n[[viewer]]\nname = \"x\"\nprogram = 'x.exe'\nextensions = [\"dwg\", 3]\n"
        );
        assert!(messages(&parse_err(&mixed)).contains("quoted file type"));
    }

    #[test]
    fn an_unknown_key_in_a_viewer_is_rejected() {
        let text =
            format!("{MINIMAL}\n[[viewer]]\nname = \"x\"\nprogram = 'x.exe'\nargs = \"-q\"\n");
        let msg = messages(&parse_err(&text));
        assert!(msg.contains("unknown key \"args\""), "{msg}");
    }

    #[test]
    fn a_viewer_written_as_a_single_table_is_rejected() {
        let text = format!("{MINIMAL}\n[viewer]\nname = \"x\"\nprogram = 'x.exe'\n");
        assert!(messages(&parse_err(&text)).contains("[[viewer]]"));
    }

    #[test]
    fn the_shipped_default_configures_no_viewers() {
        assert!(builtin().viewers.is_empty());
    }

    /// The shipped `viewer` key is uncommented (F2 rewrites it in place) and
    /// must match the `#[default]` on [`ViewerKind`].
    #[test]
    fn the_shipped_default_sets_a_viewer_the_writer_can_replace() {
        let c = builtin();
        assert_eq!(
            c.settings.viewer.as_deref(),
            Some(crate::config::ViewerKind::default().name(&Viewers::default())),
            "a fresh install would not get the default viewer"
        );
    }

    #[test]
    fn a_settings_that_is_not_a_table_is_rejected() {
        for bad in ["settings = 3", "settings = \"pdf\"", "settings = []"] {
            // Root keys must precede the first table header, so this goes
            // beside `version` rather than after the mapping.
            let errs = parse_err(&MINIMAL.replace(
                "version = 3",
                &format!(
                    "version = 3
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

    #[test]
    fn a_setting_that_should_be_a_boolean_is_rejected_when_it_is_not_one() {
        for value in ["\"true\"", "\"yes\"", "1", "0"] {
            let text = format!("{MINIMAL}\n[settings]\nhide_on_blur = {value}\n");
            let errs = parse_err(&text);
            assert!(
                messages(&errs).contains("hide_on_blur must be true or false"),
                "{value} was accepted or ignored: {:?}",
                messages(&errs)
            );
        }

        for value in ["true", "false"] {
            let text = format!("{MINIMAL}\n[settings]\nhide_on_blur = {value}\n");
            parse(&text, p(), ConfigSource::BuiltIn).expect(value);
        }
    }

    /// `enabled` defaults to `true`, so an unparsed value must not enable it.
    #[test]
    fn a_mapping_enabled_by_a_non_boolean_is_rejected_rather_than_enabled() {
        let text = "version = 3\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\n\
             kind = \"tree\"\nenabled = \"no\"\n";
        let errs = parse_err(text);
        assert!(
            messages(&errs).contains("enabled must be true or false"),
            "{:?}",
            messages(&errs)
        );
    }

    #[test]
    fn two_enabled_flat_mappings_are_both_accepted() {
        let text = r#"
version = 3

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

    #[test]
    fn ten_mappings_are_all_accepted_and_all_searched() {
        let mut text = String::from("version = 3\n");
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

    /// Both would share a cache key and every hit would appear twice.
    #[test]
    fn two_mappings_on_the_same_directory_are_refused() {
        let text = r#"
version = 3

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

    /// The tree walk would index the inner mapping's files, duplicating hits.
    #[test]
    fn a_mapping_nested_inside_a_walked_tree_is_refused() {
        let text = r#"
version = 3

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

    /// A flat parent lists only its own directory, so nothing is duplicated.
    #[test]
    fn a_mapping_below_a_flat_share_is_allowed() {
        let text = r#"
version = 3

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
version = 3

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

    #[test]
    fn several_tree_mappings_are_accepted() {
        let text = r#"
version = 3

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
        // Searched in configuration order.
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
        let errs = parse_err(&MINIMAL.replace("version = 3", "version = 9"));
        assert!(messages(&errs).contains("unsupported version 9"));

        let errs = parse_err(&MINIMAL.replace("version = 3\n", ""));
        assert!(messages(&errs).contains("missing `version`"));
    }

    #[test]
    fn a_file_with_no_mappings_is_rejected() {
        let errs = parse_err("version = 3\n");
        assert!(messages(&errs).contains("no `[[mapping]]`"));
    }

    /// Version 1 files are the exception and report only the migration message.
    #[test]
    fn every_error_is_reported_at_once() {
        let text = "
version = 3

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

    // --- bringing an old file forward ---------------------------------------

    /// A representative version 1 file.
    const V1_FILE: &str = r#"
version = 1

[[mapping]]
name    = "jobs"
path    = 'R:\'
kind    = "job-folder"
enabled = true
case    = "lower"

  [[mapping.rules]]
  pattern = '^([A-Z0-9]+)-([A-Z])-([A-Z0-9]+)$'
  folder  = '${1}${2}'
"#;

    #[test]
    fn a_version_one_file_is_brought_forward_rather_than_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, V1_FILE).unwrap();

        let (parsed, migrated) = load_migrating(&path, false).expect("it must load");

        assert_eq!(parsed.routes.all().len(), 1);
        assert_eq!(parsed.routes.all()[0].kind, MappingKind::Tree);
        let migrated = migrated.expect("it must report that it happened");
        assert_eq!(migrated.from, 1);
    }

    #[test]
    fn the_original_is_kept_beside_the_file_it_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, V1_FILE).unwrap();

        let (_, migrated) = load_migrating(&path, false).unwrap();
        let backup = migrated.unwrap().backup;

        assert_eq!(backup.parent(), path.parent());
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), V1_FILE);
    }

    #[test]
    fn the_file_on_disk_is_the_one_that_was_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, V1_FILE).unwrap();

        load_migrating(&path, false).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains(&format!("version = {CONFIG_VERSION}")),
            "{after}"
        );
        assert!(!after.contains("job-folder"), "{after}");
        // A second load does not migrate again.
        let (_, migrated) = load_migrating(&path, false).unwrap();
        assert_eq!(
            migrated, None,
            "it migrated a file that was already current"
        );
    }

    #[test]
    fn an_ordinary_mistake_in_a_current_file_is_reported_rather_than_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let broken = format!("version = {CONFIG_VERSION}\n[[mapping]]\nname = \"x\"\n");
        std::fs::write(&path, &broken).unwrap();

        assert!(load_migrating(&path, false).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            broken,
            "a file with an ordinary mistake in it was rewritten"
        );
    }

    #[test]
    fn a_version_from_the_future_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let future = "version = 99\n";
        std::fs::write(&path, future).unwrap();

        assert!(load_migrating(&path, false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), future);
    }

    // --- updates ------------------------------------------------------------

    #[test]
    fn an_update_folder_is_read_as_a_path() {
        // A Rust raw string holding a TOML literal string, so backslashes reach
        // the parser unchanged.
        let text = format!(
            "{MINIMAL}\n[settings]\n{}\n",
            r"update_from = '\\server\software\files'"
        );
        assert_eq!(
            parse_ok(&text).settings.update_from.as_deref(),
            Some(Path::new(r"\\server\software\files"))
        );
    }

    /// Unset means no update folder is checked at all.
    #[test]
    fn no_update_folder_is_the_default() {
        assert_eq!(parse_ok(MINIMAL).settings.update_from, None);
        assert_eq!(parse_ok(DEFAULT_CONFIG_TOML).settings.update_from, None);
    }

    // --- aliases ------------------------------------------------------------

    fn with_alias(body: &str) -> String {
        format!("{MINIMAL}\n[[alias]]\n{body}\n")
    }

    #[test]
    fn a_configuration_with_no_aliases_is_perfectly_ordinary() {
        assert!(parse_ok(MINIMAL).aliases.is_empty());
        assert!(parse_ok(DEFAULT_CONFIG_TOML).aliases.is_empty());
    }

    #[test]
    fn an_alias_is_read_back_with_its_code_and_note() {
        let parsed = parse_ok(&with_alias(
            "name = \"pw\"\ncode = \"11-D-0704\"\nnote = \"Powerwall bracket\"",
        ));
        let alias = parsed.aliases.resolve("pw").expect("pw should resolve");
        assert_eq!(alias.code.as_ref(), "11-D-0704");
        assert_eq!(alias.note.as_deref(), Some("Powerwall bracket"));
    }

    #[test]
    fn a_note_is_optional_and_an_empty_one_is_no_note() {
        let parsed = parse_ok(&with_alias(
            "name = \"pw\"\ncode = \"11-D-0704\"\nnote = \"  \"",
        ));
        assert_eq!(parsed.aliases.resolve("pw").unwrap().note, None);
    }

    /// A filter with no search term, such as `ext:pdf`, is not a valid code.
    #[test]
    fn an_alias_whose_code_cannot_be_searched_for_is_refused() {
        let errs = parse_err(&with_alias("name = \"pw\"\ncode = \"ext:pdf\""));
        let text = messages(&errs);
        assert!(text.contains("alias \"pw\""), "{text}");
        assert!(text.contains("not a valid search"), "{text}");
    }

    #[test]
    fn an_alias_may_stand_for_a_two_character_code() {
        let parsed = parse_ok(&with_alias("name = \"pw\"\ncode = \"ab\""));
        assert_eq!(parsed.aliases.resolve("pw").unwrap().code.as_ref(), "ab");
    }

    #[test]
    fn an_alias_may_stand_for_a_line_that_carries_syntax() {
        let parsed = parse_ok(&with_alias("name = \"inv\"\ncode = \"ext:pdf inverter\""));
        assert_eq!(
            parsed.aliases.resolve("inv").unwrap().code.as_ref(),
            "ext:pdf inverter"
        );
    }

    #[test]
    fn two_aliases_of_the_same_name_are_refused_rather_than_one_winning() {
        let text = format!(
            "{MINIMAL}\n[[alias]]\nname = \"pw\"\ncode = \"11-D-0704\"\n\
             \n[[alias]]\nname = \"PW\"\ncode = \"11-D-0705\"\n"
        );
        assert!(messages(&parse_err(&text)).contains("already an alias"));
    }

    #[test]
    fn an_alias_needs_both_a_name_and_a_code() {
        assert!(messages(&parse_err(&with_alias("code = \"11-D-0704\""))).contains("needs a name"));
        assert!(messages(&parse_err(&with_alias("name = \"pw\""))).contains("needs a code"));
    }

    #[test]
    fn an_alias_name_with_a_space_in_it_is_refused() {
        let errs = parse_err(&with_alias("name = \"p w\"\ncode = \"11-D-0704\""));
        assert!(messages(&errs).contains("cannot contain a space"));
    }

    #[test]
    fn a_non_ascii_alias_name_is_refused_rather_than_folded_inconsistently() {
        let errs = parse_err(&with_alias("name = \"pü\"\ncode = \"11-D-0704\""));
        assert!(messages(&errs).contains("must be ASCII"));
    }

    #[test]
    fn an_unknown_key_in_an_alias_is_an_error_like_anywhere_else() {
        let errs = parse_err(&with_alias(
            "name = \"pw\"\ncode = \"11-D-0704\"\ncolour = \"red\"",
        ));
        assert!(messages(&errs).contains("unknown key \"colour\""));
    }

    #[test]
    fn alias_is_accepted_at_the_root() {
        assert!(ROOT_KEYS.contains(&"alias"));
    }

    #[test]
    fn an_alias_error_calls_it_an_alias_and_not_a_mapping() {
        let errs = parse_err(&with_alias("name = \"pw\"\ncode = \"ext:pdf\""));
        let text = messages(&errs);
        assert!(text.contains("alias \"pw\""), "{text}");
        assert!(!text.contains("mapping \"pw\""), "{text}");
    }
}
