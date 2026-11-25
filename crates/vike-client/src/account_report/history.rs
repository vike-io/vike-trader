//! The account source's mark-to-market read: [`mtm_curve_from_store`], `--mark-to-market`'s curve.
//!
//! Thin: it reads the bars of one `vike_data::HistoryStore` and hands them to a pure fold in
//! `vike-analytics`, [`vike_analytics::reconstruct_mtm`]. It lives beside the account report, not in `vike-analytics`
//! (which may name nothing above the vocabulary floor:
//! `crates/vike-ops/tests/architecture/layer_gate/tiers.rs`'s `every_tier_15_crate_names_nothing_above_the_vocabulary`):
//! the `HistoryStore` seam is `vike-data`.
//!
//! DataFusion-free: it takes the venue-agnostic `HistoryStore` TRAIT object, never a concrete backend, exactly like
//! [`super::excursions::backfill_excursions`].
//!
//! ⚠ **Its two siblings are gone (2026-11-18)**: `equity_curve_from_store` and `default_equity_curve` plotted the
//! vike-core sampler's `portfolio`/`TOTAL` equity series, which is the whole NODE's and not an account's, so the
//! account source plots the account's realized curve by default instead (the owner's Q7).

use std::collections::{BTreeSet, HashMap};

use vike_analytics::{MarkToMarketPoint, reconstruct_mtm};
use vike_data::{HistoryStore, HistoryStoreError, TimestampRange};
use vike_marketdata::Bar;
use vike_model::events::FillEvent;

/// Reconstruct the mark-to-market equity/exposure series for `fills` against a
/// `vike_data::HistoryStore`'s `(venue, symbol, interval)` bar closes — the store-backed producer of
/// [`vike_analytics::reconstruct_mtm`]'s price closure.
///
/// The sample grid is the sorted union of every fill's `ts` and every bar `ts` in
/// `[first_fill_ts, last_fill_ts]`, so equity marks between closes as well as at fills. Prices are
/// an as-of lookup (the last bar close at or before the sample); a symbol with no bar yet at a
/// sample is the missing-price case. `venue` is fixed for all bar lookups (like
/// `backfill_excursions`); a position keyed on a symbol with no loaded bars reads as missing.
///
/// The contract multiplier is `vike_analytics::trades::MULTIPLIER` (linear, PnL `(exit - entry) · qty`):
/// the SAME constant the trades fold uses, named once. `fills` holds no other contract, because
/// the account source leaves those out (`vike_analytics::without_non_default_contracts`).
pub fn mtm_curve_from_store(
    store: &dyn HistoryStore,
    fills: &[FillEvent],
    seed: f64,
    venue: &str,
    interval: &str,
) -> Result<Vec<MarkToMarketPoint>, HistoryStoreError> {
    if fills.is_empty() {
        return Ok(Vec::new());
    }
    let first_timestamp = fills.iter().map(|f| f.timestamp).min().unwrap_or(0);
    let last_timestamp = fills.iter().map(|f| f.timestamp).max().unwrap_or(0);

    // Distinct traded symbols.
    let mut symbols: Vec<String> = fills.iter().map(|f| f.symbol.to_string()).collect();
    symbols.sort();
    symbols.dedup();

    // Preload bars per symbol over the fill window; union bar timestamps into the sample grid.
    let mut bars_by_symbol: HashMap<String, Vec<Bar>> = HashMap::new();
    let mut mark_set: BTreeSet<i64> = fills.iter().map(|f| f.timestamp).collect();
    for symbol in &symbols {
        let bars = store.load_bars(
            venue,
            symbol,
            interval,
            TimestampRange::of(first_timestamp, last_timestamp),
        )?;
        for b in &bars {
            mark_set.insert(b.timestamp);
        }
        bars_by_symbol.insert(symbol.clone(), bars);
    }
    let marks: Vec<i64> = mark_set.into_iter().collect();

    // As-of price: the last bar close at or before `ts` (bars are ts-ascending). No bar yet → None.
    let price_at = |_v: &str, s: &str, ts: i64| -> Option<f64> {
        let bars = bars_by_symbol.get(s)?;
        let index = bars.partition_point(|b| b.timestamp <= ts);
        if index == 0 { None } else { Some(bars[index - 1].close) }
    };

    Ok(reconstruct_mtm(fills, seed, &marks, vike_analytics::trades::MULTIPLIER, price_at))
}

#[path = "history_tests.rs"]
#[cfg(test)]
mod history_tests;
