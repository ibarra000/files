//! The single event and command vocabulary.
//!
//! Everything the program can react to arrives on one channel as an
//! [`AppEvent`], and everything it decides to do leaves [`crate::app::state`]
//! as a [`Cmd`]. Keeping side effects out of the state transition is what
//! makes the interaction model testable on a machine with no terminal and no
//! network drives.

use std::sync::Arc;

use crossbeam_channel::{SendError, Sender, TrySendError};
use std::time::Duration;

use crate::app::key::KeyEvent;
use smallvec::SmallVec;

use crate::config::ViewerKind;
use crate::index::errors::EnumError;
use crate::index::store::IndexStatus;
use crate::open::OpenRequest;
use crate::paths::MappingId;
use crate::search::matcher::{QueryReject, SearchOutcome};
use crate::search::query::Query;
use crate::search::verify::VerifyOutcome;

/// Something happened.
#[derive(Debug, Clone)]
pub enum AppEvent {
    Key(KeyEvent),
    /// What the pointer meant, already resolved against the layout that drew
    /// it. See [`crate::app::state::pointer`].
    Intent(crate::app::state::pointer::Intent),
    /// A control in the settings window was moved.
    Setting(crate::app::state::SettingChange),
    /// The alias list was changed in the settings window.
    Aliases(Vec<crate::alias::Alias>),
    /// The drive list was changed in the settings window.
    Drives(Vec<crate::paths::Mapping>),
    Paste(String),
    /// The window gained or lost the keyboard.
    ///
    /// Only ever `false` matters, and only when the panel is up: it is how a
    /// launcher knows to get out of the way. It arrives from the toolkit
    /// rather than from the hotkey thread, because the hotkey thread does
    /// not hear about a click on somebody else's window.
    ///
    /// The shell drops it while an auxiliary window of ours is open - see
    /// `gui::Shell::ui` - because the settings window takes the keyboard
    /// from the panel, and a panel that dismissed itself over that would
    /// close the window the user had just clicked into.
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
    /// What a look at the update folder found.
    Update(UpdateMsg),
    /// A worker thread panicked. Surfaced rather than leaving a spinner up
    /// forever.
    ActorDied {
        actor: &'static str,
        detail: String,
    },
    /// Synthesised locally by the main loop when a deadline expires. Never
    /// sent by anyone, so it cannot be lost or leaked.
    Tick,
    Shutdown,
}

/// What the hotkey thread did, after it did it.
///
/// Note the direction: this is a *report*, never a request. The hotkey thread
/// owns whether the overlay is up, because it is the only thread allowed to
/// touch a window, so the compact layout is downstream of that fact rather
/// than a peer of it. Two flags that could disagree is how "Escape left the
/// terminal 76 cells wide and permanently on top" happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyMsg {
    /// The window has been snapped and focused. Switch to the compact layout.
    Summoned,
    /// The window has been put back and minimised. Return to the full layout.
    Dismissed,
    /// The hotkey, or the window behind it, is not available on this machine.
    ///
    /// Sent at most once. The compact layout still toggles on the hotkey - a
    /// shortcut that switches to a compact view is worth having on its own -
    /// but nothing moves.
    Unavailable { reason: String },
}

#[derive(Debug, Clone)]
pub struct SearchMsg {
    pub epoch: u64,
    pub query: Query,
    pub elapsed: Duration,
    pub result: Result<SearchOutcome, QueryReject>,
}

