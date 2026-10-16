//! Named-pipe transport for [`super::Link`], with reader and writer threads.
//!
//! The panel is the server and the settings window the client, so the pipe
//! names exist exactly while a panel is running.
//!
//! # Why two pipes
//!
//! Each end runs a blocking reader thread and a writer thread. On a single
//! synchronous duplex handle, the I/O manager serialises all operations on the
//! file object, so a pending `ReadFile` blocks every `WriteFile` and both ends
//! deadlock. One half-duplex pipe per direction (`-down`: panel to window,
//! `-up`: window to panel) avoids this without overlapped I/O.
//!
//! Every failure (create, connect, broken pipe) means "no other end" and
//! clears the same liveness flag; there is no error type.

// Win32 FFI: named pipes between the panel and the settings window.
#![allow(unsafe_code)]
#![cfg(windows)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
    PIPE_ACCESS_OUTBOUND, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_MESSAGE, PIPE_WAIT, SetNamedPipeHandleState, WaitNamedPipeW,
};

use super::{Link, ToPanel, ToSettings};

/// Maximum message size in bytes, for either direction.
///
/// Real messages are under about 200 bytes. A message larger than the read
/// buffer is not reassembled (`ERROR_MORE_DATA`), so this must stay well
/// above any real message.
const MAX_MESSAGE: usize = 4096;

/// How long a client waits for a busy pipe, in milliseconds.
///
/// A pipe is busy only while another settings window is connected; the wait
/// covers one window closing as another opens.
const BUSY_WAIT_MS: u32 = 1000;

/// Returned by `ConnectNamedPipe` when the client connected before the call;
/// treated as success.
const ERROR_PIPE_CONNECTED: u32 = 535;

/// Access right `SetNamedPipeHandleState` requires, even on a read-only end.
///
/// Without it the call fails silently and the handle stays in byte mode.
/// Defined here because `windows-sys` places it in an unused constant group.
const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Returns the `-down` and `-up` pipe names for `name`.
fn ends_of(name: &str) -> (Vec<u16>, Vec<u16>) {
    (wide(&format!("{name}-down")), wide(&format!("{name}-up")))
}

/// A handle closed on drop.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: a handle this module opened and has not closed.
            unsafe { CloseHandle(self.0) };
        }
    }
}

// SAFETY: a Win32 kernel handle is a process-wide token rather than a
// pointer into this process's memory, and each one below is used by exactly
// one thread for exactly one direction.
unsafe impl Send for Owned {}
// SAFETY: the handle is a kernel token, not memory, and the Win32 calls
// made through a shared `&Owned` are themselves thread-safe.
unsafe impl Sync for Owned {}

/// Writes one newline-terminated message; returns `false` once the other end
/// has gone.
///
/// The terminator keeps framing correct even if the reader falls back to byte
/// mode (see [`FILE_WRITE_ATTRIBUTES`]), where back-to-back messages would
/// otherwise run together.
fn write_line(handle: HANDLE, line: &str) -> bool {
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(b'\n');
    if bytes.len() > MAX_MESSAGE {
        // Drop rather than truncate; a truncated line could decode as a
        // different message.
        return true;
    }
    let mut written = 0u32;
    // SAFETY: writes `bytes.len()` bytes from a live buffer, with the count
    // going to a local. The handle is owned by the calling thread.
    let ok = unsafe {
        WriteFile(
            handle,
            bytes.as_ptr(),
            bytes.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        )
    };
    ok != 0 && written as usize == bytes.len()
}

