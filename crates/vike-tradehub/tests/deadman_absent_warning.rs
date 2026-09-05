//! The dead-man's ABSENT-KEY warning is EMITTED — once, through `tracing`, for an absent key and
//! for nothing else.
//!
//! `crates/vike-tradehub/src/tradehub_cli.rs`'s `deadman_absent_warning` is the pure decision and
//! is pinned in that file's own unit tests; `warn_deadman_absent` is the `Once` latch that turns
//! the decision into a `tracing::warn!`, and THAT is what a test with no subscriber cannot see. The
//! ruling this pins: the silence-observing dead-man is OPT-IN, off unless an operator writes
//! `policy.deadman_timeout_ms`, and an absent key is off WITH one warning while an explicit `0`
//! and an armed value are silent (`vike_config::Policy::deadman_timeout_ms` carries the reversal
//! and its reasons; this file carries only the emission).
//!
//! # Why its own binary, and why the capture is hand-rolled
//!
//! The lib's unit-test binary also runs `src/feed_splice_seam_tests.rs`, which drives
//! `live_mount_with` — and so this warning's callsite — on threads with no subscriber installed.
//! `tracing` caches a per-callsite `Interest` verdict PROCESS-WIDE, written by whichever thread
//! reaches the callsite first, so a capture that shares a binary with those tests can find the
//! line switched off before it is installed: "passes alone, fails together", the default failure
//! shape for a log assertion in a grouped binary. One test, one binary, one global subscriber
//! installed before the first emit — and `tracing::callsite::rebuild_interest_cache()` after the
//! install regardless, so a verdict cached earlier in this process is discarded rather than
//! trusted.
//!
//! The subscriber is the `audit_capture` shape from `tests/control_roundtrip.rs`: `tracing-subscriber`
//! is not a dependency of this crate, so this is a minimal [`Subscriber`] that is `enabled` ONLY
//! for the module's target and appends each event's rendered message to a shared buffer.
//!
//! # Kill proof
//!
//! With the `tracing::warn!` removed from `warn_deadman_absent` (the latch left in place, the pure
//! decision unchanged) the count below reads zero and the test fails — MEASURED before this file
//! was committed, both halves.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber, span};
use vike_tradehub::tradehub_cli::warn_deadman_absent;

/// The tracing target the warning's callsite carries — the module path of the function that
/// emits it.
const TARGET: &str = "vike_tradehub::tradehub_cli";

fn captured() -> &'static Mutex<Vec<String>> {
    static LOG: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(Vec::new()))
}

/// Install the capture subscriber exactly once for this binary, then discard any `Interest` a
/// thread cached before it existed. Best-effort by design: a failed install leaves the capture
/// EMPTY, which the assertion below then fails on loudly rather than passing vacuously.
fn test_init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(Capture { next_span: AtomicU64::new(1) });
        tracing::callsite::rebuild_interest_cache();
    });
}

/// How many WARN events about the dead-man key have been captured so far.
fn deadman_warnings() -> Vec<String> {
    captured()
        .lock()
        .expect("capture poisoned")
        .iter()
        .filter(|m| m.contains("deadman_timeout_ms"))
        .cloned()
        .collect()
}

struct Capture {
    next_span: AtomicU64,
}

impl Subscriber for Capture {
    /// ONLY this module's WARN events — everything else in the process is dropped at the callsite.
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == TARGET && *metadata.level() == Level::WARN
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        captured().lock().expect("capture poisoned").push(visitor.message);
    }
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

/// The rendered `message` field. `tracing::warn!("{message}")` records its text as the `message`
/// field's `Debug` (a `fmt::Arguments`, whose `Debug` is its `Display`), so this is the string the
/// operator would read.
#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}

/// ONE test, deliberately, so the order of the four calls is the order the assertions need: the
/// `Once` latch sits INSIDE the absent-key test, so an armed policy and an explicit zero must be
/// driven FIRST — a latch around the whole check would be consumed by them and the absent call
/// would then say nothing, which is the bug this ordering exists to catch.
#[test]
fn the_absent_key_warns_exactly_once_and_neither_zero_nor_an_armed_value_warns_at_all() {
    test_init();
    let base = vike_config::Policy::default();

    let armed = vike_config::Policy {
        deadman_timeout_ms: Some(vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS),
        ..base.clone()
    };
    warn_deadman_absent(&armed);
    assert!(
        deadman_warnings().is_empty(),
        "an armed switch must not warn: {:?}",
        deadman_warnings()
    );

    let decided = vike_config::Policy {
        deadman_timeout_ms: Some(vike_config::DEADMAN_DISABLED_MS),
        ..base.clone()
    };
    warn_deadman_absent(&decided);
    assert!(
        deadman_warnings().is_empty(),
        "an explicit 0 is the operator's decision and must not warn: {:?}",
        deadman_warnings()
    );

    // The absent key — `Policy::default()` — warns. Exactly once.
    assert_eq!(base.deadman_timeout_ms, None, "the default policy must carry an ABSENT key");
    warn_deadman_absent(&base);
    let after_first = deadman_warnings();
    assert_eq!(after_first.len(), 1, "an absent key warns exactly once: {after_first:?}");
    let msg = &after_first[0];
    assert!(msg.contains("DEAD-MAN SWITCH IS OFF"), "the headline: {msg}");
    assert!(msg.contains("<project>/settings/policy.toml"), "names the file: {msg}");
    assert!(msg.contains("`deadman_timeout_ms`"), "names the key: {msg}");
    assert!(msg.contains("cancel every resting"), "says what it would do: {msg}");
    assert!(
        msg.contains(&format!(
            "deadman_timeout_ms = {}\n",
            vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS
        )),
        "the paste-ready recommendation: {msg}"
    );
    assert!(msg.contains("deadman_timeout_ms = 0\n"), "…and the line that silences it: {msg}");

    // …and a second absent-key call in the same process is latched: one line, not one per site.
    warn_deadman_absent(&base);
    assert_eq!(deadman_warnings().len(), 1, "the latch: a second call must not repeat the block");
}
