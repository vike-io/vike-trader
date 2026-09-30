use super::*;

/// The live fapi shape (`vike_binance::data`'s `BINANCE_KLINE_PERP`): `REQUEST_WEIGHT`/`MINUTE`
/// = 2400, measured `/klines?limit=1000` cost = 5.
fn fapi_budget() -> WeightBudget {
    WeightBudget { limit: 2400, interval_secs: 60 }
}

/// Weight actually spent per minute if the caller sleeps `delay` between requests, each costing
/// `per_request`. This is the quantity the pacer is steering, so asserting on IT (rather than on
/// a delay constant) is what proves the pacing rather than the arithmetic.
fn weight_per_minute(delay: Duration, per_request: f64) -> f64 {
    (60.0 / delay.as_secs_f64()) * per_request
}

/// Weight spent per minute END TO END — the gap the VENUE sees, `sleep + request`, which is the
/// quantity the utilization target is a fraction of. [`weight_per_minute`] is the same figure
/// with the request time assumed free, i.e. the arithmetic that read 40 % where the venue
/// metered 23 %.
fn end_to_end_weight_per_minute(delay: Duration, request: Duration, per_request: f64) -> f64 {
    (60.0 / (delay.as_secs_f64() + request.as_secs_f64())) * per_request
}

/// The pacer's current request-time estimate, read back through the PUBLIC surface: `eta(1)` is
/// `estimate + next_delay()` by definition, so subtracting the delay recovers it. Reading the
/// private field would be easier and would prove less — this also pins that `eta` is built on
/// the full cycle and not on the sleep alone.
fn estimate_secs(p: &Pacer) -> f64 {
    p.eta(1).expect("estimate_secs needs an observation").as_secs_f64()
        - p.next_delay().as_secs_f64()
}

fn assert_close(got: f64, want: f64, tol: f64) {
    assert!((got - want).abs() <= tol, "expected ~{want}, got {got}");
}

#[test]
fn discovered_budget_paces_to_the_targeted_share() {
    // 2400/min at 40% ⇒ 960 weight/min, whatever the per-request weight turns out to be.
    let mut p = Pacer::discovered(fapi_budget(), 0.4);

    // Before any observation the seed weight (1) is assumed: 960 requests/min.
    assert_close(weight_per_minute(p.next_delay(), SEED_WEIGHT), 960.0, 0.5);

    // Once the real weight-5 cost is observed the pace holds the SAME weight/min at 1/5 the
    // request rate — the whole point: the target is weight, not requests.
    p.observe(100);
    p.observe(105);
    assert_close(weight_per_minute(p.next_delay(), 5.0), 960.0, 0.5);
    assert_close(p.next_delay().as_secs_f64(), 0.3125, 0.001); // 60 * 5 / 960
}

#[test]
fn per_minute_normalization_makes_sub_minute_intervals_pace_alike() {
    // "600 per 10s" and "3600 per 60s" are the same budget; only the counter's denomination
    // differs, which is exactly why cooldown uses `limit` and pacing uses `per_minute()`.
    let mut short = Pacer::discovered(WeightBudget { limit: 600, interval_secs: 10 }, 0.5);
    let mut long = Pacer::discovered(WeightBudget { limit: 3600, interval_secs: 60 }, 0.5);
    assert_eq!(short.next_delay(), long.next_delay(), "same weight/min ⇒ same pace");

    // ...but the in-window cooldown targets differ (300 vs 1800), because the venue's counter
    // resets every 10s in one case and every 60s in the other.
    short.observe(300);
    long.observe(300);
    assert!(short.should_cool_down(), "300 of a 600/10s budget at 50% is the whole share");
    assert!(!long.should_cool_down(), "300 is nowhere near 1800 of the 3600/60s budget");
}

#[test]
fn observe_infers_the_per_request_weight_from_the_delta() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    assert_eq!(p.per_request, SEED_WEIGHT, "seeded until the counter speaks");
    p.observe(100);
    assert_eq!(p.per_request, SEED_WEIGHT, "one sample is not a delta");
    p.observe(105);
    assert_eq!(p.per_request, 5.0, "a weight-5 delta infers 5 — never hardcoded");
}

#[test]
fn a_repriced_endpoint_self_corrects_on_the_next_observation() {
    // The failure the module exists to prevent: the venue re-prices /klines 2 -> 5 and the
    // pager must slow down by itself instead of tripling consumption into a 418.
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe(10);
    p.observe(12); // weight 2
    let cheap = p.next_delay();
    p.observe(17); // the SAME endpoint now costs 5
    let dear = p.next_delay();
    assert!(dear > cheap, "a heavier request must widen the gap, not keep the old pace");
    // ...and still lands on the same targeted weight/min, which is the invariant.
    assert_close(weight_per_minute(cheap, 2.0), 960.0, 0.5);
    assert_close(weight_per_minute(dear, 5.0), 960.0, 0.5);
}

#[test]
fn an_unmoved_counter_keeps_the_estimate() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe(100);
    p.observe(105);
    let paced = p.next_delay();
    p.observe(105); // venue did not move the counter — no information
    assert_eq!(p.per_request, 5.0, "a zero delta must not be read as a free request");
    assert_eq!(p.next_delay(), paced);
}

#[test]
fn a_counter_reset_is_a_new_window_not_negative_weight() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe(2000);
    p.observe(2005);
    assert_eq!(p.per_request, 5.0);
    assert!(p.should_cool_down(), "2005 is past the 960 share");

    p.observe(3); // the interval rolled: 3 < 2005
    assert_eq!(p.per_request, 5.0, "a reset must not overwrite the estimate");
    assert!(p.per_request.is_sign_positive() && p.per_request.is_finite());
    assert!(!p.should_cool_down(), "a fresh window has spent almost nothing");
    // The delay is still the honest weight-5 pace — no wrapped u64 turned into a 60s stall.
    assert_close(p.next_delay().as_secs_f64(), 0.3125, 0.001);

    // ...and inference resumes from the NEW baseline.
    p.observe(9);
    assert_eq!(p.per_request, 6.0);
}

