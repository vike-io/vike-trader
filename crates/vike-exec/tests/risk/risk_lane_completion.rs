//! Risk-lane completion (wave-2, the #518 leftovers): the pre-trade gate's equity AND
//! margin-in-use are resolver-priced (`resolved_equity` / `resolved_margin_in_use_by` — the
//! one-price law reaching `gate_and_register`), and the gate's coverage basis is
//! hedge-mode-aware (`LONG`/`SHORT` buckets no longer read as a flat book, so the #458
//! anti-stranding floor bypass works for hedge accounts). Mirrors the `resolve_equity.rs`
//! integration-test convention (tests/ file, `RecordingClient`, `ExecutionEngine::new`).

use vike_exec::testing::RecordingClient;
use vike_exec::MarkSource;
use vike_exec::{
    Account, BalanceMode, ExecutionEngine, Outbox, PositionEntry, RiskGate, RiskLimits,
};
use vike_model::events::Event;
use vike_model::OrderRequest;

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
