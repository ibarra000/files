//! `ReadDirectoryChangesW` watcher for a whole share.
//!
//! One outstanding request with `bWatchSubtree` covers every directory under
//! the root. Over SMB it becomes a single `SMB2 CHANGE_NOTIFY`, which Windows
//! Server and Samba support and some NAS firmware does not. Policy lives in
//! [`crate::index::watch`]; this file is only the OS call.
//!
//! # Constraints
//!
//! 1. **The buffer cannot exceed 64 KB on a network path**; a larger one
//!    fails. Overflow is therefore routine.
//! 2. **Overflow arrives as success** with `bytes_returned == 0`, or as
//!    `ERROR_NOTIFY_ENUM_DIR`. Both must be treated as overflow, not as
//!    "nothing changed".
//! 3. **`FileNameLength` is a byte count over UTF-16 and names the file**, not
//!    the directory. The dirty entry is
//!    [`crate::index::tree::parent_rel`] of the record, the same function the
//!    walk uses.
//! 4. **The mask is `FILE_NAME | DIR_NAME` only.** `LAST_WRITE` would fire on
//!    every save for changes a name index cannot see.
//! 5. **Cancellation requires `CancelIoEx` from another thread**, because a
//!    thread blocked in a synchronous read cannot poll a flag. The blocked
//!    call then returns `ERROR_OPERATION_ABORTED`.
//!
//! Records come from a remote server and are untrusted, as in
//! [`crate::index::win_enum`]: every hop is bounds-checked, `FileNameLength`
//! must be even and fit, `NextEntryOffset` must advance, and an iteration cap
//! stops a crafted cycle. A violation ends the batch without panicking.