#[test]
fn should_cool_down_fires_on_crossing_the_target_and_not_before() {
    // `0.5` (not the `0.4` the pacing tests use) is deliberate: it is exactly representable, so
    // `2400 * 0.5` is exactly 1200.0 and the AT-the-target assertion below tests the boundary
    // rather than the last ulp of a rounded product. The pacing tests assert with a tolerance
    // precisely because they cannot make that claim.
    let mut p = Pacer::discovered(fapi_budget(), 0.5); // window target = 1200
    assert!(!p.should_cool_down(), "nothing observed yet");
    p.observe(1199);
    assert!(!p.should_cool_down(), "one unit short is not crossed");
    p.observe(1200);
    assert!(p.should_cool_down(), "at the target is crossed");
    p.observe(2399);
    assert!(p.should_cool_down(), "and stays crossed while the window holds");
}

#[test]
fn fallback_always_returns_its_fixed_delay() {
    let mut p = Pacer::fallback(Duration::from_millis(150));
    assert_eq!(p.next_delay(), Duration::from_millis(150));
    assert!(!p.should_cool_down(), "no discovered budget ⇒ no target to cross");
    // Observations are recorded but change nothing: without a budget there is no rate to steer.
    p.observe(100);
    p.observe(6000);
    assert_eq!(p.next_delay(), Duration::from_millis(150));
    assert!(!p.should_cool_down());
    p.observe(1); // reset, too
    assert_eq!(p.next_delay(), Duration::from_millis(150));
}

#[test]
fn zero_utilization_clamps_instead_of_dividing_by_zero() {
    let p = Pacer::discovered(fapi_budget(), 0.0);
    let d = p.next_delay();
    assert!(d.as_secs_f64().is_finite() && d > Duration::ZERO, "no NaN, no zero delay: {d:?}");
    // Clamped to the floor. DERIVED from the constant, not hardcoded: this expectation was
    // `24.0` (a 1% floor) until the bounds were unified onto vike-model's operator-facing
    // range, and a literal would simply have gone stale rather than saying so.
    let floor_wpm = fapi_budget().per_minute() as f64 * MIN_UTILIZATION;
    assert_close(weight_per_minute(d, SEED_WEIGHT), floor_wpm, 0.5);
}

#[test]
fn oversized_utilization_clamps_below_the_full_budget() {
    let p = Pacer::discovered(fapi_budget(), 5.0);
    // MAX of 2400, NOT 500% — a 5x over-budget hammer is exactly the 418 this prevents.
    // Never 100%: the used-weight counter is per-IP and shared with everything else on this
    // egress (the live recorder included), so the last slice is headroom we do not own.
    let ceiling_wpm = fapi_budget().per_minute() as f64 * MAX_UTILIZATION;
    assert_close(weight_per_minute(p.next_delay(), SEED_WEIGHT), ceiling_wpm, 1.0);
    assert!(p.next_delay() >= Duration::from_secs_f64(MIN_DELAY_SECS));
}

#[test]
fn negative_and_nan_utilization_resolve_to_the_conservative_floor() {
    assert_eq!(clamp_utilization(-1.0), MIN_UTILIZATION);
    assert_eq!(clamp_utilization(f64::NAN), MIN_UTILIZATION);
    assert_eq!(clamp_utilization(f64::INFINITY), MAX_UTILIZATION);
    assert_eq!(clamp_utilization(f64::NEG_INFINITY), MIN_UTILIZATION);
    // A sane value passes through untouched.
    assert_eq!(clamp_utilization(0.4), 0.4);
    // And every one of them still yields a usable delay rather than a panic.
    for u in [-1.0, 0.0, f64::NAN, f64::INFINITY, 5.0, 0.4] {
        let d = Pacer::discovered(fapi_budget(), u).next_delay();
        assert!(d > Duration::ZERO && d <= Duration::from_secs_f64(MAX_DELAY_SECS));
    }
}

#[test]
fn a_degenerate_budget_neither_divides_by_zero_nor_cools_down_instantly() {
    // Discovery that answered with a zero limit is a venue/parse bug, not a licence to panic.
    let mut p = Pacer::discovered(WeightBudget { limit: 0, interval_secs: 60 }, 0.5);
    let d = p.next_delay();
    assert!(d.as_secs_f64().is_finite() && d > Duration::ZERO);
    assert!(!p.should_cool_down(), "zero observed is not yet past the 1-unit floor target");
    p.observe(1);
    assert!(p.should_cool_down(), "and the floor target is then immediately crossed");
}

#[test]
fn an_absurd_inferred_weight_is_bounded_not_unrepresentable() {
    // A counter that jumps by a preposterous amount (venue glitch / shared-IP burst attributed
    // to us) must saturate at the ceiling — `Duration::from_secs_f64` panics on overflow.
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe(0);
    p.observe(u64::MAX);
    assert_eq!(p.next_delay(), Duration::from_secs_f64(MAX_DELAY_SECS));
}

#[test]
fn a_huge_budget_is_floored_not_a_busy_loop() {
    let p = Pacer::discovered(WeightBudget { limit: u64::MAX, interval_secs: 1 }, 0.9);
    assert_eq!(p.next_delay(), Duration::from_secs_f64(MIN_DELAY_SECS));
}

// ----- request-time compensation ------------------------------------------------------------

