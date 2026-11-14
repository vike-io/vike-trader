//! THE one scope-parameterized liquidation law — the collapse of the four incompatible
//! models (live watchdog, backtest wipe, GUI closed-form, GUI `initial_margin*0.5` hardcode) into a
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
//!   cross close-out degenerated to a one-position pool).
//!
//! Consumers (all read the SAME rate source — the operator's configured maintenance rate;
//! a per-symbol venue-reported rate is a documented follow-up, see the precedence note on
//! [`pool_breached`]):
//! - `vike_execution::check_margin_call_priced` — the live watchdog: cross arm + per-isolated arms.
//! - `vike_backtest` `StrategyEngine::check_liquidation` — the DEFAULT backtest path (the
//!   old whole-account adverse-intrabar wipe is the opt-in `venue_style_liquidation` knob).
//! - `vike_core::snapshot` — the GUI badge: `ContractValue::isolated_liquidation_price` (the
//!   closed form [`crate::liquidation_price`]) for Isolated at the real rate;
//!   `ContractValue::cross_liquidation_price` for Cross (the `initial_margin*0.5` hardcode is dead). The
//!   cross estimate was `cross_liquidation_price_est` here; it moved, bit for bit, to the type that
//!   owns contract money (owner, 2026-11-09), which withholds both badges on an inverse contract.
//!
//! PURE: no I/O, no account types — callers fold their own pool equity/maintenance with
//! their own authority (`Account::margin_in_use_priced` live; the per-row adverse fold in the
//! backtest) so existing bit-parity pins are preserved, and this module owns only the LAW. The
//! money of a candidate is the caller's too, through its `ContractValue` ([`LiquidationCandidate::unit`]).

use crate::money::margin_mode::MarginMode;

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
/// (`MarginCallConfig::maintenance_margin_requirement` live, `StrategyEngineConfig::maint_margin` in a backtest,
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
/// caller's parallel key list); `unrealized_pnl` orders the plan (losers first); `unit` prices one unit's
/// maintenance.
#[derive(Debug, Clone, PartialEq)]
pub struct LiquidationCandidate {
    pub id: usize,
    /// signed position size
    pub size: f64,
    /// What ONE unit of the position is worth in the POOL's money at the mark the pool was judged
    /// at (live: the account mark, in the quote; backtest: the adverse print, in the ledger's
    /// money), from the contract's `ContractValue` (`gross_notional(1.0, mark)`, a coin ledger's
    /// through `in_settle`). It replaced a `mark` and a `mult` the plan multiplied: `mark · mult`
    /// is this product bit for bit, for both kinds.
    pub unit: f64,
    /// unrealized PnL at that mark — the losers-first sort key
    pub unrealized_pnl: f64,
}

/// The LEAN `DefaultMarginCallModel` reduction workflow over a BREACHED cross pool:
/// sort ascending by `unrealized_pnl` (biggest losers first; the sort is STABLE, so candidates tied on
/// `unrealized_pnl` keep the caller's insertion order), then liquidate only the `excess`
/// (`pool_maintenance − pool_equity`) position by position — each frees
/// `qty · unit · maint_rate` — and STOP as soon as the pool is healthy.
///
/// Returns `(id, qty)` close intents in liquidation order. Candidates whose per-unit
/// maintenance is non-positive OR non-finite (unpriceable) are skipped, exactly as the
/// pre-law watchdog skipped the non-positive ones; the non-finite arm is a pub-API hardening
/// (a NaN `per_unit` would poison `excess` and cascade full-size closes onto every remaining
/// candidate — unreachable from both in-tree callers, whose marks are finite, but this fn is
/// `pub`). This is the EXACT loop hoisted from `vike_execution::check_margin_call_priced` — the
/// arithmetic (`(mark · mult) · rate` association, now `unit · rate` with `unit` that same
/// product, `min(excess/per_unit, |size|)`, the `excess -= qty·per_unit` walk) is load-bearing
/// for the cross-only byte-identity pin; do not reorder.
pub fn cross_liquidation_plan(
    mut candidates: Vec<LiquidationCandidate>,
    excess: f64,
    maint_rate: f64,
) -> Vec<(usize, f64)> {
    candidates.sort_by(|a, b| {
        a.unrealized_pnl.partial_cmp(&b.unrealized_pnl).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut excess = excess;
    let mut plan = Vec::new();
    for c in candidates {
        if excess <= 0.0 {
            break;
        }
        let per_unit = c.unit * maint_rate;
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
pub struct LiquidationPools {
    pub cross: Vec<usize>,
    pub isolated: Vec<usize>,
}

/// Split a position ledger (by iteration order) into liquidation pools by margin mode.
pub fn partition_pools(modes: impl IntoIterator<Item = MarginMode>) -> LiquidationPools {
    let mut out = LiquidationPools::default();
    for (i, mode) in modes.into_iter().enumerate() {
        match mode {
            MarginMode::Cross => out.cross.push(i),
            MarginMode::Isolated => out.isolated.push(i),
            MarginMode::Cash => {} // never liquidates — no pool
        }
    }
    out
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

    /// A unit-1 contract at `mark`: one unit is worth the mark.
    fn cand(id: usize, size: f64, mark: f64, unrealized_pnl: f64) -> LiquidationCandidate {
        LiquidationCandidate { id, size, unit: mark, unrealized_pnl }
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
    fn tie_on_unrealized_pnl_keeps_insertion_order() {
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
    fn nan_unrealized_pnl_sorts_stably_not_panicking() {
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

    // The cross badge's tests moved with it: `crate::money::contract_value`'s `cross_est_*`.
}
