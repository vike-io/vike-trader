//! Risk-lane completion (wave-2, the #518 leftovers): the pre-trade gate's equity AND
//! margin-in-use are resolver-priced (`resolved_equity` / `resolved_margin_in_use_by` — the
//! one-price law reaching `gate_and_register`), and the gate's coverage basis is
//! hedge-mode-aware (`LONG`/`SHORT` buckets no longer read as a flat book, so the #458
//! anti-stranding floor bypass works for hedge accounts). Mirrors the `resolve_equity.rs`
//! integration-test convention (tests/ file, `RecordingClient`, `ExecutionEngine::new`).

use vike_exec::MarkSource;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, ExecutionEngine, Outbox, PositionEntry, RiskGate, RiskLimits,
};
use vike_model::OrderRequest;
use vike_model::events::Event;

fn engine_with(limits: RiskLimits) -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

fn open_buy(qty: f64, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "c1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(price),
        ts: 1,
        ..Default::default()
    }
}

fn denied_reason(outbox: &Outbox) -> Option<String> {
    outbox.0.iter().find_map(|e| match e {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    })
}

// ---- the gate's equity is resolver-priced ------------------------------------------------

/// A quote-lane crash the stale `Account.marks` never saw must tighten admission: the SAME
/// order is admitted while the board quotes at the entry price and denied once the bid
/// crashes — under the old `equity_all` basis (stale mark 100) both cases read equity 200
/// and admitted.
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

// ---- the gate's margin-in-use is resolver-priced -----------------------------------------

/// The other direction of the same asymmetry: an absurdly-stale HIGH `Account.marks` scalar
/// must no longer overstate the open book's margin. Board prices at the entry (equity flat);
/// old basis: used = 10·1000·0.1 = 1000 → deny; one basis: used = 10·100·0.1 = 100,
/// free = 200 − 100 ≥ IM 10 → admit.
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

// ---- the reversal credit shares that basis ------------------------------------------------

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

/// THE split-basis defect #524 left inside `gate_and_register`: `margin_used` and `equity` moved
/// onto the resolver, the reversal `closing_credit` stayed on `self.mark()`. A stale-HIGH mark
/// therefore credited margin the resolver-priced `margin_used` had never charged.
///
/// Board 100, stale mark 1000. One basis: used = 10·100·0.1 = 100, credit = 2·10·100·1·0.1 = 200,
/// free = 200 − 100 + 200 = 300 < the sell-40 order's IM 40·100·0.1 = 400 → DENY. Split basis:
/// credit = 2·10·1000·1·0.1 = 2000, free = 2100 → the same order was admitted against 1700 of
/// buying power that did not exist.
#[test]
fn reversal_credit_is_resolver_priced_not_mark_priced() {
    let mut e = reversal_engine(100.0, 1000.0);
    let mut outbox = Outbox::default();
    e.submit_order(&reversal_sell(40.0), 1, &mut outbox);
    assert!(e.client.submissions.is_empty(), "a stale-high mark must not inflate the credit");
    assert_eq!(denied_reason(&outbox).as_deref(), Some("insufficient-margin"));
}

/// The same claim stated as an invariant rather than one number: the verdict on a reversal is a
/// function of the RESOLVED price alone. Three wildly different `Account.marks` scalars over the
/// same board must all reach the identical admit verdict — the mark is no longer an input.
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

/// An UNPRICEABLE position credits nothing — the same end of the law as
/// `resolved_margin_in_use_by` dropping it from `margin_used`. Empty board (every slot
/// `Missing`) with a fat stale mark: used 0, credit 0, equity = seed 200 (no unrealized), so the
/// sell-40 order's IM 400 exceeds free 200 → deny. Under the mark basis the credit was 2000.
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

// ---- the notional / exposure lane reads that basis too ------------------------------------

/// The THIRD sibling to the two resolver tests above (equity + margin-in-use): a market
/// (priceless) order's notional+exposure REFERENCE used to be the raw `Account.marks` scalar,
/// while the SAME `gate_and_register` call's equity/margin/credit were resolver-priced and the
/// single-price `SimBroker::gate_market_order` judges all four off ONE price. That split let a
/// stale-LOW mark UNDERSTATE projected exposure and admit an order the fresh board denies — the
/// risk-unsafe direction. The reference now shares the resolver, so the whole gate call (and both
/// engines) speak one price. Long 10 @ 100, board fresh at 200, stale `Account.marks` at 100,
/// `max_total_exposure` 2500: a market buy 3 projects (10+3)·200 = 2600 > 2500 → DENY. Under the
/// split basis it was (10+3)·100 = 1300 → admitted.
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

/// Stated as the invariant: the market-order notional/exposure verdict is a function of the
/// RESOLVED price alone — three wildly different `Account.marks` scalars over ONE fresh board all
/// reach the identical DENY, so the raw scalar is no longer an input (the notional-lane twin of
/// `the_reversal_verdict_is_independent_of_the_mark_scalar`). Under the split basis, stale 0 and
/// 100 ADMITTED while stale 5_000 denied — the verdict tracked the scalar, not the board.
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

// ---- the exposure cap's SCOPE -------------------------------------------------------------