/// THE measured case, reproduced as arithmetic (the CI box, 2026-08-04, 24 months of `BTCUSDT.P`):
/// a 2400/min fapi budget at 40 % with weight-5 pages and a ~280 ms round trip must spend its
/// 960 weight/min **end to end** — not the ~506 the sleep-only pacer actually delivered, which
/// metered as 556 (~23 % of the ceiling) instead of the promised 40 %.
#[test]
fn the_measured_fapi_shape_hits_its_target_end_to_end() {
    let rtt = Duration::from_millis(280);
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe_request(Some(100), rtt);
    p.observe_request(Some(105), rtt); // weight 5, and the estimate is still exactly 280ms

    let d = p.next_delay();
    // 60 * 5 / 960 = 312.5ms of TARGET GAP, of which the request already spent 280.
    assert_close(d.as_secs_f64(), 0.0325, 1e-6);
    assert_close(end_to_end_weight_per_minute(d, rtt, 5.0), 960.0, 1.0);
    assert_close(60.0 / (d.as_secs_f64() + rtt.as_secs_f64()), 192.0, 0.2); // requests/min

    // The regression this exists to prevent: sleeping the WHOLE 312.5ms would have spaced
    // requests 592.5ms apart — ~506 weight/min, roughly half the target.
    let sleep_only = Duration::from_secs_f64(0.3125);
    assert!(
        end_to_end_weight_per_minute(sleep_only, rtt, 5.0) < 550.0,
        "the sleep-only pace must be the SLOW one, or this test proves nothing"
    );
}

#[test]
fn next_delay_shrinks_by_the_observed_request_time() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe(100);
    p.observe(105);
    let unmeasured = p.next_delay(); // 312.5ms — the pre-measurement arithmetic
    assert_close(unmeasured.as_secs_f64(), 0.3125, 1e-6);

    p.observe_request(None, Duration::from_millis(100));
    let measured = p.next_delay();
    assert_close(measured.as_secs_f64(), 0.2125, 1e-6);
    assert!(measured < unmeasured);
    // ...and the END-TO-END pace is unchanged by the correction — that is the whole point.
    assert_close(
        end_to_end_weight_per_minute(measured, Duration::from_millis(100), 5.0),
        960.0,
        1.0,
    );
}

#[test]
fn a_request_slower_than_the_target_interval_floors_the_delay() {
    // 5s per request against a 312.5ms target gap: there is no negative sleep, no panic, and no
    // zero-delay hammer — just the floor. (The venue is metering us below target either way;
    // the pacer cannot make a slow request faster.)
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe_request(Some(100), Duration::from_secs(5));
    p.observe_request(Some(105), Duration::from_secs(5));
    let d = p.next_delay();
    assert_eq!(d, Duration::from_secs_f64(MIN_DELAY_SECS));
    assert!(d > Duration::ZERO && d.as_secs_f64().is_finite());
}

#[test]
fn an_absurd_request_time_neither_panics_nor_goes_negative() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe_request(None, Duration::MAX);
    assert_eq!(p.next_delay(), Duration::from_secs_f64(MIN_DELAY_SECS));
    // And the ETA saturates rather than overflowing `Duration::from_secs_f64` into a panic.
    assert_eq!(p.eta(u64::MAX), Some(Duration::MAX));
}

#[test]
fn the_request_time_estimate_is_smoothed_not_latest_wins() {
    // A budget whose target gap is a whole 2s, so the delay does not floor and the smoothing is
    // visible in `next_delay` itself: 300/min at 50% = 150 weight/min, 60 * 5 / 150 = 2s.
    let mut p = Pacer::discovered(WeightBudget { limit: 300, interval_secs: 60 }, 0.5);
    p.observe(10);
    p.observe(15); // weight 5
    for _ in 0..4 {
        p.observe_request(None, Duration::from_millis(280));
    }
    assert_close(estimate_secs(&p), 0.280, 1e-6);
    assert_close(p.next_delay().as_secs_f64(), 1.720, 1e-6);

    // ONE 5x-slow sample (1.4s). Latest-wins would make the estimate 1.4 and the delay 0.6.
    p.observe_request(None, Duration::from_millis(1400));
    let est = estimate_secs(&p);
    assert_close(est, 0.560, 1e-6); // 0.28*0.75 + 1.4*0.25
    assert!(est < 1.4 * 0.5, "one outlier must not become the estimate: {est}");
    assert!(p.next_delay() > Duration::from_millis(1300), "and must not gut the sleep");

    // ...and it decays back toward the truth as normal pages resume.
    for _ in 0..6 {
        p.observe_request(None, Duration::from_millis(280));
    }
    assert!(estimate_secs(&p) < 0.35, "the outlier's pull must fade, not persist");

    // A genuine STEP change still converges — smoothing must not mean deafness.
    for _ in 0..20 {
        p.observe_request(None, Duration::from_millis(900));
    }
    assert_close(estimate_secs(&p), 0.900, 0.01);
}

#[test]
fn eta_is_none_until_a_request_is_observed_then_scales_linearly() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    assert_eq!(p.eta(1_000), None, "nothing measured ⇒ no ETA, not a guess from the sleep");
    p.observe(100);
    p.observe(105);
    assert_eq!(p.eta(1_000), None, "a WEIGHT observation is not a duration observation");

    let rtt = Duration::from_millis(280);
    p.observe_request(Some(110), rtt); // weight 5 again, estimate seeded at 280ms

    // One full cycle is the target gap itself: 280ms of request + 32.5ms of sleep.
    assert_close(p.eta(1).unwrap().as_secs_f64(), 0.3125, 1e-6);
    assert_close(p.eta(1_000).unwrap().as_secs_f64(), 312.5, 1e-3);
    assert_close(p.eta(2_000).unwrap().as_secs_f64(), 625.0, 1e-3);
    assert_eq!(p.eta(0), Some(Duration::ZERO));

    // The shape the pager reports: 24 months of 1m bars ≈ 1051 pages of 1000.
    let eta = p.eta(1_051).unwrap();
    assert!(
        eta > Duration::from_secs(320) && eta < Duration::from_secs(340),
        "expected ~5m28s for the measured shape, got {eta:?}"
    );
}

