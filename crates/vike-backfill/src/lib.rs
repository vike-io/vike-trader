//! vike-backfill — the venue-free ingest glue for historical market data, plus the vendor
//! collectors that have no bridge crate to live in (hist-store migration, slice 6).
//!
//! THE CONTRACT: venue history *fetchers* live in the per-venue bridge crates (`vike-dukascopy`,
//! `vike-binance`, `vike-bybit`, `vike-okx`, `vike-aster`, `vike-deribit`, `vike-hyperliquid`) — the
//! same crates that also depend on `vike-data` directly to implement the live `DataClient` seam
//! ("venues stays data-free" was dropped as a layering rule in crate-reorg spec D2; bridge crates
//! depending on vike-data is expected) — and the ONE dispatch registry that names them,
//! `KLINE_SOURCES`, lives in `crates/vike-datahub/src/backfill.rs`, the process that actually
//! dispatches a venue string to a collector (docs/decisions/0094). What vike-backfill adds
//! is the *ingest* half those bridges share (commit-window bookkeeping, the forming-bar refusal;
//! `klines::ingest_klines` / `kline_source::backfill_kline_source`), Dukascopy's own store-side
//! ingest (it alone still owns real store code, see [`venues::dukascopy`]'s module doc), the market
//! funding-rate ingest (`funding_rate::backfill_funding_rate`, dispatched the same way by the
//! datahub's `FUNDING_SOURCES`) — and the VENDOR collectors, which fetch *and* ingest in one place
//! because no bridge crate exists for a vendor: EOD (Yahoo), pmxt, Databento, Tardis, and
//! data.vike.io's cohort-metrics and Polymarket archive/events lanes. vike-backfill names no bridge
//! crate at all, not even optionally, and depends on vike-data (with the `hist-datafusion` backend)
//! and vike-model. One crate depends back on IT, through an optional feature: vike-datahub
//! (`backfill-serve`, backfill-on-demand), for `kline_source::backfill_kline_source`. (vike-app's
//! bulk Backfill edge went with the GUI's local core, and the crate-root
//! `backfill_{binance,bybit,okx}_klines` re-exports went with the one-shot kline programs,
//! docs/decisions/0094.)
//!
//! Ported source: the Dukascopy fetch half is `vike_dukascopy` (a port of the Python
//! `dukascopy_source`; moved into its own bridge crate in crate-reorg Phase 3, PR J); the
//! ingest/derive half is the `vike_data::HistStore` seam
//! (`docs/superpowers/specs/2026-07-05-tickstore-datafusion-spec.md`). The glue itself is new
//! plumbing for the DataFusion hist store — there is no single Python twin for it.
//!
//! [`venues::dukascopy`] is the first collector: keyless `.bi5` tick history, fetched by
//! `vike_dukascopy::fetch_quotes_range` since docs/decisions/0094, → `QuoteTick`s → the
//! store, plus a quote→bar resample — the one collector still local to this crate: unlike the six
//! kline venues below, it owns real store code of its own (`ingest_quotes`'s `append_quotes` plus
//! the resample), so even with its fetch gone to the bridge too it still has something to adapt.
//! binance/bybit/okx/aster/deribit/hyperliquid are the crypto twins: paged REST klines → OHLCV
//! `Bar`s → the store (no resample — klines are already bars), each keying its own
//! `{venue}:{symbol}:{interval}:{start}-{end}` commit window through this crate's shared `klines`
//! module. Each of the six is a thin `KlineSource` impl living in its own bridge crate
//! (docs/decisions/0094) — a binding over that bridge's own
//! `fetch_klines_range`/`fetch_candles_range` — dispatched by `vike_datahub::backfill::KLINE_SOURCES`
//! rather than a static of this crate's own. All share the one [`CollectError`].

#![warn(unreachable_pub)]