/// Build the scope-pin engine: mounted on `("sim", "BTCUSDT")` with `max_total_exposure = cap`
/// and every OTHER lane disarmed (`RiskLimits::new()` leaves `im_requirement`/`min_*`/
/// `max_notional_per_order`/the throttle all off), then seed one arbitrary position bucket.
///
/// The submitted order below is always a PRICED limit, so `ctx.mark_price` is the order's own
/// price and no mark/resolver plumbing can move the number under test.
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
/// `vike_exec::ProfileRisk::max_total_exposure`, `vike_mount::BUDGET_EXAMPLES` and
/// `docs/ops/run-profile-live.toml` all now say. If a future change makes this lane genuinely
/// account-aggregate, those two arms go RED — that is the alarm working. INVERT them and update
/// all four texts in the same PR; never delete them, or the name becomes a lie again with nothing
/// watching.
#[test]
fn max_total_exposure_is_scoped_to_one_venue_and_one_symbol() {
    // The probe's own projected notional on a flat book: |0 + 1*5| * 100 * 1.0.
    const OWN: f64 = 500.0;

    // (1) THE CAP IS ARMED. The order symbol's own bucket at the engine's own venue enters
    // `projected`, and the comparison is strict (`projected > cap`): at the exact boundary the
    // order is admitted, a hair under it is denied. Without this arm, arms 2 and 3 could pass
    // simply because nothing was being checked at all.
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

// ---- hedge-mode-aware coverage ------------------------------------------------------------

fn min_qty_limits() -> RiskLimits {
    RiskLimits { min_qty: Some(1.0), ..RiskLimits::new() }
}

fn hedge_engine(long: f64, short: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = engine_with(min_qty_limits());
    if long != 0.0 {
        e.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "LONG".into()),
            PositionEntry { size: long, avg_px: 100.0, ..Default::default() },
        );
    }
    if short != 0.0 {
        e.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "SHORT".into()),
            PositionEntry { size: short, avg_px: 100.0, ..Default::default() },
        );
    }
    e
}

fn below_min_close(side: i32) -> OrderRequest {
    OrderRequest { reduce_only: true, ..open_buy(0.5, 100.0) }.with_side(side)
}

trait WithSide {
    fn with_side(self, side: i32) -> Self;
}
impl WithSide for OrderRequest {
    fn with_side(mut self, side: i32) -> Self {
        self.side = side;
        self
    }
}

/// THE hedge-mode anti-stranding fix: a below-min reduce-only flatten of a hedge LONG bucket
/// must pass the #458 floor bypass. Before, the ctx carried `position_size("BOTH")` = 0, so
/// `is_covered_reduce` read a flat book and DENIED the close — stranding exactly the dust the
/// bypass protects.
#[test]
fn hedge_mode_below_min_close_passes_the_floor_bypass() {
    let mut e = hedge_engine(5.0, 0.0);
    let mut outbox = Outbox::default();
    e.submit_order(&below_min_close(-1), 1, &mut outbox);
    assert_eq!(
        e.client.submissions.len(),
        1,
        "a covered hedge-bucket close must bypass the min-qty floor: {:?}",
        denied_reason(&outbox)
    );
    // the SHORT bucket mirrors it (bucket size folded signed: SHORT ≤ 0)
    let mut e = hedge_engine(0.0, -5.0);
    let mut outbox = Outbox::default();
    e.submit_order(&below_min_close(1), 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1, "short-bucket close covered too");
}

/// A below-min OPENING order in hedge mode stays floor-gated (the bypass is coverage-gated,
/// not a hedge-mode blanket), and a both-buckets account NETS toward zero — a "close" the net
/// does not cover is still denied (conservative, documented on `gate_position_size`).
#[test]
fn hedge_mode_floor_still_gates_openings_and_netted_books() {
    // opening (same direction as the bucket): no coverage → below-min-qty
    let mut e = hedge_engine(5.0, 0.0);
    let mut outbox = Outbox::default();
    e.submit_order(&open_buy(0.5, 100.0), 1, &mut outbox);
    assert!(e.client.submissions.is_empty());
    assert_eq!(denied_reason(&outbox).as_deref(), Some("below-min-qty"));
    // LONG 5 + SHORT −5 nets to 0: no single-number coverage → still denied
    let mut e = hedge_engine(5.0, -5.0);
    let mut outbox = Outbox::default();
    e.submit_order(&below_min_close(-1), 1, &mut outbox);
    assert!(e.client.submissions.is_empty(), "a fully-netted hedge book stays conservative");
    assert_eq!(denied_reason(&outbox).as_deref(), Some("below-min-qty"));
}

/// One-way accounts are byte-identical: the `BOTH` bucket is non-zero, so the hedge fallback
/// is never consulted — the covered close bypasses and the flat book still floor-gates,
/// exactly as before this change.
#[test]
fn one_way_coverage_is_unchanged() {
    // covered one-way close: admitted (as always)
    let mut e = engine_with(min_qty_limits());
    e.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        PositionEntry { size: 5.0, avg_px: 100.0, ..Default::default() },
    );
    let mut outbox = Outbox::default();
    e.submit_order(&below_min_close(-1), 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1);
    // flat book + reduce_only flag: still an opening order, still floor-gated
    let mut e = engine_with(min_qty_limits());
    let mut outbox = Outbox::default();
    e.submit_order(&below_min_close(-1), 1, &mut outbox);
    assert!(e.client.submissions.is_empty());
    assert_eq!(denied_reason(&outbox).as_deref(), Some("below-min-qty"));
}

