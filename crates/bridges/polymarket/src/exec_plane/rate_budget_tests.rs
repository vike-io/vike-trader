use super::*;

const EPS: f64 = 1e-9;

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < EPS
}

#[test]
fn tier_selection_matches_documented_thresholds() {
    assert_eq!(Tier::from_volume_usd_30d(0.0), Tier::Standard);
    assert_eq!(Tier::from_volume_usd_30d(29_999.0), Tier::Standard);
    assert_eq!(Tier::from_volume_usd_30d(30_000.0), Tier::Copper);
    assert_eq!(Tier::from_volume_usd_30d(99_999.0), Tier::Copper);
    assert_eq!(Tier::from_volume_usd_30d(100_000.0), Tier::Silver);
    assert_eq!(Tier::from_volume_usd_30d(5_000_000.0), Tier::Silver);
    assert!(approx(Tier::Standard.per_sec(), 40.0));
    assert!(approx(Tier::Copper.per_sec(), 60.0));
    assert!(approx(Tier::Silver.per_sec(), 200.0));
}

#[test]
fn bucket_refills_at_rate_and_clamps_to_capacity() {
    let mut b = TokenBucket::new(40.0, 40.0, 0);
    assert!(b.try_take(40.0, 0)); // drain
    assert!(!b.try_take(1.0, 0)); // empty
    assert!(approx(b.available(500), 20.0)); // 0.5s * 40/s = 20
    assert!(approx(b.available(10_000), 40.0)); // long wait clamps at capacity
}

#[test]
fn bucket_never_refills_backwards_on_non_monotonic_clock() {
    let mut b = TokenBucket::new(40.0, 40.0, 1_000);
    b.take_saturating(10.0, 1_000);
    // A clock that went backwards must not accrue tokens.
    assert!(approx(b.available(500), 30.0));
}

#[test]
fn take_saturating_goes_negative_for_cancel_all_model() {
    let mut b = TokenBucket::new(5.0, 40.0, 0);
    b.take_saturating(8.0, 0); // 1 + 7 canceled, only 5 in bucket
    assert!(b.available(0) < 0.0);
    assert!(approx(b.available(0), -3.0));
}

#[test]
fn submit_allows_then_throttles_with_eta() {
    let mut rb = RateBudget::new(Tier::Standard, 0);
    // Drain the order bucket.
    for _ in 0..40 {
        assert_eq!(rb.submit_decision(0), SubmitDecision::Allow);
        rb.on_submit(0);
    }
    match rb.submit_decision(0) {
        SubmitDecision::Throttle { retry_ms } => assert!(retry_ms > 0 && retry_ms <= 25),
        other => panic!("expected throttle, got {other:?}"),
    }
    // 25ms → exactly one token at 40/s.
    assert_eq!(rb.submit_decision(25), SubmitDecision::Allow);
}

#[test]
fn routine_cancel_defers_at_reserve_but_flatten_always_fires() {
    // Standard: cap 40, reserve 25% = 10.
    let mut rb = RateBudget::new(Tier::Standard, 0);
    // Drain cancel bucket down toward the reserve via routine cancels.
    let mut allowed = 0;
    for _ in 0..40 {
        if let CancelDecision::Allow = rb.routine_cancel_decision(0) {
            rb.on_cancel(0);
            allowed += 1;
        } else {
            break;
        }
    }
    // Routine churn stops leaving ~the reserve (10) intact: allowed ~= 30.
    assert!((29..=30).contains(&allowed), "allowed={allowed}");
    assert!(rb.cancel_available(0) >= 10.0 - EPS);
    // A risk-off flatten ignores the decision and fires regardless, spending into the reserve.
    rb.on_cancel(0);
    rb.on_cancel(0);
    assert!(rb.cancel_available(0) < 10.0);
}

#[test]
fn cancel_strategy_prefers_targeted_when_cancel_all_would_go_negative() {
    let mut rb = RateBudget::new(Tier::Standard, 0); // cap 40
    // Plenty of budget -> cancel-all is fine.
    assert_eq!(rb.cancel_strategy(20, 0), CancelStrategy::CancelAll);
    // Drain to 5 tokens.
    rb.cancel.take_saturating(35.0, 0);
    assert!(approx(rb.cancel_available(0), 5.0));
    // 1 + 10 = 11 > 5 → would go negative → targeted.
    assert_eq!(rb.cancel_strategy(10, 0), CancelStrategy::Targeted);
    // 1 + 3 = 4 <= 5 → still safe to cancel-all.
    assert_eq!(rb.cancel_strategy(3, 0), CancelStrategy::CancelAll);
}

#[test]
fn reconcile_snaps_buckets_to_venue_remaining() {
    let mut rb = RateBudget::new(Tier::Silver, 0); // cap 200
    let sig = RateLimitSignal {
        order_remaining: Some(3.0),
        cancel_remaining: Some(150.0),
        ..Default::default()
    };
    rb.reconcile(&sig, 0);
    assert!(approx(rb.order_available(0), 3.0));
    assert!(approx(rb.cancel_available(0), 150.0));
    // Server value above capacity is clamped, never inflated.
    let over = RateLimitSignal { order_remaining: Some(9_999.0), ..Default::default() };
    rb.reconcile(&over, 0);
    assert!(approx(rb.order_available(0), 200.0));
}

#[test]
fn retier_rescales_capacity_and_reserve() {
    let mut rb = RateBudget::new(Tier::Standard, 0); // cap 40, reserve 10
    rb.retier(Tier::Silver, DEFAULT_RESERVE_FRAC, 0); // cap 200, reserve 50
    assert_eq!(rb.tier(), Tier::Silver);
    assert!(rb.cancel_available(0) <= 200.0 + EPS);
    // After retier a full-ish standard bucket is clamped to <= old fill, then refills to new cap.
    assert!(approx(rb.order_available(10_000), 200.0));
}

#[test]
fn parse_headers_reads_warning_and_numbers_case_insensitively() {
    let sig = parse_headers([
        ("Poly-RateLimit-Warning", "1"),
        ("poly-ratelimit-order-remaining", "7"),
        ("POLY-RATELIMIT-CANCEL-REMAINING", "12"),
        ("Poly-RateLimit-Reset", "180"),
        ("content-type", "application/json"),
    ]);
    assert!(sig.warning);
    assert_eq!(sig.raw_warning.as_deref(), Some("1"));
    assert_eq!(sig.order_remaining, Some(7.0));
    assert_eq!(sig.cancel_remaining, Some(12.0));
    assert_eq!(sig.reset_secs, Some(180));
    assert!(sig.is_actionable());
}

#[test]
fn parse_headers_treats_zero_false_empty_as_no_warning() {
    for v in ["0", "false", "False", "", "   "] {
        let sig = parse_headers([("poly-ratelimit-warning", v)]);
        assert!(!sig.warning, "value {v:?} should not warn");
    }
    // No rate headers at all → non-actionable default.
    let none = parse_headers([("content-type", "application/json")]);
    assert!(!none.is_actionable());
}
