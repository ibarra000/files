//! The seam between the index and the filesystem.
//!
//! Everything above this trait can be exercised against an in-memory fake, so
//! the actor, the store, the search pipeline and the whole UI state machine
//! are testable on a machine where the network drives do not exist. The only
//! untestable code is the Windows implementation on the other side.
//!
//! Entries are **pushed**, not returned. That lets the Windows enumerators
//! stream UTF-16 filenames straight into the arena with no intermediate
//! `Vec<String>`, and lets the fake feed a million synthetic entries without
//! materialising them.

use std::path::Path;
use std::time::{Duration, Instant};

use super::DirStamp;
use super::builder::SnapshotBuilder;
use super::errors::EnumError;
use crate::config::{DIR_BUFFER_BYTES, EnumStrategy};
use crate::util::cancel::CancelToken;

/// Per-entry metadata available without an extra round trip.
#[derive(Debug, Clone, Copy, Default)]
pub struct EntryMeta {
    pub attributes: u32,
}

/// Receives filenames during an enumeration.
pub trait EntrySink {
    /// Appends a name given as UTF-16, as Windows delivers it.
    ///
    /// Returns `false` when the sink is full and enumeration should stop.
    fn push_wide(&mut self, name: &[u16], meta: EntryMeta) -> bool;

    /// Appends a name given as UTF-8.
    fn push_str(&mut self, name: &str, meta: EntryMeta) -> bool;

    /// Entries accepted so far.
    fn accepted(&self) -> usize;
}

impl EntrySink for SnapshotBuilder {
    fn push_wide(&mut self, name: &[u16], _meta: EntryMeta) -> bool {
        SnapshotBuilder::push_wide(self, name)
    }

    fn push_str(&mut self, name: &str, _meta: EntryMeta) -> bool {
        SnapshotBuilder::push_str(self, name)
    }

    fn accepted(&self) -> usize {
        self.len()
    }
}

/// Counts names without storing them. Used by `--bench` to time enumeration
/// without paying for arena construction.
#[derive(Debug, Default)]
pub struct CountingSink {
    pub count: usize,
    pub name_bytes: usize,
}

impl EntrySink for CountingSink {
    fn push_wide(&mut self, name: &[u16], _meta: EntryMeta) -> bool {
        self.count += 1;
        self.name_bytes += name.len();
        true
    }

    fn push_str(&mut self, name: &str, _meta: EntryMeta) -> bool {
        self.count += 1;
        self.name_bytes += name.len();
        true
    }

    fn accepted(&self) -> usize {
        self.count
    }
}

/// Collects names into a `Vec`. Test and diagnostic use only.
#[derive(Debug, Default)]
pub struct VecSink {
    pub names: Vec<String>,
}

impl EntrySink for VecSink {
    fn push_wide(&mut self, name: &[u16], _meta: EntryMeta) -> bool {
        self.names.push(String::from_utf16_lossy(name));
        true
    }

    fn push_str(&mut self, name: &str, _meta: EntryMeta) -> bool {
        self.names.push(name.to_string());
        true
    }

    fn accepted(&self) -> usize {
        self.names.len()
    }
}

/// Enumeration tuning.
#[derive(Debug, Clone)]
pub struct ListOpts {
    /// Skip subdirectories. Always true in this application: files in
    /// subfolders of the resolved path must never be considered.
    pub files_only: bool,
    pub buffer_bytes: usize,
    pub max_entries: usize,
    pub deadline: Option<Instant>,
    /// Overrides the configured strategy, for `--bench`.
    pub force: Option<EnumStrategy>,
    /// Skip files Windows marks hidden or system.
    ///
    /// Acted on here rather than at search time because the attribute is not
    /// part of the name and is therefore not kept: the snapshot stores bytes,
    /// so this is the last moment anyone knows. The consequence, which
    /// `config::hidden` sets out, is that turning this on only takes effect
    /// once the share is next scanned.
    ///
    /// Off by default so that a caller which has not been told otherwise -
    /// `--bench`, the parity fixtures - enumerates the directory as it is.
    pub hide_system: bool,
}

impl Default for ListOpts {
    fn default() -> Self {
        Self {
            files_only: true,
            buffer_bytes: DIR_BUFFER_BYTES,
            max_entries: usize::MAX,
            deadline: None,
            force: None,
            hide_system: false,
        }
    }
}

