//! One historical collector local to this crate: dukascopy's store-side ingest + resample.
//! `venue-backfill` gated this directory while it also held the fetch; docs/decisions/0094
//! moved the fetch into the bridge (`vike_dukascopy::fetch_quotes_range`), so what remains here names
//! no bridge crate and needs no feature gate.
//!
//! ⚠ **This directory held SEVEN modules until docs/decisions/0094.** IBKR's own module —
//! a pure window planner, behind the `ibkr` feature — was the first to go, alongside the
//! `ibkr_backfill` bin that spent its keys, measured unused: no `venue=ibkr` series existed in any
//! store. `vike_ibkr::HistoricalFetcher` stays in the bridge crate. docs/decisions/0094 then
//! moved the SIX kline collectors (`aster`/`binance`/`bybit`/`deribit`/`hyperliquid`/`okx`) out as
//! well — each was a
//! thin `KlineSource` impl over its own bridge crate's `fetch_klines_range`/`fetch_candles_range`
//! and named no store code of its own, so the impl moved to live beside the fetcher it wrapped
//! (`vike_binance::data::BinanceKlines` and five siblings; `vike_hyperliquid::history::
//! HyperliquidKlines`, which also carries the one-symbol dispatch refusal).
//! `vike_datahub::backfill::KLINE_SOURCES` registry names them there now (moved up out of this
//! crate in turn by docs/decisions/0094).
//!
//! `dukascopy` is what remains, and unlike those six it does not move out whole: it owns real store
//! code of its own (`ingest_quotes`'s `append_quotes`, plus the quote→bar resample), so even with its
//! fetch gone to the bridge too it still has something to adapt, and stays a module of this crate —
//! taking already-fetched quotes, or a fetch closure, as a parameter instead of naming
//! `vike_dukascopy` directly.

pub mod dukascopy;