// ⚠ `archive_store` LEFT this crate on 2026-09-20 — it is `vike_data::store::archive_store` now, and
// `VENUE`/`select_row_groups` went down with it from `vike_archive` below. A read-in-place
// `HistStore` over downloaded `data.vike.io` Parquet is STORAGE, and storage ranks with the store:
// held here, above the venue bridges this crate drives, it sat above the engine that wanted to read
// it, which is why `poly_ch_backtest` existed as a "leaf binary". The DOWNLOADER stays here, where
// fetching belongs, and imports the two read-side helpers downward.
// Shared Arrow/Parquet-decode helpers (typed column accessors + row-group reader skeleton) — see
// `arrowutil.rs`; the Arrow twin of `csvutil`, consumed by the pmxt + archive ingest paths.
mod arrowutil;
// Shared backfill-CLI harness (store_root/arg/--symbols) — see `cli.rs`; consumed by the bins.
pub mod cli;
// Which SOURCE can serve which gap, per venue and per kind — the backfill planner's vocabulary.
// See `caps.rs`.
//
// ⚠ **No module under this crate reaches ClickHouse any more, and this is where the last three
// went.** `clickhouse_poly` (the Polymarket export→decode→append collector) and its sibling
// `clickhouse_spot` both stood here; the read-side `backtest_bridge`
// (`ClickHousePolyHistStore`, a `vike_data::HistStore` over `polymarket.book_events`/`l1_quotes`)
// stood between `aster` and `binance`. All three are DELETED under the owner's rule: data is
// fetched by API or from the venue directly, never by reaching ClickHouse. What replaced the
// LANES, and the two capabilities nothing replaced, are argued at
// `crates/vike-data/src/store/backtest_store.rs`'s module doc — the file an operator asking
// "where does a Polymarket backtest read from" lands in.
pub mod caps;
#[cfg(any(feature = "databento", feature = "tardis"))]
mod csvutil;
#[cfg(feature = "databento")]
pub mod databento;
pub mod eod;
// Gap-targeted fill PLANNING: turn a coverage report into "which days, which kinds, from a source
// that can actually serve them". The piece that stops duplicates being CREATED (fetch only what is
// missing) rather than resolved after the fact. See `gapfill.rs`.
mod error;
pub mod gapfill;
// data.vike.io LIVE events API ingest (Polymarket L2 book/trade/status rows, JSON, paged, one
// token at a time) — the low-latency counterpart to `vike_archive`'s bulk Parquet path for a
// customer with no ClickHouse and no economical multi-GB day-file download. See `events_api.rs`'s
// module doc. Behind the EXISTING `vike-archive` feature (no new feature) — it needs only `ureq`
// (already unconditional in this crate) plus `vike-bridge-core`'s credential-store loader (the
// bin, not this module, reads it), both already pulled by that feature for the sibling archive
// module.
#[cfg(feature = "vike-archive")]
pub mod events_api;
pub mod exec_import;
// Market perpetual-futures funding-RATE history INGEST (kind=bar / Bar.funding under the reserved
// `interval=funding` label): the data enabler for a funding-capture backtest's `funding_source`
// seam. It names no venue — the fetch is a `vike_data::source::FundingRateSource` from a bridge
// crate, and the datahub's `FUNDING_SOURCES` picks which (docs/decisions/0094). Distinct from the
// ACCOUNT realized-funding `kind=exec_funding` series (presently produced by nothing in this crate).
// ⚠ ONE fetch fills TWO series: the rate bars above, plus a source's per-interval `premium` (only
// Hyperliquid sends one) as `kind=perp_metrics`. See `funding_rate.rs`.
pub mod funding_rate;
// Shared blocking HTTP GET helpers (string/bytes/stream-to-file) — see `http.rs`; consumed by the
// eod/databento/tardis/pmxt collectors. `pub` so unused-in-a-given-feature-set helpers (e.g.
// `get_to_bytes` in a build without `tardis`) are public API rather than dead code under -D warnings.
pub mod http;
// One historical collector local to this crate (dukascopy); ungated since
// docs/decisions/0094 moved its fetch into the bridge.
pub mod venues;
// The store-writing kline ingest (`backfill_kline_source`, over `vike_data::source::KlineSource`
// impls). ⚠ Until docs/decisions/0094 this module ALSO held `KLINE_SOURCES`, the ONE
// dispatch registry every kline path folded — it replaced `supervisor/registry.rs`'s `COLLECTORS`
// and the six hand-written closures in `crates/vike-datahub/src/backfill.rs`.
// docs/decisions/0094 moved that static up into the datahub itself; see this module's own doc
// for where and why.
pub mod kline_source;
mod klines;
pub mod pmxt;
// ⚠ `poly_backtest_store` STOOD HERE and LEFT on 2026-09-20 — it is `vike_data::store::backtest_store`
// now, renamed because the `poly` in it was never true of the code: it selects between a
// `DataFusionHist` root and an archive reader, and since that day both of those live in vike-data.
// Holding the SELECTOR here is what stopped the engine choosing its own store, which is why the two
// bins it served lived here too. Both have gone: `poly_ch_backtest` with its cause (the engine
// takes `--archive` itself), `poly_mm_batch` to the `vike-poly-research` crate.
//
// The third flag it once carried, `--clickhouse`, reached the recorder's database through
// `backtest_bridge` and was DELETED on 2026-09-20 under the rule that data is fetched by API or
// from the venue directly. `crates/vike-data/src/store/backtest_store.rs`'s module doc is where the two
// capabilities that deletion cost are recorded.
#[cfg(feature = "tardis")]
pub mod tardis;
// data.vike.io COHORT METRICS (`kind=cohort`) — the graded positioning panel, paged by cursor. Its
// own directory behind its own `vikedata` feature, in the mod/client/parse/ingest shape every other
// vendor here uses; `vikedata/mod.rs` carries the argument for why that shape is the point. Distinct
// from the two `vike-archive` files below, which are the same VENDOR's Polymarket L2 lanes.
#[cfg(feature = "vikedata")]
pub mod vikedata;
// data.vike.io archive backfill (Polymarket L2 book_events/trades/l1_quotes) — a ranged-HTTP
// `ChunkReader` that row-group-prunes by token_id instead of downloading whole date-partitioned
// files. See `vike_archive.rs`'s module doc. Behind its own `vike-archive` feature (needs
// `dep:bytes` + `dep:vike-bridge-core`), so a default build never compiles it.
#[cfg(feature = "vike-archive")]
pub mod vike_archive;
// WHICH CLOCK a Polymarket backtest window is scoped on — `ts` (the venue's own as-of stamp) or
// `local_ts` (the recorder's socket read), the two stamps every recorded row carries — plus the
// counter the archive store reports the difference with. ⚠ UNGATED because it was the authority for
// a divergence between `backtest_bridge` (ungated) and the selector (gated); that store is deleted
// and every surviving store scopes on `ts`, so what this module now describes is a property of the
// TAPE rather than a disagreement between two stores. See its module doc.
// ⚠ `window_clock` LEFT this crate on 2026-09-20, with `archive_store` and for the same reason: it
// is `vike_data::window_clock` now. It depends on nothing but `TsRange`, so nothing kept it here
// except the module that used it.

pub use error::CollectError;
