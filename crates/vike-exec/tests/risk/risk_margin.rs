//! Phase B gate: the opt-in pre-trade buying-power check (LEAN BuyingPowerModel semantics,
//! RUST-NATIVE — `exec/risk.py` has no margin checks). Also proves the knob OFF path is
//! byte-identical to today's gate (the r5 risk fixtures pin that separately).

use vike_exec::{RiskContext, RiskGate, RiskLimits, TradingState};
use vike_model::OrderRequest;

fn market(side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "t1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        order_type: "market".to_string(),
        side,
        qty,
        ..Default::default()
    }
}

fn ctx(pos: f64, mark: f64, equity: f64, margin_used: f64, closing_credit: f64) -> RiskContext {
    RiskContext {
        position_size: pos,
        mark_price: mark,
        trading_state: TradingState::Active,
        now_ms: 0,
        equity,
        margin_used,
        closing_credit,
        multiplier: 1.0,
    }
}

fn gate(im: Option<f64>) -> RiskGate {
    RiskGate::new(RiskLimits { im_requirement: im, ..RiskLimits::new() })
}

#[test]
fn knob_off_ignores_margin_fields() {
    // zero equity, huge margin used — order still passes with the knob off
    let v = gate(None).check(&market(1, 100.0), &ctx(0.0, 50_000.0, 0.0, 1e12, 0.0));
    assert!(v.ok);
}

#[test]
fn open_denied_when_margin_insufficient() {
    // order IM = 1 * 100 * 0.1 = 10; free = equity 5 - used 0 = 5 -> deny
    let v = gate(Some(0.1)).check(&market(1, 1.0), &ctx(0.0, 100.0, 5.0, 0.0, 0.0));
    assert!(!v.ok);
    assert_eq!(v.reason, "insufficient-margin");
}

#[test]
fn open_allowed_when_margin_sufficient() {
    // order IM 10; free = 15 - 0 -> ok
    let v = gate(Some(0.1)).check(&market(1, 1.0), &ctx(0.0, 100.0, 15.0, 0.0, 0.0));
    assert!(v.ok);
}

#[test]
fn existing_margin_use_counts() {
    // order IM 10; free = equity 15 - used 8 = 7 -> deny
    let v = gate(Some(0.1)).check(&market(1, 1.0), &ctx(0.0, 100.0, 15.0, 8.0, 0.0));
    assert!(!v.ok);
    assert_eq!(v.reason, "insufficient-margin");
}

#[test]
fn pure_reduce_bypasses_even_with_zero_equity() {
    // long 5, sell 3 (LEAN: |holdings| >= |order| on the opposite side never blocks)
    let v = gate(Some(0.1)).check(&market(-1, 3.0), &ctx(5.0, 100.0, 0.0, 1e9, 0.0));
    assert!(v.ok);
}

#[test]
fn flip_uses_closing_credit() {
    // long 5, sell 8: not a pure reduce. order IM = 8*100*0.1 = 80.
    // Without credit: free = equity 30 - used 50 = 0 -> denied.
    let no_credit = gate(Some(0.1)).check(&market(-1, 8.0), &ctx(5.0, 100.0, 30.0, 50.0, 0.0));
    assert!(!no_credit.ok);
    // With the LEAN reversal credit (2 * 5*100*0.1 = 100): free = 30 - 50 + 100 = 80 -> ok
    let credited = gate(Some(0.1)).check(&market(-1, 8.0), &ctx(5.0, 100.0, 30.0, 50.0, 100.0));
    assert!(credited.ok);
}

#[test]
fn free_bp_haircut_applies() {
    let mut lim = RiskLimits::new();
    lim.im_requirement = Some(0.1);
    lim.required_free_bp_pct = 0.5; // half the equity must stay free
    let mut g = RiskGate::new(lim);
    // order IM 10; equity 15 -> free = 15 - 0 - 7.5 = 7.5 -> deny
    let v = g.check(&market(1, 1.0), &ctx(0.0, 100.0, 15.0, 0.0, 0.0));
    assert!(!v.ok);
    assert_eq!(v.reason, "insufficient-margin");
}

#[test]
fn denied_margin_order_consumes_no_rate_slot() {
    let mut lim = RiskLimits::new();
    lim.im_requirement = Some(0.1);
    lim.max_orders_per_window = Some(1);
    let mut g = RiskGate::new(lim);
    // first order denied on margin — must NOT consume the single rate slot
    let denied = g.check(&market(1, 1.0), &ctx(0.0, 100.0, 5.0, 0.0, 0.0));
    assert!(!denied.ok);
    // second order with margin ok — the slot must still be free
    let ok = g.check(&market(1, 1.0), &ctx(0.0, 100.0, 100.0, 0.0, 0.0));
    assert!(ok.ok);
}

#[test]
fn im_for_prefers_per_symbol_then_default() {
    let mut lim = RiskLimits::new();
    lim.im_requirement = Some(0.5); // venue default 2x
    lim.im_by_symbol.insert("BTCUSDT".to_string(), 0.1); // BTC override 10x
    assert_eq!(lim.im_for("BTCUSDT"), Some(0.1));
    assert_eq!(lim.im_for("ETHUSDT"), Some(0.5)); // falls back to default
    assert_eq!(RiskLimits::new().im_for("BTCUSDT"), None); // gate off
}

