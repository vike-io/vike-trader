//! Report-from-journal mark-to-market equity reconstruction + runtime-exposure stats.
//!
//! Two REPORT-SIDE additions (nothing persisted — the live vike-core equity sampler's
//! `kind=equity` series stays the sole PERSISTED, PRIMARY equity source; this is an in-memory
//! ALTERNATIVE the tearsheet can opt into):
//!
//!  (a) [`reconstruct_mtm`] folds a chronological `FillEvent` stream into positions over time
//!      (the SAME `(venue, symbol, position_side)` keying + `vike_model::TradeFold` cost-basis
//!      primitive [`crate::trades::reconstruct_trades`] uses) and marks the open positions to
//!      market at a caller-supplied timestamp grid, joining prices from a `price_at` closure —
//!      `vike_report::mtm_curve_from_store` sources that closure from a `vike_data::HistStore`'s
//!      bars. [`reconstruct_mtm`] itself takes the CLOSURE, so the fold is pure and needs no data
//!      layer — which is why it could move here out of `vike-report` on 2026-09-28 while the store
//!      builder stayed behind (`crates/vike-report/src/store.rs`). Equity is
//!      `seed + Σ_closed(pnl - fees) + Σ_open unrealized`, so at a flat sample it equals the
//!      realized-only [`crate::equity::equity_curve_from_trades`] fallback and BETWEEN closes it
//!      adds the mark-to-market carry that a bare fill stream cannot. It does NOT try to bit-match
//!      the live sampler's venue-balance-aware equity (funding/deposits the fill stream lacks).
//!
//!  MISSING-PRICE POLICY — reuses the equity sampler's convention (`resolve_equity`
//!  /`Account::margin_in_use_priced` in vike-exec): an OPEN position with no mark at a sample
//!  contributes 0.0 unrealized (silent-zero) and increments that point's `missing_prices`, and is
//!  EXCLUDED from the gross/net notional (the LEAN "unpriceable → contributes 0" skip). The sample
//!  is still EMITTED — a price gap is FLAGGED, never dropped. FLAT (zero-size) positions are
//!  skipped entirely (never counted missing), matching `net_notional_priced`'s flat-skip.
//!
//!  (b) [`RuntimeStats`] summarizes that same positions-over-time fold — turnover (cumulative
//!      traded notional / equity), peak gross- & net-exposure RATIOS (Σ|size|·px / equity and
//!      |Σ size·px| / equity — the leverage twin of, and distinct from, `vike_analytics::metrics
//!      ::exposure`, which measures TIME-in-market), and peak margin-used (the unlevered gross
//!      notional — no leverage schedule exists at report time). Sourced from the fold, NOT sampled
//!      on any hot path; `html::render_html_with_stats` renders it as an additive HTML section.
//!
//! ⚠ The private `MULTIPLIER` const that stood here (the journaled venues' linear 1.0, threaded
//! into [`reconstruct_mtm`]'s `multiplier`) left with the store builder, its ONLY reader. It had
//! to be `journal`-gated while both lived in vike-report, or the feature-off build warned it dead —
//! the first defect the feature-off CI lane ever found, and a lane that no longer exists because
//! the feature does not. It lives in `crates/vike-report/src/store.rs` now, beside the one call
//! that passes it.

use serde::Serialize;

use vike_model::events::FillEvent;
use vike_model::{TradeFold, py_sum};

/// One reconstructed sample: mark-to-market equity PLUS the exposure/turnover snapshot at `ts`.
/// This IS the runtime-stats "sampled series" — report-side only, never persisted.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MtmPoint {
    /// sample timestamp (epoch ms) — one of the caller's `marks`
    pub ts: i64,
    /// `seed + Σ_closed(pnl - fees) + Σ_open unrealized` at `ts`
    pub equity: f64,
    /// cumulative realized-net (`Σ_closed(pnl - fees)`) up to and including `ts`
    pub realized: f64,
    /// mark-to-market PnL of the OPEN, PRICED positions at `ts` (missing ones contribute 0.0)
    pub unrealized: f64,
    /// count of OPEN positions with no mark at `ts` (silent-zero + count; the sample is still
    /// emitted — a price gap is flagged, not dropped)
    pub missing_prices: u32,
    /// gross notional `Σ|size|·px·mult` across open, PRICED positions at `ts`
    pub gross_notional: f64,
    /// signed net notional `Σ size·px·mult` across open, PRICED positions (long +, short −)
    pub net_notional: f64,
    /// cumulative traded notional `Σ|qty|·px·mult` over every fill folded up to and including `ts`
    pub traded_notional: f64,
}

