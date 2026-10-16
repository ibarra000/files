//! Plain data types used by the interaction state machine.
//!
//! Nothing here knows about events, commands or the clock.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::paths::MappingId;
use crate::search::live::{LiveCoverage, LiveSkip};

/// How far the current query has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryPhase {
    /// Nothing typed.
    Idle,
    /// The line holds search syntax that could not be honoured.
    ///
    /// A phase rather than a toast so it clears as soon as the line is fixed.
    BadQuery { detail: String },
    /// Typed, but no enabled share is configured.
    NoShares,
    /// Dispatched to the matcher. Sub-millisecond, so rarely rendered.
    LocalPending,
    /// Showing results from the in-memory index.
    Local,
    /// Local results shown while the server is being consulted.
    Verifying { since: Instant },
    /// Confirmed against the server, or proven current by an unchanged
    /// directory stamp.
    Verified { took: Duration, by_stamp: bool },
    /// Verification failed; local results remain on screen.
    VerifyFailed { detail: String },
}

impl QueryPhase {
    pub fn is_verifying(&self) -> bool {
        matches!(self, Self::Verifying { .. })
    }

    /// Whether verification has already reported for this query.
    ///
    /// The local match and the server check are dispatched on the same tick
    /// and either can answer first (`run_verify` answers immediately with
    /// `Skipped(SeveralShares)` when several shares are configured). A local
    /// result must not overwrite a server phase.
    pub fn server_has_spoken(&self) -> bool {
        matches!(
            self,
            Self::Verifying { .. } | Self::Verified { .. } | Self::VerifyFailed { .. }
        )
    }
}

/// Whether the edit that produced a query was a keystroke or a whole query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// Typed a character at a time. Waits out `SEARCH_DEBOUNCE`.
    Typed,
    /// Arrived whole (pasted, recalled, read from the clipboard). Runs now.
    Complete,
}

/// Why the result list is empty.
///
/// The results widget takes this rather than an empty slice, so a blank list
/// always carries a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmptyReason {
    NoQuery,
    /// The line holds search syntax that could not be honoured.
    BadQuery {
        detail: String,
    },
    NoSharesConfigured,
    NoMatches {
        searched: u32,
    },
    /// Nothing matched in the part of a live share that could be reached.
    ///
    /// Kept distinct from [`Self::NoMatches`]: a partial search must not be
    /// reported as proof that the file does not exist.
    LiveIncomplete {
        name: String,
        searched: u32,
        skipped: u32,
    },
    /// A live share could not be asked at all.
    LiveUnavailable {
        name: String,
        detail: String,
    },
    IndexUnavailable {
        detail: String,
    },
    PathNotFound {
        dir: PathBuf,
    },
    AccessDenied {
        dir: PathBuf,
    },
    NotSearchedYet,
}

/// A transient message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// How long a transient message stays on screen.
pub const TOAST_LIFETIME: Duration = Duration::from_secs(5);

/// Progress of the live shares on the current query.
///
/// Separate from [`QueryPhase`] because it runs concurrently with the
/// already-answered index phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveProgress {
    pub since: Instant,
    /// Shares that have not answered yet.
    pub outstanding: usize,
    /// What each share that answered was able to reach.
    pub reached: Vec<(MappingId, LiveCoverage)>,
    pub skipped: Vec<(MappingId, LiveSkip)>,
    pub failed: Vec<(MappingId, String)>,
}

impl LiveProgress {
    pub fn asking(since: Instant, outstanding: usize) -> Self {
        Self {
            since,
            outstanding,
            reached: Vec::new(),
            skipped: Vec::new(),
            failed: Vec::new(),
        }
    }

    pub fn is_asking(&self) -> bool {
        self.outstanding > 0
    }

    /// True when every share answered for the full configured depth.
    ///
    /// An empty list may be reported as "no matches" only when this is true.
    pub fn complete(&self) -> bool {
        self.outstanding == 0
            && self.skipped.is_empty()
            && self.failed.is_empty()
            && self.reached.iter().all(|(_, c)| c.complete())
    }

    /// The share whose answer is least complete, for the status line.
    pub fn worst(&self) -> Option<(MappingId, &LiveCoverage)> {
        self.reached
            .iter()
            .filter(|(_, c)| !c.complete())
            .max_by_key(|(_, c)| c.dirs_skipped)
            .map(|(id, c)| (*id, c))
    }
}
