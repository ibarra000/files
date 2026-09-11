//! Windows directory enumeration.
//!
//! `std::fs::read_dir` goes through `FindFirstFileW` with a roughly 4 KB
//! fetch buffer. Over SMB2 that is one `QUERY_DIRECTORY` round trip per
//! ~25 entries, so a million-entry share costs on the order of **45,000
//! serialised round trips** - about 22 seconds at 0.5 ms RTT and minutes over
//! a VPN. The wall clock is governed by round-trip count, not bandwidth, and
//! that is what this module attacks.
//!
//! | Strategy | Buffer | Round trips @ 1M entries |
//! |---|---|---|
//! | `read_dir` | ~4 KB | ~45,000 |
//! | `FindFirstFileExW` + `LARGE_FETCH` | 64 KiB | ~2,800 |
//! | `GetFileInformationByHandleEx` | 1 MiB | ~150 |
//! | server-side wildcard | n/a | **1** |
//!
//! # Why the handle API is the default
//!
//! `GetFileInformationByHandleEx` was chosen over `NtQueryDirectoryFileEx`
//! deliberately. It takes an arbitrary caller-supplied buffer, so it captures
//! essentially the whole round-trip win, while being fully documented, free of
//! any `ntdll` linkage, and free of the `STATUS_PENDING` hazard that comes
//! with getting the handle's synchronicity wrong. Since none of this can be
//! exercised before it reaches the real drives, that risk difference outweighs
//! the last ~35% of throughput.
//!
//! `FindFirstFileExW` remains, both as the fallback and because it is the only
//! API here that accepts a search pattern - which is what makes server-side
//! filtering possible.
//!
//! # Safety model
//!
//! Both enumerators walk variable-length record chains supplied by a remote
//! server, so those bytes are treated as untrusted input. Every hop checks:
//!
//! * the fixed header fits within the valid region;
//! * `FileNameLength` is even (it is a **byte** count, not a character count);
//! * the name fits within the valid region;
//! * `NextEntryOffset` is either zero (terminator) or at least the header
//!   size, and advancing by it stays in bounds;
//! * a hard iteration cap, as a final backstop against a crafted cycle.
//!
//! A violation returns `EnumError::Corrupt` and falls through to the next
//! strategy. It never panics and never indexes out of bounds.

use std::mem::{offset_of, size_of};
use std::path::Path;
use std::time::Instant;

use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FULL_DIR_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FIND_FIRST_EX_LARGE_FETCH, FileBasicInfo,
    FileFullDirectoryInfo, FileFullDirectoryRestartInfo, FindExInfoBasic, FindExInfoStandard,
    FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, GetFileAttributesExW,
    GetFileAttributesW, GetFileExInfoStandard, GetFileInformationByHandleEx, OPEN_EXISTING,
    WIN32_FILE_ATTRIBUTE_DATA, WIN32_FIND_DATAW,
};

use super::DirStamp;
use super::enumerate::{
    DirSource, EntryMeta, EntrySink, ListOpts, ListStats, is_dot_entry_wide, is_listable_file,
};
use super::errors::{EnumError, code};
use super::win_util::{AlignedBuf, FindHandle, OwnedHandle, last_error, wide_path, wide_pattern};
use crate::config::{DIR_BUFFER_MIN, EnumStrategy};
use crate::util::cancel::CancelToken;

/// Backstop against a malformed record chain that never terminates.
const MAX_RECORDS_PER_BUFFER: usize = 1 << 22;

// --- directory handle ------------------------------------------------------

