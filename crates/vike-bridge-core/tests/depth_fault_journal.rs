//! **A depth feed that keeps faulting must SAY SO in the journal.**
//!
//! `vike_bridge_core::depth`'s `run_depth_feed` otherwise discloses a fault only as a
//! `vike_model::StreamStatus` handed to a sink, which a headless process shows to no one:
//! a binance perp depth lane once reconnect-looped at 4 % of its expected rate with zero journal
//! warnings.
//! `DepthFaultLog` is the journal half.
//!
//! # Why the throttle is the interesting part
//!
//! An edge-triggered log on `StreamHealth::enter_gap` would not help: each cycle of such a loop
//! seeds, publishes a book (closing the gap) and faults again, so every ~4.3 s cycle is a fully
//! recovered outage, ~20,000 lines a day into a file layer that defaults to `trace`. So
//! `DepthFaultLog` speaks the FIRST fault immediately, then at most one line per
//! `FAULT_LOG_EVERY` carrying how many faults it covers: the RATE is the operator's signal.
//!
//! # Why its own binary
//!
//! `tracing` caches an `Interest` per callsite, process-globally, written by whichever thread hits
//! it first, so a capture sharing a binary with a test that drives the same callsite uncaptured
//! loses the line. One file is one binary, and `vike_log::capture::global` installs the GLOBAL
//! capture for the depth module's target once, before the first emit.
//!
//! # Kill proof
//!
//! Delete the `tracing::warn!` from `DepthFaultLog::fault` and the first assertion below reads zero
//! lines; delete the `tracing::info!` from `recovered` and the recovery assertion fails; make the
//! throttle speak on every fault and the line counts fail.

use std::time::Duration;

use tracing::Level;
use vike_bridge_core::depth::{DepthFaultLog, FAULT_LOG_EVERY};
use vike_log::capture::{CapturedEvent, global};

/// The tracing target `DepthFaultLog` stamps on both of its callsites.
const TARGET: &str = "vike_bridge_core::depth";

/// Every line `DepthFaultLog` said so far (the first call installs the capture).
fn lines() -> Vec<CapturedEvent> {
    global(TARGET)
}

/// A real perp depth URL — the lane that failed.
const URL: &str = "wss://fstream.binance.com/ws/btcusdt@depth@100ms";
/// The error text `run_depth_session` returns on a misread sequence gap, verbatim.
const GAP_ERR: &str = "depth sequence gap — resync";

/// ONE test, deliberately: the capture buffer is process-global and the assertions count lines
/// after a sequence, so separate `#[test]`s would race each other inside one binary.
#[test]
fn a_faulting_depth_feed_reaches_the_journal_and_the_throttle_holds_it_to_a_rate() {
    lines();
    let mut log = DepthFaultLog::new();

    // --- the FIRST fault speaks immediately: no waiting a minute to learn a lane stopped working.
    assert!(log.fault(URL, GAP_ERR, 0, FAULT_LOG_EVERY), "the first fault of a streak speaks");
    let after_first = lines();
    assert_eq!(after_first.len(), 1, "exactly one line so far: {after_first:?}");
    let msg = &after_first[0];
    assert_eq!(msg.level, Level::WARN, "a feed that stopped working is a WARN: {msg:?}");
    assert_eq!(msg.field("url"), Some(URL), "the line names the lane: {msg:?}");
    assert_eq!(
        msg.field("error"),
        Some(GAP_ERR),
        "…and the error that killed the session: {msg:?}"
    );
    assert_eq!(msg.field("faults"), Some("1"), "…and how many faults it covers: {msg:?}");

    // --- A broken lane's ~4.3 s cadence: more cycles inside one throttle window are recorded,
    // --- NONE is spoken.
    for cycle in 1..=13i64 {
        assert!(
            !log.fault(URL, GAP_ERR, cycle * 4_300, FAULT_LOG_EVERY),
            "cycle {cycle} is inside the throttle window and must not speak"
        );
    }
    assert_eq!(
        lines().len(),
        1,
        "a reconnect loop must not write one line per cycle: {:?}",
        lines()
    );

    // --- …and once the window elapses it speaks ONCE, carrying the whole burst (the count is the
    // --- signal: many faults a minute is a broken lane, one is a blip).
    let past_window = FAULT_LOG_EVERY.as_millis() as i64 + 1;
    assert!(log.fault(URL, GAP_ERR, past_window, FAULT_LOG_EVERY), "the window elapsed — speak");
    let after_window = lines();
    assert_eq!(after_window.len(), 2, "one line per window: {after_window:?}");
    assert_eq!(
        after_window[1].field("faults"),
        Some("14"),
        "the throttled line must carry every fault it covers, or the throttle HIDES the rate \
         instead of summarising it: {:?}",
        after_window[1]
    );
    assert_eq!(after_window[1].field("streak"), Some("15"), "…and the streak so far");

    // --- RECOVERY speaks too, as INFO, so a resolved incident is told apart from a live one.
    assert!(log.recovered(URL), "a streak was open, so recovery speaks");
    let after_recovery = lines();
    assert_eq!(after_recovery.len(), 3, "{after_recovery:?}");
    assert_eq!(after_recovery[2].level, Level::INFO, "recovery is not a warning");
    assert_eq!(after_recovery[2].field("faults"), Some("15"), "…and reports the whole streak");

    // --- A healthy feed's scheduled re-seed says NOTHING: `run_depth_feed` calls `recovered` on
    // --- every `SessionOutcome::Reseed`, on every depth lane.
    assert!(!log.recovered(URL), "no streak was open — recovery must be silent");
    assert_eq!(lines().len(), 3, "a healthy re-seed writes nothing: {:?}", lines());

    // --- …and the throttle RESETS with the streak: a new incident is not silenced because the last
    // --- one ended inside the same window.
    assert!(
        log.fault(URL, GAP_ERR, past_window + 1, FAULT_LOG_EVERY),
        "a NEW incident speaks immediately even though the last line is one millisecond old"
    );
    let after_new = lines();
    assert_eq!(after_new.len(), 4, "{after_new:?}");
    assert_eq!(after_new[3].field("faults"), Some("1"), "a fresh streak counts from one");
}

/// The throttle window must stay far above a reconnect loop's ~4.3 s fault cadence, or the
/// mechanism degenerates into one line per reconnect.
#[test]
fn the_throttle_window_is_far_above_the_observed_fault_cadence() {
    assert!(
        FAULT_LOG_EVERY >= Duration::from_secs(30),
        "FAULT_LOG_EVERY ({FAULT_LOG_EVERY:?}) must stay well above the ~4.3 s reconnect cadence a \
         broken depth lane runs at, or a fault streak writes ~20,000 journal lines a day"
    );
    assert!(
        FAULT_LOG_EVERY <= Duration::from_secs(600),
        "…and low enough that a lane which breaks is reported inside an operator's attention span"
    );
}
