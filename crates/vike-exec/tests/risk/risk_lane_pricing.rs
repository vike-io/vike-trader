//! The pre-trade gate speaks ONE price: equity, margin-in-use, the reversal credit and a market
//! order's notional/exposure reference are all resolver-priced (`resolved_equity` /
//! `resolved_margin_in_use_by`, the one-price law reaching `gate_and_register`), never the raw
//! `Account.marks` scalar. Plus the SCOPE pin of `max_total_exposure` (one venue, one symbol).

use super::risk_lane_common::*;
use vike_exec::MarkSource;
use vike_exec::testing::RecordingClient;
use vike_exec::{ExecutionEngine, Outbox, PositionEntry};
use vike_model::OrderRequest;
use vike_model::RiskLimits;

// --- the gate's equity is resolver-priced ---

/// A quote-lane crash the stale `Account.marks` never saw must tighten admission: the SAME order is
/// admitted while the board quotes at the entry price and denied once the bid crashes (an
/// `equity_all` basis on the stale mark 100 reads equity 200 for both).
#[test]
fn gate_equity_reads_the_resolver_not_the_stale_mark() {
    let mk = |bid: f64, ask: f64| {
        let mut e = engine_with(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() });
        e.equity_seed = 200.0;
        e.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
        );
        e.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0); // stale — no longer the gate's basis
        e.price_board.set_quote("sim", "BTCUSDT", bid, ask, 1);
        e
    };
    // healthy board (bid at entry): equity 200, used 10·100·0.1 = 100, order IM 10 → admit
    let mut healthy = mk(100.0, 100.5);
    let mut outbox = Outbox::default();
    healthy.submit_order(&open_buy(1.0, 100.0), 1, &mut outbox);
    assert_eq!(healthy.client.submissions.len(), 1, "healthy quotes admit the order");
    // crashed bid: equity 200 + 10·(40−100) = −400 → deny (equity_all still said 200)
    let mut crashed = mk(40.0, 40.5);
    let mut outbox = Outbox::default();
    crashed.submit_order(&open_buy(1.0, 100.0), 1, &mut outbox);
    assert!(crashed.client.submissions.is_empty(), "a quote-lane crash must deny admission");
    assert_eq!(denied_reason(&outbox).as_deref(), Some("insufficient-margin"));
}

// --- the gate's margin-in-use is resolver-priced ---

/// The other direction: a stale HIGH `Account.marks` scalar must not overstate the open book's
/// margin. Board at the entry: used = 10·100·0.1 = 100, free = 200 − 100 ≥ IM 10 → admit (the mark
/// would charge 10·1000·0.1 = 1000 and deny).
#[test]
fn gate_margin_in_use_reads_the_resolver_not_the_stale_mark() {
    let mut e = engine_with(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() });
    e.equity_seed = 200.0;
    e.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
    );
    e.account.set_mark_from("sim", "BTCUSDT", 1000.0, MarkSource::VenueMark, 0); // stale-high — old fold priced margin here
    e.price_board.set_mark("sim", "BTCUSDT", 100.0, 1);
    let mut outbox = Outbox::default();
    e.submit_order(&open_buy(1.0, 100.0), 1, &mut outbox);
    assert_eq!(
        e.client.submissions.len(),
        1,
        "margin must price off the board, not the stale mark: {:?}",
        denied_reason(&outbox)
    );
}

// --- the reversal credit shares that basis ---

/// Long 10 @ 100 with `im 0.1`, board mark `board`, `Account.marks` scalar `stale`: sell `qty`
/// is an UNCOVERED reversal, so it faces buying power with the closing credit applied.
fn reversal_engine(board: f64, stale: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = engine_with(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() });
    e.equity_seed = 200.0;
    e.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
    );
    e.account.set_mark_from("sim", "BTCUSDT", stale, MarkSource::VenueMark, 0);
    e.price_board.set_mark("sim", "BTCUSDT", board, 1);
    e
}

fn reversal_sell(qty: f64) -> OrderRequest {
    OrderRequest { side: -1, qty, ..open_buy(qty, 100.0) }
}

/// A priceless (market) buy — the `request.price == None` arm that falls to the mark reference.
fn market_buy(qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "m1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: "market".into(),
        price: None,
        ts: 1,
        ..Default::default()
    }
}

/// The reversal `closing_credit` in `gate_and_register` reads the resolver price `margin_used`
/// does, or a stale-HIGH mark credits margin that was never charged.
///
/// Board 100, stale mark 1000. One basis: used = 10·100·0.1 = 100, credit = 2·10·100·1·0.1 = 200,
/// free = 200 − 100 + 200 = 300 < the sell-40 order's IM 40·100·0.1 = 400 → DENY. A mark-priced
/// credit (2000, free 2100) would admit it against 1700 of buying power that does not exist.
#[test]
fn reversal_credit_is_resolver_priced_not_mark_priced() {
    let mut e = reversal_engine(100.0, 1000.0);
    let mut outbox = Outbox::default();
    e.submit_order(&reversal_sell(40.0), 1, &mut outbox);
    assert!(e.client.submissions.is_empty(), "a stale-high mark must not inflate the credit");
    assert_eq!(denied_reason(&outbox).as_deref(), Some("insufficient-margin"));
}

