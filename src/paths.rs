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
#[derive(Debug)]
pub struct Mapping {
    pub id: MappingId,
    pub name: Box<str>,
    pub path: PathBuf,
    pub kind: MappingKind,
    pub enabled: bool,
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
}
