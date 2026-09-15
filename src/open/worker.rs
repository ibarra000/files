//! The thread that opens things.
//!
//! `open_async` used to spawn a thread per keypress. That was tolerable when
//! the work was a single `spawn` call, and is not once Enter means reading and
//! merging tens of PDFs off an SMB share. It also contradicted the invariant
//! `app::actors` opens with: *"The thread population is fixed for the life of
//! the process ... Nothing is spawned per keystroke, so thread growth is
//! impossible by construction rather than by discipline."*
//!
//! # A queue, not a latest-wins slot
//!
//! The search, verify and prefetch workers are fed by
//! [`crate::util::latest_slot::LatestSlot`], which discards a pending request
//! when a newer one arrives. That is right for a search - only the newest
//! query matters - and wrong here: every Enter is a thing the user asked for,
//! and dropping one looks exactly like the program ignoring them. A bounded
//! channel keeps them all, in order.
//!
//! # Panics are reported
//!
//! This thread parses documents that came off a share and are not trusted to
//! be well formed. `panic = "unwind"` is deliberate for this crate, so a
//! malformed file must not be able to take the worker down silently - it is
//! caught and surfaced as `ActorDied`, exactly as the search and verify
//! workers do.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};

use super::{OpenContext, OpenRequest};
use crate::app::event::{AppEvent, Events, OpenMsg};
use crate::search::worker::Backend;

/// Queued opens.
///
/// Deep enough that a leaning-on-Enter burst is never dropped, shallow enough
/// that it cannot become a queue of work nobody is waiting for any more.
const QUEUE_DEPTH: usize = 16;

/// How long a merged document is kept before it is collected.
///
/// The file cannot be deleted after it is opened - the viewer is holding it -
/// so it is swept on a later run. A week means reopening last week's drawing
/// is still instant, while the directory cannot grow without bound.
pub const CACHE_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The open worker.
pub struct Opener {
    tx: Sender<OpenRequest>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// What the last `shutdown` concluded, so a second call cannot upgrade an
    /// abandoned thread to a clean one.
    stopped_cleanly: bool,
}

impl Opener {
    /// Queues an open.
    ///
    /// Never blocks the UI thread: a full queue means something is badly
    /// wedged, and dropping the newest request is better than freezing the
    /// interface behind it.
    pub fn request(&self, request: OpenRequest, events: &Events) {
        let detail = match self.tx.try_send(request) {
            Ok(()) => return,
            Err(TrySendError::Full(r)) => (r.path, "still opening the last one"),
            // The worker is gone - it panicked on a malformed document, or the
            // program is shutting down. Dropping this silently was the exact
            // failure this module was written to remove: after one crash every
            // later Enter did nothing at all, with nothing on screen to say so.
            Err(TrySendError::Disconnected(r)) => {
                (r.path, "the opener stopped; restart to open files again")
            }
        };
        // `try_send`, not `send`, and the distinction is a hang.
        //
        // This runs on the interface's own thread, and the interface is the
        // only thing draining the event channel. A blocking send closes a
        // cycle: the channel is full, so the send waits; the send is on the
        // thread that would have drained it, so it waits for ever. It needs a
        // full opener queue and a full event channel at the same moment, which
        // is rare - and a wedged program with nothing on screen to explain it,
        // which is not a thing to leave lying about because it is rare.
        //
        // The cost of the other choice is losing this one message when the
        // channel is already holding two hundred and fifty-six others, at
        // which point the interface is about to redraw anyway. That is a
        // report delayed into irrelevance, against a program that stops.
        let _ = events.try_send(AppEvent::Open(OpenMsg::Failed {
            path: detail.0,
            detail: detail.1.into(),
        }));
    }

