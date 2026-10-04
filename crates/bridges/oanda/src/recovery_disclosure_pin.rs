//! **The bars lane must rewrite its healthy string on RECOVERY, not only at seed time** — the
//! oanda half of the 2026-09-10 feed-health latch.
//!
//! `bars_main` polls REST on a cadence rather than riding a session, so it has no
//! `SessionStatus::Live` arm for `vike_bridge_core::market_pump`'s fix to reach: this venue is
//! `OwnPump` in `vike_bridge_core::pump_spec` and calls none of the shared driver's entry
//! points, which is exactly why `crates/vike-ops/tests/feed_success_disclosure_gate.rs`'s
//! roster scan is structurally blind to it (its `OWN_PUMP_RESIDUALS` table is where that
//! blindness is declared, with this venue's verdict).
//!
//! The defect this pins was STRICTLY WORSE than bybit's: the healthy write sat inside
//! `if !seeded`, so after one failed poll wrote `"… poll failed (HTTP …); retrying"` — which
//! `vike_model::feed_status::parse_feed_status` reads as `Error` — every LATER SUCCESSFUL poll
//! took the `else` branch and rewrote nothing. Permanent, with no session boundary to hang a
//! cure on, on a status mutex shared last-writer-wins with `quotes_main`.
//!
//! A behavioural test would need a live REST endpoint (`bars_main` dials one on its first
//! line), so this is a SOURCE pin over the recovery arm — the same shape as
//! `crates/bridges/bybit/src/market_feed.rs`'s `healthy_string_pin`, including its lesson:
//! COMMENT lines are skipped, because the comments in this file quote the very strings being
//! reasoned about and a scan that read them would be green on a broken tree.

const SRC: &str = include_str!("market_feed.rs");

/// The lines of `bars_main`'s `health.recover()` arm, comments dropped.
fn recovery_arm() -> Vec<String> {
    let at = SRC
        .find("fn bars_main(")
        .expect("`bars_main` has been renamed — re-anchor this pin, do not delete it");
    let body = &SRC[at..];
    let start = body
        .find("health.recover()")
        .expect("the bars lane no longer computes a recovery edge — the pin cannot see it");
    let arm = &body[start..];
    let end = arm.find("(ctx.wake)();").unwrap_or(arm.len());
    arm[..end].lines().map(|l| l.trim().to_string()).filter(|l| !l.starts_with("//")).collect()
}

/// The recovery edge must publish the healthy STRING, not only the typed `StreamStatus`. Those
/// are two different channels with two different consumers, and shipping only the typed half is
/// precisely the half-fix ig had made for itself.
#[test]
fn a_recovered_poll_rewrites_the_healthy_status_string() {
    let arm = recovery_arm();
    assert!(
        arm.iter().any(|l| l.contains("stream_status(")),
        "the typed disclosure has left the recovery arm — this pin is now anchored on the \
             wrong block: {arm:?}"
    );
    assert!(
        arm.iter().any(|l| l.contains("set_status(live_status")),
        "the bars lane recovers its typed StreamStatus but NOT its status string, so a failed \
             poll latches an Error-reading text that no later successful poll rewrites — the \
             2026-09-10 the CI box latch, in the one venue the shared driver's fix cannot reach. Write \
             `live_status` here. Arm was: {arm:?}"
    );
}

/// …and the text it writes must classify as healthy, or the rewrite is cosmetic. ig's old
/// `"IG stream up (…)"` passed a scan exactly like the one above and still parsed `Unknown`.
#[test]
fn the_bars_healthy_string_reads_as_connected() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};
    let s = "LIVE · OANDA bars EURUSD@1m";
    assert_eq!(parse_feed_status(s), ConnectionState::Connected, "{s}");
    // …and the failure text it replaces must NOT, or there was never anything to cure.
    assert_eq!(
        parse_feed_status("OANDA bars EURUSD@1m: poll failed (HTTP 502); retrying"),
        ConnectionState::Error,
        "the poll-failure text is what latched; if it stopped reading as Error this pin's \
             premise is gone and the whole block needs re-arguing"
    );
}