// ---- the lanes that carry NO covered-reduce bypass -----------------------------------------
//
// CHARACTERIZATION. These tests REVEAL the current verdict; none of them argues for a cure.
//
// `RiskGate::check_inner` computes `covered_reduce` ONCE and hands it to the anti-stranding
// bypasses — the min-qty and min-notional floors, the fat-finger price collar, the buying-power
// charge and the pre-trade impact veto all read it, and the `Halted` kill switch is decided by
// the same `vike_model::is_covered_reduce` predicate. THREE lanes downstream of it do not mention
// it at all:
//
//   * `RiskGate::admit_throttle` (the `"rate-limited"` lane, guarded only by `consume_throttle`),
//   * `RiskLimits::max_notional_per_order` (`"over-max-notional"`),
//   * `RiskLimits::max_total_exposure` (`"over-max-exposure"`).
//
// The last two are the ones `vike_mount::require_live_risk_budget` refuses to start a LIVE mount
// without, so on a live venue they are ARMED BY CONSTRUCTION — they are the MORE reachable
// denials, not the throttle. Nothing here claims the throttle is the only such lane.
//
// Why nothing covered this before: `vike_core::runtime::apply`'s `test_core`/`test_core_with`
// build their gate from `RiskLimits::new()`, whose `max_orders_per_window` is `None`, so every
// `OrderIntent::MarketExit` test in the tree — the halt-does-not-trap-you one included — runs
// with the throttle DISARMED.

/// An engine holding one one-way (`BOTH`) position of `pos` in the mounted symbol, `limits` armed.
fn engine_holding(limits: RiskLimits, pos: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = engine_with(limits);
    e.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        PositionEntry { size: pos, avg_px: 100.0, ..Default::default() },
    );
    e
}

/// Price the mounted symbol on BOTH stores at `px`, so no resolver arm can move the number under
/// test (the `reversal_engine` idiom above, which seeds the stale scalar and the board together).
fn price_at(e: &mut ExecutionEngine<RecordingClient>, px: f64) {
    e.account.set_mark_from("sim", "BTCUSDT", px, MarkSource::VenueMark, 0);
    e.price_board.set_mark("sim", "BTCUSDT", px, 1);
}

/// The EXACT shape the panic button lowers a position into: `vike_core`'s
/// `CoreThread::market_exit_flatten_legs` emits one `OrderIntent::Flatten` per non-flat position,
/// and that intent's arm mints a `reduce_only` MARKET order for `|position|` on
/// `vike_model::closing_side`. So this is `is_covered_reduce` in every sense the gate has —
/// direction opposes the position AND `|position| >= |qty|` — with the caller's flag set too.
fn flatten_leg(coid: &str, pos: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: vike_model::closing_side(pos),
        qty: pos.abs(),
        order_type: "market".into(),
        reduce_only: true,
        ts: 1,
        ..Default::default()
    }
}

/// An opening BUY under `coid` — `open_buy`'s terms with a fresh id, so the throttle-filling
/// submits do not collapse onto one registry entry.
fn opening(coid: &str) -> OrderRequest {
    OrderRequest { client_order_id: coid.into(), ..open_buy(1.0, 100.0) }
}

/// Submit `req` at `now_ms` and report the denial reason, `None` when it was admitted — the
/// `exposure_verdict` idiom above, over an arbitrary request and an arbitrary clock stamp.
fn verdict_at(
    e: &mut ExecutionEngine<RecordingClient>,
    req: &OrderRequest,
    now_ms: i64,
) -> Option<String> {
    let mut outbox = Outbox::default();
    e.submit_order(req, now_ms, &mut outbox);
    denied_reason(&outbox)
}

/// THE THROTTLE LANE, CHARACTERIZED: a POSITION-COVERED reduce is metered by the same
/// session-wide sliding window every opening order spends, and an exhausted window refuses it.
///
/// `RiskGate`'s own doc calls the window "shared across ALL symbols routed through it (a
/// session-level order-rate limit)", and `RiskGate::check_inner`'s last lane is
/// `if consume_throttle && !self.admit_throttle(ctx.now_ms)` — no `covered_reduce` term, unlike
/// every bypass above it. This test spends the window on OPENING submits stamped at ONE `now_ms`
/// (the shape the runtime produces: `CoreThread::dispatch` reads the injected clock once per
/// message, and `CoreThread::apply_intent` stamps every leg of one compound verb with that same
/// `now`), then crosses the flatten leg the panic button would mint.
///
/// ⚠ It PINS the verdict, whatever it is. If a covered-reduce bypass is ever added to
/// `admit_throttle`'s guard this goes red — that is the alarm working, and the response is to
/// invert the assertion in the same PR, never to delete it.
#[test]
fn a_position_covered_reduce_is_metered_by_the_shared_order_rate_window() {
    // two orders per window; `RiskLimits::new()` supplies `window_ms = 1000`
    let mut e =
        engine_holding(RiskLimits { max_orders_per_window: Some(2), ..RiskLimits::new() }, 2.0);
    for coid in ["open-1", "open-2"] {
        assert_eq!(
            verdict_at(&mut e, &opening(coid), 1),
            None,
            "precondition: the window admits {coid}"
        );
    }
    // ...and the window really is spent — a third OPENING order at the same stamp is refused.
    assert_eq!(
        verdict_at(&mut e, &opening("open-3"), 1).as_deref(),
        Some("rate-limited"),
        "precondition: the throttle is armed"
    );

    // THE MEASUREMENT: the exit leg, at the same clock stamp, against the same spent window.
    assert_eq!(
        verdict_at(&mut e, &flatten_leg("exit-1", 2.0), 1).as_deref(),
        Some("rate-limited"),
        "a position-covered reduce crosses the SAME session window an opening order does — the \
         throttle lane carries no `covered_reduce` bypass"
    );
    assert_eq!(
        e.client.submissions.len(),
        2,
        "and the exit never reached the venue: {:?}",
        e.client.submissions
    );

    // MUTATION SENTINEL: it was the WINDOW that refused it, not some other lane. Once the window
    // has slid past the opening stamps (`admit_throttle`'s cutoff is `now_ms - window_ms`), the
    // identical leg is admitted.
    assert_eq!(
        verdict_at(&mut e, &flatten_leg("exit-2", 2.0), 1_002),
        None,
        "the same leg one window later must be admitted — otherwise this test pins the wrong lane"
    );
    assert_eq!(e.client.submissions.len(), 3);
}