    /// Stops the worker, waiting up to `budget`.
    ///
    /// Returns false when it had to be abandoned - expected when it is blocked
    /// reading from a share that has stopped answering. Merged documents are
    /// written temp-then-rename, so nothing is left half-written.
    pub fn shutdown(&mut self, budget: Duration) -> bool {
        let (tx, _) = bounded(0);
        drop(std::mem::replace(&mut self.tx, tx));

        let Some(handle) = self.handle.take() else {
            // Taken already, which only happens after a shutdown that timed
            // out and abandoned the thread. Reporting `true` here would let a
            // second call claim a clean stop for a thread still blocked in an
            // SMB read.
            return self.stopped_cleanly;
        };
        let deadline = Instant::now() + budget;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                self.stopped_cleanly = false;
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.stopped_cleanly = handle.join().is_ok();
        self.stopped_cleanly
    }
}

/// Starts the open worker.
pub fn spawn(backend: Arc<Backend>, events: Events) -> std::io::Result<Opener> {
    let (tx, rx) = bounded::<OpenRequest>(QUEUE_DEPTH);

    let handle = std::thread::Builder::new()
        .name("files-open".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&backend, &rx, &events);
            }));
            if let Err(payload) = result {
                let _ = events.send(AppEvent::ActorDied {
                    actor: "open",
                    detail: crate::util::once::panic_detail(&payload),
                });
            }
        })?;

    Ok(Opener {
        tx,
        handle: Some(handle),
        stopped_cleanly: true,
    })
}

fn run(backend: &Backend, rx: &Receiver<OpenRequest>, events: &Events) {
    while let Ok(request) = rx.recv() {
        let msg = serve(backend, &request);
        if events.send(AppEvent::Open(msg)).is_err() {
            return;
        }
    }
}

