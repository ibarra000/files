//! The configured shares, and what a query is searched against.
//!
//! This module used to answer a harder question: *which folder does this code
//! live in?* Six hardcoded regexes, then a configured list of ordered rules,
//! all so that a code could be turned into one directory before anything went
//! looking in it.
//!
//! It answered wrongly and silently. A file whose folder did not follow the
//! naming rule was not reported missing - it was invisible, because nothing
//! ever looked in the folder it was actually in. Every share is indexed now,
//! so the question is answered by looking rather than by deducing, and what
//! remains here is the list of shares and their identities.
//!
//! `tests/tree_parity.rs` holds the frozen record of what the rules used to
//! resolve, and asserts the index finds all of it.

use std::path::{Path, PathBuf};

use smallvec::SmallVec;

use crate::util::winpath;

/// Creates the directories this program writes into, and says which it had to.
///
/// Every one of these is created on demand by whatever writes there, so this
/// is not what makes them exist - it is what makes their absence *visible*. A
/// profile where `%APPDATA%` is redirected to a share that is not mounted
/// fails one write at a time, in a worker, hours apart, and each failure looks
/// like a different bug. Asked all at once at startup, it is one line in
/// `--doctor` naming the directory nobody can write.
///
/// Returns what was created rather than what exists, so an ordinary start says
/// nothing at all and a first run after an upgrade says exactly what it added.
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
        // Best effort, and silently so per directory: a read-only profile is
        // a thing this program runs on, and the built-in defaults work there.
        // What it must not do is stop starting over a folder it only wanted.
        .filter(|dir| std::fs::create_dir_all(dir).is_ok())
        .collect()
}

/// Why a set of drives cannot be used together.
///
/// Shared by the loader and by the settings window, so that a configuration
/// the file refuses is one the window refuses, in the same words. These are
/// the rules about mappings *as a set* - a single mapping's own problems are
/// reported against the line that holds them, which only the parser can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// Nothing enabled, so nothing could ever be searched.
    NothingEnabled,
    /// Two mappings on one directory.
    ///
    /// Refused rather than merged: the persisted index is keyed by a hash of
    /// the path, so both actors would write the same cache entry and each cold
    /// start would restore whichever wrote last - and every result would
    /// appear twice in one merged list.
    SameDirectory { a: String, b: String, at: PathBuf },
    /// One mapping inside a walked tree.
    ///
    /// A tree walks the inner mapping's files as well as its own, so every hit
    /// inside appears twice and is walked twice on every pass. A *flat* parent
    /// is legal - it lists only its own directory - and so is a *live* one,
    /// which skips any directory that is the root of another enabled mapping.
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
            Self::NothingEnabled => "no enabled drives; nothing could ever be searched".into(),
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

/// Everything wrong with this set of drives taken together.
///
/// All of them rather than the first, for the reason the configuration loader
/// reports every error at once: making somebody fix four problems in four
/// attempts is gratuitous when all four are already known.
pub fn conflicts(mappings: &[Mapping]) -> Vec<Conflict> {
    let mut out = Vec::new();
    if !mappings.iter().any(|m| m.enabled) {
        out.push(Conflict::NothingEnabled);
    }

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
    /// Never read in full. Every search is put to the file server as a
    /// wildcard query against the share itself.
    ///
    /// The answer to a share a walk cannot pay for. Some are large enough that
    /// one pass costs minutes and a few hundred megabytes, and the honest
    /// options are to leave them out of the search altogether or to ask the
    /// server per query. This is the second. It costs a round trip on every
    /// settled keystroke, and it can only ever report on the folders one query
    /// had time to reach - which is why everything downstream of it is built to
    /// say how much that was.
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

    /// True when a background actor reads this share and holds the result in
    /// memory, so a search is a sweep over bytes this process already owns.
    ///
    /// The note this replaces said a third kind that is not indexed was "a
    /// plausible thing to add". [`Self::Live`] is it.
    pub fn is_indexed(self) -> bool {
        matches!(self, Self::Flat | Self::Tree)
    }

    /// True when a search asks the file server about this share directly.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }

    /// True when a query is put to this share at all, by either route.
    ///
    /// Separate from [`Self::is_indexed`] because the two answer different
    /// questions, and every call site that treated them as one was answering
    /// whichever it happened to be named after. "Is it searched" governs the
    /// query; "is it indexed" governs the actor, the cache, the refresh
    /// schedule and the drive list.
    pub fn is_searched(self) -> bool {
        self.is_indexed() || self.is_live()
    }
}

