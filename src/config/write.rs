//! Changing values in `config.toml` without disturbing the rest of it.
//!
//! This is the only thing in the program that writes to the user's
//! configuration. It began with one value, because F2 switches viewers and the
//! choice should still be there tomorrow; the settings window writes the rest
//! through the same steps, every one of which is here because of a way this
//! can go wrong.
//!
//! # Why a format-preserving edit
//!
//! That file is meant to be opened and edited by hand - it ships full of
//! comments explaining why CustomPro has to come first and why a pattern must
//! be a literal string. Re-serialising it from a parsed model would return it
//! with every one of those comments gone, from a keypress the user might have
//! hit by accident. `toml_edit` replaces the one value and leaves every byte
//! around it alone.
//!
//! # Why the result is parsed before it is kept
//!
//! `SETTINGS_KEYS` is an allowlist and an unrecognised key is a *hard startup
//! error* - see the note at the top of [`super::file`] about why a bad
//! configuration must stop the program rather than fall back. That rule is
//! aimed at the user's typos, but it applies just as well to ours: a mistake
//! in this writer would leave the app refusing to start, which is a spectacular
//! punishment for pressing F2. So the rendered text goes back through the very
//! same parser the next launch will use, and is abandoned if it does not pass.
//!
//! # Why there is no lock
//!
//! Two copies running, both pressing F2, is a genuine race. It is also a race
//! where the worst outcome is that one of two viewer settings wins - both
//! re-read the file, both rewrite one value, and temp-then-rename means
//! neither can observe a half-written file. A lock file would add a
//! stale-lock failure mode that outlives the process that took it, which is
//! strictly worse than the problem it solves.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, value};

use super::ViewerKind;
use crate::app::event::{AppEvent, Events, OpenMsg};

/// Why the configuration could not be updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// There is no file to write to: `--no-config`, or a profile where one
    /// could never be created.
    NoConfigFile,
    Io(String),
    /// The rewrite would not have parsed. Refused rather than saved, because
    /// saving it would stop the program starting next time.
    WouldNotReload(String),
    /// `[settings]` is there but is not a table, so there is nothing to set a
    /// key on.
    NotATable,
    /// The value went in but did not come back out - see `save_viewer`.
    DidNotStick,
}

impl WriteError {
    pub fn detail(&self) -> String {
        match self {
            Self::NoConfigFile => "there is no configuration file to save to".into(),
            Self::Io(e) => e.clone(),
            Self::WouldNotReload(e) => {
                format!("the change was refused because it would not reload: {e}")
            }
            Self::NotATable => {
                "`settings` in the configuration file is not a [settings] table".into()
            }
            Self::DidNotStick => "the configuration file would not have taken the new value".into(),
        }
    }
}

/// A key this module is allowed to write.
///
/// An enum rather than a `&str`, so [`current`] is exhaustive over it. The
/// read-back check is the only thing that proves a save did anything, and a
/// key it forgot to check is a save that silently does nothing - which is the
/// failure this whole module exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKey {
    Viewer,
    Theme,
    Hotkey,
    History,
    StaleNotices,
    LiveUpdates,
    PdfViewer,
    HideExtensions,
    HideSystemFiles,
}

impl SettingKey {
    pub const ALL: [Self; 9] = [
        Self::Viewer,
        Self::Theme,
        Self::Hotkey,
        Self::History,
        Self::StaleNotices,
        Self::LiveUpdates,
        Self::PdfViewer,
        Self::HideExtensions,
        Self::HideSystemFiles,
    ];

