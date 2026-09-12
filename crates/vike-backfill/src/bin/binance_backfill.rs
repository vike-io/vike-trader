//! Runnable Binance Spot klines backfill into the DataFusion hist store.
//!
//! Usage:
//!   binance_backfill <ROOT_DIR> <SYMBOL> <INTERVAL> <START_MS> <END_MS>
//!
//! Fetches `[START_MS, END_MS]` klines for SYMBOL at INTERVAL (e.g. BTCUSDT 1m) from Binance's public
//! REST endpoint (paging the 1000-rows/request cap) and appends the OHLCV bars into the hist store
//! rooted at ROOT_DIR. Idempotent: re-running the same window is a no-op (0 rows). Binance klines are
//! already bars, so — unlike the dukascopy backfill — there is no resample step.
//!
//! The arg-parse/open-store/report CLI body is shared across the crypto backfill bins (see
//! `vike_backfill::run_klines_backfill_cli`); this file just binds it to Binance.
//!
//! Binance is the ONE bin of the family whose persisted pace actually STEERS, because its venue is
//! the only one of the five that publishes a `REQUEST_WEIGHT` budget for the pager to discover and
//! take a fraction of. bybit/okx/deribit persist a record too, but theirs reports a per-page cost and
//! an ETA while their pagers keep sleeping their hardcoded `PAGE_DELAY`; aster persists none at all.
//! The record lives at `<project>/settings/state/pace.json` (`$VIKE_PACE_BOOK` overrides;
//! `<ROOT_DIR>/pace.json` for a binary with no project above its working directory) and is keyed
//! `(binance, spot|perp)`: the two
//! hosts publish different budgets (6000 vs 2400 weight/min) and price `/klines` differently
//! (weight 2 vs 5), so they are separate rows and never share one. The file is a CACHE — deleting it
//! costs this run's opening pages and nothing else.

use std::process::ExitCode;

use vike_backfill::binance::{backfill_binance_klines_paced, kline_market};
use vike_backfill::run_klines_backfill_cli;

fn main() -> ExitCode {
    run_klines_backfill_cli(
        "binance",
        Some(kline_market),
        &std::env::vars().collect(),
        backfill_binance_klines_paced,
    )
}
