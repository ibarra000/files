//! Two one-liners that three modules had each grown their own copy of.
//!
//! Neither is interesting. They live here because a helper duplicated across
//! modules is a helper that drifts: the moment one copy starts saturating
//! differently, or reporting a panic payload one more way, the two stop
//! agreeing and nothing catches it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Nanoseconds since the Unix epoch, or zero.
///
/// Zero on a clock set before 1970, which is not a case worth an error: every
/// caller uses this to make a temporary file name unique, where a repeated
/// value costs one retry and nothing else.
pub fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Turns a caught panic payload into something printable.
///
/// Every worker in this crate runs its body inside `catch_unwind` and reports
/// the result rather than dying quietly, so all of them need this. A payload
/// that is neither of the two usual string types still has to produce
/// *something* - "unknown panic" beats an empty toast.
pub fn panic_detail(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_reads_forward() {
        let a = now_nanos();
        let b = now_nanos();
        assert!(a > 0, "a machine set before 1970 is not a case we handle");
        assert!(b >= a);
    }

    #[test]
    fn every_panic_payload_produces_something_printable() {
        let from_str = std::panic::catch_unwind(|| panic!("boom")).unwrap_err();
        assert_eq!(panic_detail(&*from_str), "boom");

        let from_string =
            std::panic::catch_unwind(|| panic!("{}", String::from("owned"))).unwrap_err();
        assert_eq!(panic_detail(&*from_string), "owned");

        let odd = std::panic::catch_unwind(|| std::panic::panic_any(42u8)).unwrap_err();
        assert_eq!(panic_detail(&*odd), "unknown panic");
    }
}
