//! Runnable Bybit V5 spot klines backfill into the DataFusion hist store.
//!
//! Usage:
//!   bybit_backfill <ROOT_DIR> <SYMBOL> <INTERVAL> <START_MS> <END_MS>
//!
//! Fetches `[START_MS, END_MS]` klines for SYMBOL at INTERVAL (e.g. BTCUSDT 1m) from Bybit's public
//! REST endpoint (paging the 1000-rows/request cap; Bybit serves newest-first, reversed to ascending
//! by the fetcher) and appends the OHLCV bars into the hist store rooted at ROOT_DIR. Idempotent:
//! re-running the same window is a no-op (0 rows).
//!
//! The arg-parse/open-store/report CLI body is shared across the crypto backfill bins (see
//! `vike_backfill::run_klines_backfill_cli`); this file just binds it to Bybit.
//!
//! This bin PERSISTS its measured pace, at `<project>/settings/state/pace.json`
//! (`$VIKE_PACE_BOOK` overrides; `<ROOT_DIR>/pace.json` for a binary with no project above its
//! working directory),
//! under the single key `(bybit, klines)` — one row for the venue, since V5 serves spot and linear
//! from ONE host under one IP limit (`vike_backfill::bybit::kline_market`).
//!
//! ⚠ **The record cannot make this backfill faster, and is not meant to.** Bybit publishes no
//! request-weight budget, so the pager's `next_delay()` is the bridge's hardcoded `PAGE_DELAY`
//! whatever is stored — see `vike_backfill::bybit::backfill_bybit_klines_paced`. What the file buys
//! is the ETA line from page zero, and an operator-readable answer to "what does a page here cost?"
//! — which nobody had ever measured. The file is a CACHE: deleting it costs this run's opening ETA
//! and nothing else.

use std::process::ExitCode;

use vike_backfill::bybit::{backfill_bybit_klines_paced, kline_market};
use vike_backfill::run_klines_backfill_cli;

fn main() -> ExitCode {
    run_klines_backfill_cli(
        "bybit",
        Some(kline_market),
        &std::env::vars().collect(),
        backfill_bybit_klines_paced,
    )
}
