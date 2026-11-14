use super::test_support::bar;
use super::*;
use vike_execution::ExecutionClient;
use vike_model::OrderRequest;

fn market_request(order_client_id: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    }
}

/// The `with_fee_schedule` path applies `FeeSchedule::commission` (taker for a market fill).
#[test]
fn schedule_path_charges_taker_bps_on_a_market_fill() {
    let sched = FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 };
    let mut c = ExecutionClientPaper::with_fee_schedule("binance", "BTCUSDT", 0.0, sched);
    c.submit(&market_request("m1", 1, 2.0));
    c.on_bar(&bar(1, 100.0, 101.0, 99.0, 100.0)); // market fills at next open = 100
    let fills = c.fills.lock().unwrap();
    assert_eq!(fills.len(), 1);
    // taker 10 bps of 2 * 100 = 0.2
    assert_eq!(fills[0].fee, sched.commission(false, 2.0, 100.0));
    assert_eq!(fills[0].fee, 2.0 * 100.0 * (10.0 / 10_000.0));
}

/// ⚠ **A schedule's fee scales by the contract multiplier**, as the flat arm here and both of
/// `OrderBrokerSimulator::apply_fill`'s arms do. Every constructor leaves it at `1.0`, which is why the two
/// arms agreed; the day paper gets a contract value the schedule's fee would be 1/50 of the
/// backtest's: 0.2 against 10.
#[test]
fn a_schedule_fee_scales_by_the_contract_multiplier() {
    let sched = FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 };
    let mut scheduled = ExecutionClientPaper::with_fee_schedule("binance", "BTCUSDT", 0.0, sched);
    let mut flat = ExecutionClientPaper::new("binance", "BTCUSDT", 0.0, 0.001, 0.001);
    scheduled.multiplier = 50.0;
    flat.multiplier = 50.0;
    let of = |c: &ExecutionClientPaper| c.commission_for(false, 2.0, 100.0, "BTCUSDT", 0);
    assert_eq!(of(&scheduled), sched.commission(false, 2.0, 100.0) * 50.0);
    assert_eq!(of(&flat), vike_model::ContractValue::linear(50.0).fee(2.0, 100.0, 0.001));
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
    let mut c = ExecutionClientPaper::with_fee_schedule("polymarket", "TOKEN", 0.0, sched);
    c.submit(&market_request("p1", 1, 100.0));
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
    let mut c = ExecutionClientPaper::with_fee_schedule("polymarket", "TOKEN", 0.0, sched);
    // rest a buy limit at 0.5; the bar opens above and trades through it → maker fill @ 0.5
    let req = OrderRequest {
        order_client_id: "m1".into(),
        venue: "polymarket".into(),
        symbol: "TOKEN".into(),
        side: 1,
        qty: 100.0,
        order_type: vike_model::OrderType::Limit,
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
    let mut c = ExecutionClientPaper::with_fee_schedule("polymarket", "TOKEN", 0.05, sched);
    c.submit(&market_request("clamp", 1, 100.0));
    c.on_bar(&bar(1, 0.99, 0.99, 0.99, 0.99));
    let fills = c.fills.lock().unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].price, 0.99 * (1.0 + 0.05));
    assert!(fills[0].price > 1.0, "the fill price really did leave [0,1]: {}", fills[0].price);
    // clamped to p = 1 → curve = 0 → no fee, and crucially NOT a negative (phantom-rebate) one
    assert_eq!(fills[0].fee, sched.commission(false, 100.0, fills[0].price));
    assert_eq!(fills[0].fee, 0.0);
}

