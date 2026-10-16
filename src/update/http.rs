//! HTTPS GET via WinHTTP.
//!
//! Uses the OS client to avoid an HTTP/TLS dependency; its TLS stack is
//! patched by Windows Update. `WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY` honours
//! the machine's proxy configuration, including auto-detection and PAC.
//! Access goes through [`Fetch`] so callers can be tested without a network.

// Win32 FFI: WinHTTP.
#![allow(unsafe_code)]

use std::fmt;
use std::path::Path;

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// A non-200 status.
    Status(u32),
    /// The body exceeded the caller's limit in bytes.
    TooLarge(u64),
    /// A network, TLS or file error, already described in words.
    Failed(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(404) => f.write_str("not found (404)"),
            Self::Status(code) => write!(f, "the server returned {code}"),
            Self::TooLarge(limit) => write!(f, "larger than {limit} bytes"),
            Self::Failed(detail) => f.write_str(detail),
        }
    }
}

/// Network operations used by the update check.
pub trait Fetch {
    /// Returns the body at `url` if the status is 200 and it is at most
    /// `limit` bytes.
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, HttpError>;

    /// Streams the body at `url` into `to`, which is created or truncated.
    ///
    /// On failure `to` may hold a partial body, so callers write to a scratch
    /// name and rename on success.
    fn download(&self, url: &str, to: &Path, limit: u64) -> Result<(), HttpError>;
}

/// The WinHTTP client.
pub struct WinHttp;