#[test]
fn observe_request_is_observe_plus_a_duration() {
    // Same weight sequence, zero-length requests ⇒ byte-identical to the weight-only path. The
    // forwarding contract, so a measuring caller never has to call both.
    let mut weight_only = Pacer::discovered(fapi_budget(), 0.4);
    let mut measured = Pacer::discovered(fapi_budget(), 0.4);
    for w in [100, 105, 110, 2_000] {
        weight_only.observe(w);
        measured.observe_request(Some(w), Duration::ZERO);
    }
    assert_eq!(weight_only.next_delay(), measured.next_delay());
    assert_eq!(weight_only.should_cool_down(), measured.should_cool_down());
    assert_eq!(weight_only.per_request_weight(), measured.per_request_weight());

    // A `None` weight records ONLY the duration — no phantom counter movement, so the weight
    // estimate stays where the venue last put it.
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe(10);
    p.observe(12); // weight 2
    p.observe_request(None, Duration::from_millis(280));
    p.observe_request(None, Duration::from_millis(280));
    assert_eq!(p.per_request_weight(), 2.0, "a duration must not be read as a weight delta");
    assert_eq!(p.last_used, Some(12), "nor move the delta baseline");
}

/// "Entirely" scopes to the DELAY, which is the only thing `Fixed` withholds — the ETA below
/// answers from the very same observation. See `fixed_mode_measures_and_reports_but_never_steers`
/// for the full contract.
#[test]
fn fixed_mode_ignores_observed_request_times_entirely() {
    let mut p = Pacer::fallback(Duration::from_millis(150));
    assert!(!p.is_discovered());
    p.observe_request(Some(100), Duration::from_secs(5));
    assert_eq!(p.next_delay(), Duration::from_millis(150), "a fixed delay is a fixed delay");
    p.observe_request(None, Duration::from_millis(1));
    assert_eq!(p.next_delay(), Duration::from_millis(150));
    assert!(!p.should_cool_down());

    // The ETA is still answerable — it is a measurement, not a target — and it counts the
    // fixed sleep plus the measured request, like everywhere else.
    let est = estimate_secs(&p);
    assert_close(p.eta(10).unwrap().as_secs_f64(), 10.0 * (est + 0.150), 1e-6);
}

/// THE contract this mode exists to state, on the MEASURED okx shape (the CI box, 2026-08-04: spot
/// and perp both average 476–486 ms per page against a hardcoded 200 ms `PAGE_DELAY`). A `Fixed`
/// pacer RECORDS, REPORTS and answers an ETA — and sleeps exactly its configured delay while
/// doing it.
///
/// The `next_delay` half is the load-bearing one: it is what makes measuring a no-budget venue
/// SAFE rather than a pacing change wearing a measurement's clothes.
#[test]
fn fixed_mode_measures_and_reports_but_never_steers() {
    let page_delay = Duration::from_millis(200);
    let mut p = Pacer::fallback(page_delay);
    assert!(!p.is_discovered());
    assert_eq!(p.measured(), None, "nothing timed yet ⇒ nothing to report");
    assert_eq!(p.eta(1_000), None, "and no ETA invented out of the sleep alone");
    assert_eq!(p.next_delay(), page_delay);

    // Four pages at the measured okx cost. No weight header — this venue publishes no counter.
    for _ in 0..4 {
        p.observe_request(None, Duration::from_millis(480));
    }

    // (1) It RECORDS: `measured()` hands out a persistable sample, with NO budget attached —
    // which is what confines it to another fallback pacer (`seed`'s rule 3).
    let m = p.measured().expect("a fixed pager times its pages like any other");
    assert_eq!(m.request_ms, 480, "the EWMA of four identical samples is the sample");
    assert_eq!(m.samples, 4);
    assert_eq!(m.budget_per_min, None, "nothing was discovered, so nothing is claimed");
    assert!(m.is_usable(), "and it is good enough for the pace book to store");

    // (2) It REPORTS an ETA over the FULL cycle — `sleep + request`, the only spacing the venue
    // sees. This is the number the ~480ms measurement makes available and the 200ms constant
    // alone never could: 1000 pages is ~11m20s, not the ~3m20s the sleep implies.
    assert_close(p.eta(1_000).unwrap().as_secs_f64(), 1_000.0 * (0.480 + 0.200), 1e-6);
    assert_eq!(p.eta(0), Some(Duration::ZERO));

    // (3) It does NOT steer. Whatever is observed — including the absurd — the sleep is the
    // caller's constant, to the nanosecond.
    assert_eq!(p.next_delay(), page_delay);
    for absurd in [Duration::ZERO, Duration::from_micros(1), Duration::from_secs(30), Duration::MAX]
    {
        p.observe_request(Some(10_000), absurd);
        assert_eq!(p.next_delay(), page_delay, "{absurd:?} moved the fixed delay");
        assert!(!p.should_cool_down(), "no budget ⇒ nothing to cross, whatever the counter");
    }
}

/// The cross-run payoff for a no-budget venue: a stored sample answers the ETA on page ZERO
/// (where an unseeded fallback pacer must refuse), and STILL cannot speed the sleep up. A prior
/// is evidence about cost, never permission to go faster.
#[test]
fn a_seeded_fixed_pacer_answers_from_page_zero_and_still_sleeps_its_constant() {
    let page_delay = Duration::from_millis(200);
    let mut p = Pacer::fallback(page_delay);
    assert_eq!(p.eta(1_000), None);
    assert!(p.seed(&PaceSample {
        request_ms: 480,
        per_request_weight: 1.0,
        budget_per_min: None,
        samples: 12,
    }));
    assert_close(p.eta(1_000).unwrap().as_secs_f64(), 1_000.0 * (0.480 + 0.200), 1e-6);
    assert_eq!(p.next_delay(), page_delay, "a prior must not shorten a fixed delay");
    // ...and the prior is not a measurement: a run that then times nothing reports nothing.
    assert_eq!(p.measured(), None);
}