/// The same claim as an invariant: a reversal's verdict is a function of the RESOLVED price alone.
/// Three wildly different `Account.marks` scalars over one board reach the identical admit.
#[test]
fn the_reversal_verdict_is_independent_of_the_mark_scalar() {
    for stale in [0.0, 100.0, 1000.0] {
        // sell 30 → order IM 300, free = 200 − 100 + 200 = 300 → admits on every mark
        let mut e = reversal_engine(100.0, stale);
        let mut outbox = Outbox::default();
        e.submit_order(&reversal_sell(30.0), 1, &mut outbox);
        assert_eq!(
            e.client.submissions.len(),
            1,
            "mark {stale} changed a resolver-priced verdict: {:?}",
            denied_reason(&outbox)
        );
    }
}

/// Credit and the margin it offsets move TOGETHER, because they read one price. Re-price the
/// board to 1000 and both scale ×10 — used 1000, credit 2000, equity 200 + 10·(1000−100) = 9200,
/// free = 9200 − 1000 + 2000 = 10_200 ≥ the sell-40 order's IM 4000 → the order that the
/// board-100 case denied now admits, and does so on ONE consistent valuation.
#[test]
fn credit_and_the_margin_it_offsets_scale_on_one_basis() {
    let mut e = reversal_engine(1000.0, 1000.0);
    let mut outbox = Outbox::default();
    e.submit_order(&reversal_sell(40.0), 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1, "{:?}", denied_reason(&outbox));
}

/// An UNPRICEABLE position credits nothing, as `resolved_margin_in_use_by` drops it from
/// `margin_used`. Empty board (every slot `Missing`) with a fat stale mark: used 0, credit 0,
/// equity = seed 200 (no unrealized), so the sell-40 order's IM 400 exceeds free 200 → deny.
#[test]
fn an_unpriceable_position_credits_no_margin() {
    let mut e = engine_with(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() });
    e.equity_seed = 200.0;
    e.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
    );
    e.account.set_mark_from("sim", "BTCUSDT", 1000.0, MarkSource::VenueMark, 0); // board deliberately left empty
    let mut outbox = Outbox::default();
    e.submit_order(&reversal_sell(40.0), 1, &mut outbox);
    assert!(e.client.submissions.is_empty(), "no price ⇒ no credit, symmetric with no charge");
    assert_eq!(denied_reason(&outbox).as_deref(), Some("insufficient-margin"));
}

// --- the notional / exposure lane reads that basis too ---

/// A market (priceless) order's notional+exposure REFERENCE is resolver-priced too, so the whole
/// gate call speaks one price, as the single-price `SimBroker::gate_market_order` does; a
/// stale-LOW mark would UNDERSTATE projected exposure, the risk-unsafe direction. Long 10 @ 100,
/// board fresh at 200, stale `Account.marks` at 100, `max_total_exposure` 2500: a market buy 3
/// projects (10+3)·200 = 2600 > 2500 → DENY (the mark would say 1300 and admit).
#[test]
fn gate_exposure_reads_the_resolver_not_the_stale_mark() {
    let mut e = engine_with(RiskLimits { max_total_exposure: Some(2500.0), ..RiskLimits::new() });
    e.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
    );
    e.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0); // stale-low
    e.price_board.set_mark("sim", "BTCUSDT", 200.0, 1); // fresh
    let mut outbox = Outbox::default();
    e.submit_order(&market_buy(3.0), 1, &mut outbox);
    assert!(
        e.client.submissions.is_empty(),
        "a fresh board must not be under-measured by a stale mark: {:?}",
        denied_reason(&outbox)
    );
    assert_eq!(denied_reason(&outbox).as_deref(), Some("over-max-exposure"));
}

/// As the invariant (the notional-lane twin of
/// `the_reversal_verdict_is_independent_of_the_mark_scalar`): three wildly different
/// `Account.marks` scalars over ONE fresh board all reach the identical DENY.
#[test]
fn the_notional_lane_verdict_is_independent_of_the_mark_scalar() {
    for stale in [0.0, 100.0, 5_000.0] {
        let mut e =
            engine_with(RiskLimits { max_total_exposure: Some(2500.0), ..RiskLimits::new() });
        e.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
        );
        e.account.set_mark_from("sim", "BTCUSDT", stale, MarkSource::VenueMark, 0);
        e.price_board.set_mark("sim", "BTCUSDT", 200.0, 1);
        let mut outbox = Outbox::default();
        e.submit_order(&market_buy(3.0), 1, &mut outbox);
        assert!(
            e.client.submissions.is_empty(),
            "mark {stale} changed a resolver-priced notional verdict: {:?}",
            denied_reason(&outbox)
        );
        assert_eq!(denied_reason(&outbox).as_deref(), Some("over-max-exposure"));
    }
}

// --- the exposure cap's SCOPE ---

