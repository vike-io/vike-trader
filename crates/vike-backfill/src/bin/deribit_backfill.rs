//! Runnable Deribit klines backfill into the DataFusion hist store.
//!
//! Usage:
//!   deribit_backfill <ROOT_DIR> <SYMBOL> <INTERVAL> <START_MS> <END_MS>
//!
//! Fetches `[START_MS, END_MS]` candles for SYMBOL at INTERVAL (e.g. BTC-PERPETUAL 1m) from
//! Deribit's public `get_tradingview_chart_data` endpoint and appends the OHLCV bars into the hist
//! store rooted at ROOT_DIR. Idempotent: re-running the same window is a no-op (0 rows). Deribit
//! candles are already bars, so — unlike the dukascopy backfill — there is no resample step. Reads
//! are KEYLESS (public mainnet JSON-RPC): no credentials, no `.env`.
//!
//! SYMBOL is a Deribit instrument NAME, passed through verbatim (`BTC-PERPETUAL`, `ETH-PERPETUAL`,
//! a dated future, …) — there is no `.P` perp suffix on this venue, because the name is already
//! unambiguous. See `vike_backfill::deribit`.
//!
//! ⚠ The endpoint is END-ANCHORED: an over-wide window silently returns only its newest page with
//! `status: "ok"`. `vike_deribit::data::fetch_klines_range` pages BACKWARD to cover the whole
//! request, so a wide `[START_MS, END_MS]` is safe here — but a 24-month 1m pull is a LOT of
//! requests and a lot of log: set `VIKE_LOG_FILE_LEVEL=warn` for it (the file layer is `trace` by
//! default and does not listen to `RUST_LOG`).
//!
//! The arg-parse/open-store/report CLI body is shared across the crypto backfill bins (see
//! `vike_backfill::run_klines_backfill_cli`); this file just binds it to Deribit.
//!
//! This bin PERSISTS its measured pace, at `<project>/settings/state/pace.json`
//! (`$VIKE_PACE_BOOK` overrides; `<ROOT_DIR>/pace.json` for a binary with no project above its
//! working directory),
//! under the single key `(deribit, klines)` — one row for the venue, since every instrument name
//! pages the same chart-data endpoint out of the same credit pool
//! (`vike_backfill::deribit::kline_market`).
//!
//! ⚠ **The record cannot make this backfill faster, and is not meant to.** Deribit publishes no
//! request-weight budget, so the pager's `next_delay()` is the bridge's hardcoded `PAGE_DELAY`
//! whatever is stored — see `vike_backfill::deribit::backfill_deribit_klines_paced`. What the file
//! buys is the ETA line from page zero (on exactly the "LOT of requests" run warned about above,
//! which used to print nothing at all) and an operator-readable answer to "what does a page here
//! cost?". The file is a CACHE: deleting it costs this run's opening ETA and nothing else.

use std::process::ExitCode;

use vike_backfill::deribit::{backfill_deribit_klines_paced, kline_market};
use vike_backfill::run_klines_backfill_cli;

fn main() -> ExitCode {
    run_klines_backfill_cli(
        "deribit",
        Some(kline_market),
        &std::env::vars().collect(),
        backfill_deribit_klines_paced,
    )
}
