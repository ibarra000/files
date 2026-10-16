//! System clipboard access, implemented directly over `windows-sys`.
//!
//! `OpenClipboard` fails while another process holds the clipboard, so opens
//! are retried and may block. The async entry points therefore run on a
//! detached thread and report back as an event, keeping the UI thread free.
//!
//! Off Windows, copying uses the OSC 52 terminal escape sequence and reading
//! returns `Unsupported`.

use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::app::event::{AppEvent, ClipboardMsg, Events};

/// Serialises this process's clipboard access.
///
/// `EmptyClipboard` frees every handle on the clipboard, including one another
/// thread may be reading, so overlapping operations corrupt the heap.
/// Contention with other processes is handled by retrying the open.
static CLIPBOARD: Mutex<()> = Mutex::new(());

/// Puts text on the clipboard.
pub fn set_text(text: &str) -> Result<(), ClipboardError> {
    let _guard = CLIPBOARD.lock();
    imp::set_text(text)
}

/// Reads text from the clipboard.
pub fn text() -> Result<String, ClipboardError> {
    let _guard = CLIPBOARD.lock();
    imp::text()
}

/// A clipboard operation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    /// Another process held the clipboard past the retry window.
    Busy,
    /// The clipboard holds no text.
    Empty,
    /// No clipboard on this platform, or no way to read one.
    Unsupported,
    Os(String),
}

impl ClipboardError {
    pub fn detail(&self) -> String {
        match self {
            Self::Busy => "the clipboard is in use by another program".into(),
            Self::Empty => "the clipboard has no text".into(),
            Self::Unsupported => "the clipboard is not available".into(),
            Self::Os(e) => e.clone(),
        }
    }
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail())
    }
}

impl std::error::Error for ClipboardError {}

/// Copies on a detached thread and reports the result.
pub fn copy_async(text: String, events: Events) {
    let _ = std::thread::Builder::new()
        .name("files-clipboard".into())
        .spawn(move || {
            let chars = text.chars().count();
            let msg = match set_text(&text) {
                Ok(()) => ClipboardMsg::Copied { chars },
                Err(err) => ClipboardMsg::Failed {
                    detail: err.detail(),
                },
            };
            let _ = events.send(AppEvent::Clipboard(msg));
        });
}

/// Reads on a detached thread and reports the text back for insertion.
pub fn read_async(events: Events) {
    let _ = std::thread::Builder::new()
        .name("files-clipboard".into())
        .spawn(move || {
            let msg = match text() {
                Ok(text) => ClipboardMsg::Read {
                    text: Arc::from(text.as_str()),
                },
                Err(err) => ClipboardMsg::Failed {
                    detail: err.detail(),
                },
            };
            let _ = events.send(AppEvent::Clipboard(msg));
        });
}