#[test]
fn is_discovered_distinguishes_the_two_modes() {
    assert!(Pacer::discovered(fapi_budget(), 0.4).is_discovered());
    assert!(!Pacer::fallback(Duration::from_millis(150)).is_discovered());
}

#[test]
fn per_request_weight_reports_the_seed_then_the_inferred_cost() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    assert_eq!(p.per_request_weight(), SEED_WEIGHT, "seeded until the counter speaks");
    p.observe(100);
    p.observe(102);
    assert_eq!(p.per_request_weight(), 2.0);
}

// ----- the ETA's multiplicand ---------------------------------------------------------------

/// The page count is the window's width over ONE page's span, `None` when the interval is not a
/// vike interval string at all (an ETA is a courtesy — a wrong one is worse than none). Pinned
/// across the four pagers' differing page sizes, since that is why it is a parameter.
#[test]
fn remaining_pages_divides_the_window_by_one_pages_span() {
    // 24 months of 1m bars — the measured binance shape. 730d = 63_072_000_000ms.
    let two_years = 730 * 86_400_000i64;
    assert_eq!(remaining_pages(two_years, "1m", 1_000), Some(1_051));
    // The SAME window costs 10x the pages on okx, whose history-candles cap is 100.
    assert_eq!(remaining_pages(two_years, "1m", 100), Some(10_512));
    // ...and a fifth of binance's on deribit, whose observed page is ~5001 rows.
    assert_eq!(remaining_pages(two_years, "1m", 5_001), Some(210));
    // Coarser bars ⇒ far fewer pages for the same window.
    assert_eq!(remaining_pages(two_years, "1h", 1_000), Some(17));
    assert_eq!(remaining_pages(two_years, "1d", 1_000), Some(0), "730 days fits in one page");
    // A window narrower than one page has nothing left after the page just fetched.
    assert_eq!(remaining_pages(1_000, "1m", 1_000), Some(0));
    // A NEGATIVE span (a cursor already past the end) is zero, never a wrap.
    assert_eq!(remaining_pages(-two_years, "1m", 1_000), Some(0));
    assert_eq!(remaining_pages(i64::MIN, "1m", 1_000), Some(0));
    // Unparseable / absurd intervals decline to guess instead of dividing by zero.
    assert_eq!(remaining_pages(two_years, "", 1_000), None);
    assert_eq!(remaining_pages(two_years, "1y", 1_000), None);
    assert_eq!(remaining_pages(two_years, "0m", 1_000), None);
    // A zero page size is "no page advances the cursor" — decline, never divide by zero.
    assert_eq!(remaining_pages(two_years, "1m", 0), None);
    // A page span that overflows i64 declines rather than wrapping into a tiny span and
    // reporting a preposterous page count.
    assert_eq!(remaining_pages(i64::MAX, "200000000d", 1_000), None);
    assert_eq!(remaining_pages(i64::MAX, "1m", usize::MAX), None);
}

/// The two halves compose into the line a paged backfill actually prints: pages x cycle.
#[test]
fn remaining_pages_feeds_the_eta_of_a_fixed_pager() {
    let mut p = Pacer::fallback(Duration::from_millis(200));
    p.observe_request(None, Duration::from_millis(480));
    // One day of 1m candles on okx's 100-row pages = 14 pages, at ~680ms per cycle.
    let pages = remaining_pages(86_400_000, "1m", 100).expect("a parseable interval");
    assert_eq!(pages, 14);
    assert_close(p.eta(pages).unwrap().as_secs_f64(), 14.0 * 0.680, 1e-6);
}

// ----- persisted pace: seed in, measurement out ----------------------------------------------

/// A stored sample shaped like the MEASURED fapi run: weight-5 pages, ~280 ms round trip,
/// against the 2400/min budget `fapi_budget()` publishes.
fn fapi_sample(request_ms: u64, weight: f64) -> PaceSample {
    PaceSample { request_ms, per_request_weight: weight, budget_per_min: Some(2400), samples: 8 }
}

/// The payoff: run N+1's FIRST page is already paced at run N's measurement, instead of sleeping
/// the whole target gap against the pessimistic `SEED_WEIGHT`. Same numbers the live-measured
/// test above lands on, but with ZERO observations this run.
#[test]
fn a_seed_paces_the_very_first_page_at_last_runs_measurement() {
    let mut unseeded = Pacer::discovered(fapi_budget(), 0.4);
    let mut seeded = Pacer::discovered(fapi_budget(), 0.4);
    assert!(seeded.seed(&fapi_sample(280, 5.0)), "a matching sample applies");

    // Unseeded: SEED_WEIGHT (5) and no request time ⇒ the full 312.5ms target gap slept.
    assert_close(unseeded.next_delay().as_secs_f64(), 0.3125, 1e-6);
    // Seeded: the same weight-5 gap MINUS the 280ms the request is known to cost.
    assert_close(seeded.next_delay().as_secs_f64(), 0.0325, 1e-6);
    assert_close(
        end_to_end_weight_per_minute(seeded.next_delay(), Duration::from_millis(280), 5.0),
        960.0,
        1.0,
    );
    // ...and the ETA answers from page ZERO, where the unseeded pacer must refuse.
    assert_eq!(unseeded.eta(1_051), None, "nothing measured and nothing seeded ⇒ no ETA");
    let eta = seeded.eta(1_051).expect("a seeded pacer can answer");
    assert!(eta > Duration::from_secs(300) && eta < Duration::from_secs(360), "{eta:?}");

    // The seed is a PRIOR: the first live observation drops it whole and seeds the EWMA
    // outright, so from then on the two pacers agree given the same live samples.
    for p in [&mut unseeded, &mut seeded] {
        p.observe_request(Some(100), Duration::from_millis(400));
        p.observe_request(Some(105), Duration::from_millis(400));
    }
    assert_eq!(seeded.next_delay(), unseeded.next_delay(), "a prior must not outlive the truth");
    assert_close(estimate_secs(&seeded), 0.400, 1e-6);
}

