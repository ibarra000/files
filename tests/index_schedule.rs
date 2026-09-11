//! The index schedule, simulated.
//!
//! The bug these tests exist for was reported as "random indexing reloads":
//! a million-entry share re-enumerated roughly once a minute instead of once
//! an hour, at intervals that looked arbitrary because the probe cadence
//! carries +/-10% jitter.
//!
//! It survived into a shipped build because it was **unreachable from a
//! test**. The probe interval is sixty seconds and the rescan floor is an
//! hour, so no timer-driven refresh had ever fired in the suite; the only way
//! any test could make the index actor act was to send it an explicit refresh,
//! which takes a different branch entirely.
//!
//! `files::index::schedule` reads no clock and does no I/O, so a day of
//! operation can be stepped through here in microseconds - and the question
//! "how many times does this rebuild in twenty-four hours?" becomes an
//! assertion instead of an impression.

use std::time::{Duration, Instant};

use files::index::DirStamp;
use files::index::errors::EnumError;
use files::index::schedule::{Cadence, Input, ScanReason, Scheduler, Step};
use proptest::prelude::*;

const SEED: u64 = 0xD15E_A5ED_D15E_A5ED;
const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(24 * 3600);

/// A share the simulator can interrogate. Every method is answered instantly;
/// only the simulated clock moves.
trait Share {
    fn probe(&mut self, now: Instant) -> Result<DirStamp, EnumError>;
    fn scan(&mut self, now: Instant) -> Result<Option<DirStamp>, EnumError>;
}

/// One enumeration, as observed from outside.
#[derive(Debug, Clone, Copy)]
struct Scan {
    at: Instant,
    reason: ScanReason,
    /// Whether it produced a listing. Only a *successful* enumeration resets
    /// the rescan floor, so the two are paced by different things and have to
    /// be counted separately.
    ok: bool,
}

/// Steps a [`Scheduler`] over simulated time, recording what it decided.
struct Sim {
    sched: Scheduler,
    now: Instant,
    start: Instant,
    have_snapshot: bool,
    scans: Vec<Scan>,
    probes: usize,
}

impl Sim {
    fn new(cadence: Cadence) -> Self {
        let now = Instant::now();
        Self {
            sched: Scheduler::new(cadence, Some(SEED)),
            now,
            start: now,
            have_snapshot: false,
            scans: Vec::new(),
            probes: 0,
        }
    }

    /// One wake-up: feeds inputs back until the scheduler says to wait.
    ///
    /// Mirrors the actor's driver loop exactly, including its step bound, so a
    /// rule that failed to terminate would show up here first.
    fn wake(&mut self, share: &mut dyn Share, forced: bool) -> Instant {
        let mut input = Input::Woke { forced };
        for _ in 0..6 {
            let decision = self.sched.on(self.now, input, self.have_snapshot);
            match decision.step {
                Step::Probe => {
                    self.probes += 1;
                    input = Input::Probed(share.probe(self.now));
                }
                Step::FullScan(reason) => {
                    let outcome = share.scan(self.now);
                    let ok = outcome.is_ok();
                    if ok {
                        self.have_snapshot = true;
                    }
                    self.scans.push(Scan {
                        at: self.now,
                        reason,
                        ok,
                    });
                    input = Input::Scanned(outcome);
                }
                Step::ConfirmFresh | Step::Wait => {
                    assert!(
                        decision.next_action > self.now,
                        "{:?} scheduled a wake in the past, which would spin the \
                         index thread against the file server",
                        decision.step
                    );
                    return decision.next_action;
                }
            }
        }
        panic!("the scheduler took more than six steps for one wake-up");
    }

    fn run_for(&mut self, span: Duration, share: &mut dyn Share) {
        let end = self.now + span;
        let mut wakes = 0usize;
        while self.now < end {
            self.now = self.wake(share, false);
            wakes += 1;
            assert!(
                wakes < 200_000,
                "{wakes} wake-ups in {span:?} means the schedule is not converging"
            );
        }
    }

    fn force(&mut self, share: &mut dyn Share) {
        self.now = self.wake(share, true);
    }

    fn scan_count(&self) -> usize {
        self.scans.len()
    }

