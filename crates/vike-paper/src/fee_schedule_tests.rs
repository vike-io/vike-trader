use super::*;

fn market_req(coid: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty,
        order_type: "market".into(),
        ..Default::default()
    }
}

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// The `with_fee_schedule` path applies `FeeSchedule::commission` (taker for a market fill).
#[test]
fn schedule_path_charges_taker_bps_on_a_market_fill() {
    let sched = FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 };
    let mut c = PaperExecutionClient::with_fee_schedule("binance", "BTCUSDT", 0.0, sched);
    c.submit(&market_req("m1", 1, 2.0));
    c.on_bar(&bar(1, 100.0, 101.0, 99.0, 100.0)); // market fills at next open = 100
    let fills = c.fills.lock().unwrap();
    assert_eq!(fills.len(), 1);
    // taker 10 bps of 2 * 100 = 0.2
    assert_eq!(fills[0].fee, sched.commission(false, 2.0, 100.0));
    assert_eq!(fills[0].fee, 2.0 * 100.0 * (10.0 / 10_000.0));
}

/// The schedule path handles [`FeeSchedule::ProbabilityScaled`] (pm-economics lane): a taker
/// market fill at probability price p is charged `qty × taker_rate × p × (1−p)`.
#[test]
fn schedule_path_charges_probability_curve_on_a_market_fill() {
    let sched = FeeSchedule::ProbabilityScaled {
        taker_rate: 0.02,
        maker_rate: 0.0,
        maker_rebate_share: 0.0,
    };
    let mut c = PaperExecutionClient::with_fee_schedule("polymarket", "TOKEN", 0.0, sched);
    c.submit(&market_req("p1", 1, 100.0));
    c.on_bar(&bar(1, 0.5, 0.55, 0.45, 0.5)); // market fills at next open = 0.5 (curve peak)
    let fills = c.fills.lock().unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].fee, sched.commission(false, 100.0, 0.5));
    assert_eq!(fills[0].fee, 100.0 * 0.02 * (0.5 * (1.0 - 0.5))); // = 0.5
}

/// A resting-limit (maker) fill under a rebate-bearing [`FeeSchedule::ProbabilityScaled`]
/// carries a NEGATIVE commission — the maker rebate flows through the paper fill path.
#[test]
fn schedule_path_pays_probability_curve_maker_rebate() {
    let sched = FeeSchedule::ProbabilityScaled {
        taker_rate: 0.02,
        maker_rate: 0.0,
        maker_rebate_share: 0.25,
    };
    let mut c = PaperExecutionClient::with_fee_schedule("polymarket", "TOKEN", 0.0, sched);
    // rest a buy limit at 0.5; the bar opens above and trades through it → maker fill @ 0.5
    let req = OrderRequest {
        client_order_id: "m1".into(),
        venue: "polymarket".into(),
        symbol: "TOKEN".into(),
        side: 1,
        qty: 100.0,
        order_type: "limit".into(),
        price: Some(0.5),
        ..Default::default()
    };
    c.submit(&req);
    c.on_bar(&bar(1, 0.6, 0.65, 0.4, 0.45));
    let fills = c.fills.lock().unwrap();
    assert_eq!(fills.len(), 1);
    assert!(fills[0].is_maker, "a resting limit fill is maker");
    assert_eq!(fills[0].fee, sched.commission(true, 100.0, 0.5));
    assert!(fills[0].fee < 0.0, "maker rebate = negative commission, got {}", fills[0].fee);
    assert_eq!(fills[0].fee, -(0.25 * (100.0 * 0.02 * (0.5 * (1.0 - 0.5)))));
    // = −0.125
}

/// End-to-end through the client (not just `commission()` in isolation): SLIPPAGE on a
/// near-certain fill pushes the traded price OUT of the `[0,1]` probability domain, and the
/// curve's clamp still yields a well-defined, non-negative fee — a `p > 1` would otherwise make
/// `p·(1−p)` negative and turn a taker fee into a phantom rebate.
///
/// Only the upper bound is reachable this way: adverse slippage on a BUY multiplies the price
/// UP (`raw × (1 + slippage)`), while the sell side multiplies DOWN and cannot cross zero for
/// any `slippage < 1`. The `p < 0` half of the clamp is covered on `commission()` directly.
#[test]
fn schedule_path_clamps_a_slippage_pushed_price_into_the_probability_domain() {
    let sched = FeeSchedule::ProbabilityScaled {
        taker_rate: 0.02,
        maker_rate: 0.0,
        maker_rebate_share: 0.25,
    };
    // 5% adverse slippage on a buy at 0.99 → 1.0395, outside the probability domain
    let mut c = PaperExecutionClient::with_fee_schedule("polymarket", "TOKEN", 0.05, sched);
    c.submit(&market_req("clamp", 1, 100.0));
    c.on_bar(&bar(1, 0.99, 0.99, 0.99, 0.99));
    let fills = c.fills.lock().unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].px, 0.99 * (1.0 + 0.05));
    assert!(fills[0].px > 1.0, "the fill price really did leave [0,1]: {}", fills[0].px);
    // clamped to p = 1 → curve = 0 → no fee, and crucially NOT a negative (phantom-rebate) one
    assert_eq!(fills[0].fee, sched.commission(false, 100.0, fills[0].px));
    assert_eq!(fills[0].fee, 0.0);
}