fn open_dir_handle(dir: &Path) -> Result<OwnedHandle, EnumError> {
    // A volume root needs its trailing separator; a subdirectory must not
    // have one.
    let is_root = {
        let s = dir.as_os_str().to_string_lossy();
        s.len() <= 3 && s.contains(':')
    };
    let wide = wide_path(dir, is_root);

    // SAFETY: `wide` is NUL-terminated and outlives the call.
    //
    // FILE_FLAG_BACKUP_SEMANTICS is mandatory for a directory handle; without
    // it CreateFileW fails with ERROR_ACCESS_DENIED, which reads as a
    // permissions problem and is not one.
    //
    // FILE_LIST_DIRECTORY is requested rather than GENERIC_READ: some SMB
    // shares grant the former where they deny the latter.
    //
    // FILE_READ_ATTRIBUTES is requested *as well*, and it is load-bearing.
    // `GetFileInformationByHandleEx(FileBasicInfo)` is an attribute query, and
    // a server that enforces the granted access mask strictly - which local
    // NTFS does not, but several SMB implementations do - fails it with
    // ERROR_ACCESS_DENIED when only FILE_LIST_DIRECTORY was asked for. That
    // made the freshness probe fail on the real share and never on a
    // development machine, and a probe that always fails used to mean a full
    // re-enumeration every sixty seconds.
    //
    // FILE_FLAG_OVERLAPPED is deliberately absent, which keeps the handle
    // synchronous - GetFileInformationByHandleEx has no asynchronous form.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };

    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        // Note `from_win_open`, not `from_win`: ERROR_FILE_NOT_FOUND here
        // means the directory does not exist, whereas the same code from an
        // enumeration means "no more entries".
        return Err(EnumError::from_win_open(last_error()));
    }
    Ok(OwnedHandle(handle))
}

// --- strategy: GetFileInformationByHandleEx --------------------------------

fn list_handle_dirinfo(
    dir: &Path,
    sink: &mut dyn EntrySink,
    opts: &ListOpts,
    cancel: &CancelToken,
) -> Result<ListStats, EnumError> {
    const HEADER: usize = size_of::<FILE_FULL_DIR_INFO>() - size_of::<u16>();
    const NAME_OFFSET: usize = offset_of!(FILE_FULL_DIR_INFO, FileName);

    let started = Instant::now();
    let handle = open_dir_handle(dir)?;
    let mut buf = AlignedBuf::new(opts.buffer_bytes);

    let mut class = FileFullDirectoryRestartInfo;
    let mut entries = 0usize;
    let mut round_trips = 0u32;
    let mut complete = true;
    // Two consecutive under-filled refills mean the server negotiated a
    // smaller transfer size than we asked for, so stop paying for the memory.
    let mut sparse_refills = 0u32;

    loop {
        if cancel.is_cancelled() {
            return Err(EnumError::Cancelled);
        }
        if opts.expired() {
            complete = false;
            break;
        }

        // SAFETY: `handle` is a live directory handle opened synchronously;
        // the buffer is 8-byte aligned, initialised, and its true length is
        // passed. The kernel writes at most that many bytes.
        let ok = unsafe {
            GetFileInformationByHandleEx(handle.0, class, buf.as_mut_ptr().cast(), buf.len() as u32)
        };

        if ok == 0 {
            return match last_error() {
                // Both are normal termination: the chain ended, or the
                // directory was empty to begin with.
                code::NO_MORE_FILES | code::FILE_NOT_FOUND => Ok(ListStats {
                    entries,
                    round_trips,
                    elapsed: started.elapsed(),
                    strategy: Some(EnumStrategy::HandleDirInfo),
                    complete,
                }),
                code::MORE_DATA => {
                    // A single name did not fit. Grow and retry from the
                    // start, since the cursor state is now unknown.
                    if !buf.grow() {
                        return Err(EnumError::Corrupt(code::INVALID_DATA));
                    }
                    class = FileFullDirectoryRestartInfo;
                    entries = 0;
                    continue;
                }
                e => Err(EnumError::from_win(e)),
            };
        }

        round_trips += 1;
        // Every call after the first continues from the kernel's cursor.
        class = FileFullDirectoryInfo;

        let bytes = buf.as_slice();
        let mut offset = 0usize;
        let mut records = 0usize;
        let mut consumed = 0usize;

        loop {
            records += 1;
            if records > MAX_RECORDS_PER_BUFFER {
                return Err(EnumError::Corrupt(code::INVALID_DATA));
            }
            // W1: the fixed header must fit.
            if offset + HEADER > bytes.len() {
                return Err(EnumError::Corrupt(code::INVALID_DATA));
            }

            // SAFETY: `offset + HEADER <= bytes.len()` was just checked, and
            // `read_unaligned` imposes no alignment requirement, so no
            // misaligned reference is ever formed into the buffer.
            let header: FILE_FULL_DIR_INFO =
                unsafe { std::ptr::read_unaligned(bytes.as_ptr().add(offset).cast()) };

            let name_len_bytes = header.FileNameLength as usize;
            // W2: FileNameLength counts BYTES, so it must be even.
            if !name_len_bytes.is_multiple_of(2) {
                return Err(EnumError::Corrupt(code::INVALID_DATA));
            }
            // W3: the name must fit.
            let name_start = offset + NAME_OFFSET;
            let name_end = name_start.saturating_add(name_len_bytes);
            if name_end > bytes.len() {
                return Err(EnumError::Corrupt(code::INVALID_DATA));
            }

            // SAFETY: the range was bounds-checked above; `name_start` is
            // 8-byte-aligned base plus a fixed 68-byte offset, so it is
            // 4-byte aligned and therefore validly aligned for u16.
            let name: &[u16] = unsafe {
                std::slice::from_raw_parts(
                    bytes.as_ptr().add(name_start).cast::<u16>(),
                    name_len_bytes / 2,
                )
            };

            let attributes = header.FileAttributes;
            let keep =
                !is_dot_entry_wide(name) && (!opts.files_only || is_listable_file(attributes));

            if keep {
                if entries >= opts.max_entries {
                    complete = false;
                    break;
                }
                if !sink.push_wide(name, EntryMeta { attributes }) {
                    complete = false;
                    break;
                }
                entries += 1;
            }

            let next = header.NextEntryOffset as usize;
            if next == 0 {
                // W4: zero terminates the chain for this buffer.
                consumed = name_end;
                break;
            }
            // W4/W5: a forward step of at least a header, staying in bounds.
            if next < HEADER {
                return Err(EnumError::Corrupt(code::INVALID_DATA));
            }
            let Some(advanced) = offset.checked_add(next) else {
                return Err(EnumError::Corrupt(code::INVALID_DATA));
            };
            if advanced >= bytes.len() {
                return Err(EnumError::Corrupt(code::INVALID_DATA));
            }
            offset = advanced;
        }

        if !complete {
            break;
        }

        // Adaptive sizing: a server capped below our request wastes a
        // megabyte of resident memory per scan for no benefit.
        if consumed * 4 < buf.len() && buf.len() > DIR_BUFFER_MIN {
            sparse_refills += 1;
            if sparse_refills >= 2 {
                let smaller = (buf.len() / 2).max(DIR_BUFFER_MIN);
                buf = AlignedBuf::new(smaller);
                sparse_refills = 0;
                // The kernel cursor survives a buffer change, so enumeration
                // simply continues with the smaller buffer.
            }
        } else {
            sparse_refills = 0;
        }
    }

    Ok(ListStats {
        entries,
        round_trips,
        elapsed: started.elapsed(),
        strategy: Some(EnumStrategy::HandleDirInfo),
        complete,
    })
}

