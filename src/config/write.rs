//! Edits values in `config.toml` while preserving the rest of the file.
//!
//! The only writer of the user's configuration, used by F2 and the settings
//! window.
//!
//! # Format preservation
//!
//! The file is meant to be hand-edited and ships with explanatory comments, so
//! edits go through `toml_edit`, which replaces only the changed values.
//!
//! # Validation before saving
//!
//! An unrecognised key is a hard startup error (see [`super::file`]), so the
//! rendered text is parsed with the same loader the next launch uses and
//! discarded if it fails.
//!
//! # No lock
//!
//! Concurrent saves from two instances can only make one value win: each
//! re-reads the file and the temp-then-rename write is atomic. A lock file
//! would add a stale-lock failure mode, which is worse.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, value};

use crate::app::event::{AppEvent, Events, OpenMsg};
use std::sync::Arc;

/// Why the configuration could not be updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// No file to write to, as with `--no-config` or an uncreatable profile.
    NoConfigFile,
    Io(String),
    /// The rewritten file would fail to load, so it was not saved.
    WouldNotReload(String),
    /// `settings` exists but is not a table.
    NotATable,
    /// The file reloaded, but the new value did not read back.
    DidNotStick,
}

impl WriteError {
    pub fn detail(&self) -> String {
        match self {
            Self::NoConfigFile => "there is no configuration file to save to".into(),
            Self::Io(e) => e.clone(),
            Self::WouldNotReload(e) => {
                format!("the change was rejected because the file would not reload: {e}")
            }
            Self::NotATable => {
                "`settings` in the configuration file is not a [settings] table".into()
            }
            Self::DidNotStick => "the configuration file did not keep the new value".into(),
        }
    }
}

/// A `[settings]` key this module can write.
///
/// An enum so that [`current`], which verifies each save by reading it back,
/// must handle every key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKey {
    Viewer,
    Theme,
    Hotkey,
    History,
    StaleNotices,
    DevMode,
    LiveUpdates,
    PdfViewer,
    HideExtensions,
    HideSystemFiles,
    // Append new keys at the end: `bit()` is the variant's position, and
    // `Settings::cli_pinned` is a runtime bitmask of it.
    PdfReadOnly,
    UpdateFrom,
    IndexLog,
    Backdrop,
    ResultLayout,
    HideOnBlur,
    HideAfterOpening,
    HideOnEscape,
    Dock,
    Columns,
    CheckForUpdates,
}

impl SettingKey {
    pub const ALL: [Self; 21] = [
        Self::Viewer,
        Self::Theme,
        Self::Hotkey,
        Self::History,
        Self::StaleNotices,
        Self::DevMode,
        Self::LiveUpdates,
        Self::PdfViewer,
        Self::HideExtensions,
        Self::HideSystemFiles,
        Self::PdfReadOnly,
        Self::UpdateFrom,
        Self::IndexLog,
        Self::Backdrop,
        Self::ResultLayout,
        Self::HideOnBlur,
        Self::HideAfterOpening,
        Self::HideOnEscape,
        Self::Dock,
        Self::Columns,
        Self::CheckForUpdates,
    ];

