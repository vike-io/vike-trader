//! Dead-man's switch (auto cancel-on-disconnect) — the AUTOMATIC counterpart to the manual HALT
//! sentinel (`vike_bridge_core::halt`). No Python twin; a Rust-native operational safeguard
//! (trading-hardening).
//!
//! ## What it does
//! When the live core observes that market data / venue connectivity has been silent for longer
//! than a configured `timeout`, it TRIPS: it cancels every resting (open) order through the ONE
//! order-write vocabulary ([`crate::runtime`]'s `apply_intent` → `OrderIntent::MassCancel`) and,
//! for the [`DeadManAction::CancelAllAndHalt`] action, ALSO engages HALT — both the in-process
//! `trading_state = Halted` gate (the `RiskGate` then denies every new order) AND the SAME
//! cross-process HALT sentinel file the `ExecActor` submit boundary checks — so no new order can be
//! placed until an operator clears it. This is the AUTOMATIC twin of the operator's manual
//! `touch HALT`.
//!
//! ## OFF by default — zero overhead when disabled
//! Represented as an `Option<DeadManConfig>` on [`crate::CoreConfig`], `None` by default. When
//! `None`: no [`DeadMan`] state machine is built, no timer is armed, no waker is spawned for it, and
//! the per-message fold never touches it — byte-identical to a runtime that never heard of the
//! feature.
//!
//! ## Off the hot fold
//! The trip evaluation ([`DeadMan::check`]) runs at the drain-loop boundary on the existing
//! `DeadlineTimerWheel` cadence (a sibling of the stuck-order watchdog's `StuckSweep`), NEVER the
//! per-message fold — so the `p99 < 10µs` core-hop gate is untouched. The only per-message cost is a
//! single monotonic-max store ([`DeadMan::observe`]) recording the freshest data/event timestamp,
//! and even that is skipped entirely (one `Option` branch) when the switch is disabled. Exactly ONE
//! `tracing::warn!` is emitted, on trip — a fault-transition, well within the logging budget.
//!
//! ## Freshness model
//! [`DeadMan`] is deliberately clock-free (the caller passes `ts`/`now_ms` in, so every transition
//! is unit-tested with zero sleeps). It mirrors the FRESHNESS HALF of
//! `vike_bridge_core::stream_health::StreamHealth` (observe newest data ts → trip once when it ages
//! past the threshold → recover/re-arm when fresh data resumes), reimplemented here as a tiny
//! self-contained latch rather than importing that type: `StreamHealth` lives in `vike-bridge-core`,
//! and `vike-core` deliberately depends on `vike-exec` + `vike-model` ONLY (down-only layering). The
//! logic is trivial enough that a 4-field latch is not a reinvented wheel — and it drops
//! `StreamHealth`'s transport-gap gating, which the core has no signal for anyway. Like a fresh
//! `StreamHealth` that is never `reset_freshness`-armed, a switch that has observed NO data yet never
//! trips: a core that never saw a live feed has nothing resting to cancel, and this avoids a
//! nuisance trip during pre-feed startup.

use std::path::PathBuf;
use std::time::Duration;

/// What the dead-man does when it trips. At least `CancelAll` and `CancelAllAndHalt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadManAction {
    /// Cancel every resting order. Trading state is left as-is, so a strategy could legitimately
    /// re-quote once data recovers — the switch just pulls the currently-exposed orders.
    CancelAll,
    /// Cancel every resting order AND engage HALT: set `trading_state = Halted` on every engine (the
    /// in-process `RiskGate` then denies every new order) and, when a sentinel path is configured
    /// ([`DeadManConfig::halt_file`]), write that HALT file so the venue adapter's own submit thread
    /// (`ExecActor`) also refuses new orders — the SAME sentinel the manual kill-switch uses, so
    /// new-order refusal is consistent across process boundaries.
    CancelAllAndHalt,
}