/// Fold `fills` into positions over time and mark them to market at each timestamp in `marks`,
/// emitting one [`MtmPoint`] per mark.
///
/// PRECONDITIONS: `fills` MUST be chronological (journal order) and `marks` MUST be ascending —
/// the fold is a forward two-pointer that folds every fill with `ts <= mark` before valuing that
/// sample. For complete turnover/exposure the last mark should be `>= ` the last fill's `ts`
/// (the store builder guarantees both by unioning the fill timestamps into the grid).
///
/// `price_at(venue, symbol, ts) -> Option<f64>` supplies each open position's mark; `None` = the
/// missing-price policy (see the module doc): 0.0 unrealized for that position, `missing_prices`
/// incremented, excluded from the notionals.
pub fn reconstruct_mtm<F>(
    fills: &[FillEvent],
    seed: f64,
    marks: &[i64],
    multiplier: f64,
    mut price_at: F,
) -> Vec<MtmPoint>
where
    F: FnMut(&str, &str, i64) -> Option<f64>,
{
    // (venue, symbol, position_side) -> running fold, kept in insertion order (a Vec, not a map)
    // so the per-position unrealized `py_sum` folds in a deterministic order — the sampler's law.
    let mut book: Vec<(String, String, String, TradeFold)> = Vec::new();
    let mut realized_net = 0.0_f64;
    let mut traded_notional = 0.0_f64;
    let mut fi = 0usize;
    let mut points = Vec::with_capacity(marks.len());

    for &mark_ts in marks {
        // Fold every fill at or before this mark (advances the shared pointer forward only).
        while fi < fills.len() && fills[fi].ts <= mark_ts {
            let f = &fills[fi];
            traded_notional += vike_model::gross_notional(f.last_qty, f.last_px, multiplier);
            let key = (f.venue.to_string(), f.symbol.to_string(), f.position_side.to_string());
            let idx = match book
                .iter()
                .position(|(v, s, ps, _)| *v == key.0 && *s == key.1 && *ps == key.2)
            {
                Some(i) => i,
                None => {
                    book.push((key.0.clone(), key.1.clone(), key.2.clone(), TradeFold::default()));
                    book.len() - 1
                }
            };
            // Route ALL cost-basis math through TradeFold (== reconstruct_trades / Account::fold);
            // realized-net folds `pnl - fees` exactly like `equity_curve_from_trades`.
            let step =
                book[idx].3.apply(f.side, f.last_qty, f.last_px, f.commission, f.ts, multiplier);
            if let Some(c) = step.closed {
                realized_net += c.pnl - c.fees;
            }
            fi += 1;
        }

        // Mark the open positions to market at this sample.
        let mut missing = 0u32;
        let mut per_pos_unreal: Vec<f64> = Vec::new();
        let mut gross = 0.0_f64;
        let mut net = 0.0_f64;
        for (v, s, _ps, fold) in book.iter() {
            if fold.size == 0.0 {
                continue; // flat leftover — excluded (matches net_notional_priced's flat-skip)
            }
            match price_at(v.as_str(), s.as_str(), mark_ts) {
                Some(px) => {
                    // Same shape as compute_fill's realized line at the mark: (px - avg)·size·mult.
                    per_pos_unreal.push((px - fold.avg_px) * fold.size * multiplier);
                    gross += vike_model::gross_notional(fold.size, px, multiplier);
                    net += vike_model::signed_notional(fold.size, px, multiplier);
                }
                None => {
                    missing += 1;
                    per_pos_unreal.push(0.0); // silent-zero, still counted in the fold order
                }
            }
        }
        let unrealized = py_sum(per_pos_unreal.iter().copied());
        let equity = seed + realized_net + unrealized;
        points.push(MtmPoint {
            ts: mark_ts,
            equity,
            realized: realized_net,
            unrealized,
            missing_prices: missing,
            gross_notional: gross,
            net_notional: net,
            traded_notional,
        });
    }
    points
}

/// Split a `[MtmPoint]` slice into the `(equity, ts)` pair the tearsheet consumes — the
/// mark-to-market twin of [`crate::equity::equity_curve_from_samples`], feeding
/// `LiveTearsheet::from_result_parts` and thus the reused `metrics::` catalog.
pub fn mtm_equity_curve(points: &[MtmPoint]) -> (Vec<f64>, Vec<i64>) {
    let equity = points.iter().map(|p| p.equity).collect();
    let ts = points.iter().map(|p| p.ts).collect();
    (equity, ts)
}

/// Runtime-exposure summary over a reconstructed [`MtmPoint`] series — the numbers
/// `html::render_html_with_stats` renders. All ratios use the house 0.0 sentinel on a
/// non-positive/degenerate denominator (the `vike_analytics::metrics` convention).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuntimeStats {
    /// cumulative traded notional `Σ|qty|·px·mult` over every fill (the last point's value)
    pub traded_notional: f64,
    /// portfolio turnover: `traded_notional / final_equity`
    pub turnover: f64,
    /// peak gross-exposure RATIO `Σ|size|·px·mult / equity` over the grid (gross leverage)
    pub peak_gross_exposure: f64,
    /// peak absolute net-exposure RATIO `|Σ size·px·mult| / equity` over the grid (net leverage)
    pub peak_net_exposure: f64,
    /// peak gross notional = unlevered margin requirement (no leverage schedule at report time)
    pub peak_margin_used: f64,
}

impl RuntimeStats {
    /// Summarize a reconstructed [`MtmPoint`] series. Empty series → all-zero (the house sentinel).
    pub fn from_points(points: &[MtmPoint]) -> Self {
        let traded_notional = points.last().map_or(0.0, |p| p.traded_notional);
        let final_equity = points.last().map_or(0.0, |p| p.equity);
        let turnover = if final_equity > 0.0 && traded_notional.is_finite() {
            traded_notional / final_equity
        } else {
            0.0
        };
        let mut peak_gross_exposure = 0.0_f64;
        let mut peak_net_exposure = 0.0_f64;
        let mut peak_margin_used = 0.0_f64;
        for p in points {
            if p.equity > 0.0 {
                let g = p.gross_notional / p.equity;
                if g > peak_gross_exposure {
                    peak_gross_exposure = g;
                }
                let n = (p.net_notional / p.equity).abs();
                if n > peak_net_exposure {
                    peak_net_exposure = n;
                }
            }
            if p.gross_notional > peak_margin_used {
                peak_margin_used = p.gross_notional;
            }
        }
        RuntimeStats {
            traded_notional,
            turnover,
            peak_gross_exposure,
            peak_net_exposure,
            peak_margin_used,
        }
    }
}

#[path = "mtm_tests.rs"]
#[cfg(test)]
mod mtm_tests;