// --- Windows ---------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };

    use super::ClipboardError;

    /// Defined locally to avoid enabling the `Win32_System_Ole` feature for
    /// one constant. The value is fixed ABI.
    const CF_UNICODETEXT: u32 = 13;

    /// Open retry policy: about 120 ms in total. Contention from other
    /// processes usually clears within milliseconds.
    const OPEN_ATTEMPTS: u32 = 12;
    const OPEN_RETRY: Duration = Duration::from_millis(10);

    /// An open clipboard, closed on drop.
    ///
    /// A clipboard left open blocks every other program until this process
    /// exits.
    struct OpenClip;

    impl Drop for OpenClip {
        fn drop(&mut self) {
            // SAFETY: only constructed after OpenClipboard returned success,
            // and the type is neither Copy nor Clone, so this closes once.
            unsafe { CloseClipboard() };
        }
    }

    fn open() -> Result<OpenClip, ClipboardError> {
        for attempt in 0..OPEN_ATTEMPTS {
            // SAFETY: a null owner window is documented as valid; it
            // associates the clipboard with the current task instead.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Ok(OpenClip);
            }
            if attempt + 1 < OPEN_ATTEMPTS {
                std::thread::sleep(OPEN_RETRY);
            }
        }
        Err(ClipboardError::Busy)
    }

    pub fn set_text(text: &str) -> Result<(), ClipboardError> {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        let bytes = std::mem::size_of_val(wide.as_slice());

        let _clip = open()?;

        // SAFETY: the clipboard is open and owned by this thread.
        if unsafe { EmptyClipboard() } == 0 {
            return Err(ClipboardError::Os("could not clear the clipboard".into()));
        }

        // SAFETY: a plain allocation request; the result is checked for null.
        let handle: HGLOBAL = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
        if handle.is_null() {
            return Err(ClipboardError::Os("out of memory".into()));
        }

        // SAFETY: `handle` is a live GMEM_MOVEABLE block from the call above.
        let ptr = unsafe { GlobalLock(handle) };
        if ptr.is_null() {
            // SAFETY: still owned here - SetClipboardData has not been called,
            // so freeing it is correct and happens exactly once.
            unsafe { GlobalFree(handle) };
            return Err(ClipboardError::Os("could not lock clipboard memory".into()));
        }
        // SAFETY: `ptr` is writable for `bytes`, which is exactly the size the
        // block was allocated with, and the regions cannot overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr.cast::<u16>(), wide.len());
            GlobalUnlock(handle);
        }

        // SAFETY: the clipboard is open and the handle is a valid global block.
        // On success the system takes ownership of `handle`, so it is freed
        // only on the failure path.
        let set = unsafe { SetClipboardData(CF_UNICODETEXT, handle as HANDLE) };
        if set.is_null() {
            // SAFETY: ownership was not transferred, so this frees once.
            unsafe { GlobalFree(handle) };
            return Err(ClipboardError::Os("the clipboard refused the text".into()));
        }
        Ok(())
    }

    pub fn text() -> Result<String, ClipboardError> {
        // SAFETY: no preconditions; simply reports whether the format exists.
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } == 0 {
            return Err(ClipboardError::Empty);
        }

        let _clip = open()?;

        // SAFETY: the clipboard is open. The returned handle is owned by the
        // system and must not be freed here.
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
        if handle.is_null() {
            return Err(ClipboardError::Empty);
        }
        let handle = handle as HGLOBAL;

        // SAFETY: a valid clipboard handle for a global memory format.
        let size = unsafe { GlobalSize(handle) };
        // SAFETY: as above; the pointer is valid until GlobalUnlock.
        let ptr = unsafe { GlobalLock(handle) };
        if ptr.is_null() {
            return Err(ClipboardError::Os("could not lock clipboard memory".into()));
        }

        // The block size is in bytes of UTF-16. Reads are bounded by it rather
        // than by a terminator, which a malformed block may lack.
        let units = size / 2;
        // SAFETY: `ptr` is readable for `size` bytes, hence `units` u16s, and
        // the data is not mutated while the lock is held.
        let slice = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), units) };
        let end = slice.iter().position(|&c| c == 0).unwrap_or(units);
        let text = String::from_utf16_lossy(&slice[..end]);

        // SAFETY: balances the GlobalLock above.
        unsafe { GlobalUnlock(handle) };

        if text.is_empty() {
            return Err(ClipboardError::Empty);
        }
        Ok(text)
    }
}

// --- everywhere else -------------------------------------------------------

#[cfg(not(windows))]
mod imp {
    use std::io::Write;

    use super::ClipboardError;

    /// Copies via the OSC 52 escape sequence, which the terminal handles.
    pub fn set_text(text: &str) -> Result<(), ClipboardError> {
        let payload = super::base64(text.as_bytes());
        let mut out = std::io::stdout();
        out.write_all(format!("\x1b]52;c;{payload}\x07").as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| ClipboardError::Os(e.to_string()))
    }

    /// Always `Unsupported`: terminals cannot be queried for clipboard text.
    pub fn text() -> Result<String, ClipboardError> {
        Err(ClipboardError::Unsupported)
    }
}

/// Standard padded base64, for OSC 52.
#[cfg(any(not(windows), test))]
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;
    use std::time::Duration;

    #[test]
    fn every_error_carries_a_usable_detail() {
        assert!(ClipboardError::Busy.detail().contains("another program"));
        assert!(!ClipboardError::Empty.detail().is_empty());
        assert!(!ClipboardError::Unsupported.detail().is_empty());
        assert_eq!(ClipboardError::Os("boom".into()).detail(), "boom");
    }

    #[test]
    fn base64_matches_the_known_vectors() {
        // Covers all three padding remainders.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    /// Serialises tests that use the real system clipboard.
    ///
    /// Separate from `CLIPBOARD`, which `set_text` and `text` take
    /// internally; holding that one here would deadlock.
    static REAL_CLIPBOARD: Mutex<()> = Mutex::new(());

    #[test]
    fn the_async_copy_always_reports_back() {
        let _guard = REAL_CLIPBOARD.lock();
        // Availability depends on the machine, but a report must always arrive.
        let (tx, rx) = bounded(4);
        let tx = Events::headless(tx);
        copy_async("AB-C-0704".into(), tx);
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(AppEvent::Clipboard(_)) => {}
            other => panic!("expected a clipboard report, got {other:?}"),
        }
    }

    /// Passes without asserting when the clipboard is busy, as on headless CI.
    #[test]
    #[cfg(windows)]
    fn text_written_to_the_clipboard_reads_back_unchanged() {
        let _guard = REAL_CLIPBOARD.lock();
        let sample = "AB-C-0704 café Ω";
        match set_text(sample) {
            Ok(()) => assert_eq!(text().unwrap(), sample),
            Err(ClipboardError::Busy) => {}
            Err(other) => panic!("unexpected failure: {other:?}"),
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn reading_is_reported_as_unsupported_rather_than_empty() {
        assert_eq!(text(), Err(ClipboardError::Unsupported));
    }
}