// --- strategy: FindFirstFileExW --------------------------------------------

fn find_first(
    dir: &Path,
    pattern: &str,
    info_basic: bool,
    data: &mut WIN32_FIND_DATAW,
) -> Result<FindHandle, EnumError> {
    let wide = wide_pattern(dir, pattern);
    // SAFETY: `wide` is NUL-terminated and outlives the call; `data` is a
    // valid, correctly aligned, caller-owned WIN32_FIND_DATAW.
    //
    // FindExInfoBasic suppresses 8.3 short-name retrieval, which removes
    // bytes from the wire and work from the server. It needs Windows 7+;
    // older systems answer ERROR_INVALID_PARAMETER, handled by the caller.
    //
    // FIND_FIRST_EX_LARGE_FETCH raises kernel32's internal buffer to 64 KiB
    // and is where most of the gap to `read_dir` comes from.
    //
    // FIND_FIRST_EX_CASE_SENSITIVE is deliberately not set: it would break
    // matching against mixed-case job codes. FIND_FIRST_EX_ON_DISK_ENTRIES_ONLY
    // is also avoided, as it can silently drop entries behind some filters.
    let handle = unsafe {
        FindFirstFileExW(
            wide.as_ptr(),
            if info_basic {
                FindExInfoBasic
            } else {
                FindExInfoStandard
            },
            (data as *mut WIN32_FIND_DATAW).cast(),
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };

    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(EnumError::from_win(last_error()));
    }
    Ok(FindHandle(handle))
}

