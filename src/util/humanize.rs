//! Human-readable formatting for the status line.
//!
//! Pure functions, so the exact strings the UI renders can be asserted in
//! tests without a terminal.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Formats an age as a compact relative string: `just now`, `42s`, `3m`, `2h`,
/// `4d`.
pub fn age(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..=4 => "just now".to_string(),
        5..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// How long until [`age`] would render a different string.
///
/// Exists so the main loop can wake exactly when the readout changes rather
/// than once a second forever: against a day-old cached index that is one
/// wakeup a day, not eighty-six thousand. Its bucket edges mirror `age`'s, and
/// `next_age_change_is_the_exact_distance_to_a_different_label` is what stops
/// the two drifting apart.
pub fn next_age_change(d: Duration) -> Duration {
    let secs = d.as_secs();
    let edge = match secs {
        // "just now" holds all the way to five seconds.
        0..=4 => 5,
        5..=59 => secs + 1,
        60..=3599 => secs - secs % 60 + 60,
        3600..=86_399 => secs - secs % 3600 + 3600,
        _ => secs - secs % 86_400 + 86_400,
    };
    // `edge` is strictly greater than `secs`, so this is never zero - which
    // matters, because a zero here would be a deadline that never advances.
    Duration::from_secs(edge).saturating_sub(d)
}

/// Formats a measured duration: `840us`, `42ms`, `1.4s`, `2m 05s`.
pub fn elapsed(d: Duration) -> String {
    let micros = d.as_micros();
    if micros < 1_000 {
        return format!("{micros}us");
    }
    let millis = d.as_millis();
    if millis < 1_000 {
        return format!("{millis}ms");
    }
    let secs = d.as_secs_f64();
    if secs < 60.0 {
        return format!("{secs:.1}s");
    }
    format!("{}m {:02}s", d.as_secs() / 60, d.as_secs() % 60)
}

/// Thousands-separated integer: `1,284,551`.
pub fn count(n: usize) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(b as char);
    }
    out
}

/// Binary byte size: `63.0 MiB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

const FRAMES: [char; 10] = [
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];

/// Braille spinner frame for an elapsed time. Advances every 100ms.
pub fn spinner(d: Duration) -> char {
    FRAMES[(d.as_millis() / 100) as usize % FRAMES.len()]
}

/// Spinner frame for a wall clock, for an animation with no start instant.
///
/// [`spinner`] needs something to measure from, and the index states have
/// nothing: `Activity` records what a share is doing, never when it began. The
/// status line reached for `now.elapsed()`, where `now` is the instant the
/// frame started - sampled microseconds earlier, so the frame index was zero
/// every time and the spinner watched through a three-minute walk never
/// turned.
///
/// The wall clock is the only phase a pure renderer can read that advances
/// *between* frames rather than within one. It can jump - NTP, a timezone
/// change - and the cost of that is one spinner frame out of order, which
/// nobody can see. That is cheaper than carrying a start instant through the
/// index actor for the sake of an animation.
pub fn spinner_at(wall: SystemTime) -> char {
    let since_epoch = wall.duration_since(UNIX_EPOCH).unwrap_or_default();
    FRAMES[(since_epoch.as_millis() / 100) as usize % FRAMES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_ages_across_every_bucket() {
        assert_eq!(age(Duration::from_secs(0)), "just now");
        assert_eq!(age(Duration::from_secs(4)), "just now");
        assert_eq!(age(Duration::from_secs(5)), "5s");
        assert_eq!(age(Duration::from_secs(59)), "59s");
        assert_eq!(age(Duration::from_secs(60)), "1m");
        assert_eq!(age(Duration::from_secs(3599)), "59m");
        assert_eq!(age(Duration::from_secs(3600)), "1h");
        assert_eq!(age(Duration::from_secs(86_399)), "23h");
        assert_eq!(age(Duration::from_secs(86_400)), "1d");
    }

    #[test]
    fn next_age_change_lands_exactly_on_each_bucket_edge() {
        let d = Duration::from_secs;
        assert_eq!(next_age_change(d(0)), d(5), "\"just now\" holds until 5s");
        assert_eq!(next_age_change(d(4)), d(1));
        assert_eq!(next_age_change(d(5)), d(1));
        assert_eq!(next_age_change(d(59)), d(1));
        assert_eq!(next_age_change(d(60)), d(60), "minutes tick once a minute");
        assert_eq!(next_age_change(d(61)), d(59));
        assert_eq!(next_age_change(d(3599)), d(1));
        assert_eq!(next_age_change(d(3600)), d(3600));
        assert_eq!(next_age_change(d(86_399)), d(1));
        assert_eq!(
            next_age_change(d(86_400)),
            d(86_400),
            "a day-old index costs one wakeup a day"
        );
    }

    #[test]
    fn next_age_change_accounts_for_the_fraction_of_a_second() {
        let d = Duration::from_millis(4_500);
        assert_eq!(next_age_change(d), Duration::from_millis(500));
    }

    /// The whole contract, so the two bucket tables cannot drift apart.
    #[test]
    fn next_age_change_is_the_exact_distance_to_a_different_label() {
        for secs in [
            0u64, 1, 4, 5, 30, 59, 60, 61, 119, 3599, 3600, 7199, 86_399, 86_400, 200_000,
        ] {
            let a = Duration::from_secs(secs);
            let step = next_age_change(a);
            assert_eq!(
                age(a),
                age(a + step - Duration::from_millis(1)),
                "the label changed early at {secs}s"
            );
            assert_ne!(
                age(a),
                age(a + step),
                "the label had not changed at {secs}s"
            );
        }
    }

    #[test]
    fn formats_elapsed_across_every_bucket() {
        assert_eq!(elapsed(Duration::from_micros(840)), "840us");
        assert_eq!(elapsed(Duration::from_millis(42)), "42ms");
        assert_eq!(elapsed(Duration::from_millis(1400)), "1.4s");
        assert_eq!(elapsed(Duration::from_secs(125)), "2m 05s");
    }

    #[test]
    fn groups_counts_in_threes() {
        assert_eq!(count(0), "0");
        assert_eq!(count(7), "7");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1,000");
        assert_eq!(count(1_284_551), "1,284,551");
        assert_eq!(count(1_000_000_000), "1,000,000,000");
    }

    #[test]
    fn formats_binary_sizes() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(66_060_288), "63.0 MiB");
    }

    #[test]
    fn spinner_advances_every_hundred_millis_and_wraps() {
        assert_eq!(
            spinner(Duration::from_millis(0)),
            spinner(Duration::from_millis(99))
        );
        assert_ne!(
            spinner(Duration::from_millis(0)),
            spinner(Duration::from_millis(100))
        );
        assert_eq!(
            spinner(Duration::from_millis(0)),
            spinner(Duration::from_millis(1000))
        );
    }
}
