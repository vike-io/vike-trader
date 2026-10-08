//! Hedge-mode coverage, and the lanes with NO covered-reduce bypass (throttle, notional, exposure).

use super::risk_lane_common::*;
use vike_exec::MarkSource;
use vike_exec::testing::RecordingClient;
use vike_exec::{ExecutionEngine, Outbox, PositionEntry, RiskLimits};
use vike_model::OrderRequest;

// --- hedge-mode-aware coverage ---

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

/// Hedge-mode anti-stranding: a below-min reduce-only flatten of a hedge LONG bucket passes the
/// floor bypass. A ctx reading only the `BOTH` bucket (0) would make `is_covered_reduce` see a flat
/// book and DENY the close, stranding exactly the dust the bypass protects.
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

/// One-way accounts: the `BOTH` bucket is non-zero, so the hedge fallback is never consulted; the
/// covered close bypasses and the flat book still floor-gates.
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

// --- the lanes that carry NO covered-reduce bypass ---
//
// CHARACTERIZATION: these tests REVEAL the current verdict and argue for no cure.
// `RiskGate::check_inner` computes `covered_reduce` ONCE for the anti-stranding bypasses (min-qty
// and min-notional floors, price collar, buying-power charge, impact veto; `Halted` uses the same
// `vike_model::is_covered_reduce`). THREE lanes never read it: `RiskGate::admit_throttle`
// (`"rate-limited"`), `RiskLimits::max_notional_per_order` (`"over-max-notional"`) and
// `RiskLimits::max_total_exposure` (`"over-max-exposure"`). The last two are what
// `vike_mount::require_live_risk_budget` demands before a LIVE mount, so on a live venue they are
// ARMED BY CONSTRUCTION and the MORE reachable denials. (`vike_core::runtime::apply`'s
// `test_core`/`test_core_with` gate on `RiskLimits::new()`, so every `OrderIntent::MarketExit`
// test there runs with the throttle DISARMED.)

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
/// test (`risk_lane_pricing.rs`'s `reversal_engine` idiom).
fn price_at(e: &mut ExecutionEngine<RecordingClient>, px: f64) {
    e.account.set_mark_from("sim", "BTCUSDT", px, MarkSource::VenueMark, 0);
    e.price_board.set_mark("sim", "BTCUSDT", px, 1);
}

/// An opening BUY under `coid` — `open_buy`'s terms with a fresh id, so the throttle-filling
/// submits do not collapse onto one registry entry.
fn opening(coid: &str) -> OrderRequest {
    OrderRequest { client_order_id: coid.into(), ..open_buy(1.0, 100.0) }
}

/// THE THROTTLE LANE, CHARACTERIZED: a POSITION-COVERED reduce is metered by the same session-wide
/// sliding window every opening order spends, and an exhausted window refuses it
/// (`RiskGate::check_inner`'s last lane, `if consume_throttle && !self.admit_throttle(ctx.now_ms)`,
/// has no `covered_reduce` term). The window is spent by OPENING submits at ONE `now_ms` (the
/// runtime's shape: `CoreThread::dispatch` reads the clock once per message and
/// `CoreThread::apply_intent` stamps every leg of one compound verb with it), then the panic
/// button's flatten leg crosses it.
///
/// ⚠ It PINS the verdict. If a covered-reduce bypass is ever added to `admit_throttle`'s guard this
/// goes red: invert the assertion in the same PR, never delete it.
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

/// THE TWO MANDATORY LIVE CAPS, CHARACTERIZED: `vike_mount::require_live_risk_budget` refuses a
/// live mount unless `max_notional_per_order` and `max_total_exposure` are set
/// (`MountError::MissingRiskBudget`), so on a live venue they are armed by construction, and
/// neither reads `covered_reduce`. They differ in REACHABILITY:
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