impl ListOpts {
    pub fn with_max_entries(mut self, n: usize) -> Self {
        self.max_entries = n;
        self
    }

    pub fn with_strategy(mut self, s: EnumStrategy) -> Self {
        self.force = Some(s);
        self
    }

    pub fn with_deadline(mut self, at: Instant) -> Self {
        self.deadline = Some(at);
        self
    }

    pub fn hiding_system(mut self, yes: bool) -> Self {
        self.hide_system = yes;
        self
    }

    /// True when the deadline has passed.
    pub fn expired(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

/// What an enumeration did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListStats {
    pub entries: usize,
    /// Buffer refills, i.e. an approximation of SMB round trips. Reported by
    /// `--bench`, since round-trip count is what actually governs wall clock
    /// on a network share.
    pub round_trips: u32,
    pub elapsed: Duration,
    pub strategy: Option<EnumStrategy>,
    /// True when the enumeration stopped at a cap or deadline rather than at
    /// the end of the directory.
    pub complete: bool,
}

/// A source of directory listings.
pub trait DirSource: Send + Sync {
    /// Reads the directory's timestamps without enumerating it.
    fn probe_stamp(&self, dir: &Path) -> Result<DirStamp, EnumError>;

    /// Enumerates the immediate child files of `dir`.
    fn list(
        &self,
        dir: &Path,
        sink: &mut dyn EntrySink,
        opts: &ListOpts,
        cancel: &CancelToken,
    ) -> Result<ListStats, EnumError>;

    /// Enumerates only entries matching a wildcard, evaluated by the server.
    ///
    /// Returns `Unsupported` when the source cannot push the filter down, in
    /// which case the caller must fall back to matching locally.
    fn query(
        &self,
        _dir: &Path,
        _wildcard: &str,
        _sink: &mut dyn EntrySink,
        _opts: &ListOpts,
        _cancel: &CancelToken,
    ) -> Result<ListStats, EnumError> {
        Err(EnumError::Unsupported(0))
    }

    /// Touches the root cheaply so an SMB session, and any DFS referral, is
    /// established before the user's first query needs it.
    fn prewarm(&self, _root: &Path) {}

