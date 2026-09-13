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
}

impl MappingKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "flat" => Some(Self::Flat),
            "tree" | "recursive" => Some(Self::Tree),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Tree => "tree",
        }
    }

    /// True when this mapping is indexed in the background.
    ///
    /// Every kind is, now that job folders are gone. Kept because it names the
    /// property the search actually depends on, and a third kind that is not
    /// indexed is a plausible thing to add.
    pub fn is_indexed(self) -> bool {
        matches!(self, Self::Flat | Self::Tree)
    }
}

/// One configured share.
#[derive(Clone, Debug)]
pub struct Mapping {
    pub id: MappingId,
    pub name: Box<str>,
    pub path: PathBuf,
    pub kind: MappingKind,
    pub enabled: bool,
    /// Whether a timer may re-read this share. See [`RefreshPolicy`].
    pub refresh: RefreshPolicy,
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
            MappingKind::Tree => Self::Manual,
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
}
