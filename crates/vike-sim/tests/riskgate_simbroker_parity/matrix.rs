//! The hand matrix: the curated `Row`s, pure data, each one decision the two engines reconcile.

use super::{Divergence, Row};

/// The matrix: opening / closing / reversing x below / at / above each floor x multiplier 1 and
/// != 1 x flat / long / short x `reduce_only` on and off, plus the margin lane.
pub(super) fn matrix() -> Vec<Row> {
    vec![
        // ---------------------------------------------------------------------------------
        // Baseline: no floors armed. Both sides must admit/fill unconditionally.
        // ---------------------------------------------------------------------------------
        Row { name: "unconstrained/open-long/flat", ..Row::default() },
        Row { name: "unconstrained/open-short/flat", side: -1, ..Row::default() },
        Row {
            name: "unconstrained/close-long",
            position: 5.0,
            side: -1,
            qty: 5.0,
            ..Row::default()
        },
        Row {
            name: "unconstrained/close-short",
            position: -5.0,
            side: 1,
            qty: 5.0,
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // min_qty — the #458 anti-stranding rule and its boundaries.
        // ---------------------------------------------------------------------------------
        Row {
            // OPENING below the floor: gate denies `below-min-qty`, apply_fill drops `min_qty`.
            name: "min_qty/opening-below/flat",
            qty: 0.005,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // AT the floor — `<` on both sides, so the boundary value is admitted by both.
            name: "min_qty/opening-at-floor/flat",
            qty: 0.01,
            min_qty: 0.01,
            ..Row::default()
        },
        Row {
            // ABOVE the floor.
            name: "min_qty/opening-above/flat",
            qty: 0.02,
            min_qty: 0.01,
            ..Row::default()
        },
        Row {
            // #458 REGRESSION PIN (long): a covered dust flatten below the floor must reach the
            // venue on BOTH sides. This is the bug — gate denied, SimBroker filled — that
            // stranded a position live.
            name: "min_qty/covered-flatten-below/long",
            position: 0.005,
            side: -1,
            qty: 0.005,
            min_qty: 0.01,
            ..Row::default()
        },
        Row {
            // ...and the short twin.
            name: "min_qty/covered-flatten-below/short",
            position: -0.005,
            side: 1,
            qty: 0.005,
            min_qty: 0.01,
            ..Row::default()
        },
        Row {
            // A PARTIAL reduce (qty < |position|) below the floor — covered, so both execute.
            name: "min_qty/partial-reduce-below/long",
            position: 0.02,
            side: -1,
            qty: 0.005,
            min_qty: 0.01,
            ..Row::default()
        },
        Row {
            // KNOWN-INTENTIONAL DIVERGENCE: below-min REVERSAL. 0.008 > |0.005| flips through
            // flat, so the gate refuses (it opens the far side) while apply_fill's
            // direction-only split executes the whole flip.
            name: "min_qty/reversal-below/long",
            position: 0.005,
            side: -1,
            qty: 0.008,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: true,
            divergence: Some(Divergence::BelowMinReversal),
            ..Row::default()
        },
        Row {
            // ...and the short twin of the reversal.
            name: "min_qty/reversal-below/short",
            position: -0.005,
            side: 1,
            qty: 0.008,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: true,
            divergence: Some(Divergence::BelowMinReversal),
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // reduce_only — the #481 `is_covered_reduce` contract. SimBroker has NO reduce_only
        // concept; it derives closing-ness from the ACTUAL position, which is exactly why the
        // gate's FLOOR bypass refuses to trust the bare flag.
        // ---------------------------------------------------------------------------------
        Row {
            // FLAT book + reduce_only flag is an OPENING order: the flag must buy nothing at the
            // floor. Both refuse. (Collapsing `is_covered_reduce` to `reduce_only || implicit`
            // would break this row.)
            name: "reduce_only/flat-book-flag-below-floor",
            qty: 0.005,
            reduce_only: true,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // COVERED reduce_only below the floor: both execute.
            name: "reduce_only/covered-below-floor/long",
            position: 5.0,
            side: -1,
            qty: 2.0,
            reduce_only: true,
            min_qty: 10.0,
            ..Row::default()
        },
        Row {
            // #600-P4 FIX, floor lane (long): reduce_only on a SAME-direction ADD that the
            // position "covers" in magnitude (long 5, BUY 2 tagged reduce_only). Pre-fix this was
            // a LATENT ASYMMETRY: `is_covered_reduce`'s flag arm was direction-blind
            // (`reduce_only && |pos| >= |qty|`), so the live gate ADMITTED this exposure-INCREASING
            // order with the floor bypassed, while `apply_fill`'s direction-only split called it
            // the OPENING fill it is and floor-gated it. The flag arm now also requires the
            // reducing direction, so the gate denies `below-min-qty` — AGREEING with the backtest,
            // and with binance/bybit (which reject a reduce_only order that would increase a
            // position). No `Divergence` pin: the two engines now match.
            name: "reduce_only/covered-same-direction-add-below-floor/long",
            position: 5.0,
            side: 1,
            qty: 2.0,
            reduce_only: true,
            min_qty: 10.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // ...and the short twin (short 5, SELL 2 tagged reduce_only).
            name: "reduce_only/covered-same-direction-add-below-floor/short",
            position: -5.0,
            side: -1,
            qty: 2.0,
            reduce_only: true,
            min_qty: 10.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // min_notional twin: 0.02 * 100 = 2.0 notional under the 5.0 floor, same shape.
            name: "reduce_only/covered-same-direction-add-below-notional",
            position: 0.05,
            side: 1,
            qty: 0.02,
            reduce_only: true,
            min_notional: 5.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // GUARD row: the same same-direction flagged add with NO floors armed is an ordinary
            // opening order — both sides execute it. Pins that the fix is bypass-removal (the add
            // faces the normal opening-order checks), NOT a blanket local deny of the
            // venue-contradictory flag: locally the venue rejects it server-side; the gate just
            // stops trusting the flag to skip its checks.
            name: "reduce_only/covered-same-direction-add-unconstrained",
            position: 5.0,
            side: 1,
            qty: 2.0,
            reduce_only: true,
            ..Row::default()
        },
        Row {
            // reduce_only asserted on an order the position does NOT cover — a reversal. The flag
            // does not launder it past the floor on either side; the divergence is the reversal
            // one above, not a flag one.
            name: "reduce_only/uncovered-reversal-below-floor",
            position: 0.005,
            side: -1,
            qty: 0.008,
            reduce_only: true,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: true,
            divergence: Some(Divergence::BelowMinReversal),
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // min_notional, multiplier 1 — the same three boundaries.
        // ---------------------------------------------------------------------------------
        Row {
            name: "min_notional/opening-below/flat",
            qty: 0.02,
            min_notional: 5.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // 0.05 * 100 = 5.0 exactly at the floor — `<` on both sides admits it.
            name: "min_notional/opening-at-floor/flat",
            qty: 0.05,
            min_notional: 5.0,
            ..Row::default()
        },
        Row {
            name: "min_notional/covered-flatten-below/long",
            position: 0.02,
            side: -1,
            qty: 0.02,
            min_notional: 5.0,
            ..Row::default()
        },
        Row {
            name: "min_notional/reversal-below/long",
            position: 0.02,
            side: -1,
            qty: 0.03,
            min_notional: 5.0,
            live_ok: false,
            sim_filled: true,
            divergence: Some(Divergence::BelowMinReversal),
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // multiplier != 1 — #458's second finding. The gate's notional omitted `ctx.multiplier`
        // while `apply_fill` gates on `rounded * price * multiplier`; these rows are the
        // regression pin. Under the OLD gate the first row denied live and filled in backtest.
        // ---------------------------------------------------------------------------------
        Row {
            // qty*price = 2.0 (under the 5.0 floor) but *10 multiplier = 20.0 -> both admit.
            name: "multiplier10/notional-cleared-by-multiplier",
            qty: 2.0,
            price: 1.0,
            multiplier: 10.0,
            min_notional: 5.0,
            ..Row::default()
        },
        Row {
            // still under the floor even WITH the multiplier: 0.2 * 1 * 10 = 2.0 < 5.0.
            name: "multiplier10/notional-below-even-with-multiplier",
            qty: 0.2,
            price: 1.0,
            multiplier: 10.0,
            min_notional: 5.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // multiplier interacts with the COVERED-reduce bypass identically on both sides.
            name: "multiplier10/covered-flatten-below-notional",
            position: 0.2,
            side: -1,
            qty: 0.2,
            price: 1.0,
            multiplier: 10.0,
            min_notional: 5.0,
            ..Row::default()
        },
        Row {
            // multiplier is NOT part of the min_qty floor on either side (it is a QUANTITY floor).
            name: "multiplier10/min-qty-is-unaffected-by-multiplier",
            qty: 0.005,
            price: 1.0,
            multiplier: 10.0,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // The margin lane. Since deny-vs-clamp PHASE 2 there is ONE formula: the backtest
        // crosses the literal `RiskGate::check` (leverage mapped to `im = 1/leverage`), so
        // every row here is an AGREEMENT assertion — including the knobs that used to exist
        // only live (`required_free_bp_pct`, `closing_credit`).
        // ---------------------------------------------------------------------------------
        Row {
            // Comfortably inside BOTH: order notional 5_000, margin 5_000 <= free 10_000, and
            // 5_000 <= leverage(1.0) * equity(10_000). Both admit in full.
            name: "margin/inside-both-models",
            qty: 50.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            ..Row::default()
        },
        Row {
            // 1_000 units = 100_000 notional. Live denies `insufficient-margin`; the backtest
            // crosses the SAME gate and denies with the SAME string (pinned in
            // `margin_over_budget_denies_whole_by_default` below). (Pre-phase-1 this row's
            // backtest side TRUNCATED to the 100 units that fit; that shape survives only
            // behind `clamp_to_leverage: true` and is pinned by
            // `clamp_knob_restores_the_truncate_shape_verbatim`.)
            name: "margin/over-budget-both-deny-whole",
            qty: 1_000.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // PHASE-2 FLIP (was `Divergence::MarginFormulaDivergesPendingPhase2`, "no backtest
            // twin"): `required_free_bp_pct` now reaches the backtest verbatim through
            // `EngineParams::risk_limits`. A 50% haircut leaves free BP 5_000, so the 6_000
            // order margin denies — on BOTH sides, same formula, same reason.
            name: "margin/required-free-bp-pct-now-backtest-effective",
            qty: 60.0,
            im: Some(1.0),
            free_bp_pct: 0.5,
            leverage: Some(1.0),
            equity: 10_000.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // PHASE-2 POSITIVE ASSERTION, reversal WITHIN the closing credit: long 50, sell 100
            // (an uncovered reversal — `is_covered_reduce` is false, so it faces buying power
            // for its FULL size). Order margin 10_000; free = equity 10_000 − margin_used 5_000
            // + closing_credit 2·50·100·1·1 = 10_000 -> 15_000. Both sides admit, through the
            // same `closing_credit` arithmetic (the retired leverage-room formula had no credit
            // concept — it waved every reducing-direction order through unexamined).
            name: "margin/reversal-within-credit-both-admit",
            position: 50.0,
            side: -1,
            qty: 100.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            ..Row::default()
        },
        Row {
            // PHASE-2 POSITIVE ASSERTION, reversal BEYOND the credit: long 50, sell 300. Order
            // margin 30_000 > free 15_000 -> the gate DENIES on both sides. Under the RETIRED
            // backtest formula this row ADMITTED (direction-only "reducing never denied"), so
            // this is the row where the gate's verdict deliberately CHANGED the backtest —
            // asserted positively, exactly as phase 2 intends (live is the judge; the covered
            // flatten still bypasses, so nothing is ever stranded).
            name: "margin/reversal-beyond-credit-both-deny",
            position: 50.0,
            side: -1,
            qty: 300.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // A COVERED reduce bypasses the margin check — the ONE `is_covered_reduce` bypass,
            // evaluated by the one shared gate on both sides. The position is 100 units, not
            // more: that is the row's OWN margin budget (im 1.0 x 10_000 / 100), and the setup
            // order is subject to the gate like any other (see the harness invariant in
            // `check` — an exactly-fitting setup passes whole under deny AND clamp).
            name: "margin/covered-reduce-bypasses-both",
            position: 100.0,
            side: -1,
            qty: 100.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            ..Row::default()
        },
        Row {
            // RESOLVED by the margin-coverage fix: the gate's buying-power bypass now requires
            // POSITION COVERAGE (`is_covered_reduce`) instead of honoring a bare `reduce_only`
            // flag, so a FLAT book + flag is treated as the opening order it is and faces margin.
            // Both sides now refuse a 100_000-notional order against 10_000 of equity — the live
            // gate denies `insufficient-margin`, SimBroker denies whole. This row previously pinned
            // `Divergence::FlatBookReduceOnlyFlagSkipsMargin`, discovered by this very matrix.
            name: "margin/flat-book-reduce-only-flag-now-faces-margin",
            qty: 1_000.0,
            reduce_only: true,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // multiplier != 1 in the margin lane: 50 units * 100 * mult 10 = 50_000 order
            // margin > free 10_000 -> the one gate denies on both sides; what this row adds is
            // that the shared margin formula carries the multiplier — as `max_total_exposure` now
            // also does (fixed; pinned in
            // `max_total_exposure_includes_the_contract_multiplier`).
            name: "margin/multiplier10-over-budget",
            qty: 50.0,
            multiplier: 10.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // The ACCOUNT-AGGREGATE lane (`RiskLimits::max_account_exposure`). The only lane in
        // this file that is CROSS-SYMBOL, so every row here carries an `other_position` — with
        // one symbol the account sum is empty and the lane says nothing the per-symbol
        // projection did not already say.
        //
        // The two sides fold that sum in DIFFERENT code: live in
        // `ExecutionEngine::resolved_account_exposure_excluding` (a walk of the position map and
        // the order registry, `+=`), backtest in `SimBroker::gate_order` (`py_sum` over
        // `symbols`). Same numbers or these rows fail — which is the whole reason they are here
        // rather than in either crate's own suite.
        // ---------------------------------------------------------------------------------
        Row {
            // Inside the ceiling: 50 units of the OTHER symbol at 100 = 5 000, plus this order's
            // projected 10 × 100 = 1 000, against 8 000. Both admit — the non-vacuous half.
            name: "account/inside-the-ceiling",
            qty: 10.0,
            account_cap: Some(8_000.0),
            other_position: 50.0,
            ..Row::default()
        },
        Row {
            // Over it: the same 5 000 elsewhere plus 4 000 here is 9 000 against 8 000. Both
            // deny — and NEITHER side's per-symbol cap is armed at all, so `over-max-exposure`
            // cannot be what stopped it.
            name: "account/over-the-ceiling",
            qty: 40.0,
            account_cap: Some(8_000.0),
            other_position: 50.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // The SHORT twin of the row above: the other symbol's position is negative and the
            // sum is GROSS, so it contributes exactly as much as the long did. A netting fold
            // (or a signed one) reads this account as −5 000 and ADMITS — which is the reading
            // `max_account_exposure`'s doc refuses, and this row is where the two engines would
            // be caught disagreeing about it.
            name: "account/gross-not-net-short-elsewhere",
            qty: 40.0,
            account_cap: Some(8_000.0),
            other_position: -50.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        // ⚠ NO covered-reduce row here, and the omission is reasoned rather than an oversight.
        // Reaching one would need an account ALREADY over the ceiling at the moment of the test
        // order, and every setup leg in this harness crosses the same armed gate — so the setup
        // that put the account over the ceiling would itself have been refused, and the row would
        // silently measure a different scenario (the failure mode `check`'s harness invariant
        // exists to catch). The bypass is not a CONTEXT question anyway: `covered_reduce` is
        // computed inside the shared `RiskGate::check_inner`, which both engines call, so what
        // this file reconciles — each side's ctx construction — cannot differ about it.
        // `crates/vike-exec/tests/risk/risk_account_ceiling.rs`'s
        // `the_account_ceiling_never_refuses_a_covered_reduce` is where that arm is proven.
    ]
}
