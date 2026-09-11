//! Getting text into and out of the system clipboard.
//!
//! Hand-rolled over `windows-sys`, which is already a dependency, rather than
//! pulling in a clipboard crate. That is the same trade the rest of the crate
//! makes - `util::rng` exists to avoid `rand`, and the config and cache
//! directories are resolved by hand rather than through `dirs`. The Win32
//! clipboard is four calls and an allocation; a crate for it would bring more
//! transitive dependencies than code.
//!
//! # Why it runs off the UI thread
//!
//! `OpenClipboard` fails outright while another process holds the clipboard,
//! and that is not rare: the terminal itself may still be holding it in the
//! instant after a keystroke. So the open is retried for a moment, which means
//! it can block - and the UI thread never may. Every entry point here hands
//! the work to a detached thread and reports the outcome back as an event,
//! exactly as `open::open_async` does for the viewer.
//!
//! # Off Windows
//!
//! Copy falls back to OSC 52, an escape sequence the terminal itself
//! interprets, which also happens to work over SSH. Reading has no equivalent
//! - there is no way to ask a terminal for its clipboard - so it reports
//! `Unsupported` rather than pretending.

use std::fmt;
use std::sync::Arc;

use crossbeam_channel::Sender;

use crate::app::event::{AppEvent, ClipboardMsg};

/// Why a clipboard operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    /// Another process held the clipboard for longer than we waited.
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
            Self::Empty => "the clipboard holds no text".into(),
            Self::Unsupported => "this terminal cannot be read from".into(),
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
pub fn copy_async(text: String, events: Sender<AppEvent>) {
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
pub fn read_async(events: Sender<AppEvent>) {
    let _ = std::thread::Builder::new()
        .name("files-clipboard".into())
        .spawn(move || {
            let msg = match get_text() {
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
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};

    use super::ClipboardError;

    /// Declared here rather than pulled from `Win32_System_Ole`, which would
    /// be a whole feature enabled for one number. The value is fixed ABI, and
    /// `index::errors` already keeps a hand-written table of Win32 constants
    /// for the same reason.
    const CF_UNICODETEXT: u32 = 13;

    /// How long to keep trying to open the clipboard.
    ///
    /// The terminal itself often still holds it in the moment after the
    /// keystroke that asked for the copy, and that contention clears in
    /// milliseconds. Failing on the first refusal would make copying
    /// intermittent for no reason.
    const OPEN_ATTEMPTS: u32 = 12;
    const OPEN_RETRY: Duration = Duration::from_millis(10);

    /// An open clipboard, closed on drop.
    ///
    /// Every path out of the functions below is fallible, and a clipboard left
    /// open locks it for every other program on the desktop until this process
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
        //
        // On success the system takes ownership of `handle`; freeing it after
        // this point would be a double free, which is why the only GlobalFree
        // calls are on the failure paths.
        let set = unsafe { SetClipboardData(CF_UNICODETEXT, handle as HANDLE) };
        if set.is_null() {
            // SAFETY: ownership was not transferred, so this frees once.
            unsafe { GlobalFree(handle) };
            return Err(ClipboardError::Os("the clipboard refused the text".into()));
        }
        Ok(())
    }

    pub fn get_text() -> Result<String, ClipboardError> {
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

        // The block is sized in bytes and holds UTF-16, so halve it. Reading
        // is bounded by that length rather than trusting a terminator to be
        // present - a malformed block must not walk off the end.
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

    /// OSC 52: hand the terminal the text and let it do the copying.
    pub fn set_text(text: &str) -> Result<(), ClipboardError> {
        let payload = super::base64(text.as_bytes());
        let mut out = std::io::stdout();
        out.write_all(format!("\x1b]52;c;{payload}\x07").as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| ClipboardError::Os(e.to_string()))
    }

    /// There is no way to ask a terminal what its clipboard holds.
    pub fn get_text() -> Result<String, ClipboardError> {
        Err(ClipboardError::Unsupported)
    }
}

pub use imp::{get_text, set_text};

/// Standard base64, for OSC 52.
///
/// Twenty lines rather than a dependency, and used on exactly one code path.
#[cfg(any(not(windows), test))]
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
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
        // Padding is the part that is easy to get wrong, so all three
        // remainders are covered.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_async_copy_always_reports_back() {
        // Whether the clipboard is available depends on the machine; that a
        // report arrives at all does not, and a silent failure would leave
        // the user with no idea whether the copy happened.
        let (tx, rx) = bounded(4);
        copy_async("11-D-0704".into(), tx);
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(AppEvent::Clipboard(_)) => {}
            other => panic!("expected a clipboard report, got {other:?}"),
        }
    }

    /// A round trip, skipped when the clipboard cannot be opened - on a busy
    /// desktop or a headless CI box that is a fact about the machine, not a
    /// failure of this code.
    #[test]
    #[cfg(windows)]
    fn text_written_to_the_clipboard_reads_back_unchanged() {
        let sample = "11-D-0704 café Ω";
        match set_text(sample) {
            Ok(()) => assert_eq!(get_text().unwrap(), sample),
            Err(ClipboardError::Busy) => {}
            Err(other) => panic!("unexpected failure: {other:?}"),
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn reading_is_reported_as_unsupported_rather_than_empty() {
        assert_eq!(get_text(), Err(ClipboardError::Unsupported));
    }
}