/// The cheap-endpoint case the constant costs every run: binance SPOT prices `/klines` at
/// weight 2, but `SEED_WEIGHT` is 5, so an unseeded pager opens 2.5x slower than the venue
/// permits. A stored weight fixes exactly that — and still paces UNDER the published ceiling.
#[test]
fn a_seeded_weight_replaces_the_pessimistic_constant() {
    let spot = WeightBudget { limit: 6000, interval_secs: 60 };
    let mut p = Pacer::discovered(spot, 0.4);
    assert_eq!(p.per_request_weight(), SEED_WEIGHT);
    assert!(p.seed(&PaceSample {
        request_ms: 120,
        per_request_weight: 2.0,
        budget_per_min: Some(6000),
        samples: 40,
    }));
    assert_eq!(p.per_request_weight(), 2.0, "the stored cost, not the constant");
    // 6000/min at 40% = 2400 weight/min; at weight 2 that is a 50ms gap, of which the request
    // is known to spend 120ms ⇒ the sleep floors, and the END-TO-END pace stays under budget.
    assert!(
        end_to_end_weight_per_minute(p.next_delay(), Duration::from_millis(120), 2.0) <= 6000.0,
        "a seed must never pace above the venue's published ceiling"
    );
    // The counter still overrules the seed on the first real delta — latest-wins, unchanged.
    p.observe(10);
    p.observe(13);
    assert_eq!(p.per_request_weight(), 3.0);
}

/// Rule 3 — the budget is the HOST's identity. A spot record must never seed a perp pager, or
/// the weight-2 estimate would pace the weight-5 endpoint at 2.5x its target.
#[test]
fn a_seed_from_a_different_budget_is_refused_whole() {
    let mut perp = Pacer::discovered(fapi_budget(), 0.4); // 2400/min
    let spot_record = PaceSample {
        request_ms: 120,
        per_request_weight: 2.0,
        budget_per_min: Some(6000),
        samples: 40,
    };
    assert!(!perp.seed(&spot_record), "another host's record must not apply");
    assert_eq!(perp.per_request_weight(), SEED_WEIGHT, "and must leave NOTHING behind");
    assert_close(perp.next_delay().as_secs_f64(), 0.3125, 1e-6);
    assert_eq!(perp.eta(10), None);

    // `None` is a distinct budget, in both directions: a fallback-mode record cannot seed a
    // discovered pacer, and a discovered record cannot seed a fallback one.
    assert!(!perp.seed(&PaceSample { budget_per_min: None, ..fapi_sample(280, 5.0) }));
    let mut fixed = Pacer::fallback(Duration::from_millis(150));
    assert!(!fixed.seed(&fapi_sample(280, 5.0)));
    assert!(
        fixed.seed(&PaceSample { budget_per_min: None, ..fapi_sample(280, 5.0) }),
        "a fallback record matches a fallback pacer"
    );
    // ...and even then a fixed delay is a fixed delay — the seed cannot speed it up.
    assert_eq!(fixed.next_delay(), Duration::from_millis(150));
}

/// Rule 2 plus the sanitation: a seed never lands over live evidence, and never lands at all if
/// it is the kind of number that would hang or hammer the pager.
#[test]
fn a_seed_is_refused_over_live_evidence_and_over_nonsense() {
    // Over live evidence — a weight observation counts, not only a timed request.
    let mut observed = Pacer::discovered(fapi_budget(), 0.4);
    observed.observe(100);
    assert!(!observed.seed(&fapi_sample(280, 5.0)), "a counter reading is evidence");
    let mut timed = Pacer::discovered(fapi_budget(), 0.4);
    timed.observe_request(None, Duration::from_millis(400));
    assert!(!timed.seed(&fapi_sample(280, 5.0)), "a timed request is evidence");
    assert_close(estimate_secs(&timed), 0.400, 1e-6);

    // Nonsense: every one of these leaves the pacer exactly as it was built.
    for bad in [
        PaceSample { samples: 0, ..fapi_sample(280, 5.0) },
        PaceSample { request_ms: 0, ..fapi_sample(280, 5.0) },
        fapi_sample(280, 0.0),
        fapi_sample(280, -1.0),
        fapi_sample(280, f64::NAN),
        fapi_sample(280, f64::INFINITY),
        fapi_sample((MAX_DELAY_SECS as u64) * 1_000, 5.0), // a request longer than the ceiling
        fapi_sample(u64::MAX, 5.0),
        PaceSample::default(),
    ] {
        let mut p = Pacer::discovered(fapi_budget(), 0.4);
        assert!(!p.seed(&bad), "{bad:?} must be refused");
        assert_eq!(p.per_request_weight(), SEED_WEIGHT);
        assert_eq!(p.eta(10), None, "{bad:?} must leave no prior behind");
        let d = p.next_delay();
        assert!(d.as_secs_f64().is_finite() && d > Duration::ZERO, "{bad:?} -> {d:?}");
    }
}