    /// Returns the key's name in `config.toml`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Theme => "theme",
            Self::Hotkey => "hotkey",
            Self::History => "history",
            Self::StaleNotices => "stale_notices",
            Self::DevMode => "dev_mode",
            Self::LiveUpdates => "live_updates",
            Self::PdfViewer => "pdf_viewer",
            Self::HideExtensions => "hide_extensions",
            Self::HideSystemFiles => "hide_system_files",
            Self::PdfReadOnly => "pdf_read_only",
            Self::UpdateFrom => "update_from",
            Self::IndexLog => "index_log",
            Self::Backdrop => "backdrop",
            Self::ResultLayout => "result_layout",
            Self::Dock => "dock",
            Self::Columns => "columns",
            Self::CheckForUpdates => "check_for_updates",
            Self::HideOnBlur => "hide_on_blur",
            Self::HideAfterOpening => "hide_after_opening",
            Self::HideOnEscape => "hide_on_escape",
        }
    }

    /// Returns the environment variable that overrides this key in the file.
    pub const fn env(self) -> &'static str {
        match self {
            Self::Viewer => "FILES_VIEWER",
            Self::Theme => "FILES_THEME",
            Self::Hotkey => "FILES_HOTKEY",
            Self::History => "FILES_HISTORY",
            Self::StaleNotices => "FILES_STALE_NOTICES",
            Self::DevMode => "FILES_DEV_MODE",
            Self::LiveUpdates => "FILES_LIVE_UPDATES",
            Self::PdfViewer => "FILES_PDF_VIEWER",
            Self::HideExtensions => "FILES_HIDE_EXTENSIONS",
            Self::HideSystemFiles => "FILES_HIDE_SYSTEM_FILES",
            Self::PdfReadOnly => "FILES_PDF_READ_ONLY",
            Self::UpdateFrom => "FILES_UPDATE_FROM",
            Self::IndexLog => "FILES_INDEX_LOG",
            Self::Backdrop => "FILES_BACKDROP",
            Self::ResultLayout => "FILES_RESULT_LAYOUT",
            Self::Dock => "FILES_DOCK",
            Self::Columns => "FILES_COLUMNS",
            Self::CheckForUpdates => "FILES_CHECK_FOR_UPDATES",
            Self::HideOnBlur => "FILES_HIDE_ON_BLUR",
            Self::HideAfterOpening => "FILES_HIDE_AFTER_OPENING",
            Self::HideOnEscape => "FILES_HIDE_ON_ESCAPE",
        }
    }

    /// Whether an empty value of [`Self::env`] still counts as set.
    ///
    /// True only for the hide list, where `FILES_HIDE_EXTENSIONS=` means "hide
    /// nothing". Must agree with
    /// [`crate::config::Settings::apply_file_settings`].
    pub const fn empty_is_a_value(self) -> bool {
        matches!(self, Self::HideExtensions)
    }

    /// Returns the key's bit in the bitmask in [`crate::config::Settings`].
    ///
    /// The const assertion fails the build if the keys outgrow a `u32`.
    pub const fn bit(self) -> u32 {
        const _: () = assert!(SettingKey::ALL.len() <= u32::BITS as usize);
        1 << (self as u32)
    }

    /// Whether a change is picked up without restarting.
    ///
    /// `app::state::settings` applies exactly these keys live and
    /// `view::settings` marks the rest as needing a restart. A key qualifies
    /// only if it is read at the point of use, since `Settings` is cloned into
    /// the backend, workers and index actors at startup.
    pub const fn applies_at_once(self) -> bool {
        matches!(
            self,
            Self::Theme
                | Self::ResultLayout
                | Self::Dock
                | Self::Columns
                | Self::Viewer
                | Self::History
                | Self::StaleNotices
                | Self::DevMode
                | Self::HideOnBlur
                | Self::HideAfterOpening
                | Self::HideOnEscape
        )
    }
}

/// Asserts at compile time that every [`SettingKey`] name is in
/// `SETTINGS_KEYS`, since writing an unknown key would break the next startup.
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

/// A setting value, as written and as compared on read-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scalar {
    Str(String),
    Bool(bool),
    /// A path, written as a TOML literal string (`'V:\shared\files'`) so that
    /// backslashes are not treated as escapes.
    Path(String),
    List(Vec<String>),
    /// An integer, written unquoted (`columns = 2`).
    Int(i64),
}