#[test]
fn per_symbol_leverage_admits_larger_order() {
    // default 1x (im 1.0) would deny (order IM 100 > equity 15); BTCUSDT overridden to 10x passes
    let mut lim = RiskLimits::new();
    lim.im_requirement = Some(1.0);
    lim.im_by_symbol.insert("BTCUSDT".to_string(), 0.1);
    let v = RiskGate::new(lim).check(&market(1, 1.0), &ctx(0.0, 100.0, 15.0, 0.0, 0.0));
    assert!(v.ok); // 10x -> IM 10 <= 15
}

// ---------------------------------------------------------------------------
// The margin bypass requires POSITION COVERAGE, not a bare `reduce_only` flag.
//
// The bypass used to read the LOOSE `reduce_only || is_implicit_reduce`. Since
// `is_implicit_reduce` needs `side * position < 0`, it is FALSE on a flat book — so the
// caller-asserted FLAG alone was admitting the bypass, and a mis-tagged entry order was
// admitted at any size against any equity with buying power never consulted. #458 closed
// this same class on the min_qty/min_notional floors; this closes the margin lane.
// ---------------------------------------------------------------------------

fn reduce_only(side: i32, qty: f64) -> OrderRequest {
    OrderRequest { reduce_only: true, ..market(side, qty) }
}

#[test]
fn flat_book_reduce_only_flag_now_faces_margin() {
    // FLAT book + `reduce_only` = an OPENING order however it is tagged. There is no position
    // for the venue to reduce server-side either, so this gate is the only thing standing.
    // order IM = 1 * 100 * 0.1 = 10; free = equity 5 -> deny (previously: ADMITTED, unchecked).
    let v = gate(Some(0.1)).check(&reduce_only(1, 1.0), &ctx(0.0, 100.0, 5.0, 0.0, 0.0));
    assert!(!v.ok, "flat-book reduce_only must not skip buying power");
    assert_eq!(v.reason, "insufficient-margin");
}

#[test]
fn flat_book_reduce_only_flag_passes_with_equity() {
    // Same order, sufficient equity: the fix denies only for want of margin, it does not
    // blanket-refuse a flagged order. free = 15 >= IM 10 -> ok.
    let v = gate(Some(0.1)).check(&reduce_only(1, 1.0), &ctx(0.0, 100.0, 15.0, 0.0, 0.0));
    assert!(v.ok);
}

#[test]
fn covered_reduce_still_bypasses_margin() {
    // THE REGRESSION PIN. Anti-stranding: a covered close frees margin rather than consuming
    // it, so it must reach the venue even with zero equity and the account fully committed.
    // long 5, sell 3 WITH the flag, and the exact-flatten (sell 5) boundary of `>=` coverage.
    let partial = gate(Some(0.1)).check(&reduce_only(-1, 3.0), &ctx(5.0, 100.0, 0.0, 1e9, 0.0));
    assert!(partial.ok, "covered partial reduce must still bypass margin");
    let exact = gate(Some(0.1)).check(&reduce_only(-1, 5.0), &ctx(5.0, 100.0, 0.0, 1e9, 0.0));
    assert!(exact.ok, "exact flatten is covered (|pos| >= |qty|) and must bypass");
    // short side mirrors it
    let short = gate(Some(0.1)).check(&reduce_only(1, 5.0), &ctx(-5.0, 100.0, 0.0, 1e9, 0.0));
    assert!(short.ok, "covered reduce of a short must still bypass");
}

#[test]
fn implicit_reduce_without_the_flag_is_unaffected() {
    // Opposite side + covered, flag OFF: `is_covered_reduce` admits it via the implicit arm,
    // exactly as `pure_reduce` did. Byte-identical to pre-fix.
    let v = gate(Some(0.1)).check(&market(-1, 3.0), &ctx(5.0, 100.0, 0.0, 1e9, 0.0));
    assert!(v.ok);
}

#[test]
fn uncovered_reversal_with_the_flag_faces_margin() {
    // long 5, sell 8 WITH the flag: |qty| > |position|, so it is NOT covered — it opens 3 on
    // the far side. The flag must not buy it a pass. order IM = 8*100*0.1 = 80;
    // free = 30 - 50 = 0 -> deny. (The same order without the flag already denied here, so
    // this proves the flag no longer changes the verdict.)
    let flagged = gate(Some(0.1)).check(&reduce_only(-1, 8.0), &ctx(5.0, 100.0, 30.0, 50.0, 0.0));
    assert!(!flagged.ok, "uncovered reversal must face margin even when tagged reduce_only");
    assert_eq!(flagged.reason, "insufficient-margin");
    // and with the LEAN reversal credit it passes, same as the unflagged twin
    let credited =
        gate(Some(0.1)).check(&reduce_only(-1, 8.0), &ctx(5.0, 100.0, 30.0, 50.0, 100.0));
    assert!(credited.ok);
}

#[test]
fn margin_knob_off_is_unaffected_by_the_flag() {
    // The whole lane is opt-in: with `im_requirement` None no buying-power check runs at all,
    // so a flat-book flagged order is admitted exactly as before the fix.
    let v = gate(None).check(&reduce_only(1, 100.0), &ctx(0.0, 50_000.0, 0.0, 1e12, 0.0));
    assert!(v.ok);
}