/// Reads the filename out of a find record.
///
/// `cFileName` is not guaranteed to be NUL-terminated when it occupies all
/// 260 units, so the scan is explicitly bounded.
fn find_name(data: &WIN32_FIND_DATAW) -> &[u16] {
    let n = data
        .cFileName
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(data.cFileName.len());
    &data.cFileName[..n]
}

fn list_find_first(
    dir: &Path,
    pattern: &str,
    strategy: EnumStrategy,
    sink: &mut dyn EntrySink,
    opts: &ListOpts,
    cancel: &CancelToken,
) -> Result<ListStats, EnumError> {
    let started = Instant::now();
    // SAFETY: WIN32_FIND_DATAW is a plain-old-data struct with no invalid bit
    // patterns, and it is fully written by FindFirstFileExW before any read.
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };

    let handle = match find_first(dir, pattern, true, &mut data) {
        Ok(h) => h,
        // Pre-Windows-7 rejects FindExInfoBasic. Retry once at the standard
        // level; this is an intra-strategy fallback, not a chain step.
        Err(EnumError::Unsupported(code::INVALID_PARAMETER)) => {
            find_first(dir, pattern, false, &mut data)?
        }
        Err(e) => return Err(e),
    };

    let mut entries = 0usize;
    let mut complete = true;

    loop {
        if cancel.is_cancelled() {
            return Err(EnumError::Cancelled);
        }

        let name = find_name(&data);
        let attributes = data.dwFileAttributes;
        if !is_dot_entry_wide(name) && (!opts.files_only || is_listable_file(attributes)) {
            if entries >= opts.max_entries || opts.expired() {
                complete = false;
                break;
            }
            if !sink.push_wide(name, EntryMeta { attributes }) {
                complete = false;
                break;
            }
            entries += 1;
        }

        // SAFETY: `handle` is live for the whole loop (its Drop runs after),
        // and `data` is a valid caller-owned record buffer.
        if unsafe { FindNextFileW(handle.0, &mut data) } == 0 {
            match last_error() {
                code::NO_MORE_FILES => break,
                e => return Err(EnumError::from_win(e)),
            }
        }
    }

    Ok(ListStats {
        entries,
        round_trips: 0,
        elapsed: started.elapsed(),
        strategy: Some(strategy),
        complete,
    })
}

// --- stamp probe -----------------------------------------------------------

/// Reads the directory's timestamps.
///
/// Two paths, in order of preference:
///
/// 1. `GetFileInformationByHandleEx(FileBasicInfo)`, which returns both
///    `LastWriteTime` and `ChangeTime`;
/// 2. `GetFileAttributesExW`, which needs no handle at all and returns the
///    write time only.
///
/// The fallback exists because the alternative to a coarse stamp is *no*
/// stamp, and no stamp means falling back to the hourly floor - change
/// detection at one-hour granularity instead of one minute. A write-only
/// stamp still detects entry churn on NTFS; it is only blind to metadata-only
/// changes, which do not alter a listing.
///
/// The two kinds are tagged and never compared against each other - see
/// [`super::StampKind`] - because `change_time` means different things in each.
fn probe_stamp_win(dir: &Path) -> Result<DirStamp, EnumError> {
    match probe_stamp_by_handle(dir) {
        Ok(stamp) => Ok(stamp),
        Err(handle_err) => probe_stamp_by_attributes(dir).map_err(|_| handle_err),
    }
}

fn probe_stamp_by_handle(dir: &Path) -> Result<DirStamp, EnumError> {
    #[cfg(test)]
    if FORCE_STAMP_FALLBACK.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(EnumError::AccessDenied(code::ACCESS_DENIED));
    }
    let handle = open_dir_handle(dir)?;
    // SAFETY: FILE_BASIC_INFO is plain-old-data; zeroing it is a valid
    // initial state and the API overwrites it on success.
    let mut info: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };

    // SAFETY: `handle` is live; the buffer is a correctly sized, correctly
    // aligned local of exactly the type this information class writes.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileBasicInfo,
            (&raw mut info).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(EnumError::from_win(last_error()));
    }
    Ok(DirStamp::new(info.LastWriteTime, info.ChangeTime))
}