impl DeadManAction {
    /// Whether this action engages HALT (in-process trading-state + the cross-process sentinel).
    #[must_use]
    pub fn engages_halt(self) -> bool {
        matches!(self, DeadManAction::CancelAllAndHalt)
    }
}

/// Opt-in dead-man's-switch configuration. Hung off [`crate::CoreConfig`] as an `Option`, `None`
/// (default) = disabled and byte-identical to today (see the module doc).
#[derive(Debug, Clone)]
pub struct DeadManConfig {
    /// How long market data / venue events may be silent before the switch trips. Compared strictly
    /// (`now - newest_data_ts > timeout` ⇒ trip), mirroring `StreamHealth`'s freshness boundary.
    pub timeout: Duration,
    /// What to do on trip (cancel-all, optionally + HALT).
    pub action: DeadManAction,
    /// The HALT sentinel file to write when the action engages HALT. Supplied by the BINARY via
    /// `vike_bridge_core::halt_path_from_env()` — env reads stay in binaries (repo convention) and
    /// `vike-core` avoids a `vike-bridge-core` dependency. `None` skips the file write (the
    /// in-process `trading_state = Halted` gate still engages); set it to the process's real HALT
    /// path so the automatic switch and the manual `touch HALT` write the SAME sentinel.
    pub halt_file: Option<PathBuf>,
}

/// The pure trip-logic state machine. Clock-free: the caller feeds `ts` (on each observed
/// data/event ingest) and `now_ms` (at each boundary check), so every transition is unit-tested
/// with an injected clock and zero wall-clock sleeps. Built once from a [`DeadManConfig`] and owned
/// by the runtime's `CoreThread` (only when the feature is enabled).
#[derive(Debug)]
pub(crate) struct DeadMan {
    /// The staleness threshold in ms (`timeout`), clamped `>= 1` so `now - newest > threshold` is a
    /// well-defined strict comparison even for a pathological zero timeout.
    threshold_ms: i64,
    /// What to do on trip.
    action: DeadManAction,
    /// Newest observed data/event ts (monotonic-max). `None` = nothing observed yet ⇒ never trips
    /// (a core that never saw a live feed has nothing resting to cancel; mirrors an un-armed
    /// `StreamHealth`).
    newest_ts: Option<i64>,
    /// `true` once we tripped the CURRENT outage — so the trip fires exactly ONCE per outage rather
    /// than every boundary tick while data stays silent. Cleared on recovery (fresh data again),
    /// which re-arms the switch for the next outage.
    tripped: bool,
}

impl DeadMan {
    /// Build from a [`DeadManConfig`]. The threshold is clamped to `>= 1ms`.
    pub(crate) fn new(cfg: &DeadManConfig) -> Self {
        DeadMan {
            threshold_ms: (cfg.timeout.as_millis() as i64).max(1),
            action: cfg.action,
            newest_ts: None,
            tripped: false,
        }
    }

    /// The staleness threshold in ms — the runtime derives the timer cadence (`threshold / 2`) from
    /// it, mirroring the stuck-order watchdog.
    pub(crate) fn threshold_ms(&self) -> i64 {
        self.threshold_ms
    }

    /// Record a data/event ingest at `ts`. Monotonic-max so an out-of-order older frame can't drag
    /// freshness backward (identical rule to `StreamHealth::observe_data`). Call on every liveness
    /// message (venue events + market data), NEVER on control/waker messages.
    pub(crate) fn observe(&mut self, ts: i64) {
        self.newest_ts = Some(self.newest_ts.map_or(ts, |n| n.max(ts)));
    }

