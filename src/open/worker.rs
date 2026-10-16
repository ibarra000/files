//! The long-lived thread that performs opens.
//!
//! One thread for the life of the process, per the fixed thread population
//! required by `app::actors`.
//!
//! Requests go through a bounded FIFO channel rather than
//! [`crate::util::latest_slot::LatestSlot`]: unlike searches, every open was
//! explicitly requested and none may be superseded.
//!
//! The thread parses untrusted files, so panics are caught and reported as
//! `ActorDied`, as for the search and verify workers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};

use super::{OpenContext, OpenRequest};
use crate::app::event::{AppEvent, Events, OpenMsg};
use crate::search::worker::Backend;

/// Maximum queued opens: enough to absorb a burst of Enter presses without
/// building a backlog of stale work.
const QUEUE_DEPTH: usize = 16;

/// How long a merged document is kept before it is collected on a later run.
pub const CACHE_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The open worker.
pub struct Opener {
    tx: Sender<OpenRequest>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// Result of the last `shutdown`, so a repeat call cannot report an
    /// abandoned thread as cleanly stopped.
    stopped_cleanly: bool,
}

impl Opener {
    /// Queues an open without blocking; a full queue drops the request and
    /// reports it.
    pub fn request(&self, request: OpenRequest, events: &Events) {
        let detail = match self.tx.try_send(request) {
            Ok(()) => return,
            Err(TrySendError::Full(r)) => (r.path, "still opening the previous file"),
            // The worker panicked or is shutting down; report it so later
            // opens do not fail silently.
            Err(TrySendError::Disconnected(r)) => {
                (r.path, "the opener stopped; restart to open files again")
            }
        };
        // `try_send`: this runs on the UI thread, the only consumer of the
        // event channel, so a blocking send on a full channel would deadlock.
        // Losing this report when the channel is full is acceptable.
        let _ = events.try_send(AppEvent::Open(OpenMsg::Failed {
            path: detail.0,
            detail: detail.1.into(),
        }));
    }

    /// Stops the worker, waiting up to `budget`.
    ///
    /// Returns false if the thread was abandoned, for example while blocked on
    /// an unresponsive share. Merged documents are written temp-then-rename,
    /// so nothing is left half-written.
    pub fn shutdown(&mut self, budget: Duration) -> bool {
        let (tx, _) = bounded(0);
        drop(std::mem::replace(&mut self.tx, tx));

        let Some(handle) = self.handle.take() else {
            // Already shut down; repeat the earlier result.
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
    // Only the document route needs a listing; without one it falls back to
    // the selected file.
    let snapshot = match super::route_of(request.viewer, &request.path, &request.viewers) {
        super::Route::Document => backend.snapshot_for(std::path::Path::new(request.path.as_ref())),
        super::Route::App(_) | super::Route::Shell => None,
    };

    let cx = OpenContext {
        snapshot: snapshot.as_deref(),
        cache_dir: backend.settings.cache_dir.as_deref(),
        pdf_viewer: backend.settings.pdf_viewer.as_deref(),
        read_only: backend.settings.pdf_read_only,
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
                ..Settings::fixture()
            },
            store: Arc::new(IndexStore::fixture()),
            source: Arc::new(FakeDirSource::new()),
            live: Vec::new(),
        })
    }

    fn request(path: &str) -> OpenRequest {
        OpenRequest {
            path: Arc::from(path),
            query: "11-D-0704".into(),
            viewer: ViewerKind::App(0),
            viewers: Arc::new(crate::config::Viewers::new(vec![
                crate::config::ViewerApp::new("avwin", "avwin.exe", &["dwg"]),
            ])),
        }
    }

    /// Uses a stopped worker and a zero-capacity event channel with no
    /// receiver, on which a blocking send would hang.
    #[test]
    fn reporting_a_failure_never_blocks_the_caller() {
        let (tx, _rx) = bounded::<AppEvent>(8);
        let tx = Events::headless(tx);
        let mut opener = spawn(backend(), tx).expect("the opener starts");
        opener.shutdown(Duration::from_millis(250));

        // No receiver and no capacity: `send` would block forever.
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

    /// A repeat shutdown returns the first call's result.
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
