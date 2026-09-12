//! `ReadDirectoryChangesW`, the one mechanism that scales to a whole share.
//!
//! One outstanding request with `bWatchSubtree` covers every directory
//! beneath the root. Over SMB it becomes a single `SMB2 CHANGE_NOTIFY`, which
//! Windows Server and Samba support and cheaper NAS firmware often does not.
//! One watch per directory would be three hundred thousand handles and three
//! hundred thousand outstanding requests, so there is no second option.
//!
//! Everything that *decides* anything lives in [`crate::index::watch`],
//! behind a trait and a scripted fake. This file is the call, and it cannot be
//! exercised on the machine it was written on - so it is kept as small as it
//! can be and every trap it has to avoid is named.
//!
//! # The five traps
//!
//! 1. **The buffer cannot exceed 64 KB on a network path.** A larger one
//!    fails outright, and `R:\` is a network path. So 64 KB it is, which is
//!    also why overflow is a routine event rather than a pathological one.
//! 2. **Overflow arrives as success, not failure.** When the buffer overruns,
//!    the call completes with `bytes_returned == 0` and no error at all. Read
//!    naively that is "nothing changed", which is the worst possible reading:
//!    a bulk change would be silently dropped. It is also reported as
//!    `ERROR_NOTIFY_ENUM_DIR`, so both shapes are handled.
//! 3. **`FileNameLength` is a byte count over UTF-16, and it names the
//!    *file*.** Not a character count, and not the directory. The unit this
//!    index stores is a directory listing, so the dirty entry is
//!    [`crate::index::tree::parent_rel`] of what the record says - and that is
//!    the one function both sides spell it with.
//! 4. **The mask is `FILE_NAME | DIR_NAME` only.** `LAST_WRITE` would fire on
//!    every save of every file, multiplying the event rate by the share's
//!    entire working set, to report changes an index of *names* cannot see.
//! 5. **Cancellation needs `CancelIoEx` from another thread.** A thread parked
//!    in a synchronous `ReadDirectoryChangesW` never returns to poll a flag,
//!    so shutdown has to reach into the handle. `CancelIoEx` cancels I/O on a
//!    handle whichever thread issued it, and the parked call then returns
//!    `ERROR_OPERATION_ABORTED`.
//!
//! The records themselves come from a remote server and are treated as
//! untrusted input, on exactly the terms [`crate::index::win_enum`] sets out:
//! every hop is bounds-checked, `FileNameLength` is checked for evenness and
//! for fitting, `NextEntryOffset` must not stall or point backwards, and a
//! hard iteration cap backstops a crafted cycle. A violation ends the batch
//! rather than panicking.

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

/// The largest buffer a network path accepts. See trap 1.
const BUFFER_BYTES: usize = 64 * 1024;

/// Backstop against a crafted or corrupt record chain.
const MAX_RECORDS: usize = 64 * 1024;

/// A handle that may be used from the watcher thread and cancelled from
/// another.
///
/// `HANDLE` is a raw pointer, so it is neither `Send` nor `Sync` by default.
/// Both are asserted here, and narrowly: the only operations performed across
/// threads are `ReadDirectoryChangesW` on one and `CancelIoEx` on the other,
/// which is precisely the pair the API documents as the way to interrupt a
/// blocked read. The handle is closed exactly once, when the watcher is
/// dropped, and neither thread observes it after that because the pump joins
/// before the watcher does.
struct SharedHandle(OwnedHandle);

// SAFETY: see the type's own documentation.
unsafe impl Send for SharedHandle {}
// SAFETY: see the type's own documentation.
unsafe impl Sync for SharedHandle {}

/// Watches a whole share for name changes.
pub struct DirectoryWatcher {
    handle: SharedHandle,
    /// Set by [`ChangeWatcher::stop`] before the cancel, so a read that
    /// returns for an unrelated reason does not start another one.
    stopped: AtomicBool,
    /// Set once the watch has failed for good, so the pump is told to finish
    /// rather than spinning on a handle that will never answer.
    finished: AtomicBool,
    root: Box<Path>,
}

impl DirectoryWatcher {
    /// Opens `root` for watching.
    ///
    /// `Err` carries the reason, which the caller reports as
    /// [`WatchEvent::Unavailable`] - a share that will not answer is a fact
    /// the status line has to state, not one to swallow.
    pub fn open(root: &Path) -> Result<Self, String> {
        // FILE_LIST_DIRECTORY rather than GENERIC_READ, and
        // FILE_FLAG_BACKUP_SEMANTICS because the target is a directory: the
        // same two requirements `win_enum` documents, for the same reasons.
        //
        // FILE_SHARE_DELETE is included deliberately. Holding a share root
        // open without it can block other people's operations on it, and a
        // search tool that interferes with the share it indexes would be
        // withdrawn within the day.
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
            // Described rather than numbered. This string reaches the status
            // line, and "error 53" tells the person reading it nothing they
            // can act on.
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
                1, // bWatchSubtree: the whole point.
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
                // Too many changes to describe. Not a failure: the watch is
                // alive and the caller has to re-walk.
                ERROR_NOTIFY_ENUM_DIR => Some(WatchEvent::Overflow),
                // The server does not implement CHANGE_NOTIFY at all. Common
                // on cheaper NAS firmware, and the reason the floor is not
                // optional.
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

        // Trap 2: success with nothing in the buffer *is* the overflow signal.
        // Reading it as "nothing changed" would drop the whole window.
        if returned == 0 {
            return Some(WatchEvent::Overflow);
        }

        let dirs = parse(&buf[..returned as usize]);
        // A batch that decoded to nothing is not an overflow and not an
        // error - a rename within one folder can produce records the parser
        // deliberately collapses. Reporting an empty change set would wake the
        // actor to do nothing.
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
            // An empty batch is swallowed here rather than handed up: the
            // queue would take it as a pending change and schedule an update
            // over no directories at all.
            if matches!(&event, WatchEvent::Changed(dirs) if dirs.is_empty()) {
                continue;
            }
            return Some(event);
        }
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        // Trap 5. The flag alone reaches nothing: the thread is inside the
        // call, not between two of them.
        //
        // SAFETY: the handle is open for the lifetime of `self`, and the
        // caller outlives the watcher thread by construction - `IndexActor`
        // joins the pump before dropping this.
        unsafe {
            CancelIoEx(self.handle.0.0, std::ptr::null());
        }
    }
}