/// The `new` (flat-rate) path is unchanged — byte-identical to `ContractValue::fee` at unit 1.0,
/// once `broker_sim::fee` (r7 gate).
#[test]
fn new_path_is_unchanged_flat_rate() {
    let mut c = ExecutionClientPaper::new("binance", "BTCUSDT", 0.0, 0.0002, 0.0007);
    c.submit(&market_request("m2", 1, 2.0));
    c.on_bar(&bar(1, 100.0, 101.0, 99.0, 100.0));
    let fills = c.fills.lock().unwrap();
    // taker flat 0.0007 of 2 * 100 = 0.14
    assert_eq!(fills[0].fee, vike_model::ContractValue::linear(1.0).fee(2.0, 100.0, 0.0007));
}

/// A market buy of a deribit USDC option: a coin-settled one is refused by the paper book (its premium is
/// coin, `paper_contract_value`), so the fee seam is exercised on the option the book values.
fn deribit_request(order_client_id: &str, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "deribit".into(),
        symbol: "BTC_USDC-25JUL26-60000-C".into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    }
}

/// The order's one terminal rejection reason, or `None` when nothing rejected it.
fn rejection(c: &mut ExecutionClientPaper) -> Option<String> {
    super::test_support::drain(c).into_iter().find_map(|e| match e {
        vike_model::events::Event::OrderRejected(r) => Some(r.reason.to_string()),
        _ => None,
    })
}

/// Fee model follow-up 2. A Deribit-options `PercentOfUnderlying` fill books the accurate
/// 0.03%-of-UNDERLYING (premium-cap-bounded) when an underlying source prices it; WITHOUT one the
/// order is REFUSED, never charged on the premium. With premium 3000 / underlying 60_000 the accurate
/// fee is exactly 20x the premium reading — the understatement the refusal keeps out of the book.
#[test]
fn deribit_option_uses_underlying_fee_when_source_supplied() {
    let sched = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_percent: 0.125 };

    // (a) NO underlying source: refused, nothing fills.
    let mut unpriced =
        ExecutionClientPaper::with_fee_schedule("deribit", "BTC_USDC-25JUL26-60000-C", 0.0, sched);
    unpriced.submit(&deribit_request("o1", 1.0));
    assert!(rejection(&mut unpriced).is_some_and(|r| r.starts_with("paper-option-fee")));
    unpriced.on_bar(&bar(1, 3000.0, 3000.0, 3000.0, 3000.0));
    assert!(unpriced.fills.lock().unwrap().is_empty(), "a refused order never fills");
    // 3 bps of the PREMIUM notional, the reading the refusal keeps out: 1 * 3000 * 0.0003 = 0.9
    let premium_fee = sched.commission(false, 1.0, 3000.0);

    // (b) WITH an underlying source pricing the 60_000 index: the accurate underlying fee.
    let src: UnderlyingSource = Arc::new(|venue: &str, symbol: &str, _ts: i64| {
        assert_eq!(venue, "deribit");
        assert_eq!(symbol, "BTC_USDC-25JUL26-60000-C");
        Some(60_000.0)
    });
    let mut with_underlying =
        ExecutionClientPaper::with_fee_schedule("deribit", "BTC_USDC-25JUL26-60000-C", 0.0, sched)
            .with_underlying_source(src);
    with_underlying.submit(&deribit_request("o2", 1.0));
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