/// THE TWO MANDATORY LIVE CAPS, CHARACTERIZED — the more reachable half of the same class.
///
/// `vike_mount::require_live_risk_budget` refuses to start a live mount unless BOTH
/// `max_notional_per_order` and `max_total_exposure` are set (its `MountError::MissingRiskBudget`
/// names exactly those two keys, and `vike_mount::BUDGET_EXAMPLES` carries their example values),
/// so on any live venue these lanes are armed by construction while the throttle above may not be.
/// Neither reads `covered_reduce`.
///
/// The two arms differ in REACHABILITY, and the difference is the interesting part:
///
///   * `over-max-notional` judges the order AS IT GOES ON THE WIRE, so a FULL flatten of a
///     position bigger than one order's cap is refused — the very leg the panic button mints for
///     exactly that position;
///   * `over-max-exposure` judges the PROJECTED world, so a full flatten projects `0` and always
///     passes. Only a PARTIAL reduce of a position ALREADY over the cap (a shape a reconcile fold
///     or a venue liquidation can seed) trips it.
#[test]
fn the_mandatory_live_caps_have_no_covered_reduce_bypass_either() {
    // ---- per-order notional cap: a full flatten of 5 @ 100 is 500 of wire notional vs a 100 cap
    let caps = RiskLimits { max_notional_per_order: Some(100.0), ..RiskLimits::new() };
    let mut e = engine_holding(caps, 5.0);
    price_at(&mut e, 100.0);
    assert_eq!(
        verdict_at(&mut e, &flatten_leg("exit-1", 5.0), 1).as_deref(),
        Some("over-max-notional"),
        "a covered reduce is capped like any opening order — this lane has no bypass"
    );
    assert!(e.client.submissions.is_empty(), "{:?}", e.client.submissions);

    // ---- projected-exposure cap: a FULL flatten projects |5 - 5| * 100 = 0, so it passes...
    let mut e =
        engine_holding(RiskLimits { max_total_exposure: Some(300.0), ..RiskLimits::new() }, 5.0);
    price_at(&mut e, 100.0);
    assert_eq!(
        verdict_at(&mut e, &flatten_leg("exit-1", 5.0), 1),
        None,
        "a full flatten projects a flat book, so this lane cannot refuse it"
    );
    assert_eq!(e.client.submissions.len(), 1);

    // ...while a PARTIAL covered reduce of the same over-cap position projects |5 - 1| * 100 = 400
    // and is refused, though it can only move the account TOWARD the cap.
    let mut e =
        engine_holding(RiskLimits { max_total_exposure: Some(300.0), ..RiskLimits::new() }, 5.0);
    price_at(&mut e, 100.0);
    let partial = OrderRequest { qty: 1.0, ..flatten_leg("exit-1", 5.0) };
    assert_eq!(
        verdict_at(&mut e, &partial, 1).as_deref(),
        Some("over-max-exposure"),
        "a position already over the cap cannot be reduced in steps — the lane reads the \
         projection, not the direction of travel"
    );
    assert!(e.client.submissions.is_empty(), "{:?}", e.client.submissions);
}

// ---- the ACCOUNT-AGGREGATE exposure ceiling ------------------------------------------------
//
// `RiskLimits::max_account_exposure` is the lane `max_total_exposure`'s NAME promises and its
// SCOPE has never delivered — the pin two sections up is what keeps that distinction honest, and
// it is deliberately UNCHANGED by this suite: the per-symbol lane is still per-symbol, and the
// aggregate is a second, independent axis beside it with its own reason.
//
// Every test here drives the REAL `ExecutionEngine::submit_order`, never `RiskGate::check` with a
// hand-built context. That is load-bearing rather than stylistic: the whole of this axis that could
// be wrong in production is the PRODUCER
// (`ExecutionEngine::resolved_account_exposure_excluding`, folded into `risk_ctx`), and a test that
// supplied `account_exposure_excl_order` by hand would pass against an engine that never folds it.

/// The engine this suite judges: mounted on `("sim", "BTCUSDT")`, also accepting `ETHUSDT`, with
/// BOTH exposure ceilings armed and every other lane off (`RiskLimits::new()` leaves
/// `im_requirement`/`min_*`/`max_notional_per_order`/the throttle disarmed).
///
/// `symbol_cap` is deliberately LOOSER per symbol than `account_cap` is in aggregate, which is the
/// only configuration in which the two lanes are distinguishable at all.
fn account_engine(
    symbol_cap: Option<f64>,
    account_cap: Option<f64>,
) -> ExecutionEngine<RecordingClient> {
    let mut e = engine_with(RiskLimits {
        max_total_exposure: symbol_cap,
        max_account_exposure: account_cap,
        ..RiskLimits::new()
    });
    e.extra_symbols = vec!["ETHUSDT".to_string()];
    e
}