/// Turns a `FILE_NOTIFY_INFORMATION` chain into the directories it implicates.
///
/// Deduplicated in order: a rename produces a pair of records naming the same
/// folder, and a folder being written to produces one per file. Handing the
/// queue forty copies of one name would only make it do the work of removing
/// them.
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
        // guarantee beyond the allocator's, and the fields are read
        // individually with `read_unaligned`.
        let record = unsafe { bytes.as_ptr().add(at).cast::<FILE_NOTIFY_INFORMATION>() };
        // SAFETY: as above.
        let (next, name_len) = unsafe {
            (
                std::ptr::read_unaligned(&raw const (*record).NextEntryOffset),
                std::ptr::read_unaligned(&raw const (*record).FileNameLength),
            )
        };

        // Trap 3: a *byte* count over UTF-16, so an odd value is corrupt.
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
        // The record names the file. The unit this index stores is a
        // directory listing, so the dirty entry is its parent - spelled by the
        // same function the walk spells it with, because the two have to agree
        // on exactly one string format.
        let rel = String::from_utf16_lossy(&units).replace('/', "\\");
        let dir = parent_rel(&rel).to_string();
        if !out.contains(&dir) {
            out.push(dir);
        }

        if next == 0 {
            break;
        }
        let next = next as usize;
        // Must move forward, and by at least a header, or the chain either
        // stalls or overlaps itself.
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

    /// Builds one record the way the kernel lays it out.
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

    /// A chain, with each record's `NextEntryOffset` filled in.
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
        assert_eq!(
            parse(&chain(&["11d\\0704\\drawing.dwg"])),
            vec!["11d\\0704"]
        );
    }

    #[test]
    fn a_file_in_the_root_is_reported_as_the_root() {
        assert_eq!(parse(&chain(&["readme.txt"])), vec![""]);
    }

    #[test]
    fn a_chain_yields_every_directory_in_it() {
        assert_eq!(
            parse(&chain(&["11d\\a.pdf", "ab12\\b.pdf", "11d\\c.pdf"])),
            vec!["11d", "ab12"],
            "the duplicate should have been collapsed"
        );
    }

    /// Forty records for one folder is what a CAD package saving a job looks
    /// like, and the queue should not have to do the collapsing.
    #[test]
    fn a_burst_in_one_folder_collapses_to_one_entry() {
        let names: Vec<String> = (0..40).map(|i| format!("11d\\f{i}.pdf")).collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        assert_eq!(parse(&chain(&refs)), vec!["11d"]);
    }

    #[test]
    fn an_empty_buffer_yields_nothing() {
        assert!(parse(&[]).is_empty());
    }

    /// Everything below here is a remote server's bytes being treated as
    /// untrusted input. None of it may panic or read out of bounds.
    #[test]
    fn a_truncated_header_ends_the_batch() {
        let bytes = chain(&["11d\\a.pdf"]);
        assert!(parse(&bytes[..4]).is_empty());
    }

    #[test]
    fn a_name_running_past_the_buffer_ends_the_batch() {
        let mut bytes = chain(&["11d\\a.pdf"]);
        bytes[8..12].copy_from_slice(&0xFFFF_u32.to_le_bytes());
        assert!(parse(&bytes).is_empty());
    }

    /// `FileNameLength` is a byte count over UTF-16, so an odd one is corrupt.
    #[test]
    fn an_odd_name_length_ends_the_batch() {
        let mut bytes = chain(&["11d\\a.pdf"]);
        bytes[8..12].copy_from_slice(&7u32.to_le_bytes());
        assert!(parse(&bytes).is_empty());
    }

    #[test]
    fn a_next_offset_that_does_not_advance_ends_the_batch() {
        let mut bytes = chain(&["11d\\a.pdf", "ab12\\b.pdf"]);
        bytes[0..4].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(parse(&bytes), vec!["11d"]);
    }

    #[test]
    fn a_next_offset_past_the_buffer_ends_the_batch() {
        let mut bytes = chain(&["11d\\a.pdf", "ab12\\b.pdf"]);
        bytes[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse(&bytes), vec!["11d"]);
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
