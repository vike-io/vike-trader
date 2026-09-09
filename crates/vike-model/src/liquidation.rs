//! THE one scope-parameterized liquidation law — the collapse of the four incompatible
//! models (live watchdog, backtest wipe, GUI closed-form, GUI `im*0.5` hardcode) into a
//! single rule where the POOL is the parameter:
//!
//! > `pool_equity ≤ maint_rate × pool_notional` (plus the LEAN over-the-line buffer arm)
//! > → liquidate the pool.
//!
//! The pool partition is keyed by [`MarginMode`] (see [`partition_pools`]):
//! - **Cash** — fully funded, no borrow: structurally CANNOT breach maintenance → never
//!   liquidates. Cash positions are dropped from every pool (their value remains ordinary
//!   account equity for the caller's cross-pool seed).
//! - **Cross** — ONE shared pool over the whole account's Cross positions. A breach runs the
//!   LEAN `DefaultMarginCallModel` workflow ([`cross_liquidation_plan`]): biggest losers
//!   first, liquidate only the excess over the line, stop as soon as healthy.
//! - **Isolated** — one pool PER position: its walled-off margin wallet + its own uPnL. A
//!   breach closes THAT position only; the loss is capped at its isolated margin (the
//!   Nautilus-style close-out degenerated to a one-position pool).
//!
//! Consumers (all read the SAME rate source — the operator's configured maintenance rate;
//! a per-symbol venue-reported rate is a documented follow-up, see the precedence note on
//! [`pool_breached`]):
//! - `vike_exec::check_margin_call` — the live watchdog: cross arm + per-isolated arms.
//! - `vike_backtest` `StrategyEngine::check_liquidation` — the DEFAULT backtest path (the
//!   old whole-account adverse-intrabar wipe is the opt-in `venue_style_liquidation` knob).
//! - `vike_core::snapshot` — the GUI badge: [`crate::liquidation_price`] for Isolated at the
//!   real rate; [`cross_liquidation_price_est`] for Cross (the `im*0.5` hardcode is dead).
//!
//! PURE: no I/O, no account types — callers fold their own pool equity/maintenance with
//! their own authority (`Account::margin_in_use_by` live; the per-row adverse fold in the
//! backtest) so existing bit-parity pins are preserved, and this module owns only the LAW.

use crate::venue_margin_support::MarginMode;

/// THE breach predicate — one law for every pool.
///
/// `pool_maintenance` is the pool's maintenance requirement (`maint_rate × pool_notional`,
/// folded by the CALLER's authority so its bit-pattern matches that caller's existing fold —
/// this fn deliberately takes the folded product, not `(rate, notional)`, because
/// `Σ(|q|·mark·mult·rate) ≠ rate·Σ(|q|·mark·mult)` in f64 and the live watchdog's verdict
/// must stay byte-identical to its pre-law behavior).
///
/// Breached iff ALL of:
/// 1. `pool_maintenance > 0` — an unpriceable/empty pool can never breach (LEAN skip);
/// 2. `pool_equity − pool_maintenance ≤ 0` — the pool no longer covers maintenance
///    (the `pool_equity ≤ maint_rate × pool_notional` law);
/// 3. `pool_maintenance > pool_equity · (1 + buffer)` — the LEAN
///    `DefaultMarginCallModel` over-the-line buffer (default 0.10 live; pass `0.0` for a
///    venue-style pool with no grace buffer — the Isolated arm).
///
/// NaN anywhere → `false` (comparisons with NaN are false): the law REFUSES to liquidate on
/// garbage inputs, matching the watchdog's pre-existing behavior.
///
/// RATE PRECEDENCE (the one maintenance-rate source): the operator-configured rate
/// (`MarginCallConfig::mm_requirement` live, `EngineParams::maint_margin` in a backtest,
/// threaded into the GUI badge). A per-symbol venue-reported maintenance rate would take
/// precedence when present, but NO venue adapter parses one today (verified: no bridge
/// carries a maintenance field on its instrument fetch), so the `SymbolProperties` carrier
/// is a documented follow-up rather than a guessed wire field.
pub fn pool_breached(pool_equity: f64, pool_maintenance: f64, buffer: f64) -> bool {
    if pool_maintenance <= 0.0 {
        return false;
    }
    pool_equity - pool_maintenance <= 0.0 && pool_maintenance > pool_equity * (1.0 + buffer)
}

/// One cross-pool liquidation candidate (an open, priceable Cross position), in the
/// caller's position-ledger insertion order. `id` is caller-meaningful (an index into the
/// caller's parallel key list); `upnl` orders the plan (losers first); `mark`/`mult` price
/// one unit's maintenance.
#[derive(Debug, Clone, PartialEq)]
pub struct LiqCandidate {
    pub id: usize,
    /// signed position size
    pub size: f64,
    /// the mark the pool was judged at (live: the account mark; backtest: the adverse print)
    pub mark: f64,
    pub mult: f64,
    /// unrealized PnL at that mark — the losers-first sort key
    pub upnl: f64,
}

