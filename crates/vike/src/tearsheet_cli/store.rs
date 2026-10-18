//! The two tearsheet builders that READ A STORE.
//!
//! Both are thin: each reads one `vike_data::HistStore` series and hands it to a pure fold in
//! `vike-analytics` — [`vike_analytics::equity_curve_from_samples`] for the sampler's
//! `kind=equity` series, [`vike_analytics::reconstruct_mtm`] for the mark-to-market grid. They live
//! beside the CLI, not in `vike-analytics` (which may name nothing above the vocabulary floor:
//! `crates/vike-ops/tests/architecture/layer_gate/tiers.rs`'s `every_tier_15_crate_names_nothing_above_the_vocabulary`)
//! and not in `vike-report` (which reads the command journal only): the `HistStore` seam is
//! `vike-data`, and the `--datahub` path is their one caller.
//!
//! DataFusion-free: both take the venue-agnostic `HistStore` TRAIT object, never a concrete
//! backend, exactly like [`super::excursions::backfill_excursions`]. The `tearsheet` CLI
//! (`crates/vike/src/tearsheet_cli.rs`) hands them the routed store behind its `--datahub` flag.

use std::collections::{BTreeSet, HashMap};

use vike_analytics::{MtmPoint, equity_curve_from_samples, reconstruct_mtm};
use vike_data::{DataError, HistStore, TsRange};
use vike_marketdata::Bar;
use vike_model::events::FillEvent;

/// Build an `(equity, ts)` curve straight from a `vike_data::HistStore`'s `kind=equity` series for
/// `(venue, symbol)` over `range`. Thin wrapper over `HistStore::scan_equity` +
/// [`vike_analytics::equity_curve_from_samples`]; `symbol` is the venue name (or the cross-venue
/// `"TOTAL"` rollup) the sampler wrote under. `range` defaults to `TsRange::all()` at the call
/// site.
pub fn equity_curve_from_store(
    store: &dyn HistStore,
    venue: &str,
    symbol: &str,
    range: TsRange,
) -> Result<(Vec<f64>, Vec<i64>), DataError> {
    let samples = store.scan_equity(venue, symbol, range)?;
    Ok(equity_curve_from_samples(&samples))
}

/// Reconstruct the mark-to-market equity/exposure series for `fills` against a
/// `vike_data::HistStore`'s `(venue, symbol, interval)` bar closes — the store-backed producer of
/// [`vike_analytics::reconstruct_mtm`]'s price closure.
///
/// The sample grid is the sorted union of every fill's `ts` and every bar `ts` in
/// `[first_fill_ts, last_fill_ts]`, so equity marks between closes as well as at fills. Prices are
/// an as-of lookup (the last bar close at or before the sample); a symbol with no bar yet at a
/// sample is the missing-price case. `venue` is fixed for all bar lookups (like
/// `backfill_excursions`); a position keyed on a symbol with no loaded bars reads as missing.
///
/// The contract multiplier is `vike_analytics::trades::MULTIPLIER` (the journaled venues are crypto
/// LINEAR, PnL `(exit - entry) · qty`): the SAME constant the trades fold uses, named once, so a
/// future inverse/quanto extension is one edit there and not two.
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

    Ok(reconstruct_mtm(fills, seed, &marks, vike_analytics::trades::MULTIPLIER, price_at))
}