impl Scalar {
    fn to_item(&self) -> Item {
        match self {
            Self::Str(s) => value(s.as_str()),
            Self::Bool(b) => value(*b),
            Self::Int(n) => value(*n),
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

/// Returns `path` as a literal string, or as an escaped basic string when it
/// contains a single quote or newline, which literal strings cannot hold.
fn literal(path: &str) -> toml_edit::Value {
    if path.contains('\'') || path.contains('\n') {
        return path.into();
    }
    // `toml_edit` has no public constructor for a literal string, so parse one.
    // Safe because the characters that could break the snippet are excluded
    // above.
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
    /// Replace the whole `[[alias]]` array. The comment block above the
    /// array is preserved.
    Aliases(Vec<crate::alias::Alias>),
    /// Replace the whole `[[mapping]]` array.
    ///
    /// Comments inside the mapping tables are lost; this is the only edit
    /// that drops user text, and the settings window warns before doing it.
    Mappings(Vec<crate::paths::Mapping>),
}

/// A raw value from a settings-window control: a text box or a switch.
///
/// [`Edit::from_typed`] converts it to the key's [`Scalar`] shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    Text(String),
    Flag(bool),
}

impl Edit {
    /// Converts a control's value into the edit for `key`.
    ///
    /// Must produce the same shape [`current`] reads back, or the save is
    /// reported as not sticking.
    pub fn from_typed(key: SettingKey, typed: Typed) -> Self {
        let value = match (key, typed) {
            (_, Typed::Flag(on)) => Scalar::Bool(on),

            // An empty path box means the default, so the key is removed.
            (
                SettingKey::PdfViewer | SettingKey::UpdateFrom | SettingKey::IndexLog,
                Typed::Text(text),
            ) if text.trim().is_empty() => {
                return Self::Unset { key };
            }
            (
                SettingKey::PdfViewer | SettingKey::UpdateFrom | SettingKey::IndexLog,
                Typed::Text(text),
            ) => Scalar::Path(text.trim().to_string()),

            // An empty list is written, not unset: `hide_extensions = []`
            // hides nothing, while an absent key restores the default list.
            (SettingKey::HideExtensions, Typed::Text(text)) => Scalar::List(
                text.split(',')
                    .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                    .filter(|e| !e.is_empty())
                    .collect(),
            ),

            // Written in canonical form (`Ctrl+Alt+J`) so the read-back
            // comparison in `save` matches what `describe` returns. An
            // unparseable chord is passed through so the loader reports it
            // with a line and column.
            (SettingKey::Hotkey, Typed::Text(text)) => {
                let text = text.trim();
                let tidy = crate::hotkey::spec::parse(text)
                    .map(|spec| match spec.bound() {
                        Some(hk) => crate::hotkey::spec::describe(hk),
                        None => "off".into(),
                    })
                    .unwrap_or_else(|_| text.to_string());
                Scalar::Str(tidy)
            }

            // Non-numeric text is passed through so the loader reports it with
            // a line and column.
            (SettingKey::Columns, Typed::Text(text)) => match text.trim().parse() {
                Ok(n) => Scalar::Int(n),
                Err(_) => Scalar::Str(text.trim().to_string()),
            },

            (_, Typed::Text(text)) => Scalar::Str(text.trim().to_string()),
        };
        Self::Set { key, value }
    }

    /// Returns the setting key, or `None` for alias and mapping edits.
    pub fn key(&self) -> Option<SettingKey> {
        match self {
            Self::Set { key, .. } | Self::Unset { key } => Some(*key),
            Self::Aliases(_) | Self::Mappings(_) => None,
        }
    }
}

/// Applies every edit, preserving everything else in the file.
pub fn save(path: &Path, edits: &[Edit]) -> Result<(), WriteError> {
    // Re-read so changes made since startup, by another instance or an
    // editor, are not overwritten.
    let text = std::fs::read_to_string(path).map_err(|e| WriteError::Io(e.to_string()))?;
    let updated = apply(&text, edits)?;

    // Verify the next start will accept the file.
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

    // A valid file is not enough; confirm each value reads back as written.
    for edit in edits {
        let stuck = match edit {
            Edit::Set { key, value } => current(*key, &reloaded.settings).as_ref() == Some(value),
            Edit::Unset { key } => current(*key, &reloaded.settings).is_none(),
            Edit::Aliases(want) => reloaded.aliases.all() == want.as_slice(),
            // Compared field by field: ids are list positions and paths are
            // normalised on load, so plain equality would fail.
            Edit::Mappings(want) => {
                let got = reloaded.routes.all();
                got.len() == want.len()
                    && got.iter().zip(want).all(|(g, w)| {
                        g.name == w.name
                            && g.kind == w.kind
                            && g.enabled == w.enabled
                            && crate::util::winpath::same_dir(&g.path, &w.path)
                    })
            }
        };
        if !stuck {
            return Err(WriteError::DidNotStick);
        }
    }

    write_atomically(path, &updated).map_err(|e| WriteError::Io(e.to_string()))
}

/// Returns `text` with every edit applied, as a pure string transformation.
///
pub fn apply(text: &str, edits: &[Edit]) -> Result<String, WriteError> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| WriteError::Io(e.to_string()))?;

    // A non-table `settings` (`settings = 3`, `[[settings]]`) would make the
    // `IndexMut` assignment below panic. The file on disk may have changed
    // since the loader validated it.
    if let Some(existing) = doc.get("settings")
        && !existing.is_table()
    {
        return Err(WriteError::NotATable);
    }

    // Create `[settings]` only when a value must be set.
    if edits.iter().any(|e| matches!(e, Edit::Set { .. })) && !doc.contains_key("settings") {
        let mut table = Table::new();
        // An implicit table renders no header, so its keys would land at the
        // root, which `ROOT_KEYS` rejects.
        table.set_implicit(false);
        // Without its own prefix the new table absorbs the trailing comment
        // block of the file as its decor.
        table.decor_mut().set_prefix("\n");
        doc["settings"] = Item::Table(table);
    }

