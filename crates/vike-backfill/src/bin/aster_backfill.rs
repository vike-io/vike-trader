//! Runnable Aster klines backfill into the DataFusion hist store.
//!
//! Usage:
//!   aster_backfill <ROOT_DIR> <SYMBOL> <INTERVAL> <START_MS> <END_MS>
//!
//! Fetches `[START_MS, END_MS]` klines for SYMBOL at INTERVAL (e.g. BTCUSDT 1m) from Aster's public
//! REST endpoint (paging the 1000-rows/request cap) and appends the OHLCV bars into the hist store
//! rooted at ROOT_DIR. Idempotent: re-running the same window is a no-op (0 rows). Aster klines are
//! already bars, so — unlike the dukascopy backfill — there is no resample step. Reads are KEYLESS
//! (public mainnet REST): no credentials, no `.env`.
//!
//! A SYMBOL carrying the `.P` perp suffix (e.g. `BTCUSDT.P`) fetches the USDⓈ-M perpetual and stores
//! it under that suffixed key; a bare symbol fetches spot. See `vike_backfill::aster`.
//!
//! The arg-parse/open-store/report CLI body is shared across the crypto backfill bins (see
//! `vike_backfill::run_klines_backfill_cli`); this file just binds it to Aster.
//!
//! The `None` in that call is the persisted-pace opt-out, and Aster is now the ONLY bin of the five
//! that takes it. Its pager DOES discover a budget now: the spec's `exchange_info_url` is a `Cow`,
//! so `vike_aster::data` composes the `exchangeInfo` URL of whichever host `(env, perp)` resolves.
//! It used to name `None` because the field was a `&'static str` and aster has four hosts.
//!
//! ⚠ The remaining blocker is plumbing, and it always was: `vike_aster::data` exposes no
//! `fetch_klines_range_paced` to carry a `PaceSample` in and out, because it binds the shared
//! `family::klines` fetch with an extra `Environment` argument. Adding one is the follow-up — and
//! it is worth more than it was, since a persisted seed can only steer INSIDE a discovered budget.
//! When it lands, `.P`-suffixed symbols also need the spot/perp keying binance uses: Aster mirrors
//! binance's two-host split, and MEASURED 2026-08-05 the two hosts publish genuinely different
//! budgets (fapi 2400/min, sapi 6000/min). That same call is also where
//! `fetch_klines_range_lanes` would become reachable for aster — nothing in this crate can ask for
//! lanes on this venue today, discovered budget or not.

use std::process::ExitCode;

use vike_backfill::aster::backfill_aster_klines;
use vike_backfill::run_klines_backfill_cli;

fn main() -> ExitCode {
    run_klines_backfill_cli(
        "aster",
        None,
        &std::env::vars().collect(),
        |hist, symbol, interval, start_ms, end_ms, _pace| {
            backfill_aster_klines(hist, symbol, interval, start_ms, end_ms)
        },
    )
}
