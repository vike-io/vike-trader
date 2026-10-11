use super::*;
use std::assert_matches;

#[test]
fn history_query_shapes() {
    assert_eq!(trades_query(50), "limit=50");
    assert_eq!(orders_query(25), "limit=25");
}

#[test]
fn bodies() {
    let ob = serde_json::json!({"salt":"1","side":"BUY"});
    let s = submit_body(ob, "api-key-1", "GTC");
    assert_eq!(s["owner"], "api-key-1");
    assert_eq!(s["orderType"], "GTC");
    assert_eq!(s["order"]["side"], "BUY");
    assert_eq!(cancel_body("0xdeadbeef")["orderID"], "0xdeadbeef");
}

// --- the client-side rate-budget mirror ---------------------------------------------------
//
// `crate::exec_plane::rate_budget` is a pure deterministic mirror (time is injected as `now_ms`, it calls
// no `Instant::now()` itself), so every decision this module makes from it is driven here with
// an explicit clock and an explicitly-constructed budget — no sleeping, no network, no
// process-env mutation (`set_var` is unsound under threads — the repo-wide rule).
//
// Standard tier throughout: capacity 40 tokens per bucket, reserve 25% = 10.

/// Capability 1, and the CONTRACT of this PR: with `POLY_RATE_GATE` unset the gate can only
/// ever return `Ok`, so an over-budget submit still reaches the wire exactly as it does today.
/// Turning the flag on converts the same verdict into a named refusal — which the exec thread
/// maps to an `OrderRejected`, so no order silently vanishes either way.
#[test]
fn the_submit_gate_is_inert_while_the_flag_is_unset() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    // A healthy bucket allows in both modes.
    assert_eq!(submit_gate_action(rb.submit_decision(0), false), Ok(()));
    assert_eq!(submit_gate_action(rb.submit_decision(0), true), Ok(()));

    let before = rate_gate_would_block_count();
    for _ in 0..40 {
        rb.on_submit(0); // drain the order bucket
    }
    let dry = rb.submit_decision(0);
    assert_matches!(dry, SubmitDecision::Throttle { .. }, "expected a dry bucket: {dry:?}");

    // OBSERVE-ONLY (the default): logged + counted, but the order still goes out.
    assert_eq!(submit_gate_action(dry, false), Ok(()));
    // ENFORCED (`venue.polymarket.rate_gate` = `1`): a refusal naming the real cause.
    let refused = submit_gate_action(dry, true).expect_err("enforced mode must refuse");
    assert!(refused.contains("rate budget"), "the reason must name the cause: {refused}");
    // Both modes produce the evidence for deciding to enforce.
    assert!(rate_gate_would_block_count() >= before + 2);
}

/// The venue's settings holding one machine-scoped `venue.polymarket.<field>` row — the shape the
/// composition root reads out of the settings database (decision 0095).
fn settings(field: &str, value: &str) -> vike_secrets::venue_setting::VenueSettings {
    vike_secrets::venue_setting::VenueSettings::from_rows(
        "polymarket",
        &[vike_secrets::VenueSettingRow {
            venue: "polymarket".to_string(),
            tier: None,
            field: field.to_ascii_uppercase(),
            value: value.to_string(),
        }],
    )
}

/// The enforcement flag is OFF by default and accepts the EXACT string `"1"` only, read from the
/// venue's `venue.polymarket.rate_gate` row (decision 0095: the settings database, no environment
/// layer and — since Task 7 — no credential-map fold) — the `POLY_EXEC` idiom, trailing-comment
/// tolerance included.
#[test]
fn the_rate_gate_flag_is_off_by_default_and_exact() {
    assert!(!rate_gate_enforced(&vike_secrets::venue_setting::VenueSettings::default()));
    assert!(rate_gate_enforced(&settings("rate_gate", "1")));
    assert!(rate_gate_enforced(&settings("rate_gate", " 1 ")));
    assert!(rate_gate_enforced(&settings("rate_gate", "1  # enforce once the headers agree")));
    for off in ["0", "true", "yes", "on", "", "11", "1x"] {
        assert!(!rate_gate_enforced(&settings("rate_gate", off)), "{off:?}");
    }
    // Another field's row is not this one.
    assert!(!rate_gate_enforced(&settings("presubmit_register", "1")));
}

