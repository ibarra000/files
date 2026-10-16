//! Event and command types.
//!
//! Inputs arrive on one channel as [`AppEvent`]; side effects leave
//! [`crate::app::state`] as [`Cmd`]. Keeping effects out of the transition
//! makes the interaction model testable without a window or network drives.

use std::sync::Arc;

use crossbeam_channel::{SendError, Sender, TrySendError};
use std::time::Duration;

use crate::app::key::KeyEvent;
use smallvec::SmallVec;

use crate::index::errors::EnumError;
use crate::index::store::IndexStatus;
use crate::open::OpenRequest;
use crate::paths::MappingId;
use crate::search::matcher::{QueryReject, SearchOutcome};
use crate::search::query::Query;
use crate::search::verify::VerifyOutcome;

/// An input to the state machine.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// The configuration file was rewritten; carries its new contents.
    ///
    /// The settings window and the panel share only the file, so the whole
    /// `Settings` is sent. Boxed because it is by far the largest variant.
    Adopt(Box<crate::config::Settings>),
    Key(KeyEvent),
    /// Pointer input, already resolved against the drawn layout. See
    /// [`crate::app::state::pointer`].
    Intent(crate::app::state::pointer::Intent),
    Paste(String),
    /// The window gained or lost keyboard focus.
    ///
    /// Only `false` while the panel is up matters; it dismisses the panel. It
    /// comes from the toolkit because the hotkey thread does not see clicks on
    /// other windows. `gui::Shell::ui` drops it while an auxiliary window such
    /// as settings is open, since that window takes focus from the panel.
    WindowFocus(bool),
    Search(SearchMsg),
    Verify(VerifyMsg),
    /// One live share answered. Sent once per share, not once per search.
    Live(LiveMsg),
    Index(IndexMsg),
    Open(OpenMsg),
    Clipboard(ClipboardMsg),
    /// The overlay hotkey did something. Sent only by the hotkey thread.
    Hotkey(HotkeyMsg),
    /// Result of checking the update folder.
    Update(UpdateMsg),
    /// A worker thread panicked. Surfaced so a spinner does not run forever.
    ActorDied {
        actor: &'static str,
        detail: String,
    },
    /// Synthesised by the main loop when a deadline expires; never sent over
    /// the channel.
    Tick,
    Shutdown,
}

/// A report from the hotkey thread, after the fact.
///
/// The hotkey thread is the only one allowed to touch the window, so it owns
/// whether the overlay is up and the compact layout follows these reports.
/// The state must not keep a second, independent flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyMsg {
    /// The window has been snapped and focused. Switch to the compact layout.
    Summoned,
    /// The window has been put back and minimised. Return to the full layout.
    Dismissed,
    /// The chord was registered by this process.
    ///
    /// Sent once at startup and recorded for diagnostics. `RegisterHotKey` is
    /// per-thread, so re-registering to probe would fail against this process
    /// itself. See [`crate::hotkey::Probe`].
    Claimed,
    /// The hotkey, or the window behind it, is not available on this machine.
    ///
    /// Sent at most once. The hotkey still toggles the compact layout, but the
    /// window does not move.
    Unavailable { reason: String },
}

#[derive(Debug, Clone)]
pub struct SearchMsg {
    pub epoch: u64,
    pub query: Query,
    pub elapsed: Duration,
    pub result: Result<SearchOutcome, QueryReject>,
}

/// One live share's answer to one query.
#[derive(Debug, Clone)]
pub struct LiveMsg {
    pub epoch: u64,
    pub query: Query,
    /// Which share answered; several may be configured.
    pub mapping: MappingId,
    pub elapsed: Duration,
    /// Boxed to keep `AppEvent`, whose size every event pays, small.
    pub outcome: Box<crate::search::live::LiveOutcome>,
}

#[derive(Debug, Clone)]
pub struct VerifyMsg {
    pub epoch: u64,
    pub query: Query,
    pub elapsed: Duration,
    pub outcome: VerifyOutcome,
}

#[derive(Debug, Clone)]
pub enum IndexMsg {
    /// A fresh view of one mapping's index status, published after every
    /// change. Keyed by mapping because there is one actor per share.
    Status {
        id: MappingId,
        status: Arc<IndexStatus>,
    },
    /// A new snapshot is installed; any displayed result should be recomputed.
    SnapshotChanged,
    /// Reported by an explicit refresh, so the user learns what happened.
    RefreshReport {
        /// Which share was refreshed.
        id: MappingId,
        entries: usize,
        elapsed: Duration,
        error: Option<EnumError>,
    },
}