    /// Enumerations that actually produced a listing.
    fn completed_scans(&self) -> usize {
        self.scans.iter().filter(|s| s.ok).count()
    }

    fn reasons(&self) -> Vec<ScanReason> {
        self.scans.iter().map(|s| s.reason).collect()
    }

    /// Smallest gap between two consecutive enumerations.
    fn tightest_gap(&self) -> Option<Duration> {
        self.scans
            .windows(2)
            .map(|w| w[1].at.saturating_duration_since(w[0].at))
            .min()
    }

    fn elapsed(&self) -> Duration {
        self.now.saturating_duration_since(self.start)
    }
}

// --- scripted shares -------------------------------------------------------

/// Nothing ever changes. The overwhelmingly common case in production: the
/// CustomPro directory is quiet for hours at a time.
struct Stable(DirStamp);

impl Share for Stable {
    fn probe(&mut self, _now: Instant) -> Result<DirStamp, EnumError> {
        Ok(self.0)
    }
    fn scan(&mut self, _now: Instant) -> Result<Option<DirStamp>, EnumError> {
        Ok(Some(self.0))
    }
}

/// Enumerates fine, but will not answer the freshness probe.
///
/// The exact shape of the reported failure: a server that rejects
/// `FileBasicInfo` on a directory handle. The scan's own stamp capture fails
/// for the same reason, so it yields `Ok(None)`.
struct RefusesProbe;

impl Share for RefusesProbe {
    fn probe(&mut self, _now: Instant) -> Result<DirStamp, EnumError> {
        Err(EnumError::AccessDenied(5))
    }
    fn scan(&mut self, _now: Instant) -> Result<Option<DirStamp>, EnumError> {
        Ok(None)
    }
}

/// Reachable intermittently at best - a VPN flapping, or a laptop on wifi.
struct Unreachable;

impl Share for Unreachable {
    fn probe(&mut self, _now: Instant) -> Result<DirStamp, EnumError> {
        Err(EnumError::Transient(53))
    }
    fn scan(&mut self, _now: Instant) -> Result<Option<DirStamp>, EnumError> {
        Err(EnumError::Transient(53))
    }
}

/// A busy directory: something is added every `period`.
struct Churning {
    period: Duration,
    origin: Option<Instant>,
}

impl Churning {
    fn new(period: Duration) -> Self {
        Self {
            period,
            origin: None,
        }
    }

    fn stamp_at(&mut self, now: Instant) -> DirStamp {
        let origin = *self.origin.get_or_insert(now);
        let ticks = now.saturating_duration_since(origin).as_nanos() / self.period.as_nanos();
        DirStamp::new(ticks as i64, ticks as i64)
    }
}

impl Share for Churning {
    fn probe(&mut self, now: Instant) -> Result<DirStamp, EnumError> {
        Ok(self.stamp_at(now))
    }
    fn scan(&mut self, now: Instant) -> Result<Option<DirStamp>, EnumError> {
        Ok(Some(self.stamp_at(now)))
    }
}

/// Fails the first `n` enumerations, then behaves.
struct ScanFailsAtFirst {
    remaining: u32,
    stamp: DirStamp,
}

impl Share for ScanFailsAtFirst {
    fn probe(&mut self, _now: Instant) -> Result<DirStamp, EnumError> {
        Ok(self.stamp)
    }
    fn scan(&mut self, _now: Instant) -> Result<Option<DirStamp>, EnumError> {
        if self.remaining > 0 {
            self.remaining -= 1;
            return Err(EnumError::Transient(53));
        }
        Ok(Some(self.stamp))
    }
}

// --- the reload storm ------------------------------------------------------