    /// The spelling in the file.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Theme => "theme",
            Self::Hotkey => "hotkey",
            Self::History => "history",
            Self::StaleNotices => "stale_notices",
            Self::LiveUpdates => "live_updates",
            Self::PdfViewer => "pdf_viewer",
            Self::HideExtensions => "hide_extensions",
            Self::HideSystemFiles => "hide_system_files",
        }
    }

    /// The environment variable that outranks the file for this key.
    ///
    /// Kept beside the file spelling because the two are one fact: a key the
    /// loader reads from two places is a key that can be written to one of
    /// them and then quietly ignored.
    pub const fn env(self) -> &'static str {
        match self {
            Self::Viewer => "FILES_VIEWER",
            Self::Theme => "FILES_THEME",
            Self::Hotkey => "FILES_HOTKEY",
            Self::History => "FILES_HISTORY",
            Self::StaleNotices => "FILES_STALE_NOTICES",
            Self::LiveUpdates => "FILES_LIVE_UPDATES",
            Self::PdfViewer => "FILES_PDF_VIEWER",
            Self::HideExtensions => "FILES_HIDE_EXTENSIONS",
            Self::HideSystemFiles => "FILES_HIDE_SYSTEM_FILES",
        }
    }

    /// Whether an empty value of [`Self::env`] still counts as set.
    ///
    /// True only for the hide list, where `FILES_HIDE_EXTENSIONS=` is the
    /// deliberate way to say "hide nothing for this run" - a request that
    /// would otherwise look unset and let the file quietly win.
    /// [`crate::config::Settings::apply_file_settings`] makes the same
    /// distinction, and these two have to agree.
    pub const fn empty_is_a_value(self) -> bool {
        matches!(self, Self::HideExtensions)
    }

    /// Its position, for the bitmask in [`crate::config::Settings`].
    pub const fn bit(self) -> u16 {
        1 << (self as u16)
    }

    /// Whether a change is picked up without restarting.
    ///
    /// A property of the key because two places need it and must not
    /// disagree: `app::state::settings` applies exactly these, and
    /// `view::settings` is what tells the user the rest will wait. Two lists
    /// would be one promise and one way of breaking it.
    ///
    /// What makes a key one of these is that it is read where it is used.
    /// `Settings` is cloned into the backend, both workers and every index
    /// actor, so anything one of them captured at startup cannot be changed
    /// underneath it.
    pub const fn applies_at_once(self) -> bool {
        matches!(
            self,
            Self::Theme | Self::Viewer | Self::History | Self::StaleNotices
        )
    }
}

/// Every key above is one the loader accepts.
///
/// Checked rather than trusted, because `SETTINGS_KEYS` is an allowlist and an
/// unrecognised key is a hard startup error. A typo in the table above would
/// not fail here - it would fail on the user's next launch, in a file a button
/// they pressed had just written. This makes it a build error instead.
const _: () = {
    const fn same(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    let mut i = 0;
    while i < SettingKey::ALL.len() {
        let name = SettingKey::ALL[i].name();
        let mut found = false;
        let mut j = 0;
        while j < super::file::SETTINGS_KEYS.len() {
            if same(name, super::file::SETTINGS_KEYS[j]) {
                found = true;
            }
            j += 1;
        }
        assert!(found, "a writable key is missing from SETTINGS_KEYS");
        i += 1;
    }
};

/// A value in the shape it will be written, and compared in on the way back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scalar {
    Str(String),
    Bool(bool),
    /// A path, written as a TOML *literal* string.
    ///
    /// `'V:\Documents'` rather than `"V:\\Documents"`. A basic string
    /// processes backslash escapes, so a Windows path written into one comes
    /// back either mangled or as a parse error. That is the first thing the
    /// shipped file warns the user about, and it would be a poor thing for the
    /// program to then do to its own file.
    Path(String),
    List(Vec<String>),
}

impl Scalar {
    fn to_item(&self) -> Item {
        match self {
            Self::Str(s) => value(s.as_str()),
            Self::Bool(b) => value(*b),
            Self::Path(p) => Item::Value(literal(p)),
            Self::List(items) => {
                let mut array = toml_edit::Array::new();
                for item in items {
                    array.push(item.as_str());
                }
                value(array)
            }
        }
    }
}

/// A path as a literal string, falling back to a basic one.
///
/// A literal string has no escape mechanism at all, so it cannot hold a single
/// quote. `V:\O'Brien` therefore has to be written the other way - which is
/// correct there, because the backslash escaping `toml_edit` applies is what
/// the loader will undo.
fn literal(path: &str) -> toml_edit::Value {
    // The two characters a literal string cannot hold. There is no escape for
    // either, so these go back to a basic string - which is correct there,
    // because the escaping `toml_edit` applies is what the loader will undo.
    if path.contains('\'') || path.contains('\n') {
        return path.into();
    }
    // `toml_edit` will not build a literal string directly: both repr
    // constructors are private to the crate. So one is parsed and its value
    // taken, which is sound precisely because the two cases that could make
    // the snippet mean something else are handled above.
    format!("x = '{path}'")
        .parse::<DocumentMut>()
        .ok()
        .and_then(|doc| doc.get("x").and_then(Item::as_value).cloned())
        .unwrap_or_else(|| path.into())
}