#[derive(Debug, Clone)]
pub enum ClipboardMsg {
    Copied { chars: usize },
    Read { text: Arc<str> },
    Failed { detail: String },
}

#[derive(Debug, Clone)]
pub enum OpenMsg {
    Launched {
        /// The path handed to the viewer; for a merged document this is a
        /// generated file, not one the user can see.
        path: Arc<str>,
        /// Pages handed over. One for a single file.
        pages: usize,
        /// Pages that could not be used, already described. Non-empty means
        /// the document is incomplete and the user must be told.
        skipped: Vec<String>,
        /// The match has more pages than the ceiling allows, so the document
        /// is truncated.
        truncated: bool,
    },
    Failed {
        path: Arc<str>,
        detail: String,
    },
    /// The viewer choice reached the configuration file.
    ///
    /// Separate from a launch so a successful open with a failed save can be
    /// reported.
    ViewerSaved {
        /// The configuration spelling that was written.
        name: Arc<str>,
    },
    ViewerSaveFailed {
        detail: String,
    },
    SettingSaveFailed {
        label: &'static str,
        detail: String,
    },
}

/// What the update checker found.
///
/// Sent on every check, including when there is nothing new, so the settings
/// window can show when it last looked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateMsg {
    /// Boxed to keep `AppEvent` small.
    Looked(Box<crate::update::Found>),
}

/// Whether the frame needs redrawing.
///
/// A return value rather than a mutable flag, so it cannot be ignored.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redraw {
    No,
    Yes,
}

impl Redraw {
    pub fn is_yes(self) -> bool {
        self == Self::Yes
    }

    /// `Yes` wins, so merging a burst of events redraws once.
    pub fn or(self, other: Self) -> Self {
        if self.is_yes() || other.is_yes() {
            Self::Yes
        } else {
            Self::No
        }
    }
}

/// A side effect for the caller to execute, never run inside the transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    Search {
        query: Query,
        epoch: u64,
    },
    Verify {
        query: Query,
        epoch: u64,
    },
    /// Ask the live shares. Paced separately from [`Self::Search`], which reads
    /// memory; this is a network round trip.
    Live {
        query: Query,
        epoch: u64,
    },
    RefreshIndex {
        /// Which share to re-read. A full pass over a large share costs many
        /// round trips, so refreshes target one share.
        target: RefreshTarget,
        force: bool,
    },
    Open(OpenRequest),
    /// Write the chosen viewer to the configuration file, preserving its
    /// comments. Emitted only when `Settings::can_save` holds.
    ///
    /// Carries the configuration spelling resolved by the state machine; the
    /// backend's settings copy is from startup and may list other viewers.
    SaveViewer(Arc<str>),
    /// Put text on the system clipboard.
    Copy(String),
    /// Show a native message box when the panel is already gone.
    ///
    /// Raised only for warnings or worse; see [`crate::notify`].
    Announce {
        title: String,
        detail: String,
    },
    /// Open Explorer with this file selected (`explorer.exe /select,"<path>"`).
    ///
    /// Separate from [`Self::Open`] because it opens the folder, not the file,
    /// and fails and reports differently.
    Reveal(Arc<str>),
    /// Fetch the clipboard, to be inserted at the caret.
    ReadClipboard,
    /// Store the history list.
    ///
    /// Writes the whole list so an interrupted process cannot leave a
    /// half-applied append.
    SaveHistory(Arc<Vec<String>>),
    /// Dismiss the overlay: Escape while compact, or a result just opened.
    ///
    /// A request to the hotkey thread, which owns the window and the overlay
    /// state.
    DismissOverlay,
    /// Show the settings window, or hide it if it is already up.
    ///
    /// A toggle because only the shell knows whether the window is open.
    ToggleSettings,
    Quit,
}

/// The share a refresh is for.
///
/// A newtype so it cannot be swapped with the `MappingId` it is compared
/// against in `index::actor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshTarget(pub MappingId);

impl RefreshTarget {
    pub fn wants(self, id: MappingId) -> bool {
        self.0 == id
    }
}

pub type CmdList = SmallVec<[Cmd; 2]>;

/// Wakes the UI thread.
///
/// The toolkit sleeps until told otherwise and `next_deadline` returns `None`
/// when idle, so a worker's event would otherwise go unseen.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// A sender that also wakes the UI thread.
///
/// Not `Deref`, so the inner sender cannot be used without waking.
#[derive(Clone)]
pub struct Events {
    tx: Sender<AppEvent>,
    wake: Wake,
}

