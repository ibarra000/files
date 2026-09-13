//! The vocabulary the interaction model is written in.
//!
//! Plain data, split out from the transition itself so `mod.rs` stays about
//! behaviour. Nothing here has any logic beyond a predicate or two, and
//! nothing here knows about events, commands or the clock.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How far the current query has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryPhase {
    /// Nothing typed.
    Idle,
    TooShort {
        need: usize,
    },
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
    Verifying {
        since: Instant,
    },
    /// Confirmed against the server, or proven current by an unchanged
    /// directory stamp.
    Verified {
        took: Duration,
        by_stamp: bool,
    },
    /// Verification failed; local results remain on screen.
    VerifyFailed {
        detail: String,
    },
}

impl QueryPhase {
    pub fn is_verifying(&self) -> bool {
        matches!(self, Self::Verifying { .. })
    }
}

/// Why the result list is empty.
///
/// The results widget takes this rather than an empty slice, so a blank list
/// always carries a reason. The previous implementation could show nothing at
/// all when the drive was unreachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmptyReason {
    NoQuery,
    QueryTooShort { need: usize },
    NoSharesConfigured,
    NoMatches { searched: u32 },
    IndexUnavailable { detail: String },
    PathNotFound { dir: PathBuf },
    AccessDenied { dir: PathBuf },
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
