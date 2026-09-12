//! The single event and command vocabulary.
//!
//! Everything the program can react to arrives on one channel as an
//! [`AppEvent`], and everything it decides to do leaves [`crate::app::state`]
//! as a [`Cmd`]. Keeping side effects out of the state transition is what
//! makes the interaction model testable on a machine with no terminal and no
//! network drives.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyEvent, MouseEvent};
use smallvec::SmallVec;

use crate::config::ViewerKind;
use crate::index::errors::EnumError;
use crate::index::store::IndexStatus;
use crate::open::OpenRequest;
use crate::search::matcher::{QueryReject, SearchOutcome};
use crate::search::verify::VerifyOutcome;

/// Something happened.
#[derive(Debug, Clone)]
pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    /// Carries the new size: mouse events arrive in screen coordinates, so
    /// the state machine has to know where the widgets are to interpret them.
    Resize {
        cols: u16,
        rows: u16,
    },
    Search(SearchMsg),
    Verify(VerifyMsg),
    Index(IndexMsg),
    Open(OpenMsg),
    Clipboard(ClipboardMsg),
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

#[derive(Debug, Clone)]
pub struct SearchMsg {
    pub epoch: u64,
    pub query: String,
    pub elapsed: Duration,
    pub result: Result<SearchOutcome, QueryReject>,
}

#[derive(Debug, Clone)]
pub struct VerifyMsg {
    pub epoch: u64,
    pub query: String,
    pub elapsed: Duration,
    pub outcome: VerifyOutcome,
}

#[derive(Debug, Clone)]
pub enum IndexMsg {
    /// A fresh view of the index status, published after every change.
    Status(Arc<IndexStatus>),
    /// A new snapshot is installed; any displayed result should be recomputed.
    SnapshotChanged,
    /// Reported by an explicit refresh, so the user learns what happened.
    RefreshReport {
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
        query: String,
        epoch: u64,
    },
    Verify {
        query: String,
        epoch: u64,
    },
    RefreshIndex {
        force: bool,
    },
    Open(OpenRequest),
    /// Write the chosen viewer back to the configuration file, preserving
    /// every comment in it. Emitted only when the state machine already knows
    /// the value can stick - see `Settings::viewer_persistable`.
    SaveViewer(ViewerKind),
    /// Put text on the system clipboard.
    Copy(String),
    /// Fetch the clipboard, to be inserted at the caret.
    ReadClipboard,
    /// Store the recalled codes.
    ///
    /// Carries the whole list rather than the one new entry: the file is a
    /// few kilobytes, it is written at most once per confirmed search, and a
    /// whole-list write means a crashed or killed process can never leave a
    /// half-applied append behind.
    SaveHistory(Arc<Vec<String>>),
    Quit,
}

pub type CmdList = SmallVec<[Cmd; 2]>;

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
    fn redraw_is_sticky_when_merged() {
        assert_eq!(Redraw::No.or(Redraw::No), Redraw::No);
        assert_eq!(Redraw::No.or(Redraw::Yes), Redraw::Yes);
        assert_eq!(Redraw::Yes.or(Redraw::No), Redraw::Yes);
    }

    #[test]
    fn merging_accumulates_commands_and_promotes_redraw() {
        let mut a = Response::none().with(Cmd::Quit);
        a.merge(Response::redraw().with(Cmd::RefreshIndex { force: true }));
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