/// Price ONE symbol on BOTH stores, so no resolver arm can move the number under test — the
/// `price_at` idiom above, per symbol rather than on the mounted one.
fn price_sym(e: &mut ExecutionEngine<RecordingClient>, venue: &str, symbol: &str, px: f64) {
    e.account.set_mark_from(venue, symbol, px, MarkSource::VenueMark, 0);
    e.price_board.set_mark(venue, symbol, px, 1);
}

/// Book a position the venue would have opened. `RecordingClient` records a submit and never fills,
/// so a test that wants "the first order is now a position" has to say so.
fn seed_pos(e: &mut ExecutionEngine<RecordingClient>, venue: &str, symbol: &str, size: f64) {
    e.account.positions.insert(
        (venue.into(), symbol.into(), "BOTH".into()),
        PositionEntry { size, avg_px: 100.0, ..Default::default() },
    );
}

/// **A submitted order COMPLETING**: the registry entry goes terminal and the position appears.
///
/// Both halves are required, and the first is the one a test would forget. The account fold counts
/// LIVE orders as well as positions (`ExecutionEngine::resolved_account_exposure_excluding` — the
/// resting-order half is what stops N orders inside one fill window each being judged as though the
/// others committed nothing), so seeding the position alone would leave the same notional counted
/// TWICE — once as a position and once as an order that is still, as far as the registry knows,
/// working at the venue. That is not a state the venue can produce, and a test built on it would be
/// measuring an arithmetic that never happens.
fn book_fill(
    e: &mut ExecutionEngine<RecordingClient>,
    coid: &str,
    venue: &str,
    symbol: &str,
    size: f64,
) {
    let mo = e.registry.get_mut(coid).expect("the order under test was registered");
    mo.status = vike_exec::OrderStatus::Filled;
    mo.filled_qty = size.abs();
    seed_pos(e, venue, symbol, size);
}

/// An opening BUY of `qty` in `symbol` at 100, under its own client-order-id.
fn open_in(coid: &str, symbol: &str, qty: f64) -> OrderRequest {
    OrderRequest { client_order_id: coid.into(), symbol: symbol.into(), ..open_buy(qty, 100.0) }
}

/// **THE HOLE THIS AXIS CLOSES**, end to end through the real engine: two symbols, each order
/// comfortably inside the per-symbol ceiling, together over the account's.
///
/// The per-symbol cap is 60 000 and each order is 50 000 of projected notional, so NEITHER order can
/// ever trip `over-max-exposure` — which is exactly the shape that used to leave an account
/// uncapped: an operator trading N symbols is protected by that number N times over and never once
/// in aggregate.
///
/// ⚠ The first order's ACCEPTANCE is asserted, not assumed. Without that arm this test would pass
/// just as well against an engine that refused everything — including one where the account lane
/// was armed at zero, or where the producer folded the order's OWN symbol in twice — and "the
/// second order is denied" would be evidence of nothing.
#[test]
fn two_symbols_inside_their_own_cap_are_refused_at_the_account_ceiling() {
    let mut e = account_engine(Some(60_000.0), Some(90_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);

    // (1) THE FIRST ORDER IS ADMITTED. 500 @ 100 = 50 000 projected: inside the per-symbol 60 000,
    // and the account holds nothing else, so the aggregate is 50 000 against 90 000.
    assert_eq!(
        verdict_at(&mut e, &open_in("btc-1", "BTCUSDT", 500.0), 1),
        None,
        "the account ceiling must admit the first order — a lane that refuses everything proves \
         nothing about the second"
    );
    assert_eq!(
        e.client.submissions.len(),
        1,
        "…and it reached the venue: {:?}",
        e.client.submissions
    );

    // …and it FILLS: the order goes terminal and the position appears. `RecordingClient` never
    // fills, so both halves are booked here explicitly — see `book_fill` for why the terminal half
    // is not optional (a live order and its own position would be counted twice).
    book_fill(&mut e, "btc-1", "sim", "BTCUSDT", 500.0);

    // (2) THE SECOND ORDER, IN ANOTHER SYMBOL, IS REFUSED BY THE ACCOUNT. Its own projection is the
    // identical 50 000 — still inside the per-symbol ceiling, so `over-max-exposure` cannot be what
    // stopped it — while the account now projects 50 000 + 50 000 = 100 000 against 90 000.
    let reason = verdict_at(&mut e, &open_in("eth-1", "ETHUSDT", 500.0), 2)
        .expect("the account ceiling must refuse the pair");
    assert!(
        reason.starts_with("over-account-exposure"),
        "the ACCOUNT ceiling must refuse under its OWN reason — `over-max-exposure` here would send \
         the operator to re-size the per-symbol number that was not stopping them: {reason}"
    );
    assert_eq!(
        e.client.submissions.len(),
        1,
        "…and nothing new reached the venue: {:?}",
        e.client.submissions
    );

    // (3) THE REFUSAL NAMES THE CEILING AND THE NUMBERS. An account refusal is not
    // self-explanatory — the order that trips it is ordinary and the exposure that trips it is in
    // symbols the operator is not looking at — so both the projected total and the ceiling ride in
    // the reason.
    assert!(
        reason.contains("max_account_exposure"),
        "the refusal must name the KEY the operator has to edit: {reason}"
    );
    assert!(
        reason.contains("90000.00") && reason.contains("100000.00"),
        "the refusal must carry the ceiling AND what the account would have projected: {reason}"
    );
}

/// **THE OFF PATH — absent is byte-identical to before this axis existed.**
///
/// The SAME engine, the SAME book and the SAME order that arm (2) above refuses, with
/// `max_account_exposure` set to `None` and nothing else changed: admitted. So the denial above is
/// attributable to this ceiling and to nothing else in the ladder, and every deployment that writes
/// no `policy.toml` line keeps exactly the gate it had.
#[test]
fn an_absent_account_ceiling_admits_what_an_armed_one_refuses() {
    let mut e = account_engine(Some(60_000.0), None);
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);
    seed_pos(&mut e, "sim", "BTCUSDT", 500.0);

    assert_eq!(
        verdict_at(&mut e, &open_in("eth-1", "ETHUSDT", 500.0), 2),
        None,
        "with the axis absent the account book is not consulted at all"
    );
    assert_eq!(e.client.submissions.len(), 1, "…and the order went to the venue");
}

