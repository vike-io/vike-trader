//! **A depth feed that keeps faulting must SAY SO in the journal** — the disclosure half of the
//! binance perp reconnect loop.
//!
//! # The hole
//!
//! `vike_bridge_core::depth`'s `run_depth_feed` disclosed a fault exactly one way: a `HealthEvent`
//! the venue maps onto a `vike_data::StreamStatus`. In a HEADLESS process that went nowhere a human
//! could see — `crates/bridges/binance/src/family/market_feed.rs`'s `set_status` writes a
//! `String` into a mutex for a GUI status bar and `depth_main` never called it, and
//! `crates/vike-data/src/live_rec.rs`'s `RecorderSink::stream_status` recorded markers for the
//! `book` lane only. So the CI box's `kind=depth/venue=binance/symbol=BTCUSDT.P` reconnect-looped for
//! forty days at 4 % of its expected rate with **zero** warnings of any severity in the recorder
//! journal. It was found by measuring the stored series, which is not a disclosure mechanism.
//!
//! # Why the throttle is the interesting part
//!
//! `StreamHealth::enter_gap` already dedups an outage to one `Gap`, and an edge-triggered log on it
//! would have been useless HERE: every cycle of that loop seeded, published a book (closing the gap
//! through `StreamHealth::recover`) and then faulted, so each 4.3 s cycle was a fully-recovered
//! outage in the health tracker's eyes — ~20,000 lines a day into a file layer that defaults to
//! `trace`, which is the shape that once wrote 341 GB (root `CLAUDE.md`, Logging). So
//! `DepthFaultLog` speaks the FIRST fault immediately and then at most one line per
//! `FAULT_LOG_EVERY`, carrying how many faults that line covers — the RATE, which is the signal an
//! operator actually needs.
//!
//! # Why its own binary, and why the capture is hand-rolled
//!
//! `tracing` caches an `Interest` verdict PER CALLSITE, process-globally, written by whichever
//! thread reaches it first — so a capture sharing a binary with a test that drives the same callsite
//! WITHOUT a subscriber finds the line switched off ("passes alone, fails together"). One test file
//! is one binary; the subscriber is installed once, before the first emit, and
//! `tracing::callsite::rebuild_interest_cache()` runs after the install regardless.
//! `tracing-subscriber` is not a dependency of this crate, so the capture is a minimal
//! [`Subscriber`] enabled only for the depth module's target.
//!
//! # Kill proof
//!
//! Delete the `tracing::warn!` from `DepthFaultLog::fault` (leaving the throttle arithmetic intact)
//! and the first assertion below reads zero captured lines and fails; delete the
//! `tracing::info!` from `recovered` and the recovery assertion fails. Widen the throttle to speak
//! on every fault and the line counts in `a_faulting_depth_feed_reaches_the_journal_and_the_throttle_holds_it_to_a_rate` fail.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once, OnceLock};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber, span};
use vike_bridge_core::depth::{DepthFaultLog, FAULT_LOG_EVERY};

/// The tracing target `DepthFaultLog` stamps on both of its callsites.
const TARGET: &str = "vike_bridge_core::depth";

/// A real perp depth URL — the lane that failed.
const URL: &str = "wss://fstream.binance.com/ws/btcusdt@depth@100ms";
/// The error text `run_depth_session` returns on a misread sequence gap, verbatim.
const GAP_ERR: &str = "depth sequence gap — resync";

fn captured() -> &'static Mutex<Vec<String>> {
    static LOG: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(Vec::new()))
}

fn test_init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(Capture { next_span: AtomicU64::new(1) });
        tracing::callsite::rebuild_interest_cache();
    });
}

fn lines() -> Vec<String> {
    captured().lock().expect("capture poisoned").clone()
}

struct Capture {
    next_span: AtomicU64,
}

impl Subscriber for Capture {
    /// Only the depth module's events — everything else in the process is dropped at the callsite.
    /// Both WARN (fault) and INFO (recovery) are wanted, so the level is asserted from the captured
    /// text rather than filtered here.
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == TARGET
            && (*metadata.level() == Level::WARN || *metadata.level() == Level::INFO)
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut v = FieldVisitor { out: format!("{} ", event.metadata().level()) };
        event.record(&mut v);
        captured().lock().expect("capture poisoned").push(v.out);
    }
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

/// Every field, rendered `name=value`, so the assertions can read the structured half (`faults`,
/// `url`, `error`) and not only the human message.
struct FieldVisitor {
    out: String,
}

