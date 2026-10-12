//! `backfill_excursions` — fill each reconstructed [`Trade`]'s `mae`/`mfe` from a `HistStore`.
//!
//! A bare fill stream carries no intrabar path, so [`vike_analytics::reconstruct_trades`] leaves
//! `mae`/`mfe` at `0.0` (correct, not a shortcut — see `crates/vike-analytics/src/trades.rs`).
//! When a bar series for the traded symbol IS available in a [`HistStore`], this back-fills those
//! two fields the same way the backtest does: load the OHLC bars spanning the trade's
//! `[entry_ts, exit_ts]` window and hand them to the shared `vike_analytics::excursions::mae_mfe`
//! (the ONE excursion primitive — no math is reimplemented here).
//!
//! DataFusion-free: this takes the venue-agnostic [`HistStore`] TRAIT object, never a concrete
//! backend. The `tearsheet` CLI (`crates/vike/src/tearsheet_cli.rs`) hands it the routed store
//! behind its `--datahub` flag as `&dyn HistStore`.
//!
//! It lives beside the CLI, not in `vike-analytics` beside `mae_mfe` (the `HistStore` trait is
//! `vike-data`, and `vike-analytics` may not name `vike_data`) and not in `vike-report` (which
//! reads the command journal only): the `--datahub` path is its one caller.
//!
//! Best-effort: an empty bar window or a load error leaves that trade's `mae`/`mfe` at `0.0`
//! rather than failing the whole tearsheet — the store is an enrichment, not a hard dependency.

use std::collections::BTreeMap;

use vike_data::{HistStore, TsRange};
use vike_model::{Bar, Trade};

/// Back-fill `mae`/`mfe` on each trade from `store`'s `(venue, symbol, interval)` bar series.
///
/// For each trade: load bars over its `[entry_ts, exit_ts]` window (inclusive) and, if the window
/// is non-empty, write `vike_analytics::excursions::mae_mfe(trade, &bars, None)` into
/// `trade.mae`/`trade.mfe`. Empty bars OR a load error → the trade is left untouched (its `mae`/
/// `mfe` stay `0.0`). `venue`/`interval` are fixed across the whole trade set; `symbol` comes from
/// each `Trade`.
pub fn backfill_excursions(
    trades: &mut [Trade],
    store: &dyn HistStore,
    venue: &str,
    interval: &str,
) {
    // ⚠ **ONE read per SYMBOL, not one per TRADE**, byte-identical by construction:
    // `vike_analytics::excursions::mae_mfe` filters its own input on
    // `trade.entry_ts <= b.ts && b.ts <= trade.exit_ts`, the same inclusive bound as
    // `vike_data::TsRange::of`, so a wider slice changes no number (`edge_ratio` in that module
    // already passes the whole `bars` for every trade). A per-trade range is transport only.
    //
    // Why: `docs/decisions/0084-only-the-datahub-touches-the-store.md` routes readers through the
    // datahub, and every `load_bars` on a `RemoteHistStore` is a network round trip, so one read
    // per trade would be one round trip per trade.
    //
    // Best-effort, per symbol: a failed load or an empty answer leaves EVERY trade of that symbol
    // at `mae`/`mfe` = 0.0. Accepted: a store that cannot answer for a symbol could not have
    // answered any of its windows either.
    let mut wanted: BTreeMap<&str, (i64, i64)> = BTreeMap::new();
    for t in trades.iter() {
        let e = wanted.entry(t.symbol.as_str()).or_insert((t.entry_ts, t.exit_ts));
        e.0 = e.0.min(t.entry_ts);
        e.1 = e.1.max(t.exit_ts);
    }
    let bars_by_symbol: BTreeMap<String, Vec<Bar>> = wanted
        .into_iter()
        .filter_map(|(symbol, (lo, hi))| {
            let bars = store.load_bars(venue, symbol, interval, TsRange::of(lo, hi)).ok()?;
            (!bars.is_empty()).then(|| (symbol.to_string(), bars))
        })
        .collect();

    for t in trades.iter_mut() {
        // Immutable borrow of `*t` for the compute, then write back — no borrow conflict since the
        // returned tuple is owned.
        if let Some(bars) = bars_by_symbol.get(t.symbol.as_str()) {
            let (mae, mfe) = vike_analytics::excursions::mae_mfe(t, bars, None);
            t.mae = mae;
            t.mfe = mfe;
        }
    }
}

#[path = "excursions_tests.rs"]
#[cfg(test)]
mod excursions_tests;