/// One change to the configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Set the key, replacing it if it is already there.
    Set { key: SettingKey, value: Scalar },
    /// Remove the key, returning the setting to its default.
    Unset { key: SettingKey },
}

/// What a control in the settings window produced.
///
/// Two shapes, because there are two kinds of control that can produce a
/// value: a box with text in it and a switch. Which of the four [`Scalar`]s
/// that becomes is the key's business, not the window's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    Text(String),
    Flag(bool),
}

impl Edit {
    /// What a control's value amounts to, for this key.
    ///
    /// The per-key shape lives here rather than in the window because it is
    /// the same knowledge [`current`] needs to read the value back: a key
    /// written as a list and checked as a string would report every save as
    /// having not stuck.
    pub fn from_typed(key: SettingKey, typed: Typed) -> Self {
        let value = match (key, typed) {
            (_, Typed::Flag(on)) => Scalar::Bool(on),

            // An emptied box means "no viewer of my own", which is the
            // default - so the key goes away rather than being written as a
            // path to nowhere.
            (SettingKey::PdfViewer, Typed::Text(text)) if text.trim().is_empty() => {
                return Self::Unset { key };
            }
            (SettingKey::PdfViewer, Typed::Text(text)) => Scalar::Path(text.trim().to_string()),

            // An empty list is not an absent one. `hide_extensions = []` is
            // the documented way to hide nothing, and unsetting it here would
            // silently restore the shipped list instead.
            (SettingKey::HideExtensions, Typed::Text(text)) => Scalar::List(
                text.split(',')
                    .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                    .filter(|e| !e.is_empty())
                    .collect(),
            ),

            (_, Typed::Text(text)) => Scalar::Str(text.trim().to_string()),
        };
        Self::Set { key, value }
    }

    pub fn key(&self) -> SettingKey {
        match self {
            Self::Set { key, .. } | Self::Unset { key } => *key,
        }
    }
}

/// Applies every edit, preserving everything else in the file.
pub fn save(path: &Path, edits: &[Edit]) -> Result<(), WriteError> {
    // Re-read rather than reuse whatever was parsed at startup. Another
    // instance - or the user, in an editor - may have changed a mapping since,
    // and rewriting from stale text would quietly undo their work.
    let text = std::fs::read_to_string(path).map_err(|e| WriteError::Io(e.to_string()))?;
    let updated = apply(&text, edits)?;

    // Proof that the next start will accept what is about to be written.
    let reloaded = super::file::parse(
        &updated,
        path,
        crate::paths::ConfigSource::Default(path.to_path_buf()),
    )
    .map_err(|errs| {
        WriteError::WouldNotReload(
            errs.first()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "unknown".into()),
        )
    })?;

    // Parsing is not enough: it proves the file is still *valid*, not that the
    // value is still *there*. Reading it back through the real loader is the
    // only thing that proves what the next start will actually see, and
    // reporting a save that silently does nothing is the one failure this
    // whole module exists to avoid.
    for edit in edits {
        let stuck = match edit {
            Edit::Set { key, value } => current(*key, &reloaded.settings).as_ref() == Some(value),
            Edit::Unset { key } => current(*key, &reloaded.settings).is_none(),
        };
        if !stuck {
            return Err(WriteError::DidNotStick);
        }
    }

    write_atomically(path, &updated).map_err(|e| WriteError::Io(e.to_string()))
}

