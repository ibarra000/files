//! The `DirSource` trait: the seam between the index and the filesystem.
//!
//! Everything above it can run against an in-memory fake, so only the Windows
//! implementations need real drives. Entries are pushed into an `EntrySink`
//! rather than returned, so UTF-16 names stream into the arena without an
//! intermediate `Vec<String>`.

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
    /// Appends a name given as UTF-16.
    ///
    /// Returns `false` when the sink is full and enumeration should stop.
    fn push_wide(&mut self, name: &[u16], meta: EntryMeta) -> bool;

    /// Appends a name given as UTF-8.
    fn push_str(&mut self, name: &str, meta: EntryMeta) -> bool;

    /// Returns the number of entries accepted so far.
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
/// without arena construction.
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
    /// Skip subdirectories. Always true in this application.
    pub files_only: bool,
    pub buffer_bytes: usize,
    pub max_entries: usize,
    pub deadline: Option<Instant>,
    /// Overrides the configured strategy, for `--bench`.
    pub force: Option<EnumStrategy>,
    /// Skip files Windows marks hidden or system.
    ///
    /// Applied at enumeration because the snapshot does not store attributes,
    /// so a change takes effect only on the next scan (see `config::hidden`).
    /// Off by default so `--bench` and the parity fixtures see every file.
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

    /// Returns true when the deadline has passed.
    pub fn expired(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

/// Statistics for one enumeration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListStats {
    pub entries: usize,
    /// Buffer refills, approximating SMB round trips, which dominate wall
    /// clock time on a share. Reported by `--bench`.
    pub round_trips: u32,
    pub elapsed: Duration,
    pub strategy: Option<EnumStrategy>,
    /// False when the enumeration stopped at a cap or deadline before the end
    /// of the directory.
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
    /// Returns `Unsupported` when the source cannot push the filter down; the
    /// caller must then match locally.
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

    /// Touches the root cheaply so the SMB session and any DFS referral are
    /// established before the first query.
    fn prewarm(&self, _root: &Path) {}

    fn name(&self) -> &'static str;
}

/// Returns true when a tree walk should descend into an entry.
///
/// Not the negation of [`is_listable_file`]: for a junction neither holds.
/// Directory reparse points resolve server-side and can point at another share
/// or back into this tree, and a file-id cycle guard is unreliable because
/// many SMB servers report a zero id. The walk counts skipped junctions.
#[inline]
pub fn is_walkable_dir(attributes: u32) -> bool {
    const DIRECTORY: u32 = 0x0000_0010;
    const REPARSE: u32 = 0x0000_0400;
    attributes & DIRECTORY != 0 && attributes & REPARSE == 0
}

/// Returns true when a directory entry should be listed.
///
/// Skips directories and directory reparse points (junctions, directory
/// symlinks). File reparse points such as OneDrive placeholders and
/// deduplicated files are kept.
#[inline]
pub fn is_listable_file(attributes: u32) -> bool {
    const DIRECTORY: u32 = 0x0000_0010;
    attributes & DIRECTORY == 0
}

/// Like [`is_listable_file`], also excluding hidden and system files when
/// `hide` is set.
///
/// There is no counterpart for [`is_walkable_dir`]: some servers mark a whole
/// share or department folder as system, and skipping it would hide the share.
#[inline]
pub fn is_listable_file_with(attributes: u32, hide: bool) -> bool {
    const HIDDEN: u32 = 0x0000_0002;
    const SYSTEM: u32 = 0x0000_0004;
    is_listable_file(attributes) && !(hide && attributes & (HIDDEN | SYSTEM) != 0)
}

/// Returns true for the `.` and `..` pseudo-entries.
///
/// A volume root such as `V:\` does not emit them; a subfolder such as
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

    /// Hidden and system attributes never stop a folder from being walked.
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
        // attribute without the directory attribute.
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