/// What one live share had to say about one query.
#[derive(Debug, Clone)]
pub struct LiveMsg {
    pub epoch: u64,
    pub query: Query,
    /// Which share answered.
    ///
    /// Carried because several may be configured and each answers for itself.
    /// A single unkeyed field would describe whichever spoke last, which is
    /// the bug `IndexMsg::Status` was re-keyed to fix.
    pub mapping: MappingId,
    pub elapsed: Duration,
    /// Boxed, not inline. `AppEvent` is one enum for every message in the
    /// program and travels through a bounded channel they all share, so its
    /// size is set by its largest variant and paid for by every event -
    /// including the ones carrying nothing at all. One allocation per share
    /// per settled search is not a cost worth measuring against that.
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
    /// change.
    ///
    /// Carries its mapping because there is one actor per share, and a single
    /// unkeyed field simply showed whichever of them published last.
    Status {
        id: MappingId,
        status: Arc<IndexStatus>,
    },
    /// A new snapshot is installed; any displayed result should be recomputed.
    SnapshotChanged,
    /// Reported by an explicit refresh, so the user learns what happened.
    RefreshReport {
        /// Which share was refreshed. F5 fans out to every one of them.
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
        /// What the viewer was actually handed, which for a merged document
        /// is not any of the files the user can see.
        path: Arc<str>,
        /// Pages handed over. One for avwin, so that path needs no special
        /// case downstream.
        pages: usize,
        /// Pages that could not be used, already described. Non-empty means
        /// the document opened but is not complete, which the user has to be
        /// told - silently short pages are the worst outcome available here.
        skipped: Vec<String>,
        /// The code has more pages than the ceiling allows, so the document
        /// stops short of the end.
        truncated: bool,
    },
    Failed {
        path: Arc<str>,
        detail: String,
    },
    /// The viewer choice reached the configuration file.
    ///
    /// Separate from a launch rather than folded into it: conflating the two
    /// would leave "the document opened but the setting did not stick"
    /// impossible to report.
    ViewerSaved {
        viewer: ViewerKind,
    },
    ViewerSaveFailed {
        detail: String,
    },
    /// A setting changed in the window reached the configuration file.
    ///
    /// Carries the label rather than the key, because what the user is owed
    /// is confirmation about the thing they just changed, spelled the way the
    /// form spelled it.
    SettingSaved {
        label: &'static str,
    },
    SettingSaveFailed {
        label: &'static str,
        detail: String,
    },
}

/// What the update checker has to report.
///
/// One variant, because there is one question and it always has an answer -
/// including the dull one. A check that reported only good news would leave
/// the settings window unable to say when it last looked without that reading
/// as though the thread had died.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateMsg {
    /// Boxed, because `AppEvent` is what travels the channel and every
    /// variant pays for the largest. A manifest holds three strings and this
    /// arrives a few times a day; a keystroke holds none and arrives hundreds
    /// of times a minute.
    Looked(Box<crate::update::Found>),
}

/// Whether the frame needs redrawing.
///
/// A return value rather than a mutable flag, so it cannot be forgotten.
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

/// Something to do. Executed by the caller, never inside the state
/// transition.
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
    /// Ask the live shares. Paced separately from [`Self::Search`]: that one
    /// sweeps memory this process owns, this one is a round trip on somebody
    /// else's file server.
    Live {
        query: Query,
        epoch: u64,
    },
    RefreshIndex {
        /// Which share to re-read.
        ///
        /// Carried because "refresh everything" is the expensive answer: a
        /// full pass over the job share is some nine hundred thousand round
        /// trips, and a user who only wanted one share up to date should not
        /// be made to ask for all of them.
        target: RefreshTarget,
        force: bool,
    },
    Open(OpenRequest),
    /// Write the chosen viewer back to the configuration file, preserving
    /// every comment in it. Emitted only when the state machine already knows
    /// the value can stick - see `Settings::can_save`.
    SaveViewer(ViewerKind),
    /// Write a setting changed in the window back to the configuration file.
    ///
    /// Emitted only for a setting `Settings::can_save` has already agreed to,
    /// so this never reaches the disk to report a save the next start would
    /// ignore.
    SaveSetting {
        edit: crate::config::write::Edit,
        /// How the form spells it, for the message that reports the outcome.
        label: &'static str,
    },
    /// Put text on the system clipboard.
    Copy(String),
    /// Say something the user has to see, with no panel to say it on.
    ///
    /// A native message box. Raised only when the panel has already gone and
    /// the thing to say is a warning or worse - see [`crate::notify`] for why
    /// that case exists at all, and why an open that went perfectly stays
    /// silent.
    Announce {
        title: String,
        detail: String,
    },
    /// Open Explorer with this file already picked out.
    ///
    /// `explorer.exe /select,"<path>"`, which is what "Show in folder" does
    /// everywhere else on this machine. Its own command rather than an
    /// [`Self::Open`] with a fourth route, because it does not open the file
    /// at all - it opens the folder, and the two fail for different reasons
    /// and report differently.
    Reveal(Arc<str>),
    /// Fetch the clipboard, to be inserted at the caret.
    ReadClipboard,
    /// Store the recalled codes.
    ///
    /// Carries the whole list rather than the one new entry: the file is a
    /// few kilobytes, it is written at most once per confirmed search, and a
    /// whole-list write means a crashed or killed process can never leave a
    /// half-applied append behind.
    SaveHistory(Arc<Vec<String>>),
    /// Put the overlay away: Escape while compact, or a result just opened.
    ///
    /// A request to the hotkey thread rather than a local state change,
    /// because the window work has to happen on the thread that owns the
    /// window, and because whether the overlay is up has exactly one owner.
    DismissOverlay,
    /// Show the settings window, or put it away if it is already up.
    ///
    /// A command rather than a flag on the state, because the window belongs to
    /// the shell: the state machine's business is what the keys mean, and
    /// "which windows are open" is not something it can be asked to be right
    /// about. Which is also why this says *toggle* rather than carrying the
    /// answer - only the shell knows whether the window is open, so only the
    /// shell can decide which way the press goes.
    ToggleSettings,
    Quit,
}

