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
//!      [`mtm_curve_from_store`] sources that closure from a [`HistStore`]'s bars. Equity is
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

use serde::Serialize;
use std::collections::{BTreeSet, HashMap};

use vike_data::{DataError, HistStore, TsRange};
use vike_model::events::FillEvent;
use vike_model::{Bar, TradeFold, py_sum};

/// The contract multiplier for the journaled (crypto LINEAR) venues — the SAME 1.0 constant, and
/// for the same reason, as `crate::trades`'s `MULTIPLIER`: today's journaled venues are linear
/// instruments whose PnL is `(exit - entry) · qty`. Threaded as [`reconstruct_mtm`]'s `multiplier`
/// so a future inverse/quanto extension is a single obvious edit.
const MULTIPLIER: f64 = 1.0;

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

/// Reconstruct the mark-to-market equity/exposure series for `fills` against a [`HistStore`]'s
/// `(venue, symbol, interval)` bar closes — the store-backed producer of [`reconstruct_mtm`]'s
/// price closure. DataFusion-free: takes the venue-agnostic `HistStore` trait object (no feature),
/// exactly like [`crate::excursions::backfill_excursions`].
///
/// The sample grid is the sorted union of every fill's `ts` and every bar `ts` in
/// `[first_fill_ts, last_fill_ts]`, so equity marks between closes as well as at fills. Prices are
/// an as-of lookup (the last bar close at or before the sample); a symbol with no bar yet at a
/// sample is the missing-price case. `venue` is fixed for all bar lookups (like
/// `backfill_excursions`); a position keyed on a symbol with no loaded bars reads as missing.
pub fn mtm_curve_from_store(
    store: &dyn HistStore,
    fills: &[FillEvent],
    seed: f64,
    venue: &str,
    interval: &str,
) -> Result<Vec<MtmPoint>, DataError> {
    if fills.is_empty() {
        return Ok(Vec::new());
    }
    let first_ts = fills.iter().map(|f| f.ts).min().unwrap_or(0);
    let last_ts = fills.iter().map(|f| f.ts).max().unwrap_or(0);

    // Distinct traded symbols.
    let mut symbols: Vec<String> = fills.iter().map(|f| f.symbol.to_string()).collect();
    symbols.sort();
    symbols.dedup();

    // Preload bars per symbol over the fill window; union bar timestamps into the sample grid.
    let mut bars_by_symbol: HashMap<String, Vec<Bar>> = HashMap::new();
    let mut mark_set: BTreeSet<i64> = fills.iter().map(|f| f.ts).collect();
    for sym in &symbols {
        let bars = store.load_bars(venue, sym, interval, TsRange::of(first_ts, last_ts))?;
        for b in &bars {
            mark_set.insert(b.ts);
        }
        bars_by_symbol.insert(sym.clone(), bars);
    }
    let marks: Vec<i64> = mark_set.into_iter().collect();

    // As-of price: the last bar close at or before `ts` (bars are ts-ascending). No bar yet → None.
    let price_at = |_v: &str, s: &str, ts: i64| -> Option<f64> {
        let bars = bars_by_symbol.get(s)?;
        let idx = bars.partition_point(|b| b.ts <= ts);
        if idx == 0 { None } else { Some(bars[idx - 1].close) }
    };

    Ok(reconstruct_mtm(fills, seed, &marks, MULTIPLIER, price_at))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equity::equity_curve_from_trades;
    use crate::trades::reconstruct_trades;
    use vike_model::events::TradeId;

    /// A `FillEvent` on a single venue/symbol/one-way position — mirrors `trades.rs`'s helper.
    fn fill(side: i32, qty: f64, px: f64, ts: i64, commission: f64) -> FillEvent {
        FillEvent {
            // minted here, not read off a wire — same `t<ts>-<side>` bytes as before
            trade_id: TradeId::prefixed("t", format_args!("{ts}-{side}")),
            client_order_id: String::new(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side,
            last_qty: qty,
            last_px: px,
            commission,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            ts,
            mark_price: Some(px),
            position_side: "BOTH".into(),
        }
    }

    /// A step price schedule keyed on the sample ts — no store needed for the pure core.
    fn stepped(ts: i64) -> Option<f64> {
        match ts {
            10 => Some(100.0),
            20 => Some(110.0),
            30 => Some(120.0),
            40 => Some(120.0),
            _ => None,
        }
    }

    #[test]
    fn mtm_curve_marks_between_and_after_close() {
        // buy 1 @ 100 (ts 10), sell 1 @ 120 (ts 30); marks at open / midpoint / close / past.
        let fills = vec![fill(1, 1.0, 100.0, 10, 0.0), fill(-1, 1.0, 120.0, 30, 0.0)];
        let marks = [10, 20, 30, 40];
        let pts = reconstruct_mtm(&fills, 1_000.0, &marks, 1.0, |_v, _s, ts| stepped(ts));

        assert_eq!(pts.len(), 4, "one sample per mark — none dropped");
        // ts10: long 1 @ 100 marked at 100 → flat unrealized.
        assert_eq!(pts[0].equity, 1_000.0);
        assert_eq!(pts[0].gross_notional, 100.0);
        assert_eq!(pts[0].traded_notional, 100.0);
        // ts20: still long, marked at 110 → +10 unrealized (MTM between opens/closes).
        assert_eq!(pts[1].equity, 1_010.0);
        assert_eq!(pts[1].unrealized, 10.0);
        assert_eq!(pts[1].gross_notional, 110.0);
        assert_eq!(pts[1].net_notional, 110.0);
        // ts30: sell closes the long, realized +20, flat → unrealized 0.
        assert_eq!(pts[2].equity, 1_020.0);
        assert_eq!(pts[2].realized, 20.0);
        assert_eq!(pts[2].unrealized, 0.0);
        assert_eq!(pts[2].gross_notional, 0.0);
        assert_eq!(pts[2].traded_notional, 220.0);
        // ts40: still flat, equity holds at realized.
        assert_eq!(pts[3].equity, 1_020.0);
        assert!(pts.iter().all(|p| p.missing_prices == 0));
    }

    #[test]
    fn missing_price_is_flagged_not_dropped() {
        // buy 1 @ 100 (ts 10). At ts 20 the price is missing while the position is still OPEN.
        let fills = vec![fill(1, 1.0, 100.0, 10, 0.0)];
        let marks = [10, 20];
        let pts = reconstruct_mtm(&fills, 1_000.0, &marks, 1.0, |_v, _s, ts| match ts {
            10 => Some(100.0),
            _ => None, // ts 20: gap
        });
        assert_eq!(pts.len(), 2, "the missing-price sample is EMITTED, not dropped");
        assert_eq!(pts[1].missing_prices, 1, "the gap is flagged");
        // silent-zero: the unpriceable position contributes 0.0 unrealized and 0 notional.
        assert_eq!(pts[1].unrealized, 0.0);
        assert_eq!(pts[1].gross_notional, 0.0);
        assert_eq!(pts[1].net_notional, 0.0);
        // equity still resolves (seed + realized 0 + unrealized 0).
        assert_eq!(pts[1].equity, 1_000.0);
    }

    #[test]
    fn mtm_equals_realized_curve_when_flat() {
        // With fees, the last (flat) MTM equity is bit-identical to the realized-only fallback.
        let fills = vec![fill(1, 1.0, 100.0, 10, 0.1), fill(-1, 1.0, 120.0, 30, 0.2)];
        let marks = [10, 20, 30];
        let pts = reconstruct_mtm(&fills, 1_000.0, &marks, 1.0, |_v, _s, ts| stepped(ts));

        let trades = reconstruct_trades(&fills);
        let (eq, _ts) = equity_curve_from_trades(1_000.0, &trades);
        let realized_last = *eq.last().unwrap();
        let mtm_last = pts.last().unwrap().equity;
        assert_eq!(
            mtm_last.to_bits(),
            realized_last.to_bits(),
            "flat MTM equity must bit-match the realized-only fallback"
        );
    }

    #[test]
    fn runtime_stats_turnover_and_exposure() {
        let fills = vec![fill(1, 1.0, 100.0, 10, 0.0), fill(-1, 1.0, 120.0, 30, 0.0)];
        let marks = [10, 20, 30, 40];
        let pts = reconstruct_mtm(&fills, 1_000.0, &marks, 1.0, |_v, _s, ts| stepped(ts));
        let rs = RuntimeStats::from_points(&pts);

        // total traded notional = 100 (buy) + 120 (sell) = 220.
        assert_eq!(rs.traded_notional, 220.0);
        // turnover = traded_notional / final_equity, bit-identical to the same division here.
        assert_eq!(rs.turnover.to_bits(), (220.0_f64 / 1_020.0).to_bits());
        // peak gross notional is at ts20 (110), giving the peak margin + the peak leverage ratio.
        assert_eq!(rs.peak_margin_used, 110.0);
        assert_eq!(rs.peak_gross_exposure.to_bits(), (110.0_f64 / 1_010.0).to_bits());
        // all-long book → net leverage equals gross leverage.
        assert_eq!(rs.peak_net_exposure.to_bits(), (110.0_f64 / 1_010.0).to_bits());
    }

    #[test]
    fn empty_fills_and_empty_points_are_inert() {
        let pts = reconstruct_mtm(&[], 1_000.0, &[10, 20], 1.0, |_v, _s, _ts| Some(1.0));
        // No fills → two flat samples at seed.
        assert_eq!(pts.len(), 2);
        for p in &pts {
            assert_eq!(p.equity, 1_000.0);
            assert_eq!(p.traded_notional, 0.0);
        }
        // Empty series → all-zero summary (house sentinel), never a divide-by-zero.
        let rs = RuntimeStats::from_points(&[]);
        assert_eq!(rs.turnover, 0.0);
        assert_eq!(rs.peak_gross_exposure, 0.0);
        assert_eq!(rs.peak_margin_used, 0.0);
    }

    #[test]
    fn short_position_marks_and_nets_signed() {
        // sell 1 @ 120 (ts 10): a short profits as price falls; net notional is negative.
        let fills = vec![fill(-1, 1.0, 120.0, 10, 0.0)];
        let marks = [10, 20];
        let pts = reconstruct_mtm(&fills, 1_000.0, &marks, 1.0, |_v, _s, ts| match ts {
            10 => Some(120.0),
            20 => Some(110.0), // price fell 10 → short gains 10
            _ => None,
        });
        assert_eq!(pts[1].unrealized, 10.0, "short gains as price falls: (110-120)*(-1)");
        assert_eq!(pts[1].equity, 1_010.0);
        assert_eq!(pts[1].gross_notional, 110.0, "|−1|·110");
        assert_eq!(pts[1].net_notional, -110.0, "−1·110 (signed short)");
        // net-exposure ratio uses the ABSOLUTE net notional; the peak is at ts10 (120/1000 = 0.12,
        // above ts20's 110/1010), proving the |·| is applied to the SIGNED short notional.
        let rs = RuntimeStats::from_points(&pts);
        assert_eq!(rs.peak_net_exposure.to_bits(), (120.0_f64 / 1_000.0).to_bits());
        assert_eq!(rs.peak_margin_used, 120.0, "peak gross notional is the ts10 |−1|·120");
    }
}