/// The report side: `None` until something is TIMED, so a run that measured nothing can never
/// overwrite a stored record — and once timed it carries the budget it was measured against.
#[test]
fn measured_reports_only_what_was_actually_timed() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    assert_eq!(p.measured(), None, "nothing timed ⇒ nothing to persist");
    p.observe(100);
    p.observe(105);
    assert_eq!(p.measured(), None, "a WEIGHT observation is not a duration observation");

    p.observe_request(Some(110), Duration::from_millis(280));
    let m = p.measured().expect("a timed request is reportable");
    assert_eq!(m.request_ms, 280);
    assert_eq!(m.per_request_weight, 5.0);
    assert_eq!(m.budget_per_min, Some(2400), "the budget it was measured against travels with it");
    assert_eq!(m.samples, 1);
    assert!(m.is_usable());

    // Samples accumulate with the EWMA, and the rounding is to whole milliseconds.
    for _ in 0..3 {
        p.observe_request(None, Duration::from_millis(280));
    }
    assert_eq!(p.measured().unwrap().samples, 4);
    assert_eq!(p.measured().unwrap().request_ms, 280);

    // A SEEDED pacer that then measures nothing reports nothing — the prior is not evidence,
    // so a no-op run cannot re-stamp last week's numbers as fresh.
    let mut seeded = Pacer::discovered(fapi_budget(), 0.4);
    assert!(seeded.seed(&fapi_sample(280, 5.0)));
    assert_eq!(seeded.measured(), None, "a prior is not a measurement");

    // Fallback mode still reports (the weight is real, inferred from the header deltas) — with
    // no budget, which is what stops it seeding a discovered run later.
    let mut fixed = Pacer::fallback(Duration::from_millis(150));
    fixed.observe_request(Some(10), Duration::from_millis(90));
    fixed.observe_request(Some(12), Duration::from_millis(90));
    let f = fixed.measured().expect("a fallback pager still times its requests");
    assert_eq!(f.budget_per_min, None);
    assert_eq!(f.per_request_weight, 2.0);
    assert_eq!(f.samples, 2);
}

/// The ADDITIVE contract, stated as one test: with no seed, a pacer is bit-for-bit the pacer it
/// was before any of this existed — same delays, same cooldowns, same ETAs, across the whole
/// observation sequence the measured tests use.
#[test]
fn an_unseeded_pacer_is_unchanged_in_every_observable_way() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    assert_eq!(p.eta(1_000), None);
    assert_close(p.next_delay().as_secs_f64(), 60.0 * SEED_WEIGHT / 960.0, 1e-9);
    p.observe(100);
    p.observe(105);
    assert_close(p.next_delay().as_secs_f64(), 0.3125, 1e-6);
    assert!(!p.should_cool_down());
    p.observe_request(Some(110), Duration::from_millis(280));
    assert_close(p.next_delay().as_secs_f64(), 0.0325, 1e-6);
    assert_close(p.eta(1).unwrap().as_secs_f64(), 0.3125, 1e-6);
    // The target gap is the WHOLE spacing the sleep-only pacer already produced — `next_delay`
    // plus the round trip it now subtracts. Asserted HERE, before the spike below, because it
    // is a function of the inferred per-request weight and the spike deliberately destroys it.
    assert_close(p.target_gap().as_secs_f64(), 0.3125, 1e-6);

    p.observe(2_000);
    assert!(p.should_cool_down());
    // The new accessors are pure reads — none of them steers anything.
    assert_eq!(p.budget_per_minute(), Some(2400));
    assert!(p.measured().is_some());
    assert_eq!(Pacer::fallback(Duration::from_millis(150)).budget_per_minute(), None);
    // ...and the spike's own consequence, which is NOT a regression: a 1_890-weight delta means
    // the budget affords one request every ~118s, so the gap saturates at the MAX_DELAY_SECS
    // ceiling the module doc describes as the stall. A concurrent pager spacing on this figure
    // therefore stalls exactly as the sequential one does — it cannot outrun a blown budget.
    assert_close(p.target_gap().as_secs_f64(), MAX_DELAY_SECS, 1e-9);
    // One lane until something is measured: the ratio has no numerator yet.
    assert_eq!(Pacer::discovered(fapi_budget(), 0.4).suggested_lanes(8), 1);
}

// ----- the derived lane count ---------------------------------------------------------------

/// `target_gap` is the WHOLE spacing; `next_delay` is that minus the measured round trip. The
/// distinction is the entire reason a concurrent pager may not pace on `next_delay`: with
/// requests overlapping, subtracting a round trip that no longer serialises would spend the
/// difference a second time.
#[test]
fn target_gap_is_next_delay_plus_the_measured_request() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe(100);
    p.observe(105); // weight 5 ⇒ 60 * 5 / 960 = 312.5ms
    assert_close(p.target_gap().as_secs_f64(), 0.3125, 1e-6);
    assert_eq!(p.target_gap(), p.next_delay(), "nothing measured ⇒ nothing subtracted");

    p.observe_request(None, Duration::from_millis(280));
    assert_close(p.target_gap().as_secs_f64(), 0.3125, 1e-6);
    assert_close(p.next_delay().as_secs_f64(), 0.0325, 1e-6);
    // The identity that makes the two interchangeable for the caller that knows which it wants.
    assert_close(
        p.next_delay().as_secs_f64() + estimate_secs(&p),
        p.target_gap().as_secs_f64(),
        1e-6,
    );
}

/// The MEASURED shapes, which land on opposite sides of the line — and that opposition is the
/// finding, not the arithmetic: binance PERP is already at its target sequentially, binance SPOT
/// is at ~18 % of its target and no sleep tuning can fix it (the sleep is floored at 1ms).
#[test]
fn lanes_are_derived_from_the_measured_round_trip_not_a_constant() {
    let rtt = Duration::from_millis(280);

    // fapi: 2400/min at 40%, weight-5 pages ⇒ 312.5ms gap > 280ms rtt ⇒ ONE lane.
    let mut perp = Pacer::discovered(fapi_budget(), 0.4);
    perp.observe_request(Some(100), rtt);
    perp.observe_request(Some(105), rtt);
    assert_eq!(perp.suggested_lanes(8), 1, "the budget is the binding constraint on fapi");

    // spot: 6000/min at 40%, weight-2 pages ⇒ 50ms gap ⇒ ceil(280/50) = 6 lanes.
    let mut spot = Pacer::discovered(WeightBudget { limit: 6000, interval_secs: 60 }, 0.4);
    spot.observe_request(Some(100), rtt);
    spot.observe_request(Some(102), rtt);
    assert_close(spot.target_gap().as_secs_f64(), 0.050, 1e-6);
    assert_eq!(spot.suggested_lanes(8), 6);
    // ...and the CAP is a thread bound, not a rate bound: capping it does not change the gap.
    assert_eq!(spot.suggested_lanes(4), 4);
    assert_eq!(spot.suggested_lanes(1), 1);
    assert_eq!(spot.suggested_lanes(0), 1, "a zero cap is one lane, never zero");
    assert_close(spot.target_gap().as_secs_f64(), 0.050, 1e-6);
}

