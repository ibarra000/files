//! Two named pipes, and the two threads that keep them moving.
//!
//! The panel is the server and the settings window is the client, which is
//! the way round that makes liveness mean what it needs to: the server's
//! names exist exactly while the panel does, so `--settings` finding
//! nothing to connect to *is* the answer to "is there a panel".
//!
//! # Why two pipes and not one duplex pipe
//!
//! Because one duplex pipe deadlocks, and the way it deadlocks is worth
//! writing down because it looks like it ought to work.
//!
//! Neither end may block its frame loop, so each end has a reader thread
//! and a writer thread over a channel, and the UI does a `try_recv` once a
//! frame. With one duplex handle those two threads share one *file object*,
//! and a handle opened without `FILE_FLAG_OVERLAPPED` is synchronous: the
//! I/O manager serialises every operation on the file object, whichever
//! direction it is in. So the reader thread's blocking `ReadFile` - which
//! is blocked by design, waiting for the other end to speak - holds the
//! file object, and the writer thread's `WriteFile` queues behind it and
//! never runs. Neither end can speak until the other does. It reaches that
//! state in about a millisecond and looks exactly like "the messages are
//! not arriving".
//!
//! The documented fix is overlapped I/O: two `OVERLAPPED` structures, two
//! events, `GetOverlappedResult`, and cancellation to get right on
//! shutdown. The simpler fix is two pipes, one per direction, so that each
//! handle only ever has one operation outstanding. This is the second, for
//! about sixty lines less unsafe code and one fewer way to get cancellation
//! wrong.
//!
//! The pair is `-down` (panel to window) and `-up` (window to panel), each
//! half-duplex in one direction - so a handle used the wrong way round
//! fails at the first call rather than working strangely.
//!
//! # Every failure is the same failure
//!
//! A pipe that cannot be created, a client that cannot connect, a write
//! that returns `ERROR_BROKEN_PIPE` - all of them mean "there is no other
//! end", and all of them set the same flag. There is no error type here
//! because there is nothing a caller would do differently: the settings
//! window disables three controls and changes some wording, and the panel
//! carries on exactly as it would with no window open.

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

/// The biggest message either side will send or accept.
///
/// Four kilobytes, against a longest realistic message of about two hundred
/// bytes - a `live` line carrying a Windows error string. It is a cap and
/// not a budget: in message mode a `ReadFile` with a buffer smaller than
/// the message returns `ERROR_MORE_DATA` and the rest is *dropped* below
/// rather than reassembled, so the number has to be comfortably larger than
/// anything real.
const MAX_MESSAGE: usize = 4096;

/// How long a client waits for a busy pipe before giving up.
///
/// One second. The only way to be busy is for another settings window to be
/// connected, and refusing the second is the intended behaviour - waiting
/// at all is politeness for the moment one window is closing as another
/// opens.
const BUSY_WAIT_MS: u32 = 1000;

/// A client that connects between `CreateNamedPipeW` and `ConnectNamedPipe`
/// makes the latter fail with this, which is success by another name.
const ERROR_PIPE_CONNECTED: u32 = 535;

/// What `SetNamedPipeHandleState` needs beyond `GENERIC_READ`.
///
/// It is a *write* right, on a handle this end only ever reads from, and
/// leaving it out is how the read mode silently stays byte-oriented - see
/// the note on `Client::connect_to`. `windows-sys` puts it in a constant
/// group this crate does not otherwise use, so it is spelled out here with
/// the reason attached.
const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The two names a link is made of.
fn ends_of(name: &str) -> (Vec<u16>, Vec<u16>) {
    (wide(&format!("{name}-down")), wide(&format!("{name}-up")))
}

/// A handle that closes itself.
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
unsafe impl Sync for Owned {}

/// Writes one message, terminated. `false` once the other end has gone.
///
/// # Why there is a terminator as well as message mode
///
/// Message mode is supposed to make one write one read, and when it is on
/// it does. It is one API call away from being off: `SetNamedPipeHandleState`
/// needs `FILE_WRITE_ATTRIBUTES` on the handle, a *write* right on a handle
/// that only reads, and without it the call fails and the read mode stays
/// byte-oriented. Nothing reports that. What happens instead is that four
/// messages sent back to back arrive as
/// `show diagnosticscloseexiting`, which the codec refuses as one
/// unparseable line - so three messages vanish and the fourth is corrupt,
/// silently, and only when the sender is faster than the reader.
///
/// The access right is fixed. The terminator is here so that getting it
/// wrong again cannot cost anything: framing is a byte per message, and a
/// transport whose correctness rests on a flag being set somewhere else is
/// a transport that will be wrong again.
fn write_line(handle: HANDLE, line: &str) -> bool {
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(b'\n');
    if bytes.len() > MAX_MESSAGE {
        // Dropped rather than truncated: half a message is a message that
        // decodes as something else, and the codec next door is written so
        // that a wrong line is `None` rather than a wrong meaning. Dropping
        // keeps that promise.
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

/// Reads whatever has arrived, or `None` once the other end has gone.
///
/// One message in message mode and possibly several in byte mode, which is
/// why the caller splits on the terminator rather than treating this as one
/// line. See [`write_line`].
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
    // Lossy, deliberately. The far end is not trusted to send valid UTF-8,
    // and a replacement character makes a line the codec refuses - which is
    // the correct outcome and not a reason to drop the connection.
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Starts a reader and a writer over a connected pair of handles.
///
/// Shared by both ends, because once the pipes are connected there is no
/// server and no client - there are two processes with a handle each way.
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
            // Owned by this thread, and closed when it ends - which is what
            // makes the far end's read return.
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
                    // The frame loop may be parked, so it has to be told
                    // the other end went as well as that a message
                    // arrived - otherwise a window whose panel has exited
                    // sits there offering to install an update until
                    // somebody moves the mouse.
                    wake();
                    return;
                };
                // Split on the terminator rather than trusting the read to
                // have returned exactly one message. See `write_line`.
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

/// Creates one instance of one direction.
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

/// Blocks until a client joins, or reports that it never will.
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
    /// Set by the accept thread when a window connects, cleared when it
    /// goes. An atomic rather than the mutex, because the frame loop asks
    /// every frame and must never wait on a thread that is mid-accept.
    connected: Arc<AtomicBool>,
}