impl Events {
    pub fn new(tx: Sender<AppEvent>, wake: Wake) -> Self {
        Self { tx, wake }
    }

    /// A sender that wakes nothing, for tests and headless drivers.
    pub fn headless(tx: Sender<AppEvent>) -> Self {
        Self {
            tx,
            wake: Arc::new(|| {}),
        }
    }

    /// Blocks if the channel is full.
    ///
    /// For results a worker would otherwise lose. Never call it from the thread
    /// that drains the channel; see `open::worker::Opener::request`.
    pub fn send(&self, event: AppEvent) -> Result<(), SendError<AppEvent>> {
        self.tx.send(event)?;
        (self.wake)();
        Ok(())
    }

    /// Drops the event if the channel is full.
    ///
    /// For reports whose state is readable elsewhere, such as index progress,
    /// and for any send on the UI thread.
    pub fn try_send(&self, event: AppEvent) -> Result<(), TrySendError<AppEvent>> {
        let sent = self.tx.try_send(event);
        if sent.is_ok() {
            (self.wake)();
        }
        sent
    }
}

impl std::fmt::Debug for Events {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Events").finish_non_exhaustive()
    }
}

/// What a state transition produced.
#[derive(Debug, Clone)]
pub struct Response {
    pub redraw: Redraw,
    pub cmds: CmdList,
}

impl Response {
    pub fn none() -> Self {
        Self {
            redraw: Redraw::No,
            cmds: CmdList::new(),
        }
    }

    pub fn redraw() -> Self {
        Self {
            redraw: Redraw::Yes,
            cmds: CmdList::new(),
        }
    }

    pub fn with(mut self, cmd: Cmd) -> Self {
        self.cmds.push(cmd);
        self
    }

    /// Folds in a redraw obligation produced beside this response, such as a
    /// hover cleared while the action itself needs no frame.
    pub fn with_redraw(mut self, redraw: Redraw) -> Self {
        self.redraw = self.redraw.or(redraw);
        self
    }

    /// Folds another response in, for burst coalescing.
    pub fn merge(&mut self, other: Response) {
        self.redraw = self.redraw.or(other.redraw);
        self.cmds.extend(other.cmds);
    }
}

impl Default for Response {
    fn default() -> Self {
        Self::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_send_wakes_the_drawer() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let woken = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&woken);
        let (tx, rx) = crossbeam_channel::bounded(4);
        let events = Events::new(
            tx,
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );

        events.send(AppEvent::Tick).unwrap();
        events.try_send(AppEvent::Tick).unwrap();
        assert_eq!(woken.load(Ordering::SeqCst), 2);
        assert_eq!(rx.len(), 2);
    }

    /// A `try_send` that dropped its event does not wake the UI.
    #[test]
    fn a_dropped_event_does_not_wake_anybody() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let woken = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&woken);
        let (tx, _rx) = crossbeam_channel::bounded(1);
        let events = Events::new(
            tx,
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );

        events.try_send(AppEvent::Tick).expect("the first one fits");
        assert!(
            events.try_send(AppEvent::Tick).is_err(),
            "the second does not"
        );
        assert_eq!(woken.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_headless_sender_still_delivers() {
        let (tx, rx) = crossbeam_channel::bounded(2);
        Events::headless(tx).send(AppEvent::Tick).unwrap();
        assert_eq!(rx.len(), 1);
    }

    #[test]
    fn redraw_is_sticky_when_merged() {
        assert_eq!(Redraw::No.or(Redraw::No), Redraw::No);
        assert_eq!(Redraw::No.or(Redraw::Yes), Redraw::Yes);
        assert_eq!(Redraw::Yes.or(Redraw::No), Redraw::Yes);
    }

    #[test]
    fn merging_accumulates_commands_and_promotes_redraw() {
        let mut a = Response::none().with(Cmd::Quit);
        a.merge(Response::redraw().with(Cmd::RefreshIndex {
            target: RefreshTarget(MappingId(0)),
            force: true,
        }));
        assert_eq!(a.redraw, Redraw::Yes);
        assert_eq!(a.cmds.len(), 2);
    }

    #[test]
    fn an_empty_response_asks_for_nothing() {
        let r = Response::none();
        assert_eq!(r.redraw, Redraw::No);
        assert!(r.cmds.is_empty());
    }
}