/// **A LABELLED SECOND ACCOUNT OF THE SAME VENUE IS SUMMED SEPARATELY.**
///
/// This is the property the axis lives or dies on: the aggregate must key on the ACCOUNT, not on
/// the venue STRING. `vike_mount::make_engine_for_account` builds one engine — one `RiskGate`, one
/// copy of the cap, one position book — per `(venue, account)`, addressed by
/// `ExecutionEngine::route_key` (the bare venue id for the default account, `venue#LABEL` for a
/// labelled one). Two accounts are two wallets at the venue and neither backs the other.
///
/// ⚠ **What this test proves, stated exactly, because it used to claim more than it does.** Its
/// comment said "a lane that summed by venue string would refuse BOTH orders", which is false: the
/// fold DOES key on the venue string, *within one engine's own map*, and each engine holds its own
/// book — so this pair proves that two engines carry two budgets and that the ALT engine's `Some`
/// ceiling really is armed while it admits (an unarmed one would admit too). What it cannot see is
/// the layer above it, where the ACCOUNT-to-engine routing lives: a fill routed to the wrong engine
/// would be summed against the wrong budget and every assertion here would still hold.
/// `crates/vike-core/src/runtime/mount_account_tests.rs`'s
/// `each_account_spends_its_own_share_of_the_account_exposure_ceiling` is the half that drives the
/// routing, on one `CoreThread` with both accounts mounted.
///
/// The default account is loaded to its ceiling; the ALT account, holding nothing, must still admit
/// the identical order — and the same order on the LOADED account must still be refused, which is
/// the arm that stops this test passing against an axis that simply never fires.
#[test]
fn a_labelled_second_account_of_one_venue_has_its_own_budget() {
    let mut default_acct = account_engine(None, Some(90_000.0));
    price_sym(&mut default_acct, "sim", "BTCUSDT", 100.0);
    price_sym(&mut default_acct, "sim", "ETHUSDT", 100.0);
    seed_pos(&mut default_acct, "sim", "BTCUSDT", 500.0); // 50 000 of the 90 000 spent

    let mut alt = account_engine(None, Some(90_000.0));
    alt.route_key = "sim#ALT".to_string(); // what `vike_model::account_keys::route_key_of` renders
    price_sym(&mut alt, "sim", "BTCUSDT", 100.0);
    price_sym(&mut alt, "sim", "ETHUSDT", 100.0);

    let order = open_in("eth-1", "ETHUSDT", 500.0); // 50 000 projected, in both engines

    // The LOADED account refuses it — 50 000 already held plus 50 000 projected is 100 000.
    let reason =
        verdict_at(&mut default_acct, &order, 1).expect("the default account is over its ceiling");
    assert!(reason.starts_with("over-account-exposure"), "{reason}");

    // …and the LABELLED one admits the identical order, because it holds a different book — its
    // ceiling is armed at the same number and its own budget is untouched.
    assert_eq!(
        verdict_at(&mut alt, &order, 1),
        None,
        "a second account of one venue must carry its OWN budget: one engine's position may never \
         spend another engine's ceiling"
    );
    assert_eq!(alt.client.submissions.len(), 1);
}

/// **A FOREIGN-VENUE ROW IN THIS ACCOUNT'S MAP CONTRIBUTES NOTHING.**
///
/// A row carrying another venue's id did not come from this account's own fills — it reached the
/// position map through a reconcile fold, the same shape
/// `max_total_exposure_is_scoped_to_one_venue_and_one_symbol`'s third arm describes one lane over.
/// Summing it would be summing positions that do NOT share a wallet, which is precisely the
/// mis-reading this axis exists to avoid: an "account" ceiling that quietly aggregated a foreign
/// venue would refuse orders against collateral that venue never sees.
///
/// A million units at another venue, and the order is admitted exactly as it is on a clean book.
#[test]
fn the_account_sum_excludes_a_foreign_venue_row() {
    let mut e = account_engine(None, Some(90_000.0));
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);
    price_sym(&mut e, "other", "BTCUSDT", 100.0);
    seed_pos(&mut e, "other", "BTCUSDT", 1_000_000.0);

    assert_eq!(
        verdict_at(&mut e, &open_in("eth-1", "ETHUSDT", 500.0), 1),
        None,
        "SCOPE: the account is one venue's one wallet. If this now denies, the fold stopped \
         filtering on the engine's own venue and is aggregating a book this account cannot draw on"
    );
    assert_eq!(e.client.submissions.len(), 1);
}