    for edit in edits {
        match edit {
            // A commented-out `# key = ...` line is left alone; uncommenting
            // it would need string surgery on a hand-edited file.
            Edit::Set { key, value } => doc["settings"][key.name()] = value.to_item(),
            Edit::Unset { key } => {
                if let Some(table) = doc.get_mut("settings").and_then(Item::as_table_mut) {
                    table.remove(key.name());
                }
            }
            Edit::Mappings(mappings) => {
                let mut array = toml_edit::ArrayOfTables::new();
                for mapping in mappings {
                    let mut table = Table::new();
                    table["name"] = value(mapping.name.as_ref());
                    table["path"] = Item::Value(literal(&mapping.path.to_string_lossy()));
                    table["kind"] = value(mapping.kind.label());
                    table["enabled"] = value(mapping.enabled);
                    // The loader rejects `refresh` on a live mapping and
                    // `depth` on an indexed one, so write each only where valid.
                    if mapping.kind.is_indexed() {
                        table["refresh"] = value(mapping.refresh.label());
                    }
                    if mapping.kind.is_live() {
                        table["depth"] = value(i64::from(mapping.depth));
                    }
                    array.push(table);
                }
                doc["mapping"] = Item::ArrayOfTables(array);
            }
            Edit::Aliases(aliases) => {
                doc.remove("alias");
                if !aliases.is_empty() {
                    let mut array = toml_edit::ArrayOfTables::new();
                    for alias in aliases {
                        let mut table = Table::new();
                        table["name"] = value(alias.name.as_ref());
                        table["code"] = value(alias.code.as_ref());
                        if let Some(note) = &alias.note {
                            table["note"] = value(note.as_ref());
                        }
                        array.push(table);
                    }
                    doc["alias"] = Item::ArrayOfTables(array);
                }
            }
        }
    }
    Ok(doc.to_string())
}

/// Returns the loaded value for `key` in its written [`Scalar`] shape, or
/// `None` when the file leaves it at the default.
fn current(key: SettingKey, s: &super::file::FileSettings) -> Option<Scalar> {
    match key {
        SettingKey::Viewer => s.viewer.clone().map(Scalar::Str),
        SettingKey::Theme => s.theme.clone().map(Scalar::Str),
        // Canonical spelling, matching what `Edit::from_typed` writes.
        SettingKey::Hotkey => s.hotkey.map(|spec| {
            Scalar::Str(match spec.bound() {
                Some(hk) => crate::hotkey::spec::describe(hk),
                None => "off".into(),
            })
        }),
        SettingKey::History => s.history.map(Scalar::Bool),
        SettingKey::StaleNotices => s.stale_notices.map(Scalar::Bool),
        SettingKey::DevMode => s.dev_mode.map(Scalar::Bool),
        SettingKey::LiveUpdates => s.live_updates.map(Scalar::Bool),
        SettingKey::PdfViewer => s
            .pdf_viewer
            .as_ref()
            .map(|p| Scalar::Path(p.to_string_lossy().into_owned())),
        SettingKey::HideExtensions => s.hide_extensions.clone().map(Scalar::List),
        SettingKey::HideSystemFiles => s.hide_system_files.map(Scalar::Bool),
        SettingKey::HideOnBlur => s.hide_on_blur.map(Scalar::Bool),
        SettingKey::HideAfterOpening => s.hide_after_opening.map(Scalar::Bool),
        SettingKey::HideOnEscape => s.hide_on_escape.map(Scalar::Bool),
        SettingKey::PdfReadOnly => s.pdf_read_only.map(Scalar::Bool),
        SettingKey::UpdateFrom => s
            .update_from
            .as_ref()
            .map(|p| Scalar::Path(p.to_string_lossy().into_owned())),
        SettingKey::IndexLog => s
            .index_log
            .as_ref()
            .map(|p| Scalar::Path(p.to_string_lossy().into_owned())),
        SettingKey::Backdrop => s.backdrop.clone().map(Scalar::Str),
        SettingKey::ResultLayout => s.result_layout.clone().map(Scalar::Str),
        SettingKey::Dock => s.dock.clone().map(Scalar::Str),
        SettingKey::Columns => s.columns.map(|n| Scalar::Int(n as i64)),
        SettingKey::CheckForUpdates => s.check_for_updates.map(Scalar::Bool),
    }
}

/// Writes `viewer = name` into `[settings]`, preserving everything else.
///
/// Takes the configured spelling (see [`crate::config::ViewerKind::name`])
/// because only the caller holds the viewer list.
pub fn save_viewer(path: &Path, name: &str) -> Result<(), WriteError> {
    save(path, &[viewer_edit(name)])
}

/// Returns `text` with the viewer set, as a pure string transformation.
pub fn with_viewer(text: &str, name: &str) -> Result<String, WriteError> {
    apply(text, &[viewer_edit(name)])
}