/// The primary regression test.
///
/// Before the fix this produced roughly 1,400 full enumerations a day - one
/// per probe interval - because "I have no stamp to compare against" was read
/// as "the directory changed".
#[test]
fn a_share_that_refuses_the_probe_is_enumerated_once_per_floor_not_once_per_probe() {
    let mut sim = Sim::new(Cadence::shipped());
    sim.run_for(DAY, &mut RefusesProbe);

    let ceiling = (DAY.as_secs() / HOUR.as_secs()) as usize + 2;
    assert!(
        sim.scan_count() <= ceiling,
        "{} enumerations in a day; the floor allows about {ceiling}",
        sim.scan_count()
    );
    assert!(
        sim.scan_count() >= 20,
        "it must still refresh on the floor, got {}",
        sim.scan_count()
    );
    assert!(
        sim.reasons()
            .iter()
            .skip(1)
            .all(|r| *r == ScanReason::Blind),
        "every rebuild after the first should be attributed to the missing \
         change detection: {:?}",
        sim.reasons()
    );
    assert!(
        sim.sched.stamp_health().is_blind(),
        "and the UI must be able to say so"
    );
}

#[test]
fn a_quiet_share_is_enumerated_only_on_the_floor() {
    let mut sim = Sim::new(Cadence::shipped());
    sim.run_for(DAY, &mut Stable(DirStamp::new(1, 1)));

    let expected = (DAY.as_secs() / HOUR.as_secs()) as usize;
    assert!(
        (expected..=expected + 2).contains(&sim.scan_count()),
        "expected about {expected} enumerations in a day, got {}",
        sim.scan_count()
    );
    assert!(
        sim.probes > sim.scan_count() * 20,
        "the cheap probe should be carrying the freshness check: \
         {} probes against {} enumerations",
        sim.probes,
        sim.scan_count()
    );
}

/// An unreachable drive is not a stale index. The backoff paces the probe; the
/// floor is what eventually forces a rebuild, and nothing else may.
#[test]
fn an_unreachable_share_is_not_hammered_with_enumerations() {
    let mut sim = Sim::new(Cadence::shipped());
    sim.run_for(DAY, &mut Unreachable);

    // Every scan also fails here, so the scan backoff paces them.
    let cadence = Cadence::shipped();
    let ceiling = (DAY.as_nanos() / cadence.min_scan_spacing.as_nanos()) as usize;
    assert!(
        sim.scan_count() < ceiling,
        "{} enumerations is faster than the spacing floor allows",
        sim.scan_count()
    );
    assert!(
        sim.tightest_gap().unwrap() >= cadence.min_scan_spacing,
        "two enumerations only {:?} apart",
        sim.tightest_gap().unwrap()
    );
}

/// The second regression test. `last_full_scan` used to be set only on
/// success, so a failing scan left the rescan floor permanently due and every
/// full-jitter retry - uniform in `0..=ceiling` - started another enumeration.
#[test]
fn a_failing_scan_never_retries_faster_than_the_spacing_floor() {
    let cadence = Cadence::shipped();
    let mut sim = Sim::new(cadence);
    sim.run_for(
        HOUR,
        &mut ScanFailsAtFirst {
            remaining: 20,
            stamp: DirStamp::new(1, 1),
        },
    );

    assert!(sim.scan_count() > 2, "it must keep trying");
    assert!(
        sim.tightest_gap().unwrap() >= cadence.min_scan_spacing,
        "two enumerations started {:?} apart, floor is {:?}",
        sim.tightest_gap().unwrap(),
        cadence.min_scan_spacing
    );
}

#[test]
fn a_busy_directory_is_enumerated_when_it_actually_changes() {
    let period = Duration::from_secs(300);
    let mut sim = Sim::new(Cadence::shipped());
    sim.run_for(DAY, &mut Churning::new(period));

    let changes = (DAY.as_secs() / period.as_secs()) as usize;
    assert!(
        sim.scan_count() <= changes + 2,
        "{} enumerations for {changes} changes",
        sim.scan_count()
    );
    assert!(
        sim.scan_count() >= changes / 2,
        "a directory that keeps changing must keep being re-read: {}",
        sim.scan_count()
    );
    assert!(
        sim.reasons().contains(&ScanReason::StampMoved),
        "and the reason must be the honest one: {:?}",
        sim.reasons()
    );
}

#[test]
fn an_f5_always_enumerates_however_recently_one_ran() {
    let mut share = Stable(DirStamp::new(1, 1));
    let mut sim = Sim::new(Cadence::shipped());
    sim.run_for(Duration::from_secs(120), &mut share);
    let before = sim.scan_count();

    for _ in 0..5 {
        sim.force(&mut share);
    }

    assert_eq!(
        sim.scan_count() - before,
        5,
        "the spacing floor is a backstop against the schedule, not against \
         the user"
    );
}