/// The scope-pin engine: mounted on `("sim", "BTCUSDT")` with `max_total_exposure = cap`, every
/// OTHER lane disarmed (`RiskLimits::new()`), plus one arbitrary position bucket. The probe is
/// always a PRICED limit, so `ctx.mark_price` is its own price and no resolver can move the number.
fn exposure_engine(cap: f64, other: Option<(&str, &str, f64)>) -> ExecutionEngine<RecordingClient> {
    let mut e = engine_with(RiskLimits { max_total_exposure: Some(cap), ..RiskLimits::new() });
    if let Some((venue, symbol, size)) = other {
        e.account.positions.insert(
            (venue.into(), symbol.into(), "BOTH".into()),
            PositionEntry { size, avg_px: 100.0, ..Default::default() },
        );
    }
    e
}

/// Submit the fixed probe order (BUY 5 BTCUSDT @ 100 ⇒ the order's own projected notional is
/// exactly 500.0 on a flat book) and report the denial reason, `None` when it was admitted.
fn exposure_verdict(e: &mut ExecutionEngine<RecordingClient>) -> Option<String> {
    let mut outbox = Outbox::default();
    e.submit_order(&open_buy(5.0, 100.0), 1, &mut outbox);
    let reason = denied_reason(&outbox);
    assert_eq!(
        e.client.submissions.is_empty(),
        reason.is_some(),
        "admitted-vs-denied must agree with the outbox"
    );
    reason
}

/// THE SCOPE PIN for `RiskLimits::max_total_exposure`.
///
/// The field is named as though it capped an account, and `vike_mount::require_live_risk_budget`
/// makes an operator supply it before ANY live venue mounts — but the one site that evaluates it
/// (`RiskGate::check_inner`'s `over-max-exposure` lane) reads `RiskContext::position_size`, which
/// `ExecutionEngine::gate_position_size` fills from exactly ONE `(venue, symbol)` bucket. Three
/// arms, all sharing one probe order (BUY 5 @ 100 ⇒ 500.0 of projected notional by itself) and a
/// cap armed at that exact boundary, so ANY widening of the basis pushes `projected` over it:
///
///   1. the ORDER's own `(venue, symbol)` bucket COUNTS — the cap is genuinely armed here;
///   2. another SYMBOL at the same venue contributes NOTHING;
///   3. the same SYMBOL at another venue contributes NOTHING.
///
/// ⚠ Arms 2 and 3 pin TODAY'S scope, which is also what the field's doc,
/// `vike_model::ProfileRisk::max_total_exposure`, `vike_mount::BUDGET_EXAMPLES` and
/// `docs/ops/run-profile-live.toml` say. If this lane ever becomes account-aggregate, those two
/// arms go RED — the alarm working. INVERT them and update all four texts in the same PR; never
/// delete them, or the name becomes a lie again with nothing watching.
#[test]
fn max_total_exposure_is_scoped_to_one_venue_and_one_symbol() {
    // The probe's own projected notional on a flat book: |0 + 1*5| * 100 * 1.0.
    const OWN: f64 = 500.0;

    // (1) THE CAP IS ARMED: the order's own bucket enters `projected`, and the comparison is
    // strict (`projected > cap`): the exact boundary admits, a hair under denies. Without this arm,
    // arms 2 and 3 could pass with nothing checked at all.
    assert_eq!(exposure_verdict(&mut exposure_engine(OWN, None)), None, "cap == projected admits");
    assert_eq!(
        exposure_verdict(&mut exposure_engine(OWN - 0.01, None)).as_deref(),
        Some("over-max-exposure"),
        "a hair under the boundary must deny — the lane is armed"
    );
    // ...and a real position in the ORDER's own symbol/venue does move it: |6 + 5| * 100 = 1100.
    assert_eq!(
        exposure_verdict(&mut exposure_engine(OWN, Some(("sim", "BTCUSDT", 6.0)))).as_deref(),
        Some("over-max-exposure"),
        "the order symbol's own position MUST count toward the cap"
    );

    // (2) ANOTHER SYMBOL, SAME VENUE — contributes nothing. A million units of ETHUSDT at the
    // same venue, and the BTCUSDT order still sees only its own 500.0.
    assert_eq!(
        exposure_verdict(&mut exposure_engine(OWN, Some(("sim", "ETHUSDT", 1_000_000.0)))),
        None,
        "SCOPE: `max_total_exposure` is PER SYMBOL — a position in another symbol must not \
         count. If this now denies, the lane became account-aggregate: invert this arm and \
         update `RiskLimits::max_total_exposure`'s doc, `ProfileRisk`'s twin, \
         `vike_mount::BUDGET_EXAMPLES` and `docs/ops/run-profile-live.toml` together"
    );

    // (3) SAME SYMBOL, ANOTHER VENUE — contributes nothing. `make_engine` builds one engine, and
    // so one `RiskGate` holding one copy of this cap, PER VENUE; a foreign-venue row can reach
    // this account's position map through reconcile, and it is still out of scope.
    assert_eq!(
        exposure_verdict(&mut exposure_engine(OWN, Some(("other", "BTCUSDT", 1_000_000.0)))),
        None,
        "SCOPE: `max_total_exposure` is PER VENUE — the same symbol at another venue must not \
         count. Same standing instruction as the arm above"
    );
}