/// **A COVERED REDUCE BYPASSES THE ACCOUNT CEILING, AND THAT IS THE WHOLE DIFFERENCE FROM THE
/// PER-SYMBOL LANE.**
///
/// An account ABOVE its ceiling is the ordinary state after an operator LOWERS the number or a mark
/// moves, and it is dominated by the symbols the exiting order does not touch — so without this
/// bypass the first flatten leg of a panic exit would be refused by the very ceiling it is obeying.
/// `docs/ops/kill-switches.md` states the law: a ceiling must never trap you in a position.
///
/// The characterization directly above shows the per-symbol lane has no such bypass and CAN refuse a
/// partial reduce. This lane deliberately differs; both arms are asserted here so the divergence is
/// recorded rather than discovered.
#[test]
fn the_account_ceiling_never_refuses_a_covered_reduce() {
    let mut e = account_engine(None, Some(10_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);
    // 100 000 of ETH held elsewhere in the account, against a 10 000 ceiling — massively over.
    seed_pos(&mut e, "sim", "ETHUSDT", 1_000.0);
    seed_pos(&mut e, "sim", "BTCUSDT", 5.0);

    // An OPENING order is refused, which is what makes the next arm mean something.
    let reason = verdict_at(&mut e, &open_in("btc-open", "BTCUSDT", 1.0), 1)
        .expect("an account this far over its ceiling must refuse an opening order");
    assert!(reason.starts_with("over-account-exposure"), "{reason}");

    // …and the flatten of the BTC position — the exact leg `vike_core`'s panic button mints — is
    // admitted, though the account is still far over the ceiling afterwards.
    assert_eq!(
        verdict_at(&mut e, &flatten_leg("exit-1", 5.0), 2),
        None,
        "an account over its ceiling must still be closable — a ceiling that refuses the orders \
         which bring it back under is a trap, not a ceiling"
    );
    assert_eq!(
        e.client.submissions.len(),
        1,
        "the exit reached the venue: {:?}",
        e.client.submissions
    );
}

/// **THE SUM IS GROSS, NEVER NET** — a hedge-mode LONG/SHORT pair in another symbol sums to its two
/// legs' notionals rather than to zero.
///
/// A netting fold would report a fully hedged book as flat and hand an operator an account ceiling
/// that never fires on exactly the position shape that carries the most notional. The two buckets
/// below net to zero and gross to 100 000, and the ceiling is 60 000: netting admits, gross refuses.
#[test]
fn the_account_sum_is_gross_and_does_not_net_a_hedged_pair() {
    let mut e = account_engine(None, Some(60_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);
    e.account.positions.insert(
        ("sim".into(), "ETHUSDT".into(), "LONG".into()),
        PositionEntry { size: 500.0, avg_px: 100.0, ..Default::default() },
    );
    e.account.positions.insert(
        ("sim".into(), "ETHUSDT".into(), "SHORT".into()),
        PositionEntry { size: -500.0, avg_px: 100.0, ..Default::default() },
    );

    let reason = verdict_at(&mut e, &open_in("btc-1", "BTCUSDT", 1.0), 1)
        .expect("100 000 of gross hedged exposure is over a 60 000 ceiling");
    assert!(
        reason.starts_with("over-account-exposure"),
        "a netting fold would read this book as flat and admit: {reason}"
    );
}

/// **RESTING ORDERS COUNT — the half without which the ceiling is bypassable by an arbitrary
/// multiple.**
///
/// No positions at all here: every order is still working at the venue. Under a positions-only fold
/// each of the three would see the same empty account and every one would be admitted, so a maker
/// resting quotes across symbols would blow through the ceiling and only discover it when the fills
/// landed. That is exactly the hole `ExecutionEngine::live_order_margin` was written to close on the
/// buying-power lane — its own doc carries the incident — and this is the exposure twin.
///
/// 40 000 + 40 000 fit under 90 000; the third 40 000 does not. Both admits are asserted, so the
/// test cannot pass against a lane that refuses everything.
#[test]
fn live_un_filled_orders_count_against_the_account_ceiling() {
    let mut e = account_engine(None, Some(90_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);

    assert_eq!(verdict_at(&mut e, &open_in("btc-1", "BTCUSDT", 400.0), 1), None, "first admits");
    assert_eq!(verdict_at(&mut e, &open_in("eth-1", "ETHUSDT", 400.0), 2), None, "second admits");
    assert_eq!(e.client.submissions.len(), 2, "both are working at the venue");

    let reason = verdict_at(&mut e, &open_in("eth-2", "ETHUSDT", 400.0), 3)
        .expect("80 000 of resting orders plus 40 000 more is over a 90 000 ceiling");
    assert!(
        reason.starts_with("over-account-exposure"),
        "orders in flight must be spoken for — a positions-only fold admits this one and every \
         order after it: {reason}"
    );
    assert_eq!(e.client.submissions.len(), 2, "…and the third never reached the venue");
}

/// **A RESTING COVERED REDUCE COMMITS NOTHING**, the same skip the margin twin makes.
///
/// A working exit shrinks the book it is filed against; charging the account for it would refuse
/// the next order because the account is in the middle of getting SMALLER. Same predicate
/// (`vike_model::is_covered_reduce`) as every other bypass in this ladder.
///
/// The account holds 500 BTC (50 000) with a resting flatten of all of it, against a ceiling of
/// 90 000. Counting that exit would make the total 100 000 and refuse; skipping it leaves 50 000
/// and admits the 30 000 order below.
#[test]
fn a_resting_covered_reduce_adds_no_account_exposure() {
    let mut e = account_engine(None, Some(90_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);
    seed_pos(&mut e, "sim", "BTCUSDT", 500.0);

    assert_eq!(
        verdict_at(&mut e, &flatten_leg("exit-1", 500.0), 1),
        None,
        "the exit itself is a covered reduce and bypasses the lane"
    );
    assert_eq!(
        verdict_at(&mut e, &open_in("eth-1", "ETHUSDT", 300.0), 2),
        None,
        "a WORKING exit must not be charged as exposure: 50 000 held + 30 000 new is inside \
         90 000, and only a fold that counted the flatten would refuse this"
    );
    assert_eq!(e.client.submissions.len(), 2);
}

/// **AN AMEND IS NOT CHARGED TWICE.** The order being judged is excluded from the resting-order
/// half by client-order-id, so re-pricing a working order is judged on the same account the
/// original submit was judged on.
///
/// Without the `judging` skip the resting order would be in the sum AND projected by the gate, and
/// a maker re-quoting inside its own ceiling would be refused at a number the identical order was
/// admitted under seconds earlier — the double count `still_executable` records for the per-symbol
/// lane, one lane over.
///
/// 500 @ 100 rests (50 000) against an 80 000 ceiling; the amend to 550 projects 55 000, which is
/// inside it — and would be 105 000 if the resting original were counted as well.
#[test]
fn amending_a_resting_order_is_judged_without_counting_that_order_twice() {
    let mut e = account_engine(None, Some(80_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    let req = open_in("btc-1", "BTCUSDT", 500.0);
    // Drive it to ACCEPTED, the `risk_gate_on_modify.rs` idiom — a SUBMITTED order is not
    // `is_modifiable()`, so without this the modify path is a documented no-op and the assertion
    // below would be green against anything.
    let mut outbox = Outbox::default();
    e.submit_order(&req, 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1, "precondition: the rest itself is admitted");
    let mut bus = vike_exec::EventBus::new();
    bus.publish(
        Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            client_order_id: "btc-1".into(),
            ts: 1,
        }),
        &mut e,
    );
    bus.publish(
        Event::OrderAccepted(vike_model::events::OrderAccepted {
            client_order_id: "btc-1".into(),
            venue_order_id: Some("v1".into()),
            ts: 1,
        }),
        &mut e,
    );

    let mut outbox = Outbox::default();
    e.modify_order("btc-1", Some(550.0), None, 2, &mut outbox);
    let rejected = outbox.0.iter().find_map(|ev| match ev {
        Event::OrderModifyRejected(r) => Some(r.reason.to_string()),
        _ => None,
    });
    assert_eq!(
        rejected, None,
        "an amend must be judged against the account WITHOUT its own resting order in the sum"
    );
    assert_eq!(e.client.modifies.len(), 1, "…and it reached the venue: {:?}", e.client.modifies);

    // …and the lane is genuinely armed on this path, which is what stops the arm above from being
    // vacuous: the same amend to a size the ACCOUNT cannot take is refused, by this ceiling.
    let mut outbox = Outbox::default();
    e.modify_order("btc-1", Some(900.0), None, 3, &mut outbox);
    let rejected = outbox
        .0
        .iter()
        .find_map(|ev| match ev {
            Event::OrderModifyRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .expect("90 000 projected is over the 80 000 ceiling");
    // ⚠ `contains`, not `starts_with`: the MODIFY path wraps the gate's verdict
    // (`ExecutionEngine::modify_order` publishes `format!("risk: {reason}")`), so the token is not
    // first here as it is on the submit path. Asserted on the token itself either way — what must
    // hold is that the operator is told WHICH ceiling refused the amend.
    assert!(rejected.contains("over-account-exposure"), "{rejected}");
    assert_eq!(e.client.modifies.len(), 1, "…and nothing new reached the venue");
}

/// **THE ARMING FOLD NARROWS AND NEVER WIDENS** — `RiskLimits::narrow_account_exposure`, the one
/// operation `vike_mount::make_engine_for_account` and its paper twin arm this ceiling through.
///
/// The field's whole claim is "it can only ever REFUSE", and a plain assignment makes that a
/// property of nobody else writing the field rather than of the operation. This pins the operation:
/// `min` when both sides carry a number — in BOTH argument orders, since a fold that took the
/// incoming value whenever it existed would pass a one-directional test — and never `None` over an
/// existing `Some`. `vike_config::VenueMode::cap` is the precedent, and it is a `min` for exactly
/// this reason.
#[test]
fn the_account_ceiling_fold_narrows_and_never_widens() {
    let narrow = |held: Option<f64>, incoming: Option<f64>| {
        let mut lim = RiskLimits { max_account_exposure: held, ..RiskLimits::new() };
        lim.narrow_account_exposure(incoming);
        lim.max_account_exposure
    };
    assert_eq!(narrow(None, None), None, "no ceiling anywhere ⇒ the axis stays off");
    assert_eq!(
        narrow(None, Some(100.0)),
        Some(100.0),
        "the operator's file arms an unarmed engine"
    );
    assert_eq!(
        narrow(Some(100.0), None),
        Some(100.0),
        "a policy that says nothing may not DISARM a ceiling something else already set"
    );
    assert_eq!(narrow(Some(100.0), Some(250.0)), Some(100.0), "the looser incoming value loses");
    assert_eq!(narrow(Some(250.0), Some(100.0)), Some(100.0), "…and the tighter one wins");
}
