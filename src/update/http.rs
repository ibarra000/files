//! An HTTPS GET, through the operating system.
//!
//! WinHTTP rather than a crate, for the reason `apply` hashes with BCrypt:
//! the program needs one small file and, a few times a year, one installer,
//! and the operating system already ships an HTTP client with a TLS stack
//! behind it that Windows Update keeps patched. A crate would be the largest
//! dependency in the manifest, for two requests every four hours.
//!
//! It also gets the proxy right without being told.
//! `WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY` uses whatever the machine's proxy
//! configuration says, auto-detection and PAC scripts included - which on an
//! office network is the difference between working and a timeout nobody can
//! explain.
//!
//! Behind [`Fetch`], so the decisions made about what comes back can be tested
//! without a network, and so there is exactly one place that talks to one.

use std::fmt;
use std::path::Path;

/// Why a request came to nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// The server answered, with something other than 200.
    Status(u32),
    /// More than the caller was willing to read.
    TooLarge(u64),
    /// It never got that far: no network, no name, a refused connection, a
    /// certificate that did not check out, or a file that could not be
    /// written. Already in words.
    Failed(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(404) => f.write_str("there is nothing there (404)"),
            Self::Status(code) => write!(f, "the server answered {code}"),
            Self::TooLarge(limit) => write!(f, "it is larger than {limit} bytes"),
            Self::Failed(detail) => f.write_str(detail),
        }
    }
}

/// The two things the update check asks of the network.
pub trait Fetch {
    /// The body at `url`, if it is 200 and at most `limit` bytes.
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, HttpError>;

    /// The body at `url`, written to `to`, which is created or truncated.
    ///
    /// Streamed rather than read into memory first, because an installer is
    /// tens of megabytes. On any failure `to` may hold part of the body; the
    /// caller writes to a scratch name and renames on success for exactly
    /// that reason.
    fn download(&self, url: &str, to: &Path, limit: u64) -> Result<(), HttpError>;
}

/// The operating system's HTTP client.
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

/// The host and the path of an `https://` URL, and nothing else.
///
/// Deliberately narrow: every URL this program fetches is one it built itself,
/// on `github.com`, so anything that is not plain HTTPS to a host is a bug to
/// refuse rather than a case to support. No port, no user, no plain HTTP.
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

    /// How long each stage may take, in milliseconds: resolving the name,
    /// connecting, sending, and each wait for more of the body.
    ///
    /// Short for the first two, because a machine with no route to GitHub
    /// should find out in seconds; longer for the last, because an installer
    /// over a slow VPN arrives in chunks with gaps between them.
    const TIMEOUTS: (i32, i32, i32, i32) = (10_000, 10_000, 30_000, 60_000);

    /// A WinHTTP handle, closed when it goes.
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

    /// The last WinHTTP error, in words somebody reading the About page can
    /// use. WinHTTP's own messages live in `winhttp.dll` rather than the
    /// system table, so `io::Error` would render most of them as "unknown".
    fn last_error() -> HttpError {
        // SAFETY: no preconditions.
        let code = unsafe { GetLastError() };
        let words = match code {
            12002 => "it took too long to answer",
            12007 => "the name could not be looked up, so there may be no network",
            12029 => "the connection was refused",
            12030 => "the connection was dropped",
            12038 | 12037 | 12044 | 12045 | 12175 => {
                "its certificate could not be checked, so nothing was downloaded"
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

        // Dropped in reverse order of opening, which is the order WinHTTP
        // wants them closed: request, then connection, then session.
        //
        // SAFETY for every call below: each string is NUL-terminated and
        // outlives the call; each handle is one opened just above and still
        // open; out parameters are live locals sized as the call is told.
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
        check(unsafe { WinHttpSetTimeouts(session.0, resolve, connect, send, receive) })?;
        let connection = opened(unsafe {
            WinHttpConnect(session.0, host.as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0)
        })?;
        // Redirects are followed by default, HTTPS to HTTPS only - which is
        // what a release asset needs, since github.com answers with a 302 to
        // the host the file is actually stored on.
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
        check(unsafe { WinHttpSendRequest(request.0, ptr::null(), 0, ptr::null(), 0, 0, 0) })?;
        check(unsafe { WinHttpReceiveResponse(request.0, ptr::null_mut()) })?;

        let mut status: u32 = 0;
        let mut size = size_of::<u32>() as u32;
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
            check(unsafe { WinHttpQueryDataAvailable(request.0, &mut available) })?;
            if available == 0 {
                return Ok(());
            }
            let want = (available as usize).min(buffer.len());
            let mut read: u32 = 0;
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

    /// Everything this program fetches is an address it built, so anything
    /// else is refused rather than half-supported.
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
        assert_eq!(
            HttpError::Status(404).to_string(),
            "there is nothing there (404)"
        );
        assert_eq!(
            HttpError::Status(503).to_string(),
            "the server answered 503"
        );
    }

    /// The real client against the real GitHub. Ignored by default, because
    /// a test suite that needs the internet is one that fails on a train.
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