/// The `new` (flat-rate) path is unchanged — byte-identical to `broker_sim::fee` (r7 gate).
#[test]
fn new_path_is_unchanged_flat_rate() {
    let mut c = PaperExecutionClient::new("binance", "BTCUSDT", 0.0, 0.0002, 0.0007);
    c.submit(&market_req("m2", 1, 2.0));
    c.on_bar(&bar(1, 100.0, 101.0, 99.0, 100.0));
    let fills = c.fills.lock().unwrap();
    // taker flat 0.0007 of 2 * 100 = 0.14
    assert_eq!(fills[0].fee, vike_fills::broker_sim::fee(2.0, 100.0, 0.0007, 1.0));
}

fn deribit_req(coid: &str, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "deribit".into(),
        symbol: "BTC-25JUL-60000-C".into(),
        side: 1,
        qty,
        order_type: "market".into(),
        ..Default::default()
    }
}

/// Fee model follow-up 2 — the deliberate behavior change. A Deribit-options
/// `PercentOfUnderlying` fill degrades to 0.03%-of-PREMIUM at the paper site WITHOUT an
/// underlying source, but books the accurate 0.03%-of-UNDERLYING (premium-cap-bounded) WHEN one
/// is supplied. With premium 3000 / underlying 60_000 the accurate fee is exactly 20x the
/// premium-only approximation — the ~20x understatement this change fixes.
///
/// FAIL-BEFORE / PASS-AFTER: before `with_underlying_source` existed the `deribit` book had no
/// way to reach `commission_with_underlying` from the fill site, so this 20x assertion could not
/// hold; the seam is what makes it reachable.
#[test]
fn deribit_option_uses_underlying_fee_when_source_supplied() {
    let sched = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 };

    // (a) NO underlying source: today's premium-only approximation, byte-identical to before.
    let mut premium_only =
        PaperExecutionClient::with_fee_schedule("deribit", "BTC-25JUL-60000-C", 0.0, sched);
    premium_only.submit(&deribit_req("o1", 1.0));
    premium_only.on_bar(&bar(1, 3000.0, 3000.0, 3000.0, 3000.0)); // fills at open = premium 3000
    let premium_fee = premium_only.fills.lock().unwrap()[0].fee;
    // 3 bps of the PREMIUM notional: 1 * 3000 * 0.0003 = 0.9
    assert_eq!(premium_fee, 1.0 * 3000.0 * (3.0 / 10_000.0));
    assert_eq!(premium_fee, sched.commission(false, 1.0, 3000.0));

    // (b) WITH an underlying source pricing the 60_000 index: the accurate underlying fee.
    let src: UnderlyingSource = Arc::new(|venue: &str, symbol: &str, _ts: i64| {
        assert_eq!(venue, "deribit");
        assert_eq!(symbol, "BTC-25JUL-60000-C");
        Some(60_000.0)
    });
    let mut with_underlying =
        PaperExecutionClient::with_fee_schedule("deribit", "BTC-25JUL-60000-C", 0.0, sched)
            .with_underlying_source(src);
    with_underlying.submit(&deribit_req("o2", 1.0));
    with_underlying.on_bar(&bar(1, 3000.0, 3000.0, 3000.0, 3000.0));
    let underlying_fee = with_underlying.fills.lock().unwrap()[0].fee;
    // 3 bps of the UNDERLYING notional (< 12.5% of premium, so the cap does not bind):
    // 1 * 60_000 * 0.0003 = 18.0
    assert_eq!(underlying_fee, 1.0 * 60_000.0 * (3.0 / 10_000.0));
    assert_eq!(underlying_fee, sched.commission_with_underlying(1.0, 3000.0, 60_000.0));
    // the accurate fee is exactly 20x the premium-only approximation (the fixed understatement)
    assert!(
        (underlying_fee / premium_fee - 20.0).abs() < 1e-9,
        "underlying fee {underlying_fee} should be ~20x premium-only fee {premium_fee}"
    );
}

/// The seam is inert unless BOTH conditions hold. A wired source that returns `None` (no index
/// price for this instrument at this ts), and a NON-`PercentOfUnderlying` schedule even with a
/// source, both keep the previous commission bit-for-bit — the byte-identical fallback contract.
#[test]
fn underlying_source_is_inert_without_a_price_or_on_other_shapes() {
    // (a) source returns None -> premium-only PercentOfUnderlying number, unchanged.
    let deribit = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 };
    let none_src: UnderlyingSource = Arc::new(|_v: &str, _s: &str, _ts: i64| None);
    let mut c =
        PaperExecutionClient::with_fee_schedule("deribit", "BTC-25JUL-60000-C", 0.0, deribit)
            .with_underlying_source(none_src);
    c.submit(&deribit_req("o3", 1.0));
    c.on_bar(&bar(1, 3000.0, 3000.0, 3000.0, 3000.0));
    assert_eq!(c.fills.lock().unwrap()[0].fee, deribit.commission(false, 1.0, 3000.0));

    // (b) a non-underlying schedule ignores the source entirely (delegates on the premium).
    let crypto = FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 };
    let src: UnderlyingSource = Arc::new(|_v: &str, _s: &str, _ts: i64| Some(60_000.0));
    let mut c2 = PaperExecutionClient::with_fee_schedule("binance", "BTCUSDT", 0.0, crypto)
        .with_underlying_source(src);
    c2.submit(&market_req("o4", 1, 2.0));
    c2.on_bar(&bar(1, 100.0, 101.0, 99.0, 100.0));
    assert_eq!(c2.fills.lock().unwrap()[0].fee, crypto.commission(false, 2.0, 100.0));
}