/// Returns `text` with every edit applied, as a pure string transformation.
///
/// Separated from the filesystem so every shape the user's file can be in is
/// testable without one.
pub fn apply(text: &str, edits: &[Edit]) -> Result<String, WriteError> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| WriteError::Io(e.to_string()))?;

    // `settings` present but not a table - `settings = 3`, or `[[settings]]`
    // mistyped for `[settings]` - would send the assignment below into
    // toml_edit's `IndexMut`, which is an `.expect("index not found")`. The
    // loader rejects this shape now, but this runs against whatever is on disk
    // at the moment the key is pressed, which need not be what started the
    // program.
    if let Some(existing) = doc.get("settings")
        && !existing.is_table()
    {
        return Err(WriteError::NotATable);
    }

    // Only for an edit that needs somewhere to put a value. Removing a key
    // from a table that does not exist is already done.
    if edits.iter().any(|e| matches!(e, Edit::Set { .. })) && !doc.contains_key("settings") {
        let mut table = Table::new();
        // Without this the table is "implicit" and is not rendered at all, so
        // the key would be written under no heading and read back as a
        // top-level key - which `ROOT_KEYS` rejects.
        table.set_implicit(false);
        // The shipped file ends in a comment block. A new table with no decor
        // of its own absorbs the text above it as its prefix, which visibly
        // eats the user's comments; an explicit blank line stops that.
        table.decor_mut().set_prefix("\n");
        doc["settings"] = Item::Table(table);
    }

    for edit in edits {
        match edit {
            // A plain assignment. Deliberately no attempt to find and
            // uncomment an existing `# viewer = ...` line: toml_edit has no
            // notion of commented-out keys, and doing it by string surgery is
            // how a hand-edited file gets corrupted.
            Edit::Set { key, value } => doc["settings"][key.name()] = value.to_item(),
            Edit::Unset { key } => {
                if let Some(table) = doc.get_mut("settings").and_then(Item::as_table_mut) {
                    table.remove(key.name());
                }
            }
        }
    }
    Ok(doc.to_string())
}

/// What the loader would now apply for `key`, in the shape it was written in.
///
/// `None` means the file says nothing about it, so the default is in force.
fn current(key: SettingKey, s: &super::file::FileSettings) -> Option<Scalar> {
    match key {
        SettingKey::Viewer => s.viewer.clone().map(Scalar::Str),
        SettingKey::Theme => s.theme.clone().map(Scalar::Str),
        // Compared through the canonical spelling rather than the text that
        // was written, because `ctrl+shift+space` and `Ctrl+Shift+Space` are
        // the same chord and a save is not a failure for having been tidied.
        SettingKey::Hotkey => s.hotkey.map(|spec| {
            Scalar::Str(match spec.bound() {
                Some(hk) => crate::hotkey::spec::describe(hk),
                None => "off".into(),
            })
        }),
        SettingKey::History => s.history.map(Scalar::Bool),
        SettingKey::StaleNotices => s.stale_notices.map(Scalar::Bool),
        SettingKey::LiveUpdates => s.live_updates.map(Scalar::Bool),
        SettingKey::PdfViewer => s
            .pdf_viewer
            .as_ref()
            .map(|p| Scalar::Path(p.to_string_lossy().into_owned())),
        SettingKey::HideExtensions => s.hide_extensions.clone().map(Scalar::List),
        SettingKey::HideSystemFiles => s.hide_system_files.map(Scalar::Bool),
    }
}

/// Writes `viewer` into `[settings]`, preserving everything else.
pub fn save_viewer(path: &Path, viewer: ViewerKind) -> Result<(), WriteError> {
    save(path, &[viewer_edit(viewer)])
}

/// Returns `text` with the viewer set, as a pure string transformation.
pub fn with_viewer(text: &str, viewer: ViewerKind) -> Result<String, WriteError> {
    apply(text, &[viewer_edit(viewer)])
}

fn viewer_edit(viewer: ViewerKind) -> Edit {
    Edit::Set {
        key: SettingKey::Viewer,
        value: Scalar::Str(viewer.name().to_string()),
    }
}

/// Replaces the file in one step.
///
/// The same temp-then-rename this crate uses for the index, for a sharper
/// reason: a half-written `config.toml` is not a degraded cache, it is a
/// startup failure with exit code 2, on a file the user wrote by hand.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;

    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!("config.toml.{:x}.tmp", std::process::id()));

    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Saves off the UI thread, and reports what happened.
