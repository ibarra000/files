//! The vocabulary the interaction model is written in.
//!
//! Plain data, split out from the transition itself so `mod.rs` stays about
//! behaviour. Nothing here has any logic beyond a predicate or two, and
//! nothing here knows about events, commands or the clock.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::paths::MappingId;
use crate::search::live::{LiveCoverage, LiveSkip};

/// How far the current query has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryPhase {
    /// Nothing typed.
    ///
    /// There used to be a `TooShort` beside this, for a line under three
    /// characters. The floor is one now, so "too short" and "nothing typed"
    /// are the same state, and two names for one state is one of them that
    /// nothing can ever reach.
    Idle,
    /// The line holds search syntax that could not be honoured.
    ///
    /// Its own phase rather than a toast, because it is a property of what is
    /// on the line right now: it has to clear itself the moment the line is
    /// fixed, and a toast would sit there for its five seconds saying
    /// otherwise.
    BadQuery { detail: String },
    /// Typed, but there is no share to search.
    ///
    /// Was "this does not look like a job code", back when a code had to match
    /// a pattern before anything would look for it. Every share is indexed
    /// now, so the only way a query reaches nothing is a configuration with no
    /// enabled share in it.
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

    /// Whether the verification has already spoken for this query.
    ///
    /// The local match and the server check share one pause, so with equal
    /// debounces both are dispatched on the same tick and either can answer
    /// first - `run_verify` answers instantly with `Skipped(SeveralShares)`
    /// whenever more than one share is configured, which is the shipped case.
    /// The local answer is never news about the server, so it must not
    /// overwrite what the server said, or a spinner blinks off mid-check and a
    /// `Verified` lands on a phase nobody ever saw spin.
    pub fn server_has_spoken(&self) -> bool {
        matches!(
            self,
            Self::Verifying { .. } | Self::Verified { .. } | Self::VerifyFailed { .. }
        )
    }
}

/// Whether the edit that produced a query was a keystroke or a whole query.
///
/// A parameter rather than a second function, so that no call site can forget
/// to choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// Typed a character at a time. Waits out `SEARCH_DEBOUNCE`.
    Typed,
    /// Arrived whole - pasted, recalled, read off the clipboard. Runs now.
    Complete,
}

/// Why the result list is empty.
///
/// The results widget takes this rather than an empty slice, so a blank list
/// always carries a reason. The previous implementation could show nothing at
/// all when the drive was unreachable.
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
    /// Never collapsed into [`Self::NoMatches`]. A live share answers for the
    /// folders one query had time to look in, and "there is no such file" is a
    /// different claim from "it is not in the fourteen folders I asked about" -
    /// which is the claim this program exists not to let somebody act on by
    /// mistake.
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

/// What the live shares are doing about the query on the line.
///
/// A field of its own rather than a [`QueryPhase`] variant, because the two are
/// not on one line: the indexes have already answered while this is
/// outstanding, and `QueryPhase` holds exactly one value at a time.
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

    /// True when every share answered for everything the configured depth
    /// names.
    ///
    /// The question the empty pane turns on: an empty list is only honestly
    /// "no matches" when this is true. Anything less and the answer is "not in
    /// the folders that were searched", which is a different sentence.
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