    /// Evaluate at `now_ms`. Returns `Some(action)` exactly ONCE when the newest observed data has
    /// aged strictly past the threshold and the switch has not already tripped this outage; `None`
    /// otherwise. Recovery (data fresh again) clears the trip latch, re-arming the switch for the
    /// next outage. Never trips before any data has been observed.
    pub(crate) fn check(&mut self, now_ms: i64) -> Option<DeadManAction> {
        let newest = self.newest_ts?;
        let stale = now_ms - newest > self.threshold_ms;
        if stale {
            if self.tripped {
                return None; // already tripped this outage — fire once, not every tick
            }
            self.tripped = true;
            Some(self.action)
        } else {
            self.tripped = false; // fresh again ⇒ re-arm for the next outage
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn is_tripped(&self) -> bool {
        self.tripped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(ms: u64, action: DeadManAction) -> DeadManConfig {
        DeadManConfig { timeout: Duration::from_millis(ms), action, halt_file: None }
    }

    #[test]
    fn action_engages_halt_predicate() {
        assert!(!DeadManAction::CancelAll.engages_halt());
        assert!(DeadManAction::CancelAllAndHalt.engages_halt());
    }

    // (a) OFF-by-default sense at the pure level: fresh, advancing data never trips; and a switch
    // that has observed NOTHING never trips no matter how far `now` runs.
    #[test]
    fn advancing_data_never_trips() {
        let mut dm = DeadMan::new(&cfg(1000, DeadManAction::CancelAll));
        for t in [0i64, 500, 1000, 1500, 2000] {
            dm.observe(t);
            assert_eq!(dm.check(t), None, "fresh data at t={t} must not trip");
        }
        assert!(!dm.is_tripped());
    }

    #[test]
    fn no_data_observed_never_trips() {
        let mut dm = DeadMan::new(&cfg(1000, DeadManAction::CancelAll));
        assert_eq!(dm.check(10_000_000), None, "nothing observed ⇒ nothing to protect ⇒ no trip");
        assert!(!dm.is_tripped());
    }

    // (b) trips after the timeout of no fresh data — strict boundary (== threshold is still fresh).
    #[test]
    fn trips_after_timeout_of_no_data() {
        let mut dm = DeadMan::new(&cfg(1000, DeadManAction::CancelAll));
        dm.observe(0);
        assert_eq!(dm.check(1000), None, "lag == threshold is fresh (strict >)");
        assert_eq!(dm.check(1001), Some(DeadManAction::CancelAll), "one ms past threshold ⇒ trip");
        assert!(dm.is_tripped());
    }

    #[test]
    fn trips_exactly_once_per_outage() {
        let mut dm = DeadMan::new(&cfg(1000, DeadManAction::CancelAllAndHalt));
        dm.observe(0);
        assert_eq!(dm.check(2000), Some(DeadManAction::CancelAllAndHalt), "first stale tick trips");
        // stays silent — later ticks in the SAME outage do not re-fire
        assert_eq!(dm.check(3000), None);
        assert_eq!(dm.check(9999), None);
        assert!(dm.is_tripped());
    }

    // (d) recovery re-arms — a fresh frame clears the latch, and a SECOND outage trips again.
    #[test]
    fn recovery_re_arms_for_the_next_outage() {
        let mut dm = DeadMan::new(&cfg(1000, DeadManAction::CancelAll));
        dm.observe(0);
        assert_eq!(dm.check(2000), Some(DeadManAction::CancelAll), "first outage trips");
        assert!(dm.is_tripped());
        // fresh data resumes
        dm.observe(3000);
        assert_eq!(dm.check(3000), None, "fresh again ⇒ recovery, no trip");
        assert!(!dm.is_tripped(), "recovery re-armed the switch");
        // a NEW outage after recovery trips again
        assert_eq!(dm.check(4001), Some(DeadManAction::CancelAll), "second outage trips again");
        assert!(dm.is_tripped());
    }

    #[test]
    fn out_of_order_older_frame_does_not_re_arm() {
        let mut dm = DeadMan::new(&cfg(1000, DeadManAction::CancelAll));
        dm.observe(5000);
        // an out-of-order OLDER frame must not drag freshness backward
        dm.observe(1000);
        assert_eq!(dm.check(5500), None, "aged from newest (5000), lag 500 ⇒ fresh");
        assert_eq!(dm.check(6001), Some(DeadManAction::CancelAll), "aged from 5000, not 1000");
    }
}
