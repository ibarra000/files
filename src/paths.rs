//! The configured shares (mappings) and the set of targets a query searches.
//!
//! Every enabled share is searched; no code is routed to a single folder.
//! `tests/tree_parity.rs` holds a frozen set of known file locations and
//! asserts the index finds all of them.

use std::path::{Path, PathBuf};

use smallvec::SmallVec;

use crate::util::winpath;

/// Creates the app's config, cache and history directories at startup and
/// returns those it had to create.
///
/// Writers also create these on demand; doing it up front surfaces an
/// unwritable profile once instead of as scattered worker failures.
pub fn ensure_app_dirs(settings: &crate::config::Settings) -> Vec<PathBuf> {
    let wanted = [
        crate::config::file::default_config_path().and_then(|p| p.parent().map(Path::to_path_buf)),
        settings.cache_dir.clone(),
        settings
            .history_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
    ];

    wanted
        .into_iter()
        .flatten()
        .filter(|dir| !dir.as_os_str().is_empty() && !dir.is_dir())
        // Best effort: a read-only profile must not stop startup.
        .filter(|dir| std::fs::create_dir_all(dir).is_ok())
        .collect()
}

/// Why a set of mappings cannot be used together.
///
/// Shared by the loader and the settings window so both reject the same
/// configuration with the same message. Per-mapping errors are reported by the
/// parser instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// Two mappings on one directory.
    ///
    /// The persisted index is keyed by a path hash, so both actors would share
    /// one cache entry, and every result would appear twice.
    SameDirectory { a: String, b: String, at: PathBuf },
    /// One mapping inside a tree mapping, which would index its files twice.
    ///
    /// A flat parent is allowed (it lists only its own directory), as is a live
    /// one (it skips roots of other enabled mappings).
    NestedInTree {
        inner: String,
        inner_at: PathBuf,
        outer: String,
        outer_at: PathBuf,
    },
}

impl Conflict {
    pub fn detail(&self) -> String {
        match self {
            Self::SameDirectory { a, b, at } => format!(
                "drives `{a}` and `{b}` both point at {}; \
                 indexing one directory twice returns every file twice",
                at.display()
            ),
            Self::NestedInTree {
                inner,
                inner_at,
                outer,
                outer_at,
            } => format!(
                "drive `{inner}` ({}) lies inside the walked tree `{outer}` ({}); \
                 every file under it would be indexed twice and shown twice",
                inner_at.display(),
                outer_at.display()
            ),
        }
    }
}

/// Returns every conflict among the enabled, searched mappings.
pub fn conflicts(mappings: &[Mapping]) -> Vec<Conflict> {
    let mut out = Vec::new();
    for (i, a) in mappings.iter().enumerate() {
        if !a.enabled || !a.kind.is_searched() {
            continue;
        }
        for b in mappings.iter().skip(i + 1) {
            if !b.enabled || !b.kind.is_searched() {
                continue;
            }
            if winpath::same_dir(&a.path, &b.path) {
                out.push(Conflict::SameDirectory {
                    a: a.name.to_string(),
                    b: b.name.to_string(),
                    at: a.path.clone(),
                });
                continue;
            }
            let nested = if a.kind == MappingKind::Tree && winpath::contains(&a.path, &b.path) {
                Some((a, b))
            } else if b.kind == MappingKind::Tree && winpath::contains(&b.path, &a.path) {
                Some((b, a))
            } else {
                None
            };
            if let Some((outer, inner)) = nested {
                out.push(Conflict::NestedInTree {
                    inner: inner.name.to_string(),
                    inner_at: inner.path.clone(),
                    outer: outer.name.to_string(),
                    outer_at: outer.path.clone(),
                });
            }
        }
    }
    out
}

/// Identifies a mapping by its position in the configured list.
///
/// An integer rather than the name because it is compared per result row and
/// used as a map key.
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
    /// One large directory holding every job's files directly.
    Flat,
    /// The whole tree beneath the path, walked and indexed.
    ///
    /// Costs a background walk and a few hundred megabytes of memory.
    Tree,
    /// Never read in full; each search is sent to the file server as a
    /// wildcard query.
    ///
    /// For shares too large to walk. Costs a round trip per settled keystroke
    /// and covers only the folders a query reaches in time, so results report
    /// their coverage.
    Live,
}

impl MappingKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "flat" => Some(Self::Flat),
            "tree" | "recursive" => Some(Self::Tree),
            "live" | "on-demand" => Some(Self::Live),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Tree => "tree",
            Self::Live => "live",
        }
    }

    /// True when a background actor reads this share and holds it in memory.
    pub fn is_indexed(self) -> bool {
        matches!(self, Self::Flat | Self::Tree)
    }

    /// True when a search asks the file server about this share directly.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }

    /// True when a query is sent to this share by either route.
    ///
    /// Distinct from [`Self::is_indexed`]: this governs queries, while
    /// `is_indexed` governs the actor, cache, refresh schedule and drive list.
    pub fn is_searched(self) -> bool {
        self.is_indexed() || self.is_live()
    }
}