/// Which share a refresh is for.
///
/// One, always. There used to be an `All` variant behind the `A` key in the
/// drive picker, and it is gone with it: re-reading every share at once is
/// nine hundred thousand round trips against somebody else's file server,
/// started by one keystroke, and a few hundred copies of this program able to
/// do that is not a feature anybody asked for twice.
///
/// Still a named type rather than a bare `MappingId`, because the thing it is
/// compared against in `index::actor` is also a `MappingId` and a function
/// taking two of them is a function whose arguments can be swapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshTarget(pub MappingId);

impl RefreshTarget {
    pub fn wants(self, id: MappingId) -> bool {
        self.0 == id
    }
}

pub type CmdList = SmallVec<[Cmd; 2]>;

/// Wakes whoever is drawing.
///
/// The terminal loop needed nothing like this: it blocked on the event
/// channel, so a send woke it by construction. A window toolkit owns its own
/// loop and sleeps until it is told otherwise, and `next_deadline` returns
/// `None` in exactly the idle case this program is built around - so an index
/// actor finishing a three-minute walk would post into a channel nobody was
/// waiting on, and the screen would sleep through it.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// A sender that also wakes the drawer.
///
/// A type rather than a discipline. "The event was posted" and "the interface
/// will see it" cannot come apart here, which is exactly what happens the
/// first time somebody hands a new worker the bare channel. Deliberately not
/// `Deref`: reaching the inner sender is the mistake this exists to prevent.
#[derive(Clone)]
pub struct Events {
    tx: Sender<AppEvent>,
    wake: Wake,
}

impl Events {
    pub fn new(tx: Sender<AppEvent>, wake: Wake) -> Self {
        Self { tx, wake }
    }

    /// For tests, and for anything driving the state machine by hand: the
    /// events go in the channel and nobody is woken, because nobody is
    /// drawing.
    pub fn headless(tx: Sender<AppEvent>) -> Self {
        Self {
            tx,
            wake: Arc::new(|| {}),
        }
    }

    /// Blocks if the channel is full.
    ///
    /// Which is right for anything a worker has finished and would otherwise
    /// lose - but never from the thread that drains the channel. See
    /// `open::worker::Opener::request` for what that costs.
    pub fn send(&self, event: AppEvent) -> Result<(), SendError<AppEvent>> {
        self.tx.send(event)?;
        (self.wake)();
        Ok(())
    }

    /// Drops the event if the channel is full.
    ///
    /// For the reports whose truth is also readable somewhere else - index
    /// progress, which the store holds anyway - and for any send on the
    /// interface's own thread.
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

    /// Folds in a redraw obligation produced beside this response.
    ///
    /// For the cases where something changed on screen that the response
    /// itself knows nothing about - a hover highlight dropped as the hands
    /// leave the mouse, say - and the action being reported may legitimately
    /// have asked for no frame of its own.
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

    /// The reason this type exists.
    ///
    /// The terminal loop blocked on the receiver, so posting an event woke it
    /// by construction. A window toolkit owns its own loop and sleeps; if a
    /// send does not wake it, an index actor finishing a walk while the screen
    /// is idle is a walk nobody ever sees. Posting and waking are one act
    /// here, so they cannot come apart.
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

    /// A `try_send` that dropped its event must not claim a frame is owed.
    ///
    /// Waking for an event nobody received is a frame drawn for nothing, and
    /// on a full channel there will be a great many of them.
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

    /// Nothing is drawing in a test, so nothing needs waking - and asking for
    /// a wake closure at every one of them would be noise.
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
