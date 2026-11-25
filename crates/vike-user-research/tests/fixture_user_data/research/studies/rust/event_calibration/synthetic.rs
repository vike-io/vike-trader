//! Synthetic event markets for the test binaries that run `event_calibration`
//! (`crates/vike-user-research/tests/pipeline.rs` and
//! `crates/vike-strategy-sandbox/tests/study_child.rs`). Declared under `cfg(test)`, so a study
//! built from this folder never compiles it.
//!
//! `count` markets end one day apart. Market `i`'s forecast and its outcome draw come from two
//! Weyl sequences (`frac(i·a)` for two irrational `a`), so the forecasts are spread over
//! `[0.05, 0.95)` with no order in time. The market is OVER-CONFIDENT: its event happens with
//! probability `0.5 + 0.6·(p − 0.5)`, not `p`.
//!
//! Each market's archive is the venue's (`crates/bridges/polymarket/src/catalog/rules.rs`'s
//! `archived_rows`): a rules row received at its close, half a minute before its end, and, at the
//! same instant, its outcome row. Every eleventh market has the rules row ALONE (closed, not yet
//! resolved), though its last trade is at 0.99: a final price is never read as a resolution.
//!
//! The store is [`TradeStore`], not the shared `vike_data::HistoryStoreMemory`: that double holds
//! no ticks (its trade read answers empty), and a market's prices are its trades.

use std::sync::Arc;

use vike_data::text_log::{POLYMARKET_RULES_ARCHIVE_COLLECTOR, TEXT_KIND_POLYMARKET_RULES};
use vike_data::{HistoryStore, HistoryStoreError, TextRow, TimestampRange};
use vike_marketdata::TickTrade;
use vike_marketdata::test_support::trade;

pub const VENUE: &str = "polymarket";
pub const DAY_MS: i64 = 86_400_000;
pub const FIRST_END_MS: i64 = 1_790_000_000_000;

/// A store that holds each token's trades on [`VENUE`] and the markets' rules archive, and nothing
/// else.
pub struct TradeStore {
    trades: Vec<(String, Vec<TickTrade>)>,
    /// The archive's rows, received-ascending.
    archive: Vec<TextRow>,
}

fn inside(range: TimestampRange, ts: i64) -> bool {
    range.start.is_none_or(|start| ts >= start) && range.end.is_none_or(|end| ts <= end)
}

impl HistoryStore for TradeStore {
    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        Ok(self
            .trades
            .iter()
            .filter(|(token, _)| venue == VENUE && token == symbol)
            .flat_map(|(_, trades)| trades.iter().filter(|t| inside(range, t.timestamp)).cloned())
            .collect())
    }

    fn scan_texts(
        &self,
        text_kind: &str,
        collector: &str,
        range: TimestampRange,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        let archive = (text_kind, collector)
            == (TEXT_KIND_POLYMARKET_RULES, POLYMARKET_RULES_ARCHIVE_COLLECTOR);
        Ok(self
            .archive
            .iter()
            .filter(|row| archive && inside(range, row.received_ms))
            .cloned()
            .collect())
    }

    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_quotes, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// The store holding `count` synthetic markets' trades and archive, and the recipe that lists them.
pub fn store_and_recipe(count: usize) -> (Arc<dyn HistoryStore + Send + Sync>, String) {
    let mut trades = Vec::with_capacity(count);
    let mut archive = Vec::with_capacity(2 * count);
    let mut recipe = format!("venue = \"{VENUE}\"\nhorizon_ms = {DAY_MS}\nbin_count = 5\n");
    for i in 0..count {
        let token = format!("token-{i}");
        let end_ms = FIRST_END_MS + i as i64 * DAY_MS;
        let forecast = 0.05 + 0.9 * (i as f64 * 0.754_877_666_246_693).fract();
        let truth = 0.5 + 0.6 * (forecast - 0.5);
        let happened = (i as f64 * 0.618_033_988_749_895).fract() < truth;
        let resolved = i % 11 != 10;
        let last = if happened || !resolved { 0.99 } else { 0.01 };
        trades.push((
            token.clone(),
            vec![
                trade(end_ms - DAY_MS - 60_000, forecast, 10.0, false),
                trade(end_ms - 60_000, last, 10.0, false),
            ],
        ));
        let closed_ms = end_ms - 30_000;
        let rules = TextRow {
            source_id: format!("condition-{i}"),
            subject: format!("{token} {token}-no"),
            tags: TextRow::joined_tags(&["Yes", "No"]),
            ..TextRow::new(TEXT_KIND_POLYMARKET_RULES, "Polymarket", closed_ms)
        };
        if resolved {
            let outcome = TextRow {
                actual: if happened { "Yes" } else { "No" }.to_string(),
                amends: rules.source_id.clone(),
                ..rules.clone()
            };
            archive.extend([rules, outcome]);
        } else {
            archive.push(rules);
        }
        recipe.push_str(&format!("[[market]]\ntoken = \"{token}\"\nend_ms = {end_ms}\n"));
    }
    (Arc::new(TradeStore { trades, archive }), recipe)
}
