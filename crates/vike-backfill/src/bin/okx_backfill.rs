//! Runnable OKX V5 klines backfill into the DataFusion hist store.
//!
//! Usage:
//!   okx_backfill <ROOT_DIR> <SYMBOL> <INTERVAL> <START_MS> <END_MS>
//!
//! Fetches `[START_MS, END_MS]` candles for SYMBOL at INTERVAL (e.g. BTC-USDT 1m) from OKX's public
//! `history-candles` REST endpoint (paging the 100-rows/request cap BACKWARD via the `after` cursor;
//! OKX serves newest-first, reversed to ascending by the fetcher) and appends the OHLCV bars into the
//! hist store rooted at ROOT_DIR. Idempotent: re-running the same window is a no-op (0 rows).
//!
//! The arg-parse/open-store/report CLI body is shared across the crypto backfill bins (see
//! `vike_backfill::run_klines_backfill_cli`); this file just binds it to OKX.
//!
//! This bin PERSISTS its measured pace, at `<project>/settings/state/pace.json`
//! (`$VIKE_PACE_BOOK` overrides; `<ROOT_DIR>/pace.json` for a binary with no project above its
//! working directory),
//! under the single key `(okx, klines)` — one row for the venue, since every `instId` pages the same
//! `history-candles` endpoint under one IP limit (`vike_backfill::okx::kline_market`).
//!
//! ⚠ **The record cannot make this backfill faster, and is not meant to.** OKX publishes no
//! request-weight budget, so the pager's `next_delay()` is the bridge's hardcoded `PAGE_DELAY`
//! whatever is stored — see `vike_backfill::okx::backfill_okx_klines_paced`. What the file buys is
//! the ETA line from page zero, and an operator-readable answer to "what does a page here cost?" —
//! MEASURED on the CI box 2026-08-04 at **476–486 ms**, against a 200 ms delay nobody had ever checked.
//! The file is a CACHE: deleting it costs this run's opening ETA and nothing else.

use std::process::ExitCode;

use vike_backfill::okx::{backfill_okx_klines_paced, kline_market};
use vike_backfill::run_klines_backfill_cli;

fn main() -> ExitCode {
    run_klines_backfill_cli(
        "okx",
        Some(kline_market),
        &std::env::vars().collect(),
        backfill_okx_klines_paced,
    )
}