/// Reads available data, or returns `None` once the other end has gone.
///
/// May contain several messages in byte mode, so the caller splits on the
/// terminator. See [`write_line`].
fn read_chunk(handle: HANDLE) -> Option<String> {
    let mut buf = vec![0u8; MAX_MESSAGE];
    let mut read = 0u32;
    // SAFETY: reads into a live buffer of the length passed, with the count
    // going to a local.
    let ok = unsafe {
        ReadFile(
            handle,
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut read,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 || read == 0 {
        return None;
    }
    buf.truncate(read as usize);
    // Lossy: invalid UTF-8 yields a line the codec rejects, without dropping
    // the connection.
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Starts reader and writer threads over a connected pair of handles.
///
/// Used by both server and client.
fn pump(
    outbound: Owned,
    inbound: Owned,
    alive: Arc<AtomicBool>,
    wake: impl Fn() + Send + 'static,
) -> (Sender<String>, Receiver<String>) {
    let (out_tx, out_rx) = std::sync::mpsc::channel::<String>();
    let (in_tx, in_rx) = std::sync::mpsc::channel::<String>();

    let writing = Arc::clone(&alive);
    let _ = std::thread::Builder::new()
        .name("files-ipc-write".to_owned())
        .spawn(move || {
            // Closed when this thread ends, which ends the far end's read.
            let outbound = outbound;
            while let Ok(line) = out_rx.recv() {
                if !write_line(outbound.0, &line) {
                    writing.store(false, Ordering::Relaxed);
                    return;
                }
            }
        });

    let reading = Arc::clone(&alive);
    let _ = std::thread::Builder::new()
        .name("files-ipc-read".to_owned())
        .spawn(move || {
            let inbound = inbound;
            loop {
                let Some(chunk) = read_chunk(inbound.0) else {
                    reading.store(false, Ordering::Relaxed);
                    // Wake the frame loop on disconnect too, so the UI
                    // updates without waiting for input.
                    wake();
                    return;
                };
                // A read may hold several messages. See `write_line`.
                for line in chunk.split('\n').filter(|l| !l.is_empty()) {
                    if in_tx.send(line.to_owned()).is_err() {
                        return;
                    }
                }
                wake();
            }
        });

    (out_tx, in_rx)
}

/// Creates a single-instance message-mode pipe for one direction.
fn create_instance(wide_name: &[u16], access: FILE_FLAGS_AND_ATTRIBUTES) -> Option<Owned> {
    // SAFETY: a named pipe with default security, one instance, and a live
    // NUL-terminated wide name for the duration of the call.
    let handle = unsafe {
        CreateNamedPipeW(
            wide_name.as_ptr(),
            access,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            MAX_MESSAGE as u32,
            MAX_MESSAGE as u32,
            0,
            std::ptr::null(),
        )
    };
    (handle != INVALID_HANDLE_VALUE).then_some(Owned(handle))
}

/// Blocks until a client connects; returns `false` on failure.
fn accept(handle: &Owned) -> bool {
    // SAFETY: blocks until a client connects to a handle this module made.
    let joined = unsafe { ConnectNamedPipe(handle.0, std::ptr::null_mut()) };
    // SAFETY: no arguments; reads this thread's last error.
    joined != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED
}

// -- the panel's end --------------------------------------------------------

/// The panel's end: a server that accepts one settings window at a time.
pub struct Server {
    inner: Arc<std::sync::Mutex<Option<Ends>>>,
    /// Whether a window is connected; set by the accept thread. Atomic so the
    /// frame loop never blocks on the mutex.
    connected: Arc<AtomicBool>,
}

struct Ends {
    out: Sender<String>,
    inbox: Receiver<String>,
}

impl Server {
    /// Starts listening. `wake` is called when a message arrives or the
    /// window disconnects.
    ///
    /// Returns `None` if the pipes cannot be created (another instance holds
    /// them); the panel then runs without a settings link.
    pub fn listen(wake: impl Fn() + Clone + Send + 'static) -> Option<Self> {
        Self::listen_on(&super::pipe_name(), wake)
    }

    /// [`Self::listen`] with an explicit pipe name, so parallel tests can use
    /// distinct names.
    pub fn listen_on(name: &str, wake: impl Fn() + Clone + Send + 'static) -> Option<Self> {
        // Create the first instances before returning, so a `--settings`
        // started immediately after cannot miss them.
        let (down, up) = ends_of(name);
        let first_down = create_instance(&down, PIPE_ACCESS_OUTBOUND)?;
        let first_up = create_instance(&up, PIPE_ACCESS_INBOUND)?;

        let inner: Arc<std::sync::Mutex<Option<Ends>>> = Arc::new(std::sync::Mutex::new(None));
        let connected = Arc::new(AtomicBool::new(false));
        let shared = Arc::clone(&inner);
        let flag = Arc::clone(&connected);
        let spawned = std::thread::Builder::new()
            .name("files-ipc-accept".to_owned())
            .spawn(move || {
                accept_loop(down, up, (first_down, first_up), shared, flag, wake);
            });
        if spawned.is_err() {
            return None;
        }
        Some(Self { inner, connected })
    }

    /// Whether a settings window is attached.
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Sends to the connected window, if any. Best effort.
    pub fn send(&self, msg: &ToSettings) {
        let Ok(guard) = self.inner.lock() else {
            return;
        };
        if let Some(ends) = guard.as_ref() {
            let _ = ends.out.send(msg.encode());
        }
    }

    /// Returns messages received from the window since the last call.
    pub fn poll(&self) -> Vec<ToPanel> {
        let Ok(guard) = self.inner.lock() else {
            return Vec::new();
        };
        let Some(ends) = guard.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        loop {
            match ends.inbox.try_recv() {
                Ok(line) => out.extend(ToPanel::decode(&line)),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return out,
            }
        }
    }
}

/// Serves one settings window at a time until it disconnects, then accepts
/// the next.
///
/// Single-instance pipes prevent two windows editing the config at once.
fn accept_loop(
    down: Vec<u16>,
    up: Vec<u16>,
    first: (Owned, Owned),
    inner: Arc<std::sync::Mutex<Option<Ends>>>,
    connected: Arc<AtomicBool>,
    wake: impl Fn() + Clone + Send + 'static,
) {
    let mut next = Some(first);
    loop {
        let (out_h, in_h) = match next.take() {
            Some(pair) => pair,
            None => {
                let Some(out_h) = create_instance(&down, PIPE_ACCESS_OUTBOUND) else {
                    return;
                };
                let Some(in_h) = create_instance(&up, PIPE_ACCESS_INBOUND) else {
                    return;
                };
                (out_h, in_h)
            }
        };

        // Must match the client's open order, or both sides wait forever.
        if !accept(&out_h) || !accept(&in_h) {
            continue;
        }

        let alive = Arc::new(AtomicBool::new(true));
        let (out, inbox) = pump(out_h, in_h, Arc::clone(&alive), wake.clone());
        if let Ok(mut guard) = inner.lock() {
            *guard = Some(Ends { out, inbox });
        }
        connected.store(true, Ordering::Relaxed);

        // Poll until the reader reports a disconnect; the pump threads own
        // the handles and are not joined.
        while alive.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        connected.store(false, Ordering::Relaxed);
        if let Ok(mut guard) = inner.lock() {
            // Dropping the sender ends the writer thread and closes the
            // outbound handle; the next iteration creates a fresh pair.
            *guard = None;
        }
        wake();
    }
}

// -- the settings window's end ----------------------------------------------

/// The settings window's end.
pub struct Client {
    out: Sender<String>,
    inbox: Receiver<String>,
    alive: Arc<AtomicBool>,
}

impl Client {
    /// Connects to a running panel, or returns `None` if none is running.
    pub fn connect(wake: impl Fn() + Send + 'static) -> Option<Self> {
        Self::connect_to(&super::pipe_name(), wake)
    }

    /// [`Self::connect`] with an explicit pipe name. See [`Server::listen_on`].
    pub fn connect_to(name: &str, wake: impl Fn() + Send + 'static) -> Option<Self> {
        let (down, up) = ends_of(name);
        // Down first, matching the server's accept order. The read handle
        // needs `FILE_WRITE_ATTRIBUTES` for `SetNamedPipeHandleState` below.
        let inbound = open(&down, GENERIC_READ | FILE_WRITE_ATTRIBUTES)?;
        let outbound = open(&up, GENERIC_WRITE)?;

        // Client handles open in byte mode; switch to one message per read.
        let mode = PIPE_READMODE_MESSAGE;
        // SAFETY: sets the read mode on a handle just opened; the other two
        // arguments are optional and null.
        unsafe {
            SetNamedPipeHandleState(inbound.0, &mode, std::ptr::null(), std::ptr::null());
        }

        let alive = Arc::new(AtomicBool::new(true));
        let (out, inbox) = pump(outbound, inbound, Arc::clone(&alive), wake);
        Some(Self { out, inbox, alive })
    }
}

/// Opens one direction, waiting once if it is busy.
fn open(name: &[u16], access: u32) -> Option<Owned> {
    for _ in 0..2 {
        // SAFETY: opens an existing named pipe by a live NUL-terminated
        // wide name. A name nothing is serving returns INVALID_HANDLE_VALUE,
        // which is checked.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                access,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            return Some(Owned(handle));
        }
        // SAFETY: no arguments; reads this thread's last error.
        if unsafe { GetLastError() } != ERROR_PIPE_BUSY {
            return None;
        }
        // SAFETY: waits for an instance of a named pipe to become free.
        if unsafe { WaitNamedPipeW(name.as_ptr(), BUSY_WAIT_MS) } == 0 {
            return None;
        }
    }
    None
}

impl Link for Client {
    fn send(&mut self, msg: &ToPanel) -> bool {
        self.alive() && self.out.send(msg.encode()).is_ok()
    }

    fn poll(&mut self) -> Vec<ToSettings> {
        let mut out = Vec::new();
        loop {
            match self.inbox.try_recv() {
                Ok(line) => out.extend(ToSettings::decode(&line)),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return out,
            }
        }
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{Held, Live};
    use std::time::{Duration, Instant};

    /// A pipe name nothing else in the suite uses.
    fn name(tag: &str) -> String {
        format!(r"\\.\pipe\files-test-{}-{tag}", std::process::id())
    }

    /// Polls `done` until it holds or `secs` elapse.
    fn within(secs: u64, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        done()
    }

    /// Round trip over real pipes in one process; also guards against the
    /// duplex deadlock described in the module docs.
    #[test]
    fn a_message_crosses_a_real_pipe_in_each_direction() {
        let name = name("both-ways");
        let server = Server::listen_on(&name, || {}).expect("a pipe");
        assert!(!server.connected(), "connected before anything did");

        let mut client = Client::connect_to(&name, || {}).expect("a connection");
        assert!(within(2, || server.connected()), "the server never noticed");
        assert!(client.alive());

        let sent = ToSettings::Live(Live {
            placement: Some((-1920, 40)),
            hotkey: Held::No("Ctrl+Shift+Space is taken".into()),
        });
        server.send(&sent);
        let mut got = Vec::new();
        assert!(
            within(2, || {
                got.extend(client.poll());
                !got.is_empty()
            }),
            "nothing arrived at the window"
        );
        assert_eq!(got, vec![sent]);

        assert!(client.send(&ToPanel::ForgetPlacement));
        let mut back = Vec::new();
        assert!(
            within(2, || {
                back.extend(server.poll());
                !back.is_empty()
            }),
            "nothing arrived at the panel"
        );
        assert_eq!(back, vec![ToPanel::ForgetPlacement]);
    }

    #[test]
    fn messages_arrive_in_order() {
        let name = name("in-order");
        let server = Server::listen_on(&name, || {}).expect("a pipe");
        let mut client = Client::connect_to(&name, || {}).expect("a connection");
        assert!(within(2, || server.connected()));

        let sent = [
            ToSettings::Reload,
            ToSettings::Show(crate::view::settings::PageId::Diagnostics),
            ToSettings::Close,
            ToSettings::Exiting,
        ];
        for msg in &sent {
            server.send(msg);
        }
        let mut got = Vec::new();
        assert!(
            within(3, || {
                got.extend(client.poll());
                got.len() == sent.len()
            }),
            "{got:?}"
        );
        assert_eq!(got, sent.to_vec());
    }

    /// A burst outpaces the reader, which is the only way byte-mode framing
    /// errors show up.
    #[test]
    fn a_burst_of_messages_is_not_run_together() {
        let name = name("burst");
        let server = Server::listen_on(&name, || {}).expect("a pipe");
        let mut client = Client::connect_to(&name, || {}).expect("a connection");
        assert!(within(2, || server.connected()));

        for _ in 0..64 {
            server.send(&ToSettings::Reload);
        }
        let mut got = Vec::new();
        assert!(
            within(5, || {
                got.extend(client.poll());
                got.len() == 64
            }),
            "{} of 64 arrived",
            got.len()
        );
        assert!(got.iter().all(|m| *m == ToSettings::Reload));
    }

    #[test]
    fn the_panel_notices_when_the_window_goes() {
        let name = name("hangup");
        let server = Server::listen_on(&name, || {}).expect("a pipe");
        let client = Client::connect_to(&name, || {}).expect("a connection");
        assert!(within(2, || server.connected()));

        drop(client);
        assert!(
            within(5, || !server.connected()),
            "the panel still believed a window was attached"
        );
    }

    #[test]
    fn a_window_with_no_panel_behind_it_says_so_at_once() {
        let started = Instant::now();
        let link = Client::connect_to(&name("nobody-home"), || {});
        assert!(link.is_none(), "something answered a name nothing serves");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "connecting blocked for {:?}",
            started.elapsed()
        );
    }
}