impl FieldVisitor {
    fn push(&mut self, field: &Field, value: &str) {
        self.out.push_str(field.name());
        self.out.push('=');
        self.out.push_str(value);
        self.out.push(' ');
    }
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        self.push(field, &rendered);
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value);
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        let rendered = value.to_string();
        self.push(field, &rendered);
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        let rendered = value.to_string();
        self.push(field, &rendered);
    }
}

/// ONE test, deliberately: the capture buffer is process-global and the assertions below are about
/// how many lines exist AFTER a given sequence, so splitting them into separate `#[test]`s would
/// make them race each other inside one binary.
#[test]
fn a_faulting_depth_feed_reaches_the_journal_and_the_throttle_holds_it_to_a_rate() {
    test_init();
    let mut log = DepthFaultLog::new();

    // --- the FIRST fault speaks immediately: an operator must not wait a minute to learn a lane
    // --- has stopped working.
    assert!(log.fault(URL, GAP_ERR, 0, FAULT_LOG_EVERY), "the first fault of a streak speaks");
    let after_first = lines();
    assert_eq!(after_first.len(), 1, "exactly one line so far: {after_first:?}");
    let msg = &after_first[0];
    assert!(msg.starts_with("WARN "), "a feed that stopped working is a WARN: {msg}");
    assert!(msg.contains(URL), "the line names the lane: {msg}");
    assert!(msg.contains(GAP_ERR), "…and the error that killed the session: {msg}");
    assert!(msg.contains("faults=1"), "…and how many faults it covers: {msg}");

    // --- THE DEFECT'S CADENCE. The broken lane faulted every ~4.3 s. Drive fourteen cycles inside
    // --- one throttle window: all of them are recorded, NONE of them is spoken.
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

    // --- …and once the window elapses it speaks ONCE, carrying the whole burst. That count is the
    // --- signal: "13 faults in the last minute" is a broken lane; "1" is a blip.
    let past_window = FAULT_LOG_EVERY.as_millis() as i64 + 1;
    assert!(log.fault(URL, GAP_ERR, past_window, FAULT_LOG_EVERY), "the window elapsed — speak");
    let after_window = lines();
    assert_eq!(after_window.len(), 2, "one line per window: {after_window:?}");
    assert!(
        after_window[1].contains("faults=14"),
        "the throttled line must carry every fault it covers, or the throttle HIDES the rate \
         instead of summarising it: {}",
        after_window[1]
    );
    assert!(after_window[1].contains("streak=15"), "…and the streak so far: {}", after_window[1]);

    // --- RECOVERY is a boundary too, and it is an INFO: a lane that came back must be visible or
    // --- an operator reading the warning above cannot tell a resolved incident from a live one.
    assert!(log.recovered(URL), "a streak was open, so recovery speaks");
    let after_recovery = lines();
    assert_eq!(after_recovery.len(), 3, "{after_recovery:?}");
    assert!(
        after_recovery[2].starts_with("INFO "),
        "recovery is not a warning: {}",
        after_recovery[2]
    );
    assert!(
        after_recovery[2].contains("faults=15"),
        "…and reports the whole streak: {}",
        after_recovery[2]
    );

    // --- A healthy feed reaching its scheduled re-seed says NOTHING. `run_depth_feed` calls
    // --- `recovered` on every `SessionOutcome::Reseed`, i.e. every 5 minutes on every depth lane
    // --- in the process; if that were not silent this whole mechanism would be the log spam it
    // --- exists to avoid.
    assert!(!log.recovered(URL), "no streak was open — recovery must be silent");
    assert_eq!(lines().len(), 3, "a healthy re-seed writes nothing: {:?}", lines());

    // --- …and the throttle RESET with the streak: the next incident is not silenced by the fact
    // --- that the previous one happened to end inside the same window.
    assert!(
        log.fault(URL, GAP_ERR, past_window + 1, FAULT_LOG_EVERY),
        "a NEW incident speaks immediately even though the last line is one millisecond old"
    );
    let after_new = lines();
    assert_eq!(after_new.len(), 4, "{after_new:?}");
    assert!(after_new[3].contains("faults=1"), "a fresh streak counts from one: {}", after_new[3]);
}

/// The throttle window is a CONSTANT with a stated size, not a number picked at a call site — and
/// it must stay far above the measured 4.3 s fault cadence of the defect that motivated it, or the
/// mechanism degenerates back into one line per reconnect.
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