/// Handle-free fallback: the write time from `GetFileAttributesExW`.
fn probe_stamp_by_attributes(dir: &Path) -> Result<DirStamp, EnumError> {
    let is_root = {
        let s = dir.as_os_str().to_string_lossy();
        s.len() <= 3 && s.contains(':')
    };
    let wide = wide_path(dir, is_root);

    // SAFETY: `wide` is NUL-terminated and outlives the call; `data` is a
    // correctly sized, correctly aligned local of exactly the type
    // GetFileExInfoStandard writes, and is only read after a success return.
    let mut data: WIN32_FILE_ATTRIBUTE_DATA = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileAttributesExW(wide.as_ptr(), GetFileExInfoStandard, (&raw mut data).cast())
    };
    if ok == 0 {
        return Err(EnumError::from_win_open(last_error()));
    }
    if data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(EnumError::NotADirectory(code::DIRECTORY));
    }
    Ok(DirStamp::write_only(filetime_to_i64(data.ftLastWriteTime)))
}

/// A `FILETIME` as the same 100ns-since-1601 integer `FILE_BASIC_INFO` uses,
/// so the two paths at least share units.
fn filetime_to_i64(ft: FILETIME) -> i64 {
    ((u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)) as i64
}

/// Forces the attribute fallback, so the path a hostile share would take can
/// be exercised on a machine where the handle query works fine.
#[cfg(test)]
static FORCE_STAMP_FALLBACK: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// --- the source ------------------------------------------------------------

/// Windows-native directory listings, with a fallback chain.
#[derive(Debug, Clone, Copy, Default)]
pub struct WinDirSource {
    preferred: EnumStrategy,
}

impl WinDirSource {
    pub fn new(preferred: EnumStrategy) -> Self {
        Self { preferred }
    }

    fn run(
        strategy: EnumStrategy,
        dir: &Path,
        sink: &mut dyn EntrySink,
        opts: &ListOpts,
        cancel: &CancelToken,
    ) -> Result<ListStats, EnumError> {
        match strategy {
            EnumStrategy::HandleDirInfo => list_handle_dirinfo(dir, sink, opts, cancel),
            EnumStrategy::FindFirstEx => {
                list_find_first(dir, "*", EnumStrategy::FindFirstEx, sink, opts, cancel)
            }
            EnumStrategy::StdReadDir => super::std_enum::StdDirSource.list(dir, sink, opts, cancel),
        }
    }
}

impl DirSource for WinDirSource {
    fn probe_stamp(&self, dir: &Path) -> Result<DirStamp, EnumError> {
        probe_stamp_win(dir)
    }