    fn name(&self) -> &'static str;
}

/// True when a tree walk should descend into an entry with these attributes.
///
/// Deliberately the mirror of [`is_listable_file`] rather than its negation:
/// for an ordinary entry exactly one of the two holds, and for a junction
/// *neither* does. A directory reparse point is resolved server-side on a
/// mapped share, so it can point at another share entirely or back into this
/// tree, and the only sound cycle guard - a visited set keyed on file id -
/// needs an id many SMB servers report as zero. Refusing to descend is the
/// honest position; the walk counts them so a skipped subtree is visible
/// rather than silent.
#[inline]
pub fn is_walkable_dir(attributes: u32) -> bool {
    const DIRECTORY: u32 = 0x0000_0010;
    const REPARSE: u32 = 0x0000_0400;
    attributes & DIRECTORY != 0 && attributes & REPARSE == 0
}

/// True when a directory entry with these attributes should be listed.
///
/// Mirrors the previous `DirEntry::file_type().is_file()` behaviour: skip
/// directories, and skip directory reparse points (junctions, directory
/// symlinks). File reparse points - OneDrive placeholders, deduplicated
/// files - are kept, because they are files the user genuinely wants to find
/// and excluding them would be a silent regression.
#[inline]
pub fn is_listable_file(attributes: u32) -> bool {
    const DIRECTORY: u32 = 0x0000_0010;
    attributes & DIRECTORY == 0
}

/// The same question, asked of a configuration that also hides system files.
///
/// `hide` narrows [`is_listable_file`] and nothing else. It deliberately has
/// no counterpart for [`is_walkable_dir`]: some file servers mark an entire
/// share or a whole department's folder system, and honouring that on a
/// directory would quietly remove the share this program exists to search.
/// Whatever a folder is marked, it is walked.
#[inline]
pub fn is_listable_file_with(attributes: u32, hide: bool) -> bool {
    const HIDDEN: u32 = 0x0000_0002;
    const SYSTEM: u32 = 0x0000_0004;
    is_listable_file(attributes) && !(hide && attributes & (HIDDEN | SYSTEM) != 0)
}

/// True for the `.` and `..` pseudo-entries.
///
/// A volume root such as `V:\` does not emit them, but a job folder such as
/// `R:\ab1234\` does.
#[inline]
pub fn is_dot_entry_wide(name: &[u16]) -> bool {
    matches!(name, [46] | [46, 46])
}

#[inline]
pub fn is_dot_entry(name: &str) -> bool {
    name == "." || name == ".."
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRECTORY: u32 = 0x0010;
    const REPARSE: u32 = 0x0400;
    const NORMAL: u32 = 0x0080;
    const HIDDEN: u32 = 0x0002;
    const SYSTEM: u32 = 0x0004;

    #[test]
    fn a_marked_file_is_listed_unless_the_caller_asked_otherwise() {
        for marked in [NORMAL | HIDDEN, NORMAL | SYSTEM, NORMAL | HIDDEN | SYSTEM] {
            assert!(
                is_listable_file_with(marked, false),
                "{marked:#x} vanished from a caller that never asked"
            );
            assert!(
                !is_listable_file_with(marked, true),
                "{marked:#x} was still listed"
            );
        }
    }

    #[test]
    fn an_ordinary_file_is_listed_either_way() {
        for attributes in [NORMAL, 0, NORMAL | REPARSE] {
            assert!(is_listable_file_with(attributes, true), "{attributes:#x}");
            assert!(is_listable_file_with(attributes, false), "{attributes:#x}");
        }
    }

    /// The narrowing applies to files and to nothing else. Some file servers
    /// mark a whole share or a department's folder system, and honouring that
    /// on a directory would quietly remove the share this program searches -
    /// which is a far worse failure than the noise it was meant to remove.
    #[test]
    fn a_marked_folder_is_still_walked() {
        for marked in [
            DIRECTORY | HIDDEN,
            DIRECTORY | SYSTEM,
            DIRECTORY | HIDDEN | SYSTEM,
        ] {
            assert!(is_walkable_dir(marked), "{marked:#x} would hide a share");
        }
    }

    #[test]
    fn skips_directories() {
        assert!(!is_listable_file(DIRECTORY));
        assert!(!is_listable_file(DIRECTORY | REPARSE));
    }

    #[test]
    fn keeps_ordinary_files() {
        assert!(is_listable_file(NORMAL));
        assert!(is_listable_file(0));
    }

    #[test]
    fn keeps_file_reparse_points() {
        // Cloud placeholders and deduplicated files carry the reparse
        // attribute without the directory attribute, and are still files.
        assert!(is_listable_file(REPARSE));
        assert!(is_listable_file(NORMAL | REPARSE));
    }

    #[test]
    fn recognises_dot_entries_in_both_encodings() {
        assert!(is_dot_entry("."));
        assert!(is_dot_entry(".."));
        assert!(!is_dot_entry("...")); // a real, if odd, filename
        assert!(!is_dot_entry(".hidden"));

        assert!(is_dot_entry_wide(&[46]));
        assert!(is_dot_entry_wide(&[46, 46]));
        assert!(!is_dot_entry_wide(&[46, 46, 46]));
        assert!(!is_dot_entry_wide(&[46, 104]));
    }

    #[test]
    fn counting_sink_tracks_entries_and_bytes() {
        let mut s = CountingSink::default();
        s.push_str("abc", EntryMeta::default());
        s.push_wide(&[97, 98], EntryMeta::default());
        assert_eq!(s.accepted(), 2);
        assert_eq!(s.name_bytes, 5);
    }

    #[test]
    fn default_opts_are_files_only_and_uncapped() {
        let o = ListOpts::default();
        assert!(o.files_only);
        assert_eq!(o.max_entries, usize::MAX);
        assert!(!o.expired());
    }

    #[test]
    fn an_elapsed_deadline_is_reported_as_expired() {
        let o = ListOpts::default().with_deadline(Instant::now() - Duration::from_millis(1));
        assert!(o.expired());
    }

    #[test]
    fn snapshot_builder_is_an_entry_sink() {
        let mut b = SnapshotBuilder::new("V:\\");
        let sink: &mut dyn EntrySink = &mut b;
        assert!(sink.push_str("a.txt", EntryMeta::default()));
        assert!(sink.push_wide(&[98, 46, 116], EntryMeta::default()));
        assert_eq!(sink.accepted(), 2);
    }
}
