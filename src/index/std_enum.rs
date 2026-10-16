//! Portable `std::fs::read_dir` enumeration.
//!
//! The last step of the fallback chain and the field override
//! (`FILES_FS_STRATEGY=std`). It is slow on network shares: `read_dir` fetches
//! about 4 KB per round trip, so a million-entry directory needs roughly
//! 45,000 serialised SMB requests. It also serves as the non-Windows backend.

use std::path::Path;
use std::time::Instant;

use super::DirStamp;
use super::enumerate::{
    DirSource, EntryMeta, EntrySink, ListOpts, ListStats, is_dot_entry, is_listable_file_with,
};
use super::errors::EnumError;
use crate::config::EnumStrategy;
use crate::util::cancel::CancelToken;

/// Returns the entry's hidden and system attribute bits.
///
/// `DirEntry::metadata` on Windows reuses the `WIN32_FIND_DATA` from the scan,
/// so this costs no extra round trip. Off Windows it returns 0; dotfiles are
/// not treated as hidden so both paths agree on which files exist.
#[cfg(windows)]
fn marked(entry: &std::fs::DirEntry) -> u32 {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x0000_0002;
    const SYSTEM: u32 = 0x0000_0004;
    entry
        .metadata()
        .map(|m| m.file_attributes() & (HIDDEN | SYSTEM))
        .unwrap_or(0)
}

#[cfg(not(windows))]
fn marked(_entry: &std::fs::DirEntry) -> u32 {
    0
}

/// Directory listings via the standard library.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdDirSource;

impl DirSource for StdDirSource {
    fn probe_stamp(&self, dir: &Path) -> Result<DirStamp, EnumError> {
        let meta = std::fs::metadata(dir).map_err(|e| EnumError::from_io(&e))?;
        if !meta.is_dir() {
            return Err(EnumError::NotADirectory(super::errors::code::DIRECTORY));
        }
        let secs = |t: std::io::Result<std::time::SystemTime>| -> i64 {
            t.ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
                .unwrap_or(0)
        };
        // std exposes no change time, so the stamp is write-only. Marking the
        // kind keeps it from comparing equal to a two-field Windows stamp.
        Ok(DirStamp::write_only(secs(meta.modified())))
    }

    fn list(
        &self,
        dir: &Path,
        sink: &mut dyn EntrySink,
        opts: &ListOpts,
        cancel: &CancelToken,
    ) -> Result<ListStats, EnumError> {
        let started = Instant::now();
        let rd = std::fs::read_dir(dir).map_err(|e| EnumError::from_io(&e))?;

        let mut entries = 0usize;
        let mut complete = true;

        for item in rd {
            if cancel.is_cancelled() {
                return Err(EnumError::Cancelled);
            }
            if entries >= opts.max_entries || opts.expired() {
                complete = false;
                break;
            }
            let Ok(entry) = item else { continue };

            // `file_type()` on a Windows DirEntry is free: it reads the
            // attributes already returned by the directory scan.
            let Ok(ft) = entry.file_type() else { continue };
            if opts.files_only && (ft.is_dir() || ft.is_symlink()) {
                continue;
            }

            let name = entry.file_name();
            let name = name.to_string_lossy();
            if is_dot_entry(&name) {
                continue;
            }

            let attributes = if ft.is_dir() { 0x0010 } else { 0x0080 } | marked(&entry);
            if opts.files_only && !is_listable_file_with(attributes, opts.hide_system) {
                continue;
            }

            if !sink.push_str(&name, EntryMeta { attributes }) {
                complete = false;
                break;
            }
            entries += 1;
        }

        Ok(ListStats {
            entries,
            // read_dir hides its buffering, so no refill count is available.
            round_trips: 0,
            elapsed: started.elapsed(),
            strategy: Some(EnumStrategy::StdReadDir),
            complete,
        })
    }

    fn prewarm(&self, root: &Path) {
        // Cheapest call that forces SMB session setup and tree connect.
        let _ = std::fs::metadata(root);
    }

    fn name(&self) -> &'static str {
        "std::fs::read_dir"
    }
}