/// The LEAN `DefaultMarginCallModel` reduction workflow over a BREACHED cross pool:
/// sort ascending by `upnl` (biggest losers first; the sort is STABLE, so candidates tied on
/// `upnl` keep the caller's insertion order), then liquidate only the `excess`
/// (`pool_maintenance − pool_equity`) position by position — each frees
/// `qty · mark · mult · maint_rate` — and STOP as soon as the pool is healthy.
///
/// Returns `(id, qty)` close intents in liquidation order. Candidates whose per-unit
/// maintenance is non-positive OR non-finite (unpriceable) are skipped, exactly as the
/// pre-law watchdog skipped the non-positive ones; the non-finite arm is a pub-API hardening
/// (a NaN `per_unit` would poison `excess` and cascade full-size closes onto every remaining
/// candidate — unreachable from both in-tree callers, whose marks are finite, but this fn is
/// `pub`). This is the EXACT loop hoisted from `vike_exec::check_margin_call` — the
/// arithmetic (`mark · mult · rate` association, `min(excess/per_unit, |size|)`, the
/// `excess -= qty·per_unit` walk) is load-bearing for the cross-only byte-identity pin;
/// do not reorder.
pub fn cross_liquidation_plan(
    mut candidates: Vec<LiqCandidate>,
    excess: f64,
    maint_rate: f64,
) -> Vec<(usize, f64)> {
    candidates.sort_by(|a, b| a.upnl.partial_cmp(&b.upnl).unwrap_or(std::cmp::Ordering::Equal));
    let mut excess = excess;
    let mut plan = Vec::new();
    for c in candidates {
        if excess <= 0.0 {
            break;
        }
        let per_unit = c.mark * c.mult * maint_rate;
        if per_unit <= 0.0 || !per_unit.is_finite() {
            continue; // unpriceable row (≤0 = the pre-law skip; NaN/inf = pub-API hardening)
        }
        let qty = (excess / per_unit).min(c.size.abs());
        if qty <= 0.0 {
            continue;
        }
        plan.push((c.id, qty));
        excess -= qty * per_unit;
    }
    plan
}

/// The pool partition: position indices split by [`MarginMode`], preserving the caller's
/// iteration order. `cross` share ONE pool; each entry of `isolated` is its OWN pool; Cash
/// positions appear in NEITHER — they never liquidate (their value stays ordinary account
/// equity, which the caller keeps in the cross-pool equity seed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolPartition {
    pub cross: Vec<usize>,
    pub isolated: Vec<usize>,
}

/// Split a position ledger (by iteration order) into liquidation pools by margin mode.
pub fn partition_pools(modes: impl IntoIterator<Item = MarginMode>) -> PoolPartition {
    let mut out = PoolPartition::default();
    for (i, mode) in modes.into_iter().enumerate() {
        match mode {
            MarginMode::Cross => out.cross.push(i),
            MarginMode::Isolated => out.isolated.push(i),
            MarginMode::Cash => {} // never liquidates — no pool
        }
    }
    out
}