#[test]
fn recovering_from_a_refusing_share_returns_to_the_cheap_cadence() {
    /// Refuses the probe for the first hour, then answers normally.
    struct Recovering {
        start: Option<Instant>,
        stamp: DirStamp,
    }

    impl Recovering {
        fn healed(&mut self, now: Instant) -> bool {
            let start = *self.start.get_or_insert(now);
            now.saturating_duration_since(start) > HOUR
        }
    }

    impl Share for Recovering {
        fn probe(&mut self, now: Instant) -> Result<DirStamp, EnumError> {
            if self.healed(now) {
                Ok(self.stamp)
            } else {
                Err(EnumError::AccessDenied(5))
            }
        }
        fn scan(&mut self, now: Instant) -> Result<Option<DirStamp>, EnumError> {
            Ok(self.healed(now).then_some(self.stamp))
        }
    }

    let mut sim = Sim::new(Cadence::shipped());
    sim.run_for(
        HOUR * 4,
        &mut Recovering {
            start: None,
            stamp: DirStamp::new(3, 3),
        },
    );

    assert!(
        !sim.sched.stamp_health().is_blind(),
        "a share that starts answering again must be trusted again"
    );
    assert!(
        sim.scan_count() <= 6,
        "four hours should be about four rebuilds, got {}",
        sim.scan_count()
    );
}

/// A week, in microseconds.
///
/// Not `#[ignore]`d: the whole point of a scheduler that reads no clock is
/// that a week costs the same as a second, so there is no reason for the
/// long-horizon check to be one nobody runs.
#[test]
fn a_week_of_a_share_that_cannot_be_probed_stays_on_the_floor() {
    let week = DAY * 7;
    let mut sim = Sim::new(Cadence::shipped());
    sim.run_for(week, &mut RefusesProbe);

    let floors = (week.as_secs() / HOUR.as_secs()) as usize;
    assert!(
        sim.scan_count() <= floors + 2,
        "{} enumerations in a week; the floor allows about {floors}",
        sim.scan_count()
    );
    // The failing behaviour was one enumeration per probe interval. Stating
    // the old number makes the regression unmissable if it ever returns.
    let old_behaviour = (week.as_secs() / 60) as usize;
    assert!(
        sim.scan_count() * 50 < old_behaviour,
        "{} enumerations is not meaningfully better than the {old_behaviour}          this replaced",
        sim.scan_count()
    );
}

#[test]
fn a_week_of_a_quiet_share_never_drifts() {
    let week = DAY * 7;
    let mut sim = Sim::new(Cadence::shipped());
    sim.run_for(week, &mut Stable(DirStamp::new(1, 1)));

    let floors = (week.as_secs() / HOUR.as_secs()) as usize;
    // A window either side: the floor fires at the first wake at or after
    // the hour, and the probe jitter decides whether the last one lands
    // inside the week or just past it.
    assert!(
        (floors - 1..=floors + 1).contains(&sim.scan_count()),
        "expected about {floors} enumerations in a week, got {}",
        sim.scan_count()
    );
    assert!(
        sim.tightest_gap().unwrap() > HOUR - Duration::from_secs(120),
        "the floor must not creep: tightest gap {:?}",
        sim.tightest_gap().unwrap()
    );
}

// --- properties ------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum ProbeStep {
    Same,
    Moved,
    Refused,
    Unreachable,
}

#[derive(Debug, Clone, Copy)]
enum ScanStep {
    Ok,
    NoStamp,
    Failed,
}

/// Replays a fixed script, cycling. Arbitrary, but deterministic per case.
struct Scripted {
    probes: Vec<ProbeStep>,
    scans: Vec<ScanStep>,
    probe_at: usize,
    scan_at: usize,
    stamp: i64,
}

impl Share for Scripted {
    fn probe(&mut self, _now: Instant) -> Result<DirStamp, EnumError> {
        let step = self.probes[self.probe_at % self.probes.len()];
        self.probe_at += 1;
        match step {
            ProbeStep::Same => Ok(DirStamp::new(self.stamp, self.stamp)),
            ProbeStep::Moved => {
                self.stamp += 1;
                Ok(DirStamp::new(self.stamp, self.stamp))
            }
            ProbeStep::Refused => Err(EnumError::AccessDenied(5)),
            ProbeStep::Unreachable => Err(EnumError::Transient(53)),
        }
    }