/// Capability 3: `cancel-all` is taken only while its `1 + n` debit provably cannot overshoot.
/// The squeezed case is the exact hazard the deep-dive flagged — a bulk call that would drive
/// the cancel bucket NEGATIVE and lock out every subsequent cancel.
#[test]
fn a_cancel_all_that_would_overshoot_picks_targeted_instead() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    // Wide open: 1 + 10 tokens is affordable AND leaves the reserve intact → one bulk call.
    let fat = plan_cancels(&rb, 10, CancelScope::WholeBook, CancelIntent::Routine, 0);
    assert_eq!(fat.strategy, CancelStrategy::CancelAll);
    assert_eq!((fat.send, fat.deferred), (10, 0));

    // Snap the cancel bucket to 12 the way a venue header would, then ask for 15:
    // 1 + 15 = 16 > 12, so `cancel-all` would go negative → targeted.
    rb.reconcile(&RateLimitSignal { cancel_remaining: Some(12.0), ..Default::default() }, 0);
    let squeezed = plan_cancels(&rb, 15, CancelScope::WholeBook, CancelIntent::Routine, 0);
    assert_eq!(squeezed.strategy, CancelStrategy::Targeted);
    // …and the reserve floor (10) caps this routine pass at the 2 tokens sitting above it.
    assert_eq!((squeezed.send, squeezed.deferred), (2, 13));
    assert!(squeezed.retry_ms > 0, "a deferred remainder must carry a refill ETA");

    // A risk-off flatten of the same book ignores the reserve and sends every id.
    let flatten = plan_cancels(&rb, 15, CancelScope::WholeBook, CancelIntent::RiskOff, 0);
    assert_eq!(flatten.strategy, CancelStrategy::Targeted);
    assert_eq!((flatten.send, flatten.deferred, flatten.retry_ms), (15, 0, 0));
}

/// The case the `ExecCommand::CancelBatch` seam exists FOR, and the one the earlier tests
/// never reach: the SAME whole-book batch is held back under `Routine` and goes out as ONE
/// account-wide `cancel-all` under `RiskOff`. That is the trade an emergency wants — `1 + n`
/// tokens instead of `n`, but one round trip instead of `n` — and it is only affordable
/// because the reserve refused the routine churn that would otherwise have spent it.
#[test]
fn a_riskoff_batch_takes_the_bulk_arm_the_reserve_holds_back_from_routine() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    // Snap the cancel bucket to 12 the way a venue header would. `1 + 6 = 7` is affordable
    // outright, so the venue guard clears the bulk arm — the ONLY thing left deciding is which
    // intent may spend down through the reserve floor (10).
    rb.reconcile(&RateLimitSignal { cancel_remaining: Some(12.0), ..Default::default() }, 0);

    let routine = plan_cancels(&rb, 6, CancelScope::WholeBook, CancelIntent::Routine, 0);
    assert_eq!(routine.strategy, CancelStrategy::Targeted, "churn may not spend the reserve");
    assert_eq!((routine.send, routine.deferred), (2, 4), "only the 2 tokens above the floor");
    assert!(routine.retry_ms > 0, "a deferred remainder must carry a refill ETA");

    let flatten = plan_cancels(&rb, 6, CancelScope::WholeBook, CancelIntent::RiskOff, 0);
    assert_eq!(flatten.strategy, CancelStrategy::CancelAll, "ONE round trip, not six");
    assert_eq!((flatten.send, flatten.deferred, flatten.retry_ms), (6, 0, 0));
}