/// GUI-badge estimate: the mark `P` of ONE cross position at which the WHOLE cross pool
/// first satisfies the law's condition-2 (`pool_equity(P) ≤ maint_rate · pool_notional(P)`),
/// holding every OTHER position frozen at its current mark. Solving the linear equation
/// `pool_equity + (P − own_mark)·q·M = maint_rate · (others_notional + |q|·M·P)` for `P`:
///
/// `P = (maint_rate·others_notional − pool_equity + q·M·own_mark) / (M·(q − maint_rate·|q|))`
///
/// A per-position cross liquidation price is inherently ILL-DEFINED (the pool is shared:
/// every other position's drift moves it), so this is an advisory estimate — the shape
/// venue UIs (e.g. Binance cross) display — not a trigger anything acts on; the watchdog
/// acts on the account-level law only. The LEAN buffer arm is deliberately ignored here
/// (the badge marks where condition-2 first binds, the conservative earlier line).
///
/// Returns `0.0` ("no liquidation by price") for a flat/degenerate input or a non-finite /
/// non-positive solution (e.g. a pool so healthy the long-side solution goes negative).
pub fn cross_liquidation_price_est(
    pool_equity: f64,
    own_size: f64,
    own_mark: f64,
    own_mult: f64,
    others_notional: f64,
    maint_rate: f64,
) -> f64 {
    if own_size == 0.0 || own_mark <= 0.0 || own_mult <= 0.0 || maint_rate <= 0.0 {
        return 0.0;
    }
    let denom = own_mult * (own_size - maint_rate * own_size.abs());
    if denom == 0.0 {
        return 0.0;
    }
    let p = (maint_rate * others_notional - pool_equity + own_size * own_mult * own_mark) / denom;
    if p.is_finite() && p > 0.0 { p } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- pool_breached: the ONE law -------------------------------------------------------

    #[test]
    fn breach_needs_all_three_conditions() {
        // equity 40, maint 50, buffer 0.10: 40-50 ≤ 0 AND 50 > 44 → breached
        assert!(pool_breached(40.0, 50.0, 0.10));
        // equity 48: remaining -2 ≤ 0 BUT 50 ≤ 52.8 → the buffer arm holds it back
        assert!(!pool_breached(48.0, 50.0, 0.10));
        // equity 100, maint 50: healthy
        assert!(!pool_breached(100.0, 50.0, 0.10));
    }

    #[test]
    fn empty_or_unpriceable_pool_never_breaches() {
        assert!(!pool_breached(-100.0, 0.0, 0.10)); // no maintenance → no pool to liquidate
        assert!(!pool_breached(0.0, -5.0, 0.10)); // negative maintenance = garbage → refuse
    }

    #[test]
    fn zero_buffer_is_the_isolated_arm() {
        // equity == maint exactly: condition-2 holds (0 ≤ 0) but maint > equity is FALSE →
        // the boundary itself does not breach (strictly-past-the-line, both arms agree)
        assert!(!pool_breached(50.0, 50.0, 0.0));
        assert!(pool_breached(49.999, 50.0, 0.0));
        // deep underwater isolated pool (equity ≤ 0) always breaches a positive maintenance
        assert!(pool_breached(-1.0, 0.001, 0.0));
    }

    #[test]
    fn nan_refuses_to_liquidate() {
        assert!(!pool_breached(f64::NAN, 50.0, 0.10));
        assert!(!pool_breached(40.0, f64::NAN, 0.10));
        assert!(!pool_breached(40.0, 50.0, f64::NAN)); // NaN buffer poisons arm 3 → false
    }

    // --- cross_liquidation_plan: LEAN losers-first excess-only ----------------------------

    fn cand(id: usize, size: f64, mark: f64, upnl: f64) -> LiqCandidate {
        LiqCandidate { id, size, mark, mult: 1.0, upnl }
    }

    #[test]
    fn losers_first_excess_only_stops_when_healthy() {
        // rate 0.05, marks 100 → per_unit 5. excess 80: loser (id 1) closes all 10 (frees 50),
        // then id 0 closes 6 (frees 30) and the plan STOPS.
        let plan = cross_liquidation_plan(
            vec![cand(0, 10.0, 100.0, 0.0), cand(1, 10.0, 100.0, -200.0)],
            80.0,
            0.05,
        );
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].0, 1);
        assert!((plan[0].1 - 10.0).abs() < 1e-12);
        assert_eq!(plan[1].0, 0);
        assert!((plan[1].1 - 6.0).abs() < 1e-12);
    }

    #[test]
    fn partial_close_never_exceeds_position() {
        let plan = cross_liquidation_plan(vec![cand(0, 2.0, 100.0, 0.0)], 1_000.0, 0.05);
        assert_eq!(plan, vec![(0, 2.0)]); // capped at |size| even when the excess is larger
    }

    #[test]
    fn unpriceable_candidate_is_skipped() {
        let plan = cross_liquidation_plan(
            vec![cand(0, 10.0, 0.0, -5.0), cand(1, 10.0, 100.0, 0.0)],
            10.0,
            0.05,
        );
        assert_eq!(plan, vec![(1, 2.0)]); // id 0 (mark 0 → per_unit 0) contributes nothing
    }

    #[test]
    fn non_finite_per_unit_is_skipped_not_cascaded() {
        // A NaN mark makes per_unit NaN; without the finite guard qty = (excess/NaN).min(|s|)
        // = NaN.min(10.0) = 10.0 (Rust's f64::min returns the non-NaN operand) → the row is
        // pushed FULL SIZE, then `excess -= 10·NaN` poisons `excess` to NaN, `excess <= 0.0`
        // never fires again, and every remaining candidate also closes full size. The guard
        // drops the row instead: only the priceable candidate closes, for exactly the excess.
        let plan = cross_liquidation_plan(
            vec![cand(0, 10.0, f64::NAN, -5.0), cand(1, 10.0, 100.0, 0.0)],
            10.0,
            0.05,
        );
        assert_eq!(plan, vec![(1, 2.0)]);
        // +inf mark: per_unit inf → same skip (qty would be excess/inf = 0 anyway, but the
        // guard keeps the contract uniform: non-finite = unpriceable, never in the plan).
        let plan = cross_liquidation_plan(
            vec![cand(0, 10.0, f64::INFINITY, -5.0), cand(1, 10.0, 100.0, 0.0)],
            10.0,
            0.05,
        );
        assert_eq!(plan, vec![(1, 2.0)]);
    }

    #[test]
    fn no_excess_no_plan() {
        assert!(cross_liquidation_plan(vec![cand(0, 10.0, 100.0, 0.0)], 0.0, 0.05).is_empty());
        assert!(cross_liquidation_plan(vec![cand(0, 10.0, 100.0, 0.0)], -3.0, 0.05).is_empty());
    }

    #[test]
    fn tie_on_upnl_keeps_insertion_order() {
        // stable sort: equal upnl → ids stay 0 then 1
        let plan = cross_liquidation_plan(
            vec![cand(0, 1.0, 100.0, -1.0), cand(1, 1.0, 100.0, -1.0)],
            9.0,
            0.05,
        );
        assert_eq!(plan[0].0, 0);
        assert_eq!(plan[1].0, 1);
    }

    #[test]
    fn nan_upnl_sorts_stably_not_panicking() {
        let plan = cross_liquidation_plan(
            vec![cand(0, 1.0, 100.0, f64::NAN), cand(1, 1.0, 100.0, -1.0)],
            100.0,
            0.05,
        );
        assert_eq!(plan.len(), 2); // both closed; partial_cmp fallback keeps it total
    }

    // --- partition_pools ------------------------------------------------------------------

    #[test]
    fn partition_routes_by_mode_and_drops_cash() {
        let p = partition_pools([
            MarginMode::Cross,
            MarginMode::Isolated,
            MarginMode::Cash,
            MarginMode::Cross,
            MarginMode::Isolated,
        ]);
        assert_eq!(p.cross, vec![0, 3]);
        assert_eq!(p.isolated, vec![1, 4]);
    }

    #[test]
    fn default_all_cross_partition_is_todays_pool() {
        // the compat pin: every position defaults Cross → the partition IS today's account pool
        let p = partition_pools(vec![MarginMode::default(); 3]);
        assert_eq!(p.cross, vec![0, 1, 2]);
        assert!(p.isolated.is_empty());
    }

    // --- cross_liquidation_price_est ------------------------------------------------------

    #[test]
    fn cross_est_single_long_below_mark() {
        // long 1 @ mark 100, equity 60, rate 0.05, no others:
        // P = (0 - 60 + 100) / (1·(1 - 0.05)) = 40/0.95 ≈ 42.105 — below the mark
        let p = cross_liquidation_price_est(60.0, 1.0, 100.0, 1.0, 0.0, 0.05);
        assert!((p - 40.0 / 0.95).abs() < 1e-9);
        assert!(p < 100.0);
        // sanity: at P the law's condition-2 binds: equity(P) == rate·notional(P)
        let eq_at = 60.0 + (p - 100.0) * 1.0;
        assert!((eq_at - 0.05 * p).abs() < 1e-9);
    }

    #[test]
    fn cross_est_single_short_above_mark() {
        // short 1 @ mark 100, equity 60: P = (−60 − 100)/(1·(−1 − 0.05)) = 160/1.05 ≈ 152.4
        let p = cross_liquidation_price_est(60.0, -1.0, 100.0, 1.0, 0.0, 0.05);
        assert!(p > 100.0);
        let eq_at = 60.0 + (100.0 - p) * 1.0;
        assert!((eq_at - 0.05 * p).abs() < 1e-9);
    }

    #[test]
    fn cross_est_other_positions_pull_the_line_closer() {
        let alone = cross_liquidation_price_est(60.0, 1.0, 100.0, 1.0, 0.0, 0.05);
        let crowded = cross_liquidation_price_est(60.0, 1.0, 100.0, 1.0, 500.0, 0.05);
        assert!(crowded > alone); // others' maintenance consumes shared equity → earlier liq
    }

    #[test]
    fn cross_est_healthy_long_clamps_to_zero() {
        // equity so large the solution is negative → 0.0 = "no liquidation by price"
        assert_eq!(cross_liquidation_price_est(1_000.0, 1.0, 100.0, 1.0, 0.0, 0.05), 0.0);
    }

    #[test]
    fn cross_est_degenerate_inputs_zero() {
        assert_eq!(cross_liquidation_price_est(60.0, 0.0, 100.0, 1.0, 0.0, 0.05), 0.0);
        assert_eq!(cross_liquidation_price_est(60.0, 1.0, 0.0, 1.0, 0.0, 0.05), 0.0);
        assert_eq!(cross_liquidation_price_est(60.0, 1.0, 100.0, 0.0, 0.0, 0.05), 0.0);
        assert_eq!(cross_liquidation_price_est(60.0, 1.0, 100.0, 1.0, 0.0, 0.0), 0.0);
        assert_eq!(cross_liquidation_price_est(f64::NAN, 1.0, 100.0, 1.0, 0.0, 0.05), 0.0);
    }
}