/// A wired source that returns `None` (no index price for this instrument at this ts) prices
/// nothing, so the option order is refused as with no source; a NON-`PercentOfUnderlying` schedule
/// ignores a source and keeps the previous commission bit-for-bit.
#[test]
fn underlying_source_is_inert_without_a_price_or_on_other_shapes() {
    // (a) source returns None -> refused, nothing charged on the premium.
    let deribit = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_percent: 0.125 };
    let none_source: UnderlyingSource = Arc::new(|_v: &str, _s: &str, _ts: i64| None);
    let mut c = ExecutionClientPaper::with_fee_schedule(
        "deribit",
        "BTC_USDC-25JUL26-60000-C",
        0.0,
        deribit,
    )
    .with_underlying_source(none_source);
    c.submit(&deribit_request("o3", 1.0));
    assert!(rejection(&mut c).is_some_and(|r| r.starts_with("paper-option-fee")));
    c.on_bar(&bar(1, 3000.0, 3000.0, 3000.0, 3000.0));
    assert!(c.fills.lock().unwrap().is_empty());

    // (b) a non-underlying schedule ignores the source entirely (delegates on the premium).
    let crypto = FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 };
    let src: UnderlyingSource = Arc::new(|_v: &str, _s: &str, _ts: i64| Some(60_000.0));
    let mut c2 = ExecutionClientPaper::with_fee_schedule("binance", "BTCUSDT", 0.0, crypto)
        .with_underlying_source(src);
    c2.submit(&market_request("o4", 1, 2.0));
    c2.on_bar(&bar(1, 100.0, 101.0, 99.0, 100.0));
    assert_eq!(c2.fills.lock().unwrap()[0].fee, crypto.commission(false, 2.0, 100.0));
}

/// THE MEASURED FILL, on the book the paper tier builds (deribit's own registry schedule). 0.01 of a
/// USDC option at 1,900 with BTC at 77,266.67 paid 0.2318 USDC at the venue: with a price for the
/// underlying the book charges that; with none it REFUSES the order, by name, where it used to fill and
/// charge 0.0057 on the premium. A combo with that option as a leg is refused the same way, and a
/// deribit symbol that is no option trades as before.
#[test]
fn a_deribit_usdc_option_is_charged_on_its_underlying_or_refused() {
    let sched = vike_venues::fee_schedule_for("deribit");
    let option = "BTC_USDC-27NOV26-80000-C";
    let req = |symbol: &str| OrderRequest {
        order_client_id: "m".into(),
        venue: "deribit".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 0.01,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };

    let mut unpriced = ExecutionClientPaper::with_fee_schedule("deribit", option, 0.0, sched);
    unpriced.submit(&req(option));
    let why = rejection(&mut unpriced).expect("an unpriced option is refused");
    assert!(why.contains(option) && why.contains("BTC_USDC-PERPETUAL"), "{why}");
    unpriced.on_bar(&bar(1, 1_900.0, 1_900.0, 1_900.0, 1_900.0));
    assert!(unpriced.fills.lock().unwrap().is_empty(), "never charged on the premium");

    let src: UnderlyingSource = Arc::new(|_v: &str, _s: &str, _ts: i64| Some(77_266.67));
    let mut priced = ExecutionClientPaper::with_fee_schedule("deribit", option, 0.0, sched)
        .with_underlying_source(src);
    priced.submit(&req(option));
    priced.on_bar(&bar(1, 1_900.0, 1_900.0, 1_900.0, 1_900.0));
    let fee = priced.fills.lock().unwrap()[0].fee;
    assert!((fee - 0.2318).abs() < 1e-6, "the venue's 0.2318: {fee}");

    let mut combo = ExecutionClientPaper::with_fee_schedule("deribit", "COMBO", 0.0, sched);
    let spec = vike_model::OrderCombo {
        venue: "deribit".into(),
        side: 1,
        qty: 0.01,
        legs: vec![
            vike_model::OrderComboLeg { symbol: option.into(), ratio: 1, limit: None },
            vike_model::OrderComboLeg {
                symbol: "BTC_USDC-27NOV26-90000-C".into(),
                ratio: -1,
                limit: None,
            },
        ],
        net_limit: None,
        time_in_force: vike_model::TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    };
    combo.submit(&vike_model::build_combo(&spec, "c").expect("a valid combo"));
    assert!(rejection(&mut combo).is_some_and(|r| r.starts_with("paper-option-fee")));

    let perp = "BTC_USDC-PERPETUAL";
    let mut plain = ExecutionClientPaper::with_fee_schedule("deribit", perp, 0.0, sched);
    plain.submit(&req(perp));
    assert_eq!(rejection(&mut plain), None, "no option, no refusal");
}