/// `observe_absolute` records the counter and infers NOTHING from it — the concurrent reading
/// that `observe` would have mis-read as one request's cost.
///
/// FAILS if the concurrent path is wired to `observe`: the 105 -> 500 step would be read as a
/// 395-weight request and widen the target gap from 312 ms to ~24 s, which is the systematic
/// lane-count inflation `observe_absolute`'s doc describes (and, past a larger delta, the
/// `MAX_DELAY_SECS` stall the byte-identity test pins).
#[test]
fn observe_absolute_records_the_counter_and_infers_nothing() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe(100);
    p.observe(105); // the SEQUENTIAL measurement: exactly one request outstanding ⇒ weight 5
    let sequential_gap = p.target_gap();
    assert_close(sequential_gap.as_secs_f64(), 0.3125, 1e-6);

    p.observe_absolute(500);
    assert_eq!(p.per_request_weight(), 5.0, "a concurrent reading is not a per-request cost");
    assert_eq!(p.target_gap(), sequential_gap, "...so it must not re-pace anything");
    assert!(!p.should_cool_down(), "500 is under the 960 window share");

    // The ABSOLUTE guard is exactly what the freeze keeps — it is the one that stops a 429.
    p.observe_absolute(1_000);
    assert!(p.should_cool_down(), "the counter crossing the share still fires, unattributed");
    assert_eq!(p.per_request_weight(), 5.0, "and STILL nothing is inferred");
    assert_eq!(p.target_gap(), sequential_gap);

    // The persisted floor is untouched too, so a concurrent run cannot write a polluted weight
    // into the pace book for the next run to open on.
    p.observe_request(None, Duration::from_millis(280));
    assert_eq!(p.measured().unwrap().per_request_weight, 5.0);
}

/// A window ROLL under `observe_absolute` re-baselines like everywhere else — and, because
/// nothing is inferred, it cannot produce the wrapped subtraction the module doc calls the trap.
#[test]
fn observe_absolute_survives_a_counter_reset() {
    let mut p = Pacer::discovered(fapi_budget(), 0.4);
    p.observe(100);
    p.observe(105);
    p.observe_absolute(2_000);
    assert!(p.should_cool_down());
    p.observe_absolute(3); // the interval rolled
    assert!(!p.should_cool_down(), "a fresh window has spent almost nothing");
    assert_eq!(p.per_request_weight(), 5.0);
    assert_close(p.target_gap().as_secs_f64(), 0.3125, 1e-6);
    // ...and a later `observe` measures from the NEW baseline, not across the whole gap.
    p.observe(8);
    assert_eq!(p.per_request_weight(), 5.0, "8 - 3 = 5, not 8 - 105");
}

/// The three refusals. Each is a real failure mode, not defensive decoration — and the `Fixed`
/// one is the whole reason bybit/okx/deribit stay sequential: their `page_delay` is a hand-chosen
/// constant, not a fraction of a published ceiling, so N of them in parallel is an unmeasured
/// rate increase against a venue that publishes no budget at all.
#[test]
fn lanes_refuse_without_a_measurement_a_budget_or_a_sane_ratio() {
    // (1) Nothing measured — a lane count needs a numerator.
    let mut p = Pacer::discovered(WeightBudget { limit: 6000, interval_secs: 60 }, 0.4);
    assert_eq!(p.suggested_lanes(8), 1);
    p.observe(100);
    p.observe(102);
    assert_eq!(p.suggested_lanes(8), 1, "a WEIGHT observation is not a duration observation");

    // (2) No discovered budget — however slow the venue is measured to be.
    let mut fixed = Pacer::fallback(Duration::from_millis(200));
    fixed.observe_request(None, Duration::from_millis(480)); // the measured okx page
    assert_eq!(fixed.target_gap(), Duration::from_millis(200), "the accessor answers honestly");
    assert_eq!(
        fixed.suggested_lanes(8),
        1,
        "...and the lane count still refuses: no ceiling ⇒ no aggregate to be a fraction of"
    );

    // (3) Absurd ratios saturate at the cap rather than wrapping through `as usize`.
    let mut huge = Pacer::discovered(WeightBudget { limit: u64::MAX, interval_secs: 1 }, 0.9);
    huge.observe_request(None, Duration::from_secs(30));
    assert_eq!(huge.suggested_lanes(8), 8);
    let mut absurd = Pacer::discovered(WeightBudget { limit: 6000, interval_secs: 60 }, 0.4);
    absurd.observe_request(None, Duration::MAX);
    assert_eq!(absurd.suggested_lanes(8), 8);

    // A SEEDED pacer answers from page zero — the prior IS last run's measurement.
    let mut seeded = Pacer::discovered(WeightBudget { limit: 6000, interval_secs: 60 }, 0.4);
    assert!(seeded.seed(&PaceSample {
        request_ms: 280,
        per_request_weight: 2.0,
        budget_per_min: Some(6000),
        samples: 40,
    }));
    assert_eq!(seeded.suggested_lanes(8), 6);
}
