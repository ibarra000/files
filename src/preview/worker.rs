//! The thread that asks the share what a file is.
//!
//! # A latest slot, not a queue
//!
//! The opener uses a bounded queue because every Enter matters: two codes
//! opened in quick succession are two documents somebody wants, and dropping
//! one would lose work they asked for. This is the opposite case. Arrowing down
//! a list of three hundred results asks about three hundred files, and
//! two hundred and ninety-nine of those answers are for a row the pointer has
//! already left. A queue would spend a round trip on each of them and deliver
//! the one that matters last.
//!
//! So the newest request replaces the pending one, exactly as the search and
//! verify workers do - and, as [`crate::util::latest_slot`] argues, the final
//! request is still always serviced, because a `put` that lands during a run is
//! picked up by the very next `take_blocking`.
//!
//! # Why there is no cancellation of the call in flight
//!
//! There is a [`CancelToken`], and it covers the page sweep - which is a scan
//! of memory this process owns and can be abandoned between chunks. It does not
//! cover `metadata`, because a blocked SMB call cannot be cancelled: the
//! syscall returns when the server answers or when TCP gives up, and nothing
//! this side can shorten that. The answer is allowed to arrive late and is then
//! dropped by the state machine for naming a file that is no longer the target.
//! Which is the same bargain `doctor` makes with its probe timeout, arrived at
//! from the other end.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use crate::app::event::{AppEvent, Events, PreviewMsg};
use crate::search::worker::Backend;
use crate::util::cancel::CancelToken;
use crate::util::latest_slot::LatestSlot;

use super::{Preview, Request};

/// The handle the main loop keeps.
pub struct Previewer {
    slot: Arc<LatestSlot<Request>>,
    /// Raised when a request is queued, so a sweep already running can notice
    /// that its answer is no longer wanted.
    ///
    /// The slot alone cannot say this: it holds the pending request, and asking
    /// it would mean a lock acquisition per chunk of a sweep over 1.3 million
    /// entries. A relaxed atomic read is what a cancellation check has to cost.
    superseded: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Previewer {
    /// Asks about a file, displacing any request not yet started and
    /// abandoning any sweep already under way.
    ///
    /// The flag is raised before the request is stored, never after: the other
    /// order lets the worker take the new request and clear the flag before
    /// this thread raises it, which would cancel the sweep that was just asked
    /// for rather than the one being replaced.
    pub fn request(&self, request: Request) {
        self.superseded.store(true, Ordering::Relaxed);
        self.slot.put(request);
    }

    /// Stops the thread.
    ///
    /// Unbounded, unlike the opener's budgeted shutdown, and for a reason that
    /// is only true here: the worst case is one `metadata` call, which either
    /// returns or fails when the connection does. There is no assembly of a
    /// three-hundred-page document to wait out.
    pub fn shutdown(&mut self) {
        self.slot.close();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Previewer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Starts the worker.
pub fn spawn(backend: Arc<Backend>, events: Events) -> std::io::Result<Previewer> {
    let slot = Arc::new(LatestSlot::<Request>::new());
    let superseded = Arc::new(AtomicBool::new(false));
    let worker = Arc::clone(&slot);
    let worker_flag = Arc::clone(&superseded);

    let handle = std::thread::Builder::new()
        .name("files-preview".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&backend, &worker, &worker_flag, &events);
            }));
            if let Err(payload) = result {
                let _ = events.send(AppEvent::ActorDied {
                    actor: "preview",
                    detail: crate::util::once::panic_detail(&payload),
                });
            }
        })?;

    Ok(Previewer {
        slot,
        superseded,
        handle: Some(handle),
    })
}

fn run(
    backend: &Backend,
    slot: &LatestSlot<Request>,
    superseded: &Arc<AtomicBool>,
    events: &Events,
) {
    while let Some(request) = slot.take_blocking() {
        // Cleared for the request just taken, so the flag means "something
        // newer than this arrived" for the whole of the sweep below. A request
        // that lands in the gap between the take and this line costs one
        // completed sweep nobody reads; it is not lost, because it is still
        // sitting in the slot for the next iteration.
        superseded.store(false, Ordering::Relaxed);
        let preview = describe(backend, &request, superseded);
        // `send` rather than `try_send`: this is a worker thread, so blocking
        // on a full channel is back-pressure rather than a stall, and dropping
        // the answer would leave the pane showing the previous file's facts
        // under this file's name.
        if events
            .send(AppEvent::Preview(PreviewMsg::Ready(Arc::new(preview))))
            .is_err()
        {
            return;
        }
    }
}

/// Everything one request gathers.
///
/// `superseded` is read only to notice that a newer request has landed: the
/// page sweep runs over a snapshot of up to 1.3 million entries, and finishing
/// one for a row the pointer left four rows ago is the cost this cancellation
/// exists to refuse.
fn describe(backend: &Backend, request: &Request, superseded: &Arc<AtomicBool>) -> Preview {
    let mut facts = super::facts_of(&request.path);
    let (share, folder) = super::locate(&backend.settings.routes, &request.path);
    facts.share = share;
    facts.folder = folder;

    let cancel = CancelToken::from_flag(Arc::clone(superseded));

    let pages = backend
        .snapshot_for(std::path::Path::new(&*request.path))
        .and_then(|snap| super::pages_of(&snap, &request.path, &request.query, &cancel));

    Preview {
        path: Arc::clone(&request.path),
        name: Arc::clone(&request.name),
        facts,
        pages,
    }
}