impl Fetch for WinHttp {
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, HttpError> {
        let mut body = Vec::new();
        imp::fetch(url, limit, |chunk| {
            body.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(body)
    }

    fn download(&self, url: &str, to: &Path, limit: u64) -> Result<(), HttpError> {
        use std::io::Write;

        let failed = |e: std::io::Error| HttpError::Failed(format!("{} {e}", to.display()));
        let mut file = std::fs::File::create(to).map_err(failed)?;
        imp::fetch(url, limit, |chunk| file.write_all(chunk))?;
        file.sync_all().map_err(failed)
    }
}

/// Splits an `https://` URL into host and path.
///
/// Rejects ports, user info and plain HTTP: every fetched URL is built by this
/// program, so anything else indicates a bug.
fn split_https(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let plain = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    plain.then_some((host, path))
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::ptr;

    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Networking::WinHttp::{
        INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
        WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders,
        WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    };

    use super::HttpError;

    /// Per-stage timeouts in milliseconds: resolve, connect, send, receive.
    ///
    /// Resolve and connect are short so an offline machine fails fast; receive
    /// is long to tolerate gaps on a slow VPN.
    const TIMEOUTS: (i32, i32, i32, i32) = (10_000, 10_000, 30_000, 60_000);

    /// A WinHTTP handle, closed on drop.
    struct Handle(*mut c_void);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: a handle WinHTTP gave us, closed exactly once.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Describes the last WinHTTP error. Its messages live in `winhttp.dll`,
    /// not the system table, so `io::Error` would render most as "unknown".
    fn last_error() -> HttpError {
        // SAFETY: no preconditions.
        let code = unsafe { GetLastError() };
        let words = match code {
            12002 => "the server did not respond in time",
            12007 => "the server name could not be resolved; check the network connection",
            12029 => "the connection was refused",
            12030 => "the connection was dropped",
            12038 | 12037 | 12044 | 12045 | 12175 => {
                "the certificate could not be verified; nothing was downloaded"
            }
            _ => "",
        };
        HttpError::Failed(if words.is_empty() {
            format!("the request failed (WinHTTP error {code})")
        } else {
            words.to_string()
        })
    }

    fn check(ok: i32) -> Result<(), HttpError> {
        if ok != 0 { Ok(()) } else { Err(last_error()) }
    }

    fn opened(handle: *mut c_void) -> Result<Handle, HttpError> {
        if handle.is_null() {
            Err(last_error())
        } else {
            Ok(Handle(handle))
        }
    }

    /// GETs `url` and hands the body to `sink` a chunk at a time.
    pub fn fetch(
        url: &str,
        limit: u64,
        mut sink: impl FnMut(&[u8]) -> std::io::Result<()>,
    ) -> Result<(), HttpError> {
        let Some((host, path)) = super::split_https(url) else {
            return Err(HttpError::Failed(format!("{url} is not an https address")));
        };
        let agent = wide(&format!("files/{}", env!("CARGO_PKG_VERSION")));
        let (host, path, verb) = (wide(host), wide(path), wide("GET"));

        // Handles drop in reverse order (request, connection, session), as
        // WinHTTP requires.
        //
        // Every call below relies on the same three facts, which each SAFETY
        // comment names: strings are NUL-terminated and outlive the call;
        // handles are ones opened just above and still open; out parameters
        // are live locals sized as the call is told.
        // SAFETY: `agent` is NUL-terminated and outlives the call.
        let session = opened(unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                ptr::null(),
                ptr::null(),
                0,
            )
        })?;
        let (resolve, connect, send, receive) = TIMEOUTS;
        // SAFETY: `session` is open.
        check(unsafe { WinHttpSetTimeouts(session.0, resolve, connect, send, receive) })?;
        // SAFETY: `session` is open and `host` is NUL-terminated.
        let connection = opened(unsafe {
            WinHttpConnect(session.0, host.as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0)
        })?;
        // HTTPS-to-HTTPS redirects are followed by default; release assets
        // redirect to a storage host.
        // SAFETY: `connection` is open; `verb` and `path` are NUL-terminated.
        let request = opened(unsafe {
            WinHttpOpenRequest(
                connection.0,
                verb.as_ptr(),
                path.as_ptr(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        })?;
        // SAFETY: `request` is open; no headers or body are passed.
        check(unsafe { WinHttpSendRequest(request.0, ptr::null(), 0, ptr::null(), 0, 0, 0) })?;
        // SAFETY: `request` is open and has been sent.
        check(unsafe { WinHttpReceiveResponse(request.0, ptr::null_mut()) })?;

        let mut status: u32 = 0;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: `request` is open; `status` is a live u32 and `size` says so.
        check(unsafe {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                ptr::null(),
                (&mut status as *mut u32).cast(),
                &mut size,
                ptr::null_mut(),
            )
        })?;
        if status != 200 {
            return Err(HttpError::Status(status));
        }

        let mut total: u64 = 0;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let mut available: u32 = 0;
            // SAFETY: `request` is open; `available` is a live local.
            check(unsafe { WinHttpQueryDataAvailable(request.0, &mut available) })?;
            if available == 0 {
                return Ok(());
            }
            let want = (available as usize).min(buffer.len());
            let mut read: u32 = 0;
            // SAFETY: `request` is open; `buffer` is writable for `want` bytes,
            // which never exceeds its length; `read` is a live local.
            check(unsafe {
                WinHttpReadData(
                    request.0,
                    buffer.as_mut_ptr().cast(),
                    want as u32,
                    &mut read,
                )
            })?;
            if read == 0 {
                return Ok(());
            }
            total += u64::from(read);
            if total > limit {
                return Err(HttpError::TooLarge(limit));
            }
            sink(&buffer[..read as usize]).map_err(|e| HttpError::Failed(e.to_string()))?;
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::HttpError;

    pub fn fetch(
        _url: &str,
        _limit: u64,
        _sink: impl FnMut(&[u8]) -> std::io::Result<()>,
    ) -> Result<(), HttpError> {
        Err(HttpError::Failed(
            "only Windows can look for updates".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_https_address_is_split_into_its_host_and_path() {
        assert_eq!(
            split_https("https://github.com/owner/repo/releases/latest/download/latest.toml"),
            Some((
                "github.com",
                "/owner/repo/releases/latest/download/latest.toml"
            ))
        );
        assert_eq!(split_https("https://github.com"), Some(("github.com", "/")));
    }

    #[test]
    fn anything_but_plain_https_to_a_host_is_refused() {
        assert_eq!(split_https("http://github.com/x"), None);
        assert_eq!(split_https("https://user@github.com/x"), None);
        assert_eq!(split_https("https://github.com:8443/x"), None);
        assert_eq!(split_https("https:///x"), None);
        assert_eq!(split_https("ftp://github.com/x"), None);
    }

    #[test]
    fn a_missing_file_says_so_in_words() {
        assert_eq!(HttpError::Status(404).to_string(), "not found (404)");
        assert_eq!(
            HttpError::Status(503).to_string(),
            "the server returned 503"
        );
    }

    /// Live test against github.com; ignored by default as it needs a network.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs the internet"]
    fn the_operating_systems_client_can_reach_github() {
        let body = WinHttp
            .get("https://github.com/", 4 << 20)
            .expect("github.com answered");
        assert!(!body.is_empty());
        assert_eq!(
            WinHttp.get("https://github.com/definitely/not/a/page/7713", 1 << 20),
            Err(HttpError::Status(404))
        );
    }
}