/// Capability 2, the reserve invariant: routine churn stops with the flatten reserve intact,
/// and a flatten issued right after it still fires every id.
#[test]
fn the_reserve_floor_holds_under_routine_churn() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    let plan = plan_cancels(&rb, 40, CancelScope::WholeBook, CancelIntent::Routine, 0);
    // 1 + 40 = 41 > 40 tokens, so the bulk arm is out on the venue guard alone…
    assert_eq!(plan.strategy, CancelStrategy::Targeted);
    // …and the reserve caps the churn at 30 of the 40 ids.
    assert_eq!((plan.send, plan.deferred), (30, 10));
    assert!(plan.retry_ms > 0);

    // Spending exactly what the plan authorises leaves the reserve untouched.
    for _ in 0..plan.send {
        rb.on_cancel(0);
    }
    let left = rb.cancel_available(0);
    assert!(left >= 10.0 - 1e-9, "routine churn breached the reserve: {left}");

    // Which is the whole point: the flatten that follows is NOT rate-limit-locked.
    let flatten = plan_cancels(&rb, 10, CancelScope::WholeBook, CancelIntent::RiskOff, 0);
    assert_eq!((flatten.send, flatten.deferred), (10, 0));
}

/// Capability 4, driven exactly as [`log_rate_signal`] drives it: the venue's own header pairs
/// → `parse_headers` → `fold_rate_signal`. A drifted bucket snaps to the server's number; a
/// response carrying no rate headers at all leaves the mirror untouched.
#[test]
fn a_header_reconcile_snaps_a_drifted_bucket() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    for _ in 0..40 {
        rb.on_submit(0);
    }
    assert!(rb.order_available(0) < 1.0, "the bucket must be drained first");

    let sig = crate::exec_plane::rate_budget::parse_headers([
        ("Poly-RateLimit-Warning", "1"),
        ("poly-ratelimit-order-remaining", "25"),
        ("poly-ratelimit-cancel-remaining", "7"),
    ]);
    assert!(fold_rate_signal(&mut rb, &sig, 0), "an actionable signal must be applied");
    assert!((rb.order_available(0) - 25.0).abs() < 1e-9);
    assert!((rb.cancel_available(0) - 7.0).abs() < 1e-9);

    let quiet =
        crate::exec_plane::rate_budget::parse_headers([("content-type", "application/json")]);
    assert!(!fold_rate_signal(&mut rb, &quiet, 0), "a bare response must change nothing");
    assert!((rb.order_available(0) - 25.0).abs() < 1e-9);
}

/// `DELETE /cancel-all` is ACCOUNT-WIDE, so a subset request may never take it however much
/// budget is free — and an empty request is a no-op plan, never a bulk call.
#[test]
fn a_subset_never_takes_the_account_wide_bulk_arm() {
    let rb = RateBudget::new(Tier::Standard, 0);
    let subset = plan_cancels(&rb, 5, CancelScope::Subset, CancelIntent::Routine, 0);
    assert_eq!(subset.strategy, CancelStrategy::Targeted);
    assert_eq!((subset.send, subset.deferred), (5, 0));

    let empty = plan_cancels(&rb, 0, CancelScope::WholeBook, CancelIntent::RiskOff, 0);
    assert_eq!(
        empty,
        CancelPlan { strategy: CancelStrategy::Targeted, send: 0, deferred: 0, retry_ms: 0 }
    );
}

/// A deferred routine pass is a DELAY, not a deadlock: the reported ETA really does unblock
/// churn again, and a fully refilled bucket clears the whole batch in one bulk call.
#[test]
fn a_deferred_routine_pass_recovers_after_the_reported_refill() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    rb.reconcile(&RateLimitSignal { cancel_remaining: Some(10.0), ..Default::default() }, 0);
    let stalled = plan_cancels(&rb, 4, CancelScope::WholeBook, CancelIntent::Routine, 0);
    assert_eq!((stalled.send, stalled.deferred), (0, 4), "sitting exactly on the floor");
    assert!(stalled.retry_ms > 0);

    // The ETA is "one token above the reserve again", so it unblocks the pass, not the batch.
    let after =
        plan_cancels(&rb, 4, CancelScope::WholeBook, CancelIntent::Routine, stalled.retry_ms);
    assert!(after.send > 0, "the reported ETA must actually unblock churn: {after:?}");

    // A full second of refill (Standard = 40 tokens/s) restores the cheap bulk path.
    let rested = plan_cancels(&rb, 4, CancelScope::WholeBook, CancelIntent::Routine, 1_000);
    assert_eq!(rested.strategy, CancelStrategy::CancelAll);
    assert_eq!((rested.send, rested.deferred), (4, 0));
}