struct Ends {
    out: Sender<String>,
    inbox: Receiver<String>,
}

impl Server {
    /// Starts listening. `wake` is called whenever something arrives or the
    /// window goes.
    ///
    /// `None` where the names could not be created, which means another
    /// copy of this program already holds them. The single-instance check
    /// upstream should have caught that; if it did not, running without a
    /// settings link is a far better outcome than refusing to start.
    pub fn listen(wake: impl Fn() + Clone + Send + 'static) -> Option<Self> {
        Self::listen_on(&super::pipe_name(), wake)
    }

    /// The body of [`Self::listen`] with the name as a parameter.
    ///
    /// Split only so the tests can each use a name of their own, exactly as
    /// `single::claim_named` is. They share a process and the runner runs
    /// them in parallel, and these pipes accept one instance at a time - so
    /// two tests on one name would be one test asserting that the other is
    /// not running.
    pub fn listen_on(name: &str, wake: impl Fn() + Clone + Send + 'static) -> Option<Self> {
        // The first instances are created here rather than on the thread,
        // so both names exist the moment this returns. Otherwise a
        // `--settings` launched in the same breath as the panel would race
        // the accept thread and be told there is no panel.
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

    /// Best effort, like everything here.
    pub fn send(&self, msg: &ToSettings) {
        let Ok(guard) = self.inner.lock() else {
            return;
        };
        if let Some(ends) = guard.as_ref() {
            let _ = ends.out.send(msg.encode());
        }
    }

    /// Whatever the window has said since the last call.
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

/// Accepts one window, pumps it until it goes, and waits for the next.
///
/// One at a time, deliberately. Two settings windows editing one file is a
/// state with no sensible resolution, and a pipe that accepts one instance
/// is a cheaper way to prevent it than a second single-instance mutex.
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

        // In the order the client opens them, or each side would be waiting
        // for the other's half.
        if !accept(&out_h) || !accept(&in_h) {
            continue;
        }

        let alive = Arc::new(AtomicBool::new(true));
        let (out, inbox) = pump(out_h, in_h, Arc::clone(&alive), wake.clone());
        if let Ok(mut guard) = inner.lock() {
            *guard = Some(Ends { out, inbox });
        }
        connected.store(true, Ordering::Relaxed);

        // Held until the reader says the window has gone. A poll rather
        // than a join because the pump threads own the handles and outlive
        // this iteration by a moment; the flag is the thing that matters.
        while alive.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        connected.store(false, Ordering::Relaxed);
        if let Ok(mut guard) = inner.lock() {
            // Dropping the sender ends the writer thread, which closes the
            // outbound handle. Both instances are released and the next
            // turn of this loop makes a fresh pair.
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
    /// Connects to a running panel, or `None` if there is not one.
    ///
    /// `None` is an ordinary answer and not a failure: `files --settings`
    /// from the Start menu with nothing else running is a supported way to
    /// use this program.
    pub fn connect(wake: impl Fn() + Send + 'static) -> Option<Self> {
        Self::connect_to(&super::pipe_name(), wake)
    }

    /// The body of [`Self::connect`] with the name as a parameter. See
    /// [`Server::listen_on`].
    pub fn connect_to(name: &str, wake: impl Fn() + Send + 'static) -> Option<Self> {
        let (down, up) = ends_of(name);
        // Down first, because that is the order the server accepts in.
        //
        // `FILE_WRITE_ATTRIBUTES` on a handle this end only reads from, and
        // it is load-bearing: `SetNamedPipeHandleState` below needs it, and
        // without it the call fails silently and the handle stays in byte
        // mode. See `write_line` for what that costs.
        let inbound = open(&down, GENERIC_READ | FILE_WRITE_ATTRIBUTES)?;
        let outbound = open(&up, GENERIC_WRITE)?;

        // Message mode on the reading end too, or a read returns whatever
        // happens to be in the buffer rather than one message.
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

    /// Waits for a condition, or gives up. A test that waits for ever on a
    /// thing that never happens is not a test.
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

    /// The whole thing, over real pipes in one process: a server, a client,
    /// and a message each way.
    ///
    /// One process rather than two because what is being checked is the
    /// transport and not the program - and a test that spawns `files.exe`
    /// is a test that depends on a build, a path and a window appearing.
    ///
    /// This is the test that caught the deadlock the module note is about,
    /// and would catch it again: with one duplex handle the write below
    /// never leaves the writer thread.
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

    /// Several messages arrive in the order they were sent, which a
    /// one-message test cannot show.
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

    /// Messages sent faster than they are read still arrive as messages.
    ///
    /// This is the case that found the byte-mode bug, and it found it only
    /// because the sender was faster than the reader: with anything slowing
    /// the writer down - a print, a debugger, a breakpoint - the reads
    /// happen one per write and everything looks correct. A burst is the
    /// only shape that shows it.
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

    /// A window closing is a fact the panel sees, which is the property the
    /// whole choice of transport was made for.
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

    /// And a window started with no panel running is told so, at once,
    /// which is what makes `--settings` a real shortcut rather than
    /// something that hangs when it is used the obvious way.
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
