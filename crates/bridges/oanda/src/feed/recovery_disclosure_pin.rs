//! **The bars lane must rewrite its healthy string on RECOVERY, not only at seed time** — the
//! oanda half of the 2026-09-10 feed-health latch.
//!
//! `bars_main` polls REST on a cadence rather than riding a session, so it has no
//! `WebsocketSessionStatus::Live` arm for `vike_bridge_core::websocket_session::market`'s fix to reach: the quote lane
//! rides the shared driver, but this poll calls none of its entry points, which is exactly why
//! `crates/vike-ops/tests/venues/feed_success_disclosure_gate.rs`'s call-site scan is structurally blind to it.
//! This pin is where that blindness is answered.
//!
//! The defect this pins was STRICTLY WORSE than bybit's: the healthy write sat inside
//! `if !seeded`, so after one failed poll wrote `"… poll failed (HTTP …); retrying"` — stated
//! `Error` — every LATER SUCCESSFUL poll took the `else` branch and rewrote nothing. Permanent,
//! with no session boundary to hang a cure on, on a status mutex shared last-writer-wins with
//! `quotes_main`.
//!
//! This is a SOURCE pin over the recovery arm. The behaviour is held too, a seed, a failure and a
//! recovery a poll cadence apart, by `crates/bridges/oanda/src/feed/feed_tests.rs`'s
//! `a_failed_bar_poll_states_error_and_the_recovering_poll_states_connected`. The pin has the same
//! shape as bybit's retired `healthy_string_pin` (its feed now rides the data-client engine),
//! including its lesson:
//! COMMENT lines are skipped, because the comments in this file quote the very strings being
//! reasoned about and a scan that read them would be green on a broken tree.

const SOURCE: &str = include_str!("bar/mod.rs");

/// The lines of `bars_main`'s `health.recover()` arm, comments dropped.
fn recovery_arm() -> Vec<String> {
    let at = SOURCE
        .find("fn bars_main(")
        .expect("`bars_main` has been renamed — re-anchor this pin, do not delete it");
    let body = &SOURCE[at..];
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
    let healthy_write = "set_status(DataClientConnectionState::Connected, live_status";
    assert!(
        arm.iter().any(|l| l.contains(healthy_write)),
        "the bars lane recovers its typed StreamStatus but NOT its status line, so a failed \
             poll latches an Error state that no later successful poll rewrites — the \
             2026-09-10 the CI box latch, in the one venue the shared driver's fix cannot reach. Write \
             `live_status` here, stated Connected. Arm was: {arm:?}"
    );
}