/// One configured share.
///
/// Comparable so the writer can check a saved list against what it meant to
/// save. Note that equality here is exact, which a read-back is not: an id is
/// a position and a path is normalised on the way in, so `write::save`
/// compares the parts that should survive rather than the whole.
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
    /// Meaningless on an indexed mapping, and refused there by the
    /// configuration parser rather than ignored: a key that silently does
    /// nothing leaves somebody believing a setting took.
    pub depth: u16,
}

/// When a share is re-read in full, as opposed to patched from whatever the
/// change watcher reports.
///
/// The distinction exists because the two costs are three orders of magnitude
/// apart: a patch reads the handful of folders that changed, while a full pass
/// over the job share is some nine hundred thousand round trips. One client
/// doing that on a timer is a background hum; three hundred doing it on the
/// same timer is an outage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RefreshPolicy {
    /// Re-read on the built-in cadence.
    ///
    /// Right for a share small enough that a pass is seconds - a single
    /// directory behind a three-round-trip freshness probe.
    #[default]
    Auto,
    /// Never re-read on a timer.
    ///
    /// Live updates still apply, so an ordinary day's changes still arrive
    /// within seconds; what stops is the unconditional periodic pass. Right
    /// for anything that costs minutes and is read by more than a handful of
    /// people.
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

    /// What a mapping of this kind gets when the configuration does not say.
    ///
    /// A tree defaults to `Manual` and a flat share to `Auto`, and the
    /// asymmetry is the whole point: a flat share is one directory that a
    /// stamp probe can check for three round trips, so re-reading it when the
    /// stamp moves costs almost nothing. A tree is three hundred thousand
    /// directories with no stamp to check, so the only honest default is to
    /// leave the timer off and let the watcher and the user drive it.
    pub fn default_for(kind: MappingKind) -> Self {
        match kind {
            MappingKind::Flat => Self::Auto,
            // Nothing to re-read on a timer in either case, though for
            // different reasons: a tree is too expensive to sweep on a
            // schedule, and a live share holds no index for a sweep to
            // refresh. The configuration refuses `refresh` on a live mapping
            // rather than accepting a setting that would do nothing.
            MappingKind::Tree | MappingKind::Live => Self::Manual,
        }
    }

    pub fn is_manual(self) -> bool {
        self == Self::Manual
    }
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

    /// A table holding exactly one enabled mapping.
    ///
    /// For `--doctor`, for benchmarks, and for the many tests that care about
    /// one share's behaviour rather than about routing.
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

    /// What to call a mapping in a message the user is expected to act on.
    ///
    /// The path, because every message routed through `EnumError::describe` is
    /// a sentence about a *location* - "{target} is not mapped", "access
    /// denied to {target}" - and a chosen name like "jobs" says nothing about
    /// which drive letter to go and reconnect.
    ///
    /// Falls back to the name when the path is empty, and that case is not
    /// hypothetical: it is the reported "random os error 03". `describe` was
    /// handed a derived `custpro_path`, which was an empty `PathBuf` whenever
    /// no flat mapping was enabled, and rendered a leading space followed by
    /// an error code attached to nothing at all.
    pub fn path_label(&self, id: MappingId) -> String {
        match self.get(id) {
            Some(m) if !m.path.as_os_str().is_empty() => m.path.to_string_lossy().into_owned(),
            Some(m) => format!("the {} share", m.name),
            None => "the share".into(),
        }
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
            .filter(|m| m.kind.is_searched())
            .map(|m| Target {
                mapping: m.id,
                kind: m.kind,
                dir: m.path.clone(),
            })
            .collect()
    }

    /// Whether anything is searchable at all, by either route.
    ///
    /// Renamed from `any_indexed` rather than kept alongside it: every caller
    /// gates a *query* on this, and a configuration holding nothing but live
    /// shares is searchable. Leaving both names would make picking the wrong
    /// one a silent mistake.
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

    /// Creates what is missing, and says so.
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

    /// An ordinary start says nothing at all, which is what makes the first
    /// start after an upgrade worth reading.
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

    /// A read-only profile is a thing this runs on, and the built-in defaults
    /// work there. It must not stop starting over a folder it only wanted.
    #[test]
    fn a_directory_that_cannot_be_created_is_left_out_rather_than_fatal() {
        let settings = crate::config::Settings {
            // A path under a file is not a directory anybody can make.
            cache_dir: Some(PathBuf::from(r"\?\nul\files\cache")),
            history_path: None,
            ..Default::default()
        };

        let created = ensure_app_dirs(&settings);
        assert!(created.iter().all(|p| p.is_dir()), "{created:?}");
    }
}