fn serve(backend: &Backend, request: &OpenRequest) -> OpenMsg {
    // Only the PDF route needs a listing, and a failure to get one is not a
    // failure to open: it degrades to the single selected file.
    let snapshot = match super::route_of(request.viewer, &request.path) {
        super::Route::Document => backend.snapshot_for(std::path::Path::new(request.path.as_ref())),
        // avwin is handed one file, so it has no use for a listing.
        super::Route::Avwin => None,
    };

    let cx = OpenContext {
        snapshot: snapshot.as_deref(),
        cache_dir: backend.settings.cache_dir.as_deref(),
        pdf_viewer: backend.settings.pdf_viewer.as_deref(),
    };

    match super::open(request, &cx) {
        Ok(opened) => OpenMsg::Launched {
            path: opened.path,
            pages: opened.pages,
            skipped: opened
                .skipped
                .iter()
                .map(super::pdf::Skipped::describe)
                .collect(),
            truncated: opened.truncated,
        },
        Err(err) => OpenMsg::Failed {
            path: Arc::clone(&request.path),
            detail: err.detail(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Settings, ViewerKind};
    use crate::index::fake_source::FakeDirSource;
    use crate::index::store::IndexStore;

    fn backend() -> Arc<Backend> {
        Arc::new(Backend {
            settings: Settings {
                persist: false,
                ..Default::default()
            },
            store: Arc::new(IndexStore::default()),
            source: Arc::new(FakeDirSource::new()),
            live: Vec::new(),
        })
    }

    fn request(path: &str) -> OpenRequest {
        OpenRequest {
            path: Arc::from(path),
            query: "11-D-0704".into(),
            viewer: ViewerKind::Avwin,
        }
    }

    /// Reporting a failure must never park the caller.
    ///
    /// `request` runs on the interface's own thread, and the interface is the
    /// only thing draining the event channel. A *blocking* send from here
    /// closes a cycle: the channel is full, so the send waits; the send is on
    /// the thread that would have drained it, so it waits forever. It takes a
    /// full opener queue and a full event channel at the same moment, which is
    /// rare - and a hang with no message and no way out, which is not a thing
    /// to leave lying about because it is rare.
    ///
    /// Reproduced here with a rendezvous channel nobody is receiving on, which
    /// is "full" by construction, and a shut-down worker, which is the
    /// deterministic half of the pair.
    #[test]
    fn reporting_a_failure_never_blocks_the_caller() {
        let (tx, _rx) = bounded::<AppEvent>(8);
        let tx = Events::headless(tx);
        let mut opener = spawn(backend(), tx).expect("the opener starts");
        opener.shutdown(Duration::from_millis(250));

        // Nobody is receiving, and there is no capacity, so `send` would park
        // here for the life of the process.
        let (blocked, _held) = bounded::<AppEvent>(0);
        let blocked = Events::headless(blocked);

        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        std::thread::spawn(move || {
            opener.request(request(r"V:\a.pdf"), &blocked);
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        for _ in 0..200 {
            if done.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("`request` was still blocked a second later; the interface would be wedged");
    }

    /// Silence is the failure this whole module exists to prevent: the user
    /// pressed a key, so something has to come back.
    #[test]
    fn the_worker_always_reports_back() {
        let (tx, rx) = bounded(8);
        let tx = Events::headless(tx);
        let mut opener = spawn(backend(), tx.clone()).unwrap();
        opener.request(request(r"C:\definitely-not-here-4a91\nope.pdf"), &tx);

        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(AppEvent::Open(OpenMsg::Failed { detail, .. })) => {
                assert!(detail.contains("no longer exists"), "{detail}");
            }
            other => panic!("expected a failure report, got {other:?}"),
        }
        opener.shutdown(Duration::from_millis(500));
    }

    /// Unlike a search, two opens are two things the user asked for.
    #[test]
    fn every_queued_open_is_serviced_rather_than_superseded() {
        let (tx, rx) = bounded(64);
        let tx = Events::headless(tx);
        let mut opener = spawn(backend(), tx.clone()).unwrap();
        for i in 0..4 {
            opener.request(
                request(&format!(r"C:\definitely-not-here-4a91\{i}.pdf")),
                &tx,
            );
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = 0;
        while Instant::now() < deadline && seen < 4 {
            if let Ok(AppEvent::Open(_)) = rx.recv_timeout(Duration::from_millis(50)) {
                seen += 1;
            }
        }
        assert_eq!(seen, 4, "no open may be dropped");
        opener.shutdown(Duration::from_millis(500));
    }

    /// After the worker is gone every later Enter used to vanish without a
    /// trace, which is the precise failure this module was written to remove.
    #[test]
    fn an_open_after_the_worker_has_stopped_is_still_reported() {
        let (tx, rx) = bounded(8);
        let tx = Events::headless(tx);
        let mut opener = spawn(backend(), tx.clone()).unwrap();
        assert!(opener.shutdown(Duration::from_millis(500)));

        opener.request(
            request(
                r"C:\definitely-not-here-4a91
ope.pdf",
            ),
            &tx,
        );
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(AppEvent::Open(OpenMsg::Failed { detail, .. })) => {
                assert!(detail.contains("stopped"), "{detail}");
            }
            other => panic!("expected a failure report, got {other:?}"),
        }
    }

    /// A shutdown that gave up must keep saying so; the second call has no
    /// handle left and must not read that as success.
    #[test]
    fn a_second_shutdown_does_not_upgrade_an_abandoned_worker() {
        let (tx, _rx) = bounded(8);
        let tx = Events::headless(tx);
        let mut opener = spawn(backend(), tx).unwrap();
        assert!(opener.shutdown(Duration::from_millis(500)));
        assert!(opener.shutdown(Duration::from_millis(500)));
    }

    #[test]
    fn shutdown_stops_an_idle_worker_promptly() {
        let (tx, _rx) = bounded(8);
        let tx = Events::headless(tx);
        let mut opener = spawn(backend(), tx).unwrap();
        let started = Instant::now();
        assert!(opener.shutdown(Duration::from_millis(500)));
        assert!(started.elapsed() < Duration::from_millis(400));
    }
}