use std::mem::{offset_of, size_of};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{
    ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_OPERATION_ABORTED, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows_sys::Win32::System::IO::CancelIoEx;

use super::errors::EnumError;
use super::tree::parent_rel;
use super::watch::{ChangeWatcher, WatchEvent};
use super::win_util::{OwnedHandle, last_error, wide_path};
use crate::util::cancel::CancelToken;

/// `ERROR_NOTIFY_ENUM_DIR`: too many changes to report, re-enumerate.
const ERROR_NOTIFY_ENUM_DIR: u32 = 1022;

/// The largest buffer a network path accepts. See constraint 1.
const BUFFER_BYTES: usize = 64 * 1024;

/// Iteration cap against a crafted or corrupt record chain.
const MAX_RECORDS: usize = 64 * 1024;

/// A handle used from the watcher thread and cancelled from another.
///
/// `HANDLE` is a raw pointer and so neither `Send` nor `Sync`. Both are
/// asserted because the only cross-thread operations are
/// `ReadDirectoryChangesW` on one thread and `CancelIoEx` on the other, the
/// documented way to interrupt a blocked read. The handle is closed once, on
/// drop, after the pump thread has been joined.
struct SharedHandle(OwnedHandle);

// SAFETY: see the type's own documentation.
unsafe impl Send for SharedHandle {}
// SAFETY: see the type's own documentation.
unsafe impl Sync for SharedHandle {}

/// Watches a whole share for name changes.
pub struct DirectoryWatcher {
    handle: SharedHandle,
    /// Set by [`ChangeWatcher::stop`] before cancelling, so a read that
    /// returns for another reason does not start a new one.
    stopped: AtomicBool,
    /// Set once the watch has failed permanently, so the pump finishes.
    finished: AtomicBool,
    root: Box<Path>,
}

impl DirectoryWatcher {
    /// Opens `root` for watching.
    ///
    /// `Err` carries a readable reason, which the caller reports as
    /// [`WatchEvent::Unavailable`].
    pub fn open(root: &Path) -> Result<Self, String> {
        // FILE_LIST_DIRECTORY and FILE_FLAG_BACKUP_SEMANTICS are required to
        // open a directory, as in `win_enum`. FILE_SHARE_DELETE keeps the open
        // root from blocking other users' operations on the share.
        let wide = wide_path(root, true);
        // SAFETY: `wide` is NUL-terminated and outlives the call; every other
        // argument is a constant or null.
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE || raw.is_null() {
            // Described rather than numbered, since this reaches the status line.
            let err = EnumError::from_win_open(last_error());
            return Err(err.describe(&root.display().to_string()));
        }
        Ok(Self {
            handle: SharedHandle(OwnedHandle(raw)),
            stopped: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            root: root.into(),
        })
    }

    /// Issues one blocking read and turns the result into an event.
    fn read(&self, buf: &mut [u8]) -> Option<WatchEvent> {
        let mut returned: u32 = 0;
        // SAFETY: `buf` is a live, writable allocation of `buf.len()` bytes
        // and is not aliased for the duration of the call; the handle is open
        // for FILE_LIST_DIRECTORY; the overlapped and completion arguments are
        // null, which is the documented synchronous form.
        let ok = unsafe {
            ReadDirectoryChangesW(
                self.handle.0.0,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                1, // bWatchSubtree
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME,
                &mut returned,
                std::ptr::null_mut(),
                None,
            )
        };

        if ok == 0 {
            let err = last_error();
            return match err {
                ERROR_OPERATION_ABORTED => None,
                // Too many changes to describe. The watch is still alive and
                // the caller must re-walk.
                ERROR_NOTIFY_ENUM_DIR => Some(WatchEvent::Overflow),
                // The server does not implement CHANGE_NOTIFY, as on some NAS
                // firmware. The rescan floor covers this case.
                ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED => {
                    self.finished.store(true, Ordering::Relaxed);
                    Some(WatchEvent::Unavailable(format!(
                        "{} does not support change notification",
                        self.root.display()
                    )))
                }
                _ => {
                    self.finished.store(true, Ordering::Relaxed);
                    Some(WatchEvent::Unavailable(format!(
                        "watching {} failed: {err}",
                        self.root.display()
                    )))
                }
            };
        }

        // Constraint 2: success with an empty buffer is the overflow signal.
        if returned == 0 {
            return Some(WatchEvent::Overflow);
        }

        let dirs = parse(&buf[..returned as usize]);
        // A batch can decode to nothing, for example when the parser collapses
        // records; `next_event` skips it.
        if dirs.is_empty() {
            return Some(WatchEvent::Changed(Vec::new()));
        }
        Some(WatchEvent::Changed(dirs))
    }
}

impl ChangeWatcher for DirectoryWatcher {
    fn next_event(&self, cancel: &CancelToken) -> Option<WatchEvent> {
        let mut buf = vec![0u8; BUFFER_BYTES];
        loop {
            if self.stopped.load(Ordering::Relaxed)
                || self.finished.load(Ordering::Relaxed)
                || cancel.is_cancelled()
            {
                return None;
            }
            let event = self.read(&mut buf)?;
            // Swallow empty batches; the queue would otherwise schedule an
            // update over no directories.
            if matches!(&event, WatchEvent::Changed(dirs) if dirs.is_empty()) {
                continue;
            }
            return Some(event);
        }
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        // Constraint 5: the thread is blocked inside the call, so the flag
        // alone is not enough.
        //
        // SAFETY: the handle is open for the lifetime of `self`, and the
        // caller outlives the watcher thread by construction - `IndexActor`
        // joins the pump before dropping this.
        unsafe {
            CancelIoEx(self.handle.0.0, std::ptr::null());
        }
    }
}

/// Converts a `FILE_NOTIFY_INFORMATION` chain into the directories it names.
///
/// Deduplicated in order, since a rename yields two records for one folder and
/// a busy folder yields one per file.
fn parse(bytes: &[u8]) -> Vec<String> {
    const HEADER: usize = size_of::<FILE_NOTIFY_INFORMATION>();
    const NAME_AT: usize = offset_of!(FILE_NOTIFY_INFORMATION, FileName);

    let mut out: Vec<String> = Vec::new();
    let mut at = 0usize;
    for _ in 0..MAX_RECORDS {
        if at + HEADER > bytes.len() {
            break;
        }
        // SAFETY: the header fits, checked immediately above. Read through a
        // pointer rather than a reference because the buffer has no alignment
        // guarantee beyond the allocator's; fields are read individually with
        // `read_unaligned`.
        let record = unsafe { bytes.as_ptr().add(at).cast::<FILE_NOTIFY_INFORMATION>() };
        // SAFETY: as above.
        let (next, name_len) = unsafe {
            (
                std::ptr::read_unaligned(&raw const (*record).NextEntryOffset),
                std::ptr::read_unaligned(&raw const (*record).FileNameLength),
            )
        };

        // Constraint 3: a byte count over UTF-16, so an odd value is corrupt.
        let name_len = name_len as usize;
        if !name_len.is_multiple_of(2) {
            break;
        }
        let name_at = at + NAME_AT;
        let Some(end) = name_at.checked_add(name_len) else {
            break;
        };
        if end > bytes.len() {
            break;
        }

        let units: Vec<u16> = bytes[name_at..end]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        // The record names a file; the dirty entry is its parent directory,
        // spelled by `parent_rel` so it matches the walk's format exactly.
        let rel = String::from_utf16_lossy(&units).replace('/', "\\");
        let dir = parent_rel(&rel).to_string();
        if !out.contains(&dir) {
            out.push(dir);
        }

        if next == 0 {
            break;
        }
        let next = next as usize;
        // Must advance by at least a header, or the chain stalls or overlaps.
        if next < HEADER {
            break;
        }
        let Some(advanced) = at.checked_add(next) else {
            break;
        };
        at = advanced;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one record in the kernel's layout.
    fn record(name: &str, next: u32) -> Vec<u8> {
        const NAME_AT: usize = offset_of!(FILE_NOTIFY_INFORMATION, FileName);
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut out = vec![0u8; NAME_AT];
        out[0..4].copy_from_slice(&next.to_le_bytes());
        out[4..8].copy_from_slice(&1u32.to_le_bytes()); // FILE_ACTION_ADDED
        out[8..12].copy_from_slice(&((units.len() * 2) as u32).to_le_bytes());
        for u in units {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    }

    /// Builds a chain with each record's `NextEntryOffset` filled in.
    fn chain(names: &[&str]) -> Vec<u8> {
        let sizes: Vec<usize> = names.iter().map(|n| record(n, 0).len()).collect();
        let mut out = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let next = if i + 1 == names.len() {
                0
            } else {
                sizes[i] as u32
            };
            out.extend_from_slice(&record(name, next));
        }
        out
    }

    #[test]
    fn a_record_names_the_file_so_the_dirty_entry_is_its_parent() {
        assert_eq!(parse(&chain(&["abc\\0704\\photo.tif"])), vec!["abc\\0704"]);
    }

    #[test]
    fn a_file_in_the_root_is_reported_as_the_root() {
        assert_eq!(parse(&chain(&["readme.txt"])), vec![""]);
    }

    #[test]
    fn a_chain_yields_every_directory_in_it() {
        assert_eq!(
            parse(&chain(&["abc\\a.pdf", "ab12\\b.pdf", "abc\\c.pdf"])),
            vec!["abc", "ab12"],
            "the duplicate should have been collapsed"
        );
    }

    /// Many records for one folder collapse to a single entry.
    #[test]
    fn a_burst_in_one_folder_collapses_to_one_entry() {
        let names: Vec<String> = (0..40).map(|i| format!("abc\\f{i}.pdf")).collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        assert_eq!(parse(&chain(&refs)), vec!["abc"]);
    }

    #[test]
    fn an_empty_buffer_yields_nothing() {
        assert!(parse(&[]).is_empty());
    }

    /// The following tests feed malformed server bytes; none may panic or read
    /// out of bounds.
    #[test]
    fn a_truncated_header_ends_the_batch() {
        let bytes = chain(&["abc\\a.pdf"]);
        assert!(parse(&bytes[..4]).is_empty());
    }

    #[test]
    fn a_name_running_past_the_buffer_ends_the_batch() {
        let mut bytes = chain(&["abc\\a.pdf"]);
        bytes[8..12].copy_from_slice(&0xFFFF_u32.to_le_bytes());
        assert!(parse(&bytes).is_empty());
    }

    /// `FileNameLength` is a byte count over UTF-16, so an odd one is corrupt.
    #[test]
    fn an_odd_name_length_ends_the_batch() {
        let mut bytes = chain(&["abc\\a.pdf"]);
        bytes[8..12].copy_from_slice(&7u32.to_le_bytes());
        assert!(parse(&bytes).is_empty());
    }

    #[test]
    fn a_next_offset_that_does_not_advance_ends_the_batch() {
        let mut bytes = chain(&["abc\\a.pdf", "ab12\\b.pdf"]);
        bytes[0..4].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(parse(&bytes), vec!["abc"]);
    }

    #[test]
    fn a_next_offset_past_the_buffer_ends_the_batch() {
        let mut bytes = chain(&["abc\\a.pdf", "ab12\\b.pdf"]);
        bytes[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse(&bytes), vec!["abc"]);
    }

    /// A buffer of nothing but self-referential records must terminate.
    #[test]
    fn a_cycle_terminates() {
        let one = record("a.pdf", 0).len();
        let mut bytes = Vec::new();
        for _ in 0..8 {
            bytes.extend_from_slice(&record("a.pdf", one as u32));
        }
        assert_eq!(parse(&bytes), vec![""]);
    }
}
