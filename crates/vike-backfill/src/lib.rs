//! vike-backfill — the venue-free ingest glue for historical market data, plus the vendor
//! collectors that have no bridge crate to live in (hist-store migration, slice 6).
//!
//! THE CONTRACT: venue history *fetchers* live in the per-venue bridge crates, and the ONE dispatch
//! registries that name them, `KLINE_SOURCES` and `FUNDING_SOURCES`, live in
//! `crates/vike-datahub/src/backfill.rs`, the process that dispatches a venue string to a collector
//! (docs/decisions/0094). This crate names no bridge crate at all, not even optionally, and depends
//! on vike-data, vike-store (the DataFusion backend) and vike-model. What it adds:
//!
//! - the *ingest* half the bridges share: commit-window bookkeeping and the forming-bar refusal
//!   (`klines::ingest_klines`, `kline_source::backfill_kline_source`);
//! - Dukascopy's own store-side ingest and quote→bar resample ([`venues::dukascopy`]): the one venue
//!   here with real store code of its own, because its fetch is a tick feed, not a one-symbol
//!   kline seam;
//! - the market funding-rate ingest (`funding_rate::backfill_funding_rate`), dispatched the same
//!   way by the datahub's `FUNDING_SOURCES`;
//! - the VENDOR collectors, which fetch *and* ingest in one place because no bridge exists for a
//!   vendor: EOD (Yahoo), pmxt, Databento, Tardis, and data.vike.io's cohort-metrics and
//!   Polymarket archive/events lanes.
//!
//! One crate depends back on this one, through an optional feature: vike-datahub (`backfill-serve`,
//! backfill-on-demand). Every collector shares the one [`CollectError`].

#![warn(unreachable_pub)]

// Shared Arrow/Parquet-decode helpers (typed column accessors + row-group reader skeleton) — see
// `arrowutil.rs`; the Arrow twin of `csvutil`, consumed by the pmxt + archive ingest paths.
mod arrowutil;
// Shared backfill-CLI harness (store_root/arg/--symbols) — see `cli.rs`; consumed by the bins.
pub mod cli;
// Which SOURCE can serve which gap, per venue and per kind — the backfill planner's vocabulary.
// See `caps.rs`.
//
// ⚠ No module under this crate reaches ClickHouse: data is fetched by API or from the venue
// directly. `crates/vike-store/src/backtest_store.rs`'s module doc records what that cost.
pub mod caps;
#[cfg(any(feature = "databento", feature = "tardis"))]
mod csvutil;
#[cfg(feature = "databento")]
pub mod databento;
pub mod eod;
mod error;
// Gap-targeted fill PLANNING: turn a coverage report into "which days, which kinds, from a source
// that can actually serve them", so duplicates are not CREATED. See `gapfill.rs`.
pub mod gapfill;
// data.vike.io LIVE events API ingest (Polymarket L2 book/trade/status rows, JSON, paged, one
// token at a time) — the low-latency counterpart to `vike_archive`'s bulk Parquet path. Behind the
// `vike-archive` feature; see `events_api.rs`'s module doc.
#[cfg(feature = "vike-archive")]
pub mod events_api;
pub mod exec_import;
// Market perpetual-futures funding-RATE history INGEST (kind=bar / Bar.funding under the reserved
// `interval=funding` label): the data enabler for a funding-capture backtest's `funding_source`
// seam. It names no venue — the fetch is a `vike_data::source::FundingRateSource` from a bridge
// crate. ⚠ ONE fetch fills TWO series: the rate bars, plus a source's per-interval `premium` (only
// Hyperliquid sends one) as `kind=perp_metrics`. See `funding_rate.rs`.
pub mod funding_rate;
// Shared blocking HTTP GET helpers (string/bytes/stream-to-file) — see `http.rs`. `pub` so helpers
// unused in a given feature set (e.g. `get_to_bytes` without `tardis`) are public API rather than
// dead code under -D warnings.
pub mod http;
// One historical collector local to this crate (dukascopy).
pub mod venues;
// The store-writing kline ingest (`backfill_kline_source`, over `vike_data::source::KlineSource`
// impls); the registry that dispatches them is the datahub's.
pub mod kline_source;
mod klines;
pub mod pmxt;
#[cfg(feature = "tardis")]
pub mod tardis;
// data.vike.io COHORT METRICS (`kind=cohort`) — the graded positioning panel, paged by cursor. Its
// own directory behind its own `vikedata` feature; `vikedata/mod.rs` carries the argument for the
// shape. Distinct from `vike_archive`, which is the same VENDOR's Polymarket L2 lane.
#[cfg(feature = "vikedata")]
pub mod vikedata;
// data.vike.io archive backfill (Polymarket L2 book_events/trades/l1_quotes) — a ranged-HTTP
// `ChunkReader` that row-group-prunes by token_id instead of downloading whole date-partitioned
// files. See `vike_archive.rs`'s module doc. Behind its own `vike-archive` feature (needs
// `dep:bytes` + `dep:rayon`), so a default build never compiles it.
#[cfg(feature = "vike-archive")]
pub mod vike_archive;

pub use error::CollectError;