/// One configured share.
///
/// Equality is exact. A read-back is not (ids are positions and paths are
/// normalised), so `write::save` compares only the fields that round-trip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub id: MappingId,
    pub name: Box<str>,
    pub path: PathBuf,
    pub kind: MappingKind,
    pub enabled: bool,
    /// Whether a timer may re-read this share. See [`RefreshPolicy`].
    pub refresh: RefreshPolicy,
    /// How far below the root a live query descends.
    ///
    /// The parser rejects `depth` on indexed mappings, where it has no effect.
    pub depth: u16,
}

/// When a share is re-read in full, as opposed to patched from change-watcher
/// events.
///
/// A full pass over a large share can be hundreds of thousands of round trips,
/// so many clients doing it on a timer can overload the file server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RefreshPolicy {
    /// Re-read on the built-in cadence. Suited to shares that take seconds to
    /// read.
    #[default]
    Auto,
    /// Never re-read on a timer; watcher updates still apply.
    Manual,
}

impl RefreshPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "manual" | "on-demand" => Some(Self::Manual),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }

    /// Returns the default policy for a mapping kind.
    ///
    /// Flat shares default to `Auto` because a cheap stamp probe gates each
    /// re-read; trees have no such probe and default to `Manual`.
    pub fn default_for(kind: MappingKind) -> Self {
        match kind {
            MappingKind::Flat => Self::Auto,
            // Trees are too expensive to sweep on a schedule; live shares have
            // no index to refresh (the parser rejects `refresh` on them).
            MappingKind::Tree | MappingKind::Live => Self::Manual,
        }
    }

    pub fn is_manual(self) -> bool {
        self == Self::Manual
    }
}

/// Where a code resolves to.
///
/// `dir` is fully joined, so callers never join roots themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub mapping: MappingId,
    pub kind: MappingKind,
    pub dir: PathBuf,
}

/// Inline capacity of two covers typical configurations without allocating.
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

    /// A table holding exactly one enabled mapping, for `--doctor`, benchmarks
    /// and tests.
    pub fn single(name: &str, path: PathBuf, kind: MappingKind) -> Self {
        Self::new(
            vec![Mapping {
                id: MappingId(0),
                name: name.into(),
                path: winpath::normalise_root(&path),
                kind,
                enabled: true,
                refresh: RefreshPolicy::default_for(kind),
                depth: crate::config::DEFAULT_LIVE_DEPTH,
            }],
            ConfigSource::BuiltIn,
        )
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

    /// Names a mapping in an actionable error message.
    ///
    /// Uses the path, since messages from `EnumError::describe` refer to a
    /// location. Falls back to the name when the path is empty, which would
    /// otherwise render an error with no subject.
    pub fn path_label(&self, id: MappingId) -> String {
        match self.get(id) {
            Some(m) if !m.path.as_os_str().is_empty() => m.path.to_string_lossy().into_owned(),
            Some(m) => format!("the {} drive", m.name),
            None => "the drive".into(),
        }
    }

    /// Every enabled flat mapping, in configuration order.
    pub fn flat(&self) -> impl Iterator<Item = &Mapping> {
        self.enabled().filter(|m| m.kind == MappingKind::Flat)
    }

    /// Repoints a mapping by name, for an environment or command-line
    /// override.
    ///
    /// Returns false when no such mapping exists; callers should treat that as
    /// an error, since a dropped override searches the wrong share.
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

    /// Returns the configured mapping names, for error messages.
    pub fn names(&self) -> Vec<&str> {
        self.mappings.iter().map(|m| m.name.as_ref()).collect()
    }

    /// Every searched share, in configuration order. Independent of the query.
    pub fn targets(&self) -> TargetList {
        self.enabled()
            .filter(|m| m.kind.is_searched())
            .map(|m| Target {
                mapping: m.id,
                kind: m.kind,
                dir: m.path.clone(),
            })
            .collect()
    }

    /// Whether any enabled mapping is searchable, indexed or live.
    pub fn any_searchable(&self) -> bool {
        self.enabled().any(|m| m.kind.is_searched())
    }

    /// Every enabled live mapping, in configuration order.
    pub fn live(&self) -> impl Iterator<Item = &Mapping> {
        self.enabled().filter(|m| m.kind == MappingKind::Live)
    }
}

#[cfg(test)]
mod dir_tests {
    use super::*;

    #[test]
    fn a_missing_directory_is_created_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache").join("deeper");
        let settings = crate::config::Settings {
            cache_dir: Some(cache.clone()),
            history_path: None,
            ..Default::default()
        };

        let created = ensure_app_dirs(&settings);

        assert!(cache.is_dir(), "the directory was not created");
        assert!(created.contains(&cache), "{created:?}");
    }

    #[test]
    fn a_directory_that_is_already_there_is_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        let settings = crate::config::Settings {
            cache_dir: Some(dir.path().to_path_buf()),
            history_path: None,
            ..Default::default()
        };

        assert!(!ensure_app_dirs(&settings).contains(&dir.path().to_path_buf()));
    }

    #[test]
    fn a_directory_that_cannot_be_created_is_left_out_rather_than_fatal() {
        let settings = crate::config::Settings {
            // A path under a device file cannot be created.
            cache_dir: Some(PathBuf::from(r"\?\nul\files\cache")),
            history_path: None,
            ..Default::default()
        };

        let created = ensure_app_dirs(&settings);
        assert!(created.iter().all(|p| p.is_dir()), "{created:?}");
    }
}