    fn scan(&mut self, _now: Instant) -> Result<Option<DirStamp>, EnumError> {
        let step = self.scans[self.scan_at % self.scans.len()];
        self.scan_at += 1;
        match step {
            ScanStep::Ok => Ok(Some(DirStamp::new(self.stamp, self.stamp))),
            ScanStep::NoStamp => Ok(None),
            ScanStep::Failed => Err(EnumError::Transient(53)),
        }
    }
}

fn probe_steps() -> impl Strategy<Value = Vec<ProbeStep>> {
    proptest::collection::vec(
        prop_oneof![
            6 => Just(ProbeStep::Same),
            2 => Just(ProbeStep::Moved),
            1 => Just(ProbeStep::Refused),
            1 => Just(ProbeStep::Unreachable),
        ],
        1..8,
    )
}

fn scan_steps() -> impl Strategy<Value = Vec<ScanStep>> {
    proptest::collection::vec(
        prop_oneof![
            6 => Just(ScanStep::Ok),
            1 => Just(ScanStep::NoStamp),
            2 => Just(ScanStep::Failed),
        ],
        1..8,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// The invariant the whole rewrite rests on. Whatever the share does, two
    /// enumerations of a million-entry directory never start closer together
    /// than the spacing floor - and nothing but an F5 can bypass it.
    #[test]
    fn no_sequence_of_answers_can_produce_back_to_back_enumerations(
        probes in probe_steps(),
        scans in scan_steps(),
    ) {
        let cadence = Cadence::shipped();
        let mut sim = Sim::new(cadence);
        sim.run_for(HOUR * 6, &mut Scripted {
            probes, scans, probe_at: 0, scan_at: 0, stamp: 1,
        });

        if let Some(gap) = sim.tightest_gap() {
            prop_assert!(
                gap >= cadence.min_scan_spacing,
                "two enumerations {gap:?} apart, floor is {:?}",
                cadence.min_scan_spacing
            );
        }
    }

    /// A bound on the damage, stated in the units the user feels: how many
    /// times can the screen say "building index" in six hours?
    #[test]
    fn the_number_of_enumerations_is_bounded_by_the_elapsed_time(
        probes in probe_steps(),
        scans in scan_steps(),
    ) {
        let cadence = Cadence::shipped();
        let span = HOUR * 6;
        let mut sim = Sim::new(cadence);
        sim.run_for(span, &mut Scripted {
            probes, scans, probe_at: 0, scan_at: 0, stamp: 1,
        });

        let ceiling = (sim.elapsed().as_nanos() / cadence.min_scan_spacing.as_nanos()) as usize + 2;
        prop_assert!(
            sim.scan_count() <= ceiling,
            "{} enumerations in {:?}", sim.scan_count(), sim.elapsed()
        );
    }

    /// The regression property, stated for every possible scan behaviour: a
    /// share whose probe always fails must fall back to the rescan floor, not
    /// to the probe interval.
    ///
    /// Counted over *completed* enumerations, because only those reset the
    /// floor. Failed ones retry on the scan backoff, which the two properties
    /// above already bound - and conflating the two is how the first draft of
    /// this test managed to fail against an entirely legitimate schedule.
    #[test]
    fn refusing_every_probe_falls_back_to_the_floor(
        scans in scan_steps(),
    ) {
        let span = HOUR * 6;
        let cadence = Cadence::shipped();

        let mut refusing = Sim::new(cadence);
        refusing.run_for(span, &mut Scripted {
            probes: vec![ProbeStep::Refused],
            scans,
            probe_at: 0, scan_at: 0, stamp: 1,
        });

        let floors = (span.as_nanos() / cadence.rescan_floor.as_nanos()) as usize;
        prop_assert!(
            refusing.completed_scans() <= floors + 2,
            "{} completed enumerations across {floors} floors is the probe interval, not the floor",
            refusing.completed_scans()
        );
    }
}
