//! Small helpers shared by several modules, kept in one place so copies do
//! not drift apart.

use std::time::{SystemTime, UNIX_EPOCH};

/// Returns nanoseconds since the Unix epoch, or zero if the clock is earlier.
///
/// Used only to make temporary file names unique, where a repeat costs one
/// retry.
pub fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Converts a caught panic payload into a message for worker failure reports.
///
/// Payloads that are not `&str` or `String` yield a generic message.
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