fn viewer_edit(name: &str) -> Edit {
    Edit::Set {
        key: SettingKey::Viewer,
        value: Scalar::Str(name.to_string()),
    }
}

/// Atomically replaces the whole file with already-validated text.
///
/// Used by migration.
pub fn replace(path: &Path, text: &str) -> Result<(), String> {
    write_atomically(path, text).map_err(|e| e.to_string())
}

/// Replaces the file via temp-then-rename, so a failed write can never leave a
/// truncated `config.toml`, which would be a startup failure.
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

/// Saves the viewer choice on a background thread and reports the outcome.
///
/// Runs off the UI thread because `%APPDATA%` is often redirected to a network
/// share. The thread is not tracked by `Actors::shutdown`, so exiting mid-write
/// can leave a stray `.tmp`, but never a damaged configuration.
pub fn save_viewer_async(path: Option<PathBuf>, name: Arc<str>, events: Events) {
    save_async(path, vec![viewer_edit(&name)], events, move |outcome| {
        AppEvent::Open(match outcome {
            Ok(()) => OpenMsg::ViewerSaved { name },
            Err(detail) => OpenMsg::ViewerSaveFailed { detail },
        })
    });
}

/// Saves off the UI thread, reporting the outcome through `report`.
///
/// The caller builds the event, since F2 and the settings window handle a
/// failed save differently.
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
            // Catch panics so a failure is still reported rather than the
            // thread dying silently.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match path {
                None => Err(WriteError::NoConfigFile.detail()),
                Some(path) => save(&path, &edits).map_err(|e| e.detail()),
            }));

            let _ = events.send(report(outcome.unwrap_or_else(|_| {
                Err("the configuration file could not be rewritten".into())
            })));
        });

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

    /// Returns a non-default value for `key`. Exhaustive, so a new key must
    /// be added here.
    fn different(key: SettingKey) -> Typed {
        match key {
            SettingKey::Viewer => Typed::Text("pdf".into()),
            SettingKey::Theme => Typed::Text("dark".into()),
            SettingKey::Hotkey => Typed::Text("ctrl+alt+j".into()),
            SettingKey::PdfViewer => Typed::Text(r"C:\viewer.exe".into()),
            // A UNC path checks that `Scalar::Path` writes a literal string.
            SettingKey::UpdateFrom => Typed::Text(r"\\fileserver\software\files".into()),
            SettingKey::IndexLog => Typed::Text(r"C:\temp\files-index.log".into()),
            SettingKey::Backdrop => Typed::Text("mica".into()),
            SettingKey::ResultLayout => Typed::Text("detailed".into()),
            SettingKey::Dock => Typed::Text("bottom".into()),
            SettingKey::Columns => Typed::Text("2".into()),
            SettingKey::CheckForUpdates => Typed::Flag(false),
            SettingKey::HideExtensions => Typed::Text("zzz".into()),
            SettingKey::DevMode => Typed::Flag(true),
            SettingKey::History
            | SettingKey::StaleNotices
            | SettingKey::LiveUpdates
            | SettingKey::PdfReadOnly
            | SettingKey::HideOnBlur
            | SettingKey::HideAfterOpening
            | SettingKey::HideOnEscape
            | SettingKey::HideSystemFiles => Typed::Flag(false),
        }
    }

    /// Every key round-trips through `apply`, the loader and [`current`].
    #[test]
    fn every_writable_key_goes_into_the_file_and_is_recognised_coming_back() {
        for key in SettingKey::ALL {
            let edit = Edit::from_typed(key, different(key));
            let Edit::Set { value, .. } = &edit else {
                panic!("{} produced an unset from a real value", key.name());
            };
            let expected = value.clone();

            let after = apply(DEFAULT_CONFIG_TOML, std::slice::from_ref(&edit))
                .unwrap_or_else(|e| panic!("{} would not write: {:?}", key.name(), e));
            let parsed = reload(&after);

            assert_eq!(
                current(key, &parsed.settings),
                Some(expected),
                "{} was written and not read back",
                key.name()
            );
        }
    }

    #[test]
    fn a_chord_typed_in_lower_case_still_reports_that_it_saved() {
        let edit = Edit::from_typed(SettingKey::Hotkey, Typed::Text("ctrl+alt+j".into()));
        let after = apply(DEFAULT_CONFIG_TOML, std::slice::from_ref(&edit)).unwrap();

        let Edit::Set { value, .. } = &edit else {
            panic!("a chord is not an unset");
        };
        assert_eq!(
            current(SettingKey::Hotkey, &reload(&after).settings).as_ref(),
            Some(value),
            "a chord that saved reported that it had not"
        );
    }

    #[test]
    fn claiming_no_key_at_all_also_reports_that_it_saved() {
        let edit = Edit::from_typed(SettingKey::Hotkey, Typed::Text("OFF".into()));
        let after = apply(DEFAULT_CONFIG_TOML, std::slice::from_ref(&edit)).unwrap();

        let Edit::Set { value, .. } = &edit else {
            panic!("off is not an unset");
        };
        assert_eq!(
            current(SettingKey::Hotkey, &reload(&after).settings).as_ref(),
            Some(value)
        );
    }

    #[test]
    fn emptying_a_path_box_takes_the_key_out_of_the_file() {
        for key in [
            SettingKey::PdfViewer,
            SettingKey::UpdateFrom,
            SettingKey::IndexLog,
        ] {
            let edit = Edit::from_typed(key, Typed::Text("   ".into()));
            assert_eq!(
                edit,
                Edit::Unset { key },
                "{} wrote an empty path",
                key.name()
            );

            let set = apply(
                DEFAULT_CONFIG_TOML,
                &[Edit::from_typed(key, different(key))],
            )
            .unwrap();
            let cleared = apply(&set, &[edit]).unwrap();
            assert_eq!(
                current(key, &reload(&cleared).settings),
                None,
                "{} survived being emptied",
                key.name()
            );
        }
    }

    /// A UNC path is written as a literal string and reads back unchanged.
    #[test]
    fn a_unc_folder_survives_the_round_trip_intact() {
        const UNC: &str = r"\\fileserver\software\files";
        let after = apply(
            DEFAULT_CONFIG_TOML,
            &[Edit::from_typed(
                SettingKey::UpdateFrom,
                Typed::Text(UNC.into()),
            )],
        )
        .unwrap();
        assert!(
            after.contains(&format!("'{UNC}'")),
            "the folder was not written as a literal string"
        );
        assert_eq!(
            reload(&after).settings.update_from,
            Some(PathBuf::from(UNC))
        );
    }

    fn reload(text: &str) -> super::super::file::ParsedConfig {
        super::super::file::parse(
            text,
            Path::new("cfg.toml"),
            crate::paths::ConfigSource::BuiltIn,
        )
        .expect("a rewritten configuration must load")
    }

    #[test]
    fn rewriting_the_viewer_leaves_every_comment_intact() {
        let before = DEFAULT_CONFIG_TOML;
        let after = with_viewer(before, "pdf").unwrap();

        for line in before.lines().filter(|l| l.trim_start().starts_with('#')) {
            assert!(after.contains(line), "lost a comment: {line}");
        }
        assert_eq!(reload(&after).settings.viewer.as_deref(), Some("pdf"));
    }

    #[test]
    fn rewriting_the_viewer_replaces_rather_than_duplicates_it() {
        let once = with_viewer(DEFAULT_CONFIG_TOML, "auto").unwrap();
        let twice = with_viewer(&once, "pdf").unwrap();

        // Count whole `viewer =` lines; a substring count would also match
        // `# pdf_viewer = ...` in the shipped comments.
        let written = twice
            .lines()
            .filter(|l| l.trim_start().starts_with("viewer ="))
            .count();
        assert_eq!(written, 1, "the viewer was written twice");
        assert_eq!(reload(&twice).settings.viewer.as_deref(), Some("pdf"));
    }

    #[test]
    fn rewriting_the_viewer_when_the_key_is_absent_adds_it_to_the_existing_table() {
        let text = "version = 3\n\n[[mapping]]\nname = 'projects'\npath = 'R:\\'\n\
                    kind = \"flat\"\n\n\
                    [settings]\n# matcher = \"simd\"\npersist = true\n";
        let after = with_viewer(text, "pdf").unwrap();

        assert!(after.contains("persist = true"), "{after}");
        assert!(after.contains("# matcher"), "{after}");
        assert_eq!(reload(&after).settings.viewer.as_deref(), Some("pdf"));
    }

    #[test]
    fn rewriting_the_viewer_with_no_settings_table_does_not_swallow_the_trailing_comments() {
        let text = "version = 3\n\n[[mapping]]\nname = 'projects'\npath = 'R:\\'\n\
                    kind = \"flat\"\n\n\
                    # a parting note the user wrote\n# and a second line of it\n";
        let after = with_viewer(text, "pdf").unwrap();

        assert!(
            after.contains("# a parting note the user wrote"),
            "the comment must survive: {after}"
        );
        assert!(after.contains("# and a second line of it"), "{after}");
        assert!(after.contains("[settings]"), "{after}");
        assert_eq!(reload(&after).settings.viewer.as_deref(), Some("pdf"));
    }

    #[test]
    fn every_viewer_the_writer_emits_still_reloads() {
        // Configure an app viewer so every `ViewerKind` variant is covered.
        let text = format!(
            "{DEFAULT_CONFIG_TOML}
[[viewer]]
name = \"sketch\"
program = 'sketch.exe'
"
        );
        let viewers = reload(&text).viewers;
        for viewer in crate::config::ViewerKind::all(&viewers) {
            let name = viewer.name(&viewers);
            let after = with_viewer(&text, name).unwrap();
            assert_eq!(reload(&after).settings.viewer.as_deref(), Some(name));
        }
    }

    #[test]
    fn a_rewrite_replaces_the_file_rather_than_truncating_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, DEFAULT_CONFIG_TOML).unwrap();

        save_viewer(&path, "pdf").unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        assert!(after.contains("viewer = \"pdf\""), "{after}");
        assert!(after.contains("example-flat"), "the body must be untouched");
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
        let err =
            save_viewer(Path::new(r"C:\definitely-not-here-4a91\config.toml"), "pdf").unwrap_err();
        assert!(matches!(err, WriteError::Io(_)));
        assert!(!err.detail().is_empty());
    }

    #[test]
    fn a_rewrite_that_would_not_reload_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Valid TOML but an invalid configuration.
        std::fs::write(&path, "version = 1\n").unwrap();

        let err = save_viewer(&path, "pdf").unwrap_err();
        assert!(matches!(err, WriteError::WouldNotReload(_)), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "version = 1\n",
            "the file must be left exactly as it was"
        );
    }

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
                with_viewer(text, "pdf"),
                Err(WriteError::NotATable),
                "{text:?}"
            );
        }
    }

    /// An inline `settings = { .. }` parses but is ignored by the loader, so
    /// the read-back check must reject the save.
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
        assert!(save_viewer(&path, "pdf").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "a refused save must leave the file alone"
        );
    }

    // --- drives -------------------------------------------------------------

    fn mapping(
        i: u16,
        name: &str,
        path: &str,
        kind: crate::paths::MappingKind,
    ) -> crate::paths::Mapping {
        crate::paths::Mapping {
            id: crate::paths::MappingId(i),
            name: name.into(),
            path: std::path::PathBuf::from(path),
            kind,
            enabled: true,
            refresh: crate::paths::RefreshPolicy::default_for(kind),
            depth: crate::config::DEFAULT_LIVE_DEPTH,
        }
    }

    #[test]
    fn writing_drives_produces_a_file_that_loads_them_back() {
        use crate::paths::MappingKind;
        let after = apply(
            DEFAULT_CONFIG_TOML,
            &[Edit::Mappings(vec![
                mapping(0, "shared", r"V:\shared\files", MappingKind::Flat),
                mapping(1, "projects", r"R:\", MappingKind::Tree),
            ])],
        )
        .unwrap();

        let parsed = reload(&after);
        assert_eq!(parsed.routes.names(), ["shared", "projects"]);
        assert_eq!(parsed.routes.all()[0].kind, MappingKind::Flat);
        assert_eq!(parsed.routes.all()[1].kind, MappingKind::Tree);
    }

    #[test]
    fn a_drive_path_is_written_as_a_literal_string() {
        use crate::paths::MappingKind;
        let after = apply(
            DEFAULT_CONFIG_TOML,
            &[Edit::Mappings(vec![mapping(
                0,
                "projects",
                r"V:\shared\files",
                MappingKind::Flat,
            )])],
        )
        .unwrap();

        assert!(after.contains(r"'V:\shared\files'"), "{after}");
        assert_eq!(
            reload(&after).routes.all()[0].path,
            std::path::PathBuf::from(r"V:\shared\files")
        );
    }

    /// `refresh` is written only for indexed kinds and `depth` only for live.
    #[test]
    fn only_the_keys_that_mean_something_for_a_kind_are_written() {
        use crate::paths::MappingKind;
        let after = apply(
            DEFAULT_CONFIG_TOML,
            &[Edit::Mappings(vec![
                mapping(0, "walked", r"R:\", MappingKind::Tree),
                mapping(1, "asked", r"S:\", MappingKind::Live),
            ])],
        )
        .unwrap();

        reload(&after);
        assert_eq!(after.matches("refresh =").count(), 1, "{after}");
        assert_eq!(after.matches("depth =").count(), 1, "{after}");
    }

    /// Two mappings on one directory are rejected and the file is unchanged.
    #[test]
    fn a_drive_list_the_loader_would_refuse_is_refused_by_the_writer_too() {
        use crate::paths::MappingKind;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, DEFAULT_CONFIG_TOML).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let err = save(
            &path,
            &[Edit::Mappings(vec![
                mapping(0, "one", r"R:\", MappingKind::Tree),
                mapping(1, "two", r"R:\", MappingKind::Tree),
            ])],
        )
        .unwrap_err();

        assert!(matches!(err, WriteError::WouldNotReload(_)), "{err:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn writing_drives_twice_replaces_rather_than_appends() {
        use crate::paths::MappingKind;
        let once = apply(
            DEFAULT_CONFIG_TOML,
            &[Edit::Mappings(vec![mapping(
                0,
                "a",
                r"A:\",
                MappingKind::Tree,
            )])],
        )
        .unwrap();
        let twice = apply(
            &once,
            &[Edit::Mappings(vec![mapping(
                0,
                "b",
                r"B:\",
                MappingKind::Tree,
            )])],
        )
        .unwrap();

        assert_eq!(reload(&twice).routes.names(), ["b"]);
    }

    // --- aliases ------------------------------------------------------------

    fn alias(name: &str, code: &str) -> crate::alias::Alias {
        crate::alias::Alias {
            name: name.into(),
            code: code.into(),
            note: None,
        }
    }

    #[test]
    fn writing_aliases_produces_a_file_that_loads_them_back() {
        let after = apply(
            DEFAULT_CONFIG_TOML,
            &[Edit::Aliases(vec![
                alias("pw", "AB-C-0704"),
                crate::alias::Alias {
                    name: "inv".into(),
                    code: "ext:pdf invoice".into(),
                    note: Some("The invoice scans".into()),
                },
            ])],
        )
        .unwrap();

        let parsed = reload(&after);
        assert_eq!(parsed.aliases.len(), 2);
        assert_eq!(
            parsed.aliases.resolve("pw").unwrap().code.as_ref(),
            "AB-C-0704"
        );
        assert_eq!(
            parsed.aliases.resolve("inv").unwrap().note.as_deref(),
            Some("The invoice scans")
        );
    }

    #[test]
    fn writing_aliases_leaves_the_comments_around_them_intact() {
        let after = apply(
            DEFAULT_CONFIG_TOML,
            &[Edit::Aliases(vec![alias("pw", "AB-C-0704")])],
        )
        .unwrap();

        for line in DEFAULT_CONFIG_TOML
            .lines()
            .filter(|l| l.trim_start().starts_with('#'))
        {
            assert!(after.contains(line), "lost a comment: {line}");
        }
    }

    #[test]
    fn writing_aliases_twice_replaces_rather_than_appends() {
        let once = apply(
            DEFAULT_CONFIG_TOML,
            &[Edit::Aliases(vec![alias("pw", "AB-C-0704")])],
        )
        .unwrap();
        let twice = apply(&once, &[Edit::Aliases(vec![alias("pw", "AB-C-0999")])]).unwrap();

        let parsed = reload(&twice);
        assert_eq!(parsed.aliases.len(), 1);
        assert_eq!(
            parsed.aliases.resolve("pw").unwrap().code.as_ref(),
            "AB-C-0999"
        );
    }

    #[test]
    fn writing_an_empty_list_removes_the_aliases_entirely() {
        let with = apply(
            DEFAULT_CONFIG_TOML,
            &[Edit::Aliases(vec![alias("pw", "AB-C-0704")])],
        )
        .unwrap();
        let without = apply(&with, &[Edit::Aliases(Vec::new())]).unwrap();

        assert!(reload(&without).aliases.is_empty());
        // No live table remains, but the commented-out example survives.
        assert!(
            !without.lines().any(|l| l.trim() == "[[alias]]"),
            "{without}"
        );
        assert!(
            without.lines().any(|l| l.trim() == "# [[alias]]"),
            "the example in the comments must survive"
        );
    }

    #[test]
    fn an_alias_write_is_checked_on_the_way_back_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, DEFAULT_CONFIG_TOML).unwrap();

        save(&path, &[Edit::Aliases(vec![alias("pw", "AB-C-0704")])]).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(reload(&after).aliases.len(), 1);
    }

    /// An alias with no search terms is rejected and the file is unchanged.
    #[test]
    fn an_alias_the_loader_would_refuse_is_refused_by_the_writer_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, DEFAULT_CONFIG_TOML).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let err = save(&path, &[Edit::Aliases(vec![alias("pw", "ext:pdf")])]).unwrap_err();

        assert!(matches!(err, WriteError::WouldNotReload(_)), "{err:?}");
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