    fn list(
        &self,
        dir: &Path,
        sink: &mut dyn EntrySink,
        opts: &ListOpts,
        cancel: &CancelToken,
    ) -> Result<ListStats, EnumError> {
        let chain = opts.force.unwrap_or(self.preferred).chain();
        let mut last = EnumError::Unsupported(0);
        for &strategy in chain {
            match Self::run(strategy, dir, sink, opts, cancel) {
                Ok(stats) => return Ok(stats),
                Err(e) if e.should_try_next_strategy() => {
                    // Only a genuine "this API cannot work here" advances the
                    // chain. Everything else is an answer.
                    last = e;
                    continue;
                }
                Err(EnumError::Empty) => {
                    return Ok(ListStats {
                        entries: 0,
                        round_trips: 0,
                        elapsed: std::time::Duration::ZERO,
                        strategy: Some(strategy),
                        complete: true,
                    });
                }
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    fn query(
        &self,
        dir: &Path,
        wildcard: &str,
        sink: &mut dyn EntrySink,
        opts: &ListOpts,
        cancel: &CancelToken,
    ) -> Result<ListStats, EnumError> {
        // Only FindFirstFileExW accepts a pattern, so this path does not
        // consult the configured strategy.
        list_find_first(dir, wildcard, EnumStrategy::FindFirstEx, sink, opts, cancel)
    }

    fn prewarm(&self, root: &Path) {
        // The cheapest call that forces SMB session setup, tree connect and
        // any DFS referral. Deliberately not WNetAddConnection2W, which can
        // pop a credential prompt - catastrophic underneath a TUI.
        let wide = wide_path(root, true);
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        let _ = unsafe { GetFileAttributesW(wide.as_ptr()) };
    }

    fn name(&self) -> &'static str {
        "windows"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::enumerate::VecSink;

    fn temp_with(names: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for n in names {
            std::fs::write(dir.path().join(n), b"x").unwrap();
        }
        dir
    }

    // --- path conversion (pure) -------------------------------------------

    #[test]
    fn find_name_is_bounded_when_the_buffer_is_full() {
        // SAFETY: plain-old-data, zeroed then fully populated below.
        let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        data.cFileName = [b'a' as u16; 260]; // no NUL anywhere
        assert_eq!(find_name(&data).len(), 260, "must not read past the array");
    }

    #[test]
    fn find_name_stops_at_the_terminator() {
        // SAFETY: as above.
        let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        for (i, c) in "ab.txt".encode_utf16().enumerate() {
            data.cFileName[i] = c;
        }
        assert_eq!(String::from_utf16_lossy(find_name(&data)), "ab.txt");
    }

    #[test]
    fn the_record_header_size_matches_the_documented_layout() {
        // 4 + 4 + 6*8 + 4 + 4 + 4 = 68 bytes before the variable-length name.
        assert_eq!(offset_of!(FILE_FULL_DIR_INFO, FileName), 68);
    }

    // --- against the real local filesystem --------------------------------
    //
    // These exercise the unsafe walk on a local NTFS directory. They cannot
    // reproduce SMB behaviour, but they do prove the record chain is parsed
    // correctly and that handles are not leaked.

    #[test]
    fn handle_enumeration_lists_local_files() {
        let dir = temp_with(&["a.txt", "b.txt", "c.txt"]);
        let mut sink = VecSink::default();
        let stats = list_handle_dirinfo(
            dir.path(),
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        )
        .unwrap();
        sink.names.sort();
        assert_eq!(sink.names, vec!["a.txt", "b.txt", "c.txt"]);
        assert_eq!(stats.entries, 3);
        assert!(stats.complete);
        assert!(stats.round_trips >= 1);
    }

    #[test]
    fn handle_enumeration_skips_dot_entries_and_subdirectories() {
        let dir = temp_with(&["file.txt"]);
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut sink = VecSink::default();
        list_handle_dirinfo(
            dir.path(),
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        )
        .unwrap();
        assert_eq!(sink.names, vec!["file.txt"]);
    }

    #[test]
    fn find_first_enumeration_agrees_with_the_handle_strategy() {
        let dir = temp_with(&["one.txt", "two.pdf", "three.dwg"]);

        let mut a = VecSink::default();
        list_handle_dirinfo(
            dir.path(),
            &mut a,
            &ListOpts::default(),
            &CancelToken::never(),
        )
        .unwrap();
        let mut b = VecSink::default();
        list_find_first(
            dir.path(),
            "*",
            EnumStrategy::FindFirstEx,
            &mut b,
            &ListOpts::default(),
            &CancelToken::never(),
        )
        .unwrap();

        a.names.sort();
        b.names.sort();
        assert_eq!(
            a.names, b.names,
            "strategies must agree on the same directory"
        );
    }

    #[test]
    fn all_three_strategies_agree_on_entry_count() {
        // The cross-check `--bench` performs against the real drives.
        let names: Vec<String> = (0..250).map(|i| format!("f{i:04}.dat")).collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let dir = temp_with(&refs);
        let src = WinDirSource::default();

        let mut counts = Vec::new();
        for strategy in [
            EnumStrategy::HandleDirInfo,
            EnumStrategy::FindFirstEx,
            EnumStrategy::StdReadDir,
        ] {
            let mut sink = VecSink::default();
            let stats = src
                .list(
                    dir.path(),
                    &mut sink,
                    &ListOpts::default().with_strategy(strategy),
                    &CancelToken::never(),
                )
                .unwrap();
            counts.push((strategy, stats.entries));
        }
        assert!(
            counts.windows(2).all(|w| w[0].1 == w[1].1),
            "strategies disagreed: {counts:?}"
        );
        assert_eq!(counts[0].1, 250);
    }

    #[test]
    fn a_large_directory_spanning_several_refills_is_enumerated_completely() {
        let names: Vec<String> = (0..3000)
            .map(|i| format!("entry_{i:06}_padding.dat"))
            .collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let dir = temp_with(&refs);

        let mut sink = VecSink::default();
        // A deliberately small buffer forces many refills, exercising the
        // record-chain walk across buffer boundaries.
        let opts = ListOpts {
            buffer_bytes: DIR_BUFFER_MIN,
            ..Default::default()
        };
        let stats =
            list_handle_dirinfo(dir.path(), &mut sink, &opts, &CancelToken::never()).unwrap();
        assert_eq!(stats.entries, 3000);
        assert_eq!(sink.names.len(), 3000);
        assert!(stats.round_trips > 1, "should have needed several refills");
    }

    #[test]
    fn non_ascii_filenames_survive_enumeration() {
        let dir = temp_with(&["Écoles-Été.pdf", "ПРИВЕТ.txt"]);
        let mut sink = VecSink::default();
        list_handle_dirinfo(
            dir.path(),
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        )
        .unwrap();
        sink.names.sort();
        // Sorted by code point: U+00C9 precedes U+041F.
        assert_eq!(sink.names, vec!["Écoles-Été.pdf", "ПРИВЕТ.txt"]);
    }

    #[test]
    fn an_empty_directory_is_an_empty_listing() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = VecSink::default();
        let stats = list_handle_dirinfo(
            dir.path(),
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        )
        .unwrap();
        assert_eq!(stats.entries, 0);
        assert!(stats.complete);
    }

    #[test]
    fn a_missing_directory_reports_a_missing_path() {
        let mut sink = VecSink::default();
        let err = list_handle_dirinfo(
            Path::new("C:\\definitely-not-here-8f21a"),
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        )
        .unwrap_err();
        assert!(matches!(err, EnumError::PathNotFound(_)), "got {err:?}");
    }

    #[test]
    fn the_entry_cap_is_honoured_and_reported() {
        let names: Vec<String> = (0..100).map(|i| format!("f{i:03}")).collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let dir = temp_with(&refs);
        let mut sink = VecSink::default();
        let stats = list_handle_dirinfo(
            dir.path(),
            &mut sink,
            &ListOpts::default().with_max_entries(10),
            &CancelToken::never(),
        )
        .unwrap();
        assert_eq!(stats.entries, 10);
        assert!(!stats.complete);
    }

    #[test]
    fn server_side_patterns_filter_the_results() {
        let dir = temp_with(&["alpha.txt", "beta.txt", "alpha_two.txt"]);
        let mut sink = VecSink::default();
        list_find_first(
            dir.path(),
            "*alpha*",
            EnumStrategy::FindFirstEx,
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        )
        .unwrap();
        sink.names.sort();
        assert_eq!(sink.names, vec!["alpha.txt", "alpha_two.txt"]);
    }

    #[test]
    fn a_pattern_matching_nothing_reports_empty_rather_than_failing() {
        let dir = temp_with(&["alpha.txt"]);
        let mut sink = VecSink::default();
        let err = list_find_first(
            dir.path(),
            "*zzzz*",
            EnumStrategy::FindFirstEx,
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        )
        .unwrap_err();
        assert_eq!(err, EnumError::Empty, "zero matches is an answer");
    }

    /// Restores the fallback switch even if the test panics, so one failure
    /// cannot silently reroute every other probe in the process.
    struct ForcedFallback;

    impl ForcedFallback {
        fn on() -> Self {
            FORCE_STAMP_FALLBACK.store(true, std::sync::atomic::Ordering::Relaxed);
            Self
        }
    }

    impl Drop for ForcedFallback {
        fn drop(&mut self) {
            FORCE_STAMP_FALLBACK.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The share that prompted all of this refuses the handle query. Without
    /// the fallback that means no stamp at all, and no stamp means change
    /// detection drops from one minute to one hour.
    #[test]
    fn probe_stamp_falls_back_to_file_attributes_when_the_handle_query_fails() {
        let dir = temp_with(&["a.txt"]);
        let _forced = ForcedFallback::on();

        let stamp = probe_stamp_win(dir.path()).expect("the fallback must carry the probe");
        assert_eq!(
            stamp.kind,
            crate::index::StampKind::WriteOnly,
            "and it must admit which fields it actually filled in"
        );
        assert!(stamp.last_write > 0);
    }

    #[test]
    fn both_stamp_paths_read_the_same_write_time() {
        let dir = temp_with(&["a.txt"]);
        let by_handle = probe_stamp_by_handle(dir.path()).unwrap();
        let by_attrs = probe_stamp_by_attributes(dir.path()).unwrap();
        assert_eq!(
            by_handle.last_write, by_attrs.last_write,
            "the two paths must at least agree on units and epoch"
        );
    }

    #[test]
    fn the_fallback_stamp_still_moves_when_the_directory_changes() {
        let dir = temp_with(&["a.txt"]);
        let before = probe_stamp_by_attributes(dir.path()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.path().join("b.txt"), b"x").unwrap();
        let after = probe_stamp_by_attributes(dir.path()).unwrap();
        assert_ne!(
            before, after,
            "a coarse stamp is only worth having if it detects entry churn"
        );
    }

    #[test]
    fn the_attribute_probe_rejects_a_file() {
        let dir = temp_with(&["a.txt"]);
        assert!(matches!(
            probe_stamp_by_attributes(&dir.path().join("a.txt")),
            Err(EnumError::NotADirectory(_))
        ));
    }

    /// The handle query is an attribute read, so the handle has to have been
    /// opened for one. Requesting only `FILE_LIST_DIRECTORY` is what made the
    /// probe fail on a strict SMB server and never on local NTFS.
    #[test]
    fn the_directory_handle_is_opened_for_attribute_reads() {
        let dir = temp_with(&["a.txt"]);
        assert!(
            probe_stamp_by_handle(dir.path()).is_ok(),
            "FILE_READ_ATTRIBUTES must be in the access mask"
        );
    }

    #[test]
    fn probe_stamp_reads_directory_timestamps() {
        let dir = temp_with(&["a.txt"]);
        let stamp = probe_stamp_win(dir.path()).unwrap();
        assert!(stamp.last_write > 0);
        assert!(stamp.change_time > 0);
    }

    #[test]
    fn the_stamp_moves_when_the_directory_changes() {
        // The freshness mechanism the periodic full rescan is replaced by.
        let dir = temp_with(&["a.txt"]);
        let before = probe_stamp_win(dir.path()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.path().join("b.txt"), b"x").unwrap();
        let after = probe_stamp_win(dir.path()).unwrap();
        assert_ne!(before, after, "adding an entry must move the stamp");
    }

    #[test]
    fn a_cancelled_enumeration_stops() {
        let names: Vec<String> = (0..500).map(|i| format!("f{i:04}")).collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let dir = temp_with(&refs);

        let epoch = crate::util::cancel::Epoch::new();
        let token = epoch.token(epoch.current());
        epoch.bump();

        let mut sink = VecSink::default();
        let err =
            list_handle_dirinfo(dir.path(), &mut sink, &ListOpts::default(), &token).unwrap_err();
        assert_eq!(err, EnumError::Cancelled);
    }

    #[test]
    fn prewarm_does_not_panic_on_an_unmapped_drive() {
        WinDirSource::default().prewarm(Path::new("Q:\\"));
    }

    #[test]
    fn handles_are_not_leaked_across_many_enumerations() {
        // A leaked find handle would hold a server-side open on a shared
        // drive. Repeating the operation many times would exhaust the process
        // handle table if Drop were not doing its job.
        let dir = temp_with(&["a.txt", "b.txt"]);
        for _ in 0..2000 {
            let mut sink = VecSink::default();
            list_find_first(
                dir.path(),
                "*",
                EnumStrategy::FindFirstEx,
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never(),
            )
            .unwrap();
            let mut sink = VecSink::default();
            list_handle_dirinfo(
                dir.path(),
                &mut sink,
                &ListOpts::default(),
                &CancelToken::never(),
            )
            .unwrap();
        }
    }
}