///
/// Detached rather than run inline in `dispatch`, because `%APPDATA%` roams:
/// in a domain that maps `R:\` and `V:\` it is very often redirected to a
/// network path, and a synchronous write would put an SMB round trip - or an
/// SMB timeout - directly in the keypress.
///
/// The cost of that choice is that this thread is not tracked by
/// `Actors::shutdown`, so F2 immediately followed by closing the window can
/// kill it mid-write. Temp-then-rename bounds that to a stray `.tmp` beside
/// the configuration rather than a damaged one.
pub fn save_viewer_async(path: Option<PathBuf>, viewer: ViewerKind, events: Events) {
    save_async(path, vec![viewer_edit(viewer)], events, move |outcome| {
        AppEvent::Open(match outcome {
            Ok(()) => OpenMsg::ViewerSaved { viewer },
            Err(detail) => OpenMsg::ViewerSaveFailed { detail },
        })
    });
}

/// Saves off the UI thread, reporting the outcome through `report`.
///
/// The caller supplies the event because what to say about a failed save
/// depends on what was being saved: F2 has a footer to correct, and the
/// settings window has a field to put back.
pub fn save_async(
    path: Option<PathBuf>,
    edits: Vec<Edit>,
    events: Events,
    report: impl FnOnce(Result<(), String>) -> AppEvent + Send + 'static,
) {
    let reporter = events.clone();
    let spawned = std::thread::Builder::new()
        .name("files-config-write".into())
        .spawn(move || {
            // Every other worker in this crate catches its own panics, and
            // this one parses a file a user hand-edits. Without this a panic
            // would take the thread with it and send nothing at all, so F2
            // would flip the footer and silently never save - and the default
            // hook would print the panic straight into the raw-mode terminal
            // the panel is drawn in.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match path {
                None => Err(WriteError::NoConfigFile.detail()),
                Some(path) => save(&path, &edits).map_err(|e| e.detail()),
            }));

            let _ = events.send(report(outcome.unwrap_or_else(|_| {
                Err("the configuration file could not be rewritten".into())
            })));
        });

    // A thread that never started would otherwise be the quietest failure of
    // the lot.
    if spawned.is_err() {
        let _ = reporter.send(AppEvent::Open(OpenMsg::ViewerSaveFailed {
            detail: "could not start the configuration writer".into(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::file::DEFAULT_CONFIG_TOML;

    fn reload(text: &str) -> super::super::file::ParsedConfig {
        super::super::file::parse(
            text,
            Path::new("cfg.toml"),
            crate::paths::ConfigSource::BuiltIn,
        )
        .expect("a rewritten configuration must load")
    }

    /// The whole reason this uses `toml_edit` rather than re-serialising.
    #[test]
    fn rewriting_the_viewer_leaves_every_comment_intact() {
        let before = DEFAULT_CONFIG_TOML;
        let after = with_viewer(before, ViewerKind::Avwin).unwrap();

        for line in before.lines().filter(|l| l.trim_start().starts_with('#')) {
            assert!(after.contains(line), "lost a comment: {line}");
        }
        assert_eq!(reload(&after).settings.viewer.as_deref(), Some("avwin"));
    }

    #[test]
    fn rewriting_the_viewer_replaces_rather_than_duplicates_it() {
        let once = with_viewer(DEFAULT_CONFIG_TOML, ViewerKind::Avwin).unwrap();
        let twice = with_viewer(&once, ViewerKind::Pdf).unwrap();

        assert_eq!(twice.matches("viewer = ").count(), 1);
        assert_eq!(reload(&twice).settings.viewer.as_deref(), Some("pdf"));
    }

    /// What an older config looks like: the table is there, the key is not.
    #[test]
    fn rewriting_the_viewer_when_the_key_is_absent_adds_it_to_the_existing_table() {
        let text = "version = 2\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\n\
                    kind = \"flat\"\n\n\
                    [settings]\n# matcher = \"simd\"\npersist = true\n";
        let after = with_viewer(text, ViewerKind::Avwin).unwrap();

        assert!(after.contains("persist = true"), "{after}");
        assert!(after.contains("# matcher"), "{after}");
        assert_eq!(reload(&after).settings.viewer.as_deref(), Some("avwin"));
    }

    /// A new table appended to a file that ends in comments will swallow them
    /// into its own decor unless it is given a prefix of its own.
    #[test]
    fn rewriting_the_viewer_with_no_settings_table_does_not_swallow_the_trailing_comments() {
        let text = "version = 2\n\n[[mapping]]\nname = 'jobs'\npath = 'R:\\'\n\
                    kind = \"flat\"\n\n\
                    # a parting note the user wrote\n# and a second line of it\n";
        let after = with_viewer(text, ViewerKind::Pdf).unwrap();

        assert!(
            after.contains("# a parting note the user wrote"),
            "the comment must survive: {after}"
        );
        assert!(after.contains("# and a second line of it"), "{after}");
        assert!(after.contains("[settings]"), "{after}");
        assert_eq!(reload(&after).settings.viewer.as_deref(), Some("pdf"));
    }

    /// Everything the writer can emit has to survive the loader, or F2 could
    /// leave the program unable to start.
    #[test]
    fn every_viewer_the_writer_emits_still_reloads() {
        for viewer in ViewerKind::ALL {
            let after = with_viewer(DEFAULT_CONFIG_TOML, viewer).unwrap();
            assert_eq!(
                reload(&after).settings.viewer.as_deref(),
                Some(viewer.name())
            );
        }
    }

    #[test]
    fn a_rewrite_replaces_the_file_rather_than_truncating_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, DEFAULT_CONFIG_TOML).unwrap();

        save_viewer(&path, ViewerKind::Avwin).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        assert!(after.contains("viewer = \"avwin\""), "{after}");
        assert!(after.contains("CustomPro"), "the body must be untouched");
        assert!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")),
            "no temporary file should be left behind"
        );
    }

    #[test]
    fn a_missing_file_is_reported_rather_than_created() {
        let err = save_viewer(
            Path::new(r"C:\definitely-not-here-4a91\config.toml"),
            ViewerKind::Pdf,
        )
        .unwrap_err();
        assert!(matches!(err, WriteError::Io(_)));
        assert!(!err.detail().is_empty());
    }

    /// Refusing is the point: the alternative is a config that stops the
    /// program starting, caused by a keypress.
    #[test]
    fn a_rewrite_that_would_not_reload_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Valid TOML, but not a valid configuration - no mappings at all.
        std::fs::write(&path, "version = 1\n").unwrap();

        let err = save_viewer(&path, ViewerKind::Pdf).unwrap_err();
        assert!(matches!(err, WriteError::WouldNotReload(_)), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "version = 1\n",
            "the file must be left exactly as it was"
        );
    }

    /// `doc["settings"]["viewer"] = ...` is an `.expect()` underneath, so a
    /// `settings` that is not a table used to panic the writer thread - which
    /// had no panic handler, so F2 silently did nothing at all.
    #[test]
    fn a_malformed_settings_table_is_refused_rather_than_panicking() {
        for text in [
            "version = 1
settings = 3
",
            "version = 1
settings = \"pdf\"
",
            "version = 1
settings = []
",
        ] {
            assert_eq!(
                with_viewer(text, ViewerKind::Avwin),
                Err(WriteError::NotATable),
                "{text:?}"
            );
        }
    }

    /// Parsing only proves the file is still valid. An inline
    /// `settings = { .. }` accepts the key and then the loader ignores the
    /// whole block, so the save "succeeded" and the setting never applied.
    #[test]
    fn a_rewrite_the_loader_would_ignore_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = DEFAULT_CONFIG_TOML.replace("[settings]", "[other]");
        std::fs::write(
            &path,
            format!(
                "{original}
settings = {{ persist = true }}
"
            ),
        )
        .unwrap();

        let before = std::fs::read_to_string(&path).unwrap();
        assert!(save_viewer(&path, ViewerKind::Avwin).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "a refused save must leave the file alone"
        );
    }

    #[test]
    fn every_error_carries_a_usable_detail() {
        assert!(
            WriteError::NoConfigFile
                .detail()
                .contains("no configuration")
        );
        assert_eq!(WriteError::Io("boom".into()).detail(), "boom");
        assert!(
            WriteError::WouldNotReload("x".into())
                .detail()
                .contains("would not reload")
        );
    }
}