/// A cancel budget sitting EXACTLY on the reserve floor: Standard tier is a 40-token bucket
/// with a 25% reserve, so 10 remaining is the boundary the per-order gate must act on.
fn budget_at_the_reserve_floor() -> RateBudget {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    rb.reconcile(&RateLimitSignal { cancel_remaining: Some(10.0), ..Default::default() }, 0);
    rb
}

/// THE POINT OF THE WHOLE CHANGE: at the reserve floor a ROUTINE cancel is held back, and the
/// refusal says when to retry. Above the floor it fires like any other cancel.
#[test]
fn the_reserve_holds_back_a_routine_cancel() {
    let mut rb = budget_at_the_reserve_floor();
    let shed = gate_cancel(&mut rb, CancelIntent::Routine, 0)
        .expect_err("a routine cancel at the reserve floor must be shed");
    assert!(shed.contains("reserve"), "the refusal must name why: {shed}");
    assert!(shed.contains("retry"), "…and when to retry: {shed}");

    // Standard refills 40 tokens/s, so a quarter second is comfortably back above the floor.
    assert!(
        gate_cancel(&mut rb, CancelIntent::Routine, 250).is_ok(),
        "above the floor, routine churn is ordinary"
    );
}

/// …and the reserve exists so that THIS one still fires. Same budget, same instant, drained
/// far past the floor and even NEGATIVE (the `cancel-all` over-debit model): an emergency
/// cancel is never held back, which is what the held-back tokens were being held FOR.
#[test]
fn the_reserve_never_holds_back_an_emergency_cancel() {
    let mut rb = budget_at_the_reserve_floor();
    assert!(gate_cancel(&mut rb, CancelIntent::RiskOff, 0).is_ok());

    rb.on_cancel_all(60, 0); // drive the bucket negative
    assert!(rb.cancel_available(0) < 0.0, "fixture: the bucket must be negative");
    assert!(
        gate_cancel(&mut rb, CancelIntent::RiskOff, 0).is_ok(),
        "a flatten fires even from a locked-out bucket — a 429 beats not trying"
    );
}

/// The backward-compatibility claim, as a test: a cancel that named no intent behaves EXACTLY
/// as this venue behaved before the intent existed — it fires, and the gate neither reads nor
/// moves the bucket on its way through.
#[test]
fn an_unspecified_cancel_behaves_exactly_as_today() {
    let mut rb = budget_at_the_reserve_floor();
    let before = rb.cancel_available(0);
    assert!(
        gate_cancel(&mut rb, CancelIntent::Unspecified, 0).is_ok(),
        "an unclassified cancel is treated as the flatten, never shed"
    );
    assert!(
        (rb.cancel_available(0) - before).abs() < 1e-9,
        "the gate must not debit — `cancel_order` owns the debit, and only for what it sends"
    );

    rb.on_cancel_all(60, 0); // negative bucket: still not this gate's business
    assert!(gate_cancel(&mut rb, CancelIntent::Unspecified, 0).is_ok());
}

/// The relayer header pair is now built in ONE place ([`relayer_headers`]) for submit, cancel
/// and the bulk cancel — pin its exact wire names, since the deposit-wallet flow is rejected
/// outright if either is misspelled.
#[test]
fn relayer_headers_keep_the_exact_wire_names() {
    assert_eq!(
        relayer_headers("rk", "0xaddr"),
        vec![
            ("RELAYER_API_KEY".to_string(), "rk".to_string()),
            ("RELAYER_API_KEY_ADDRESS".to_string(), "0xaddr".to_string()),
        ]
    );
}