// Tests only: SetFileAttributesW, to make a hidden file.
#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use crate::index::enumerate::VecSink;

    fn temp_dir_with(names: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for n in names {
            std::fs::write(dir.path().join(n), b"x").unwrap();
        }
        dir
    }

    /// Marks a real file hidden through the Win32 API.
    #[cfg(windows)]
    fn mark_hidden(path: &Path) {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW};
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: a NUL-terminated wide path to a file that exists; the call
        // reads the buffer and returns a success flag, which is checked.
        let ok = unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN) };
        assert_ne!(ok, 0, "could not mark {} hidden", path.display());
    }

    /// A file Windows marks hidden is listed by default and dropped only when
    /// `hide_system` is set.
    #[test]
    #[cfg(windows)]
    fn a_file_windows_marks_hidden_is_dropped_only_when_asked() {
        let dir = temp_dir_with(&["sheet.pdf", "Thumbs.db"]);
        mark_hidden(&dir.path().join("Thumbs.db"));

        let listed = |hide_system: bool| {
            let mut sink = VecSink::default();
            StdDirSource
                .list(
                    dir.path(),
                    &mut sink,
                    &ListOpts::default().hiding_system(hide_system),
                    &CancelToken::never(),
                )
                .unwrap();
            sink.names.sort();
            sink.names
        };

        assert_eq!(
            listed(false),
            vec!["Thumbs.db", "sheet.pdf"],
            "the default must enumerate the directory as it is"
        );
        assert_eq!(listed(true), vec!["sheet.pdf"]);
    }

    #[test]
    fn lists_files_in_a_directory() {
        let dir = temp_dir_with(&["a.txt", "b.txt"]);
        let mut sink = VecSink::default();
        let stats = StdDirSource
            .list(
                dir.path(),
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never(),
            )
            .unwrap();
        sink.names.sort();
        assert_eq!(sink.names, vec!["a.txt", "b.txt"]);
        assert_eq!(stats.entries, 2);
        assert!(stats.complete);
    }

    #[test]
    fn skips_subdirectories_and_never_recurses() {
        let dir = temp_dir_with(&["file.txt"]);
        let sub = dir.path().join("nested");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("hidden.txt"), b"x").unwrap();

        let mut sink = VecSink::default();
        StdDirSource
            .list(
                dir.path(),
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never(),
            )
            .unwrap();
        assert_eq!(
            sink.names,
            vec!["file.txt"],
            "must not descend or list the folder"
        );
    }

    #[test]
    fn an_empty_directory_is_an_empty_listing_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = VecSink::default();
        let stats = StdDirSource
            .list(
                dir.path(),
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never(),
            )
            .unwrap();
        assert_eq!(stats.entries, 0);
        assert!(sink.names.is_empty());
    }

    #[test]
    fn a_missing_directory_reports_path_not_found() {
        let mut sink = VecSink::default();
        let err = StdDirSource
            .list(
                Path::new("./definitely-not-a-real-directory-9f3a"),
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never(),
            )
            .unwrap_err();
        assert!(matches!(err, EnumError::PathNotFound(_)), "got {err:?}");
    }

    #[test]
    fn honours_the_entry_cap_and_reports_incompleteness() {
        let dir = temp_dir_with(&["a", "b", "c", "d", "e"]);
        let mut sink = VecSink::default();
        let stats = StdDirSource
            .list(
                dir.path(),
                &mut sink,
                &ListOpts::default().with_max_entries(2),
                &CancelToken::never(),
            )
            .unwrap();
        assert_eq!(stats.entries, 2);
        assert!(!stats.complete);
    }

    #[test]
    fn a_cancelled_enumeration_reports_cancelled() {
        let dir = temp_dir_with(&["a", "b"]);
        let epoch = crate::util::cancel::Epoch::new();
        let token = epoch.token(epoch.current());
        epoch.bump();

        let mut sink = VecSink::default();
        let err = StdDirSource
            .list(dir.path(), &mut sink, &ListOpts::default(), &token)
            .unwrap_err();
        assert_eq!(err, EnumError::Cancelled);
    }

    #[test]
    fn probe_stamp_reports_a_directory_timestamp() {
        let dir = temp_dir_with(&["a"]);
        let stamp = StdDirSource.probe_stamp(dir.path()).unwrap();
        assert!(stamp.last_write > 0);
    }

    #[test]
    fn probe_stamp_rejects_a_file() {
        let dir = temp_dir_with(&["a"]);
        let err = StdDirSource.probe_stamp(&dir.path().join("a")).unwrap_err();
        assert!(matches!(err, EnumError::NotADirectory(_)));
    }

    #[test]
    fn server_side_filtering_is_reported_as_unsupported() {
        // std cannot push a filter to the server, so the caller must match
        // locally.
        let dir = temp_dir_with(&["a"]);
        let mut sink = VecSink::default();
        let err = StdDirSource
            .query(
                dir.path(),
                "*a*",
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never(),
            )
            .unwrap_err();
        assert!(matches!(err, EnumError::Unsupported(_)));
    }
}
