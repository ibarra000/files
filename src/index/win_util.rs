//! Low-level Windows plumbing shared by the enumerators.
//!
//! Kept separate from the enumeration loops themselves so each file stays
//! about one thing: this one is about turning Rust paths into what the API
//! wants, owning kernel handles safely, and providing a correctly aligned
//! buffer for the kernel to write records into.

use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::Storage::FileSystem::FindClose;

use crate::config::DIR_BUFFER_MIN;

// --- path conversion -------------------------------------------------------

/// Converts a path to a NUL-terminated UTF-16 buffer.
///
/// `keep_trailing_sep` matters: a volume root must be given as `V:\`, while a
/// subdirectory must be given without a trailing separator.
pub fn wide_path(path: &Path, keep_trailing_sep: bool) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut v: Vec<u16> = path.as_os_str().encode_wide().collect();
    if !keep_trailing_sep {
        while v.len() > 1 && matches!(v.last(), Some(&c) if c == b'\\' as u16 || c == b'/' as u16) {
            v.pop();
        }
    }
    // An interior NUL would silently truncate the path at the API boundary.
    if let Some(pos) = v.iter().position(|&c| c == 0) {
        v.truncate(pos);
    }
    v.push(0);
    v
}

/// Builds a NUL-terminated search pattern such as `V:\*` or `R:\ab1234\*foo*`.
pub(crate) fn wide_pattern(dir: &Path, pattern: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut v: Vec<u16> = dir.as_os_str().encode_wide().collect();
    if let Some(pos) = v.iter().position(|&c| c == 0) {
        v.truncate(pos);
    }
    if !matches!(v.last(), Some(&c) if c == b'\\' as u16 || c == b'/' as u16) {
        v.push(b'\\' as u16);
    }
    v.extend(pattern.encode_utf16());
    v.push(0);
    v
}

pub(crate) fn last_error() -> u32 {
    // SAFETY: GetLastError has no preconditions.
    unsafe { GetLastError() }
}

// --- RAII wrappers ---------------------------------------------------------

/// A file or directory handle, closed on drop.
pub(crate) struct OwnedHandle(pub(crate) HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: only constructed from a checked CreateFileW success, and
        // closed exactly once because the type is neither Copy nor Clone.
        unsafe { CloseHandle(self.0) };
    }
}

/// A find handle, released on drop.
///
/// Distinct from [`OwnedHandle`] because find handles require `FindClose`,
/// not `CloseHandle`; the two are not interchangeable. Leaking one holds a
/// server-side open and a directory lease on a share other people are using.
pub(crate) struct FindHandle(pub(crate) HANDLE);

impl Drop for FindHandle {
    fn drop(&mut self) {
        // SAFETY: only constructed from a checked FindFirstFileExW success.
        unsafe { FindClose(self.0) };
    }
}

/// An 8-byte-aligned heap buffer.
///
/// The kernel aligns each record to 8 bytes, but a `Vec<u8>` only guarantees
/// one. Allocating with the right alignment makes the `u16` name slices
/// provably aligned; headers are additionally read with `read_unaligned`, so
/// a mis-sized future record layout still cannot cause undefined behaviour.
#[repr(C, align(8))]
struct Align8(#[allow(dead_code)] [u8; 8]);

pub(crate) struct AlignedBuf {
    data: Vec<Align8>,
    len: usize,
}

impl AlignedBuf {
    pub(crate) fn new(bytes: usize) -> Self {
        let bytes = bytes.max(DIR_BUFFER_MIN);
        let units = bytes.div_ceil(8);
        Self {
            data: (0..units).map(|_| Align8([0; 8])).collect(),
            len: units * 8,
        }
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.data.as_mut_ptr().cast()
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        // SAFETY: `data` owns `len` contiguous initialised bytes (every
        // Align8 is zero-initialised at construction) and the lifetime is tied
        // to `&self`.
        unsafe { std::slice::from_raw_parts(self.data.as_ptr().cast::<u8>(), self.len) }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn grow(&mut self) -> bool {
        if self.len >= 16 << 20 {
            return false;
        }
        *self = Self::new(self.len * 2);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_path_is_nul_terminated() {
        let w = wide_path(Path::new(r"V:\"), true);
        assert_eq!(*w.last().unwrap(), 0);
        assert_eq!(String::from_utf16_lossy(&w[..w.len() - 1]), r"V:\");
    }

    #[test]
    fn wide_path_can_drop_the_trailing_separator() {
        let w = wide_path(Path::new(r"R:\ab1234\"), false);
        assert_eq!(String::from_utf16_lossy(&w[..w.len() - 1]), r"R:\ab1234");
    }

    #[test]
    fn wide_path_keeps_the_root_separator_when_asked() {
        let w = wide_path(Path::new(r"V:\"), true);
        assert_eq!(String::from_utf16_lossy(&w[..w.len() - 1]), r"V:\");
    }

    /// An interior NUL would otherwise silently truncate the path at the API
    /// boundary, so the truncation is done deliberately and exactly once.
    #[test]
    fn wide_path_ends_with_exactly_one_terminator() {
        let w = wide_path(Path::new(r"V:\folder"), true);
        assert_eq!(w.iter().filter(|&&c| c == 0).count(), 1);
    }

    #[test]
    fn wide_pattern_joins_exactly_one_separator() {
        let a = wide_pattern(Path::new(r"V:\"), "*");
        assert_eq!(String::from_utf16_lossy(&a[..a.len() - 1]), r"V:\*");

        let b = wide_pattern(Path::new(r"R:\ab1234"), "*foo*");
        assert_eq!(
            String::from_utf16_lossy(&b[..b.len() - 1]),
            r"R:\ab1234\*foo*"
        );
    }

    #[test]
    fn the_aligned_buffer_is_eight_byte_aligned_and_at_least_the_minimum() {
        let mut b = AlignedBuf::new(1024);
        assert!(b.len() >= DIR_BUFFER_MIN);
        assert_eq!(b.as_mut_ptr() as usize % 8, 0);
    }

    #[test]
    fn the_aligned_buffer_starts_initialised_and_reports_its_true_length() {
        let b = AlignedBuf::new(DIR_BUFFER_MIN);
        assert_eq!(b.as_slice().len(), b.len());
        assert!(b.as_slice().iter().all(|&x| x == 0));
    }

    #[test]
    fn the_aligned_buffer_grows_until_a_sane_ceiling() {
        let mut b = AlignedBuf::new(DIR_BUFFER_MIN);
        let before = b.len();
        assert!(b.grow());
        assert_eq!(b.len(), before * 2);
        assert_eq!(b.as_mut_ptr() as usize % 8, 0, "alignment survives a grow");

        let mut huge = AlignedBuf::new(16 << 20);
        assert!(!huge.grow(), "must refuse to grow without bound");
    }
}
