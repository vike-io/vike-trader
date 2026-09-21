//! vike-backfill — the fetch→ingest glue for historical market data (hist-store migration, slice 6).
//!
//! THE CONTRACT: venue history *fetchers* live in the per-venue bridge crates (`vike-dukascopy`,
//! `vike-binance`, `vike-bybit`, `vike-okx`) — the same crates that also depend on `vike-data`
//! directly to implement the live `DataClient` seam ("venues stays data-free" was dropped as a
//! layering rule in crate-reorg spec D2; bridge crates depending on vike-data is expected). What
//! vike-backfill adds on top is the *historical backfill* path: one-shot paged REST/`.bi5` pulls
//! plus ingest into the `vike-data` hist store (with commit-window bookkeeping), kept as its own
//! crate so that machinery doesn't bloat every bridge crate's live-feed code. vike-backfill depends
//! on the bridge crates + vike-data (with the `hist-datafusion` backend) + vike-model. Two crates
//! depend back on IT, both through optional features: vike-app (its default `fat` feature, the
//! GUI's bulk Backfill action) and vike-datahub (`backfill-serve`, backfill-on-demand) — both
//! spell the crate-root `backfill_{binance,bybit,okx}_klines` re-exports below.
//!
//! Ported source: the Dukascopy fetch half is `vike_dukascopy` (a port of the Python
//! `dukascopy_source`; moved into its own bridge crate in crate-reorg Phase 3, PR J); the
//! ingest/derive half is the `vike_data::HistStore` seam
//! (`docs/superpowers/specs/2026-07-05-tickstore-datafusion-spec.md`). The glue itself is new
//! plumbing for the DataFusion hist store — there is no single Python twin for it.
//!
//! [`dukascopy`] is the first collector: keyless `.bi5` tick history → `QuoteTick`s → the store,
//! plus a quote→bar resample. [`binance`], [`bybit`], [`okx`], [`aster`] and [`deribit`] are the
//! crypto twins: paged REST klines → OHLCV `Bar`s → the store (no resample — klines are already
//! bars), each keying its own `{venue}:{symbol}:{interval}:{start}-{end}` commit window. The crypto
//! collectors share their fetch→ingest orchestration, commit-key format, and CLI body in the
//! internal `klines` module — each is a thin venue binding (its own `VENUE` const + bridge crate's
//! `fetch_klines_range`). More venue collectors slot in beside them as the store grows; all share
//! the one [`CollectError`].
//!
//! Every collector above is ONE-SHOT (a bin an operator runs). [`supervisor`] is the long-running
//! counterpart: a declared roster of series, a cadence per source, a per-pass gap-heal driven by
//! `vike_data::DataFusionHist::series_gaps`, and a JSON status surface — dispatching the SAME
//! `backfill_*` fns, never a second implementation. It is opt-in and additive: nothing here calls
//! into it, so with no supervisor running this crate behaves exactly as before.

// ⚠ `archive_store` LEFT this crate on 2026-09-20 — it is `vike_data::archive_store` now, and
// `VENUE`/`select_row_groups` went down with it from `vike_archive` below. A read-in-place
// `HistStore` over downloaded `data.vike.io` Parquet is STORAGE, and storage ranks with the store:
// held here, above the venue bridges this crate drives, it sat above the engine that wanted to read
// it, which is why `poly_ch_backtest` existed as a "leaf binary". The DOWNLOADER stays here, where
// fetching belongs, and imports the two read-side helpers downward.
// Shared Arrow/Parquet-decode helpers (typed column accessors + row-group reader skeleton) — see
// `arrowutil.rs`; the Arrow twin of `csvutil`, consumed by the pmxt + archive ingest paths.
mod arrowutil;
// Aster klines backfill — the crypto twin of `binance`, gated the same way. Its own module (rather
// than a `binance` arm) because Aster's `fetch_klines_range` takes a 5th `Environment` param the
// shared 4-arg fetcher shape has no slot for; the seam closes over `Environment::Live`.
#[cfg(feature = "venue-backfill")]
pub mod aster;
#[cfg(feature = "venue-backfill")]
pub mod binance;
#[cfg(feature = "venue-backfill")]
pub mod bybit;
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
// `crates/vike-data/src/backtest_store.rs`'s module doc — the file an operator asking
// "where does a Polymarket backtest read from" lands in.
pub mod caps;
#[cfg(any(feature = "databento", feature = "tardis"))]
mod csvutil;
#[cfg(feature = "databento")]
pub mod databento;
// Deribit candle backfill — the crypto twin of `binance`, gated the same way. Its own module (rather
// than a `binance` arm) for the same reason every venue has one: it names its own bridge crate's
// `fetch_klines_range`. Deribit's is a plain 4-arg fetcher like binance's (no closure needed) — the
// venue's columnar body and BACKWARD paging (the endpoint is end-anchored) are absorbed inside
// `vike-deribit`, and the symbol (`BTC-PERPETUAL`) passes through verbatim: no `.P` suffix exists on
// this venue.
#[cfg(feature = "venue-backfill")]
pub mod deribit;
#[cfg(feature = "venue-backfill")]
pub mod dukascopy;
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
// (already unconditional in this crate) plus `vike-bridge-core`'s dotenv loader (the bin, not this
// module, reads it), both already pulled by that feature for the sibling archive module.
#[cfg(feature = "vike-archive")]
pub mod events_api;
pub mod exec_import;
// Market perpetual-futures funding-RATE history backfill (venue REST → kind=bar / Bar.funding): the
// data enabler for a funding-capture backtest's `funding_source` seam. Binance + Hyperliquid, each a
// pure fixture-tested parser + a paged fetch. Distinct from `hyperliquid::backfill_hyperliquid_funding`
// (the ACCOUNT realized-funding `kind=funding` series). ⚠ ONE fetch now fills TWO series: the
// rate bars above, plus Hyperliquid's per-interval `premium` as `kind=perp_metrics` — a value this
// collector parsed and discarded until it had a home. See `funding_rate.rs`.
#[cfg(feature = "venue-backfill")]
pub mod funding_rate;
// Shared blocking HTTP GET helpers (string/bytes/stream-to-file) — see `http.rs`; consumed by the
// eod/databento/tardis/pmxt collectors. `pub` so unused-in-a-given-feature-set helpers (e.g.
// `get_to_bytes` in a build without `tardis`) are public API rather than dead code under -D warnings.
pub mod http;
#[cfg(feature = "venue-backfill")]
pub mod hyperliquid;
#[cfg(feature = "ibkr")]
pub mod ibkr;
// THE ONE kline registry (`docs/decisions/0059-…`'s Phase 3): the `KlineSource` trait, the
// `KLINE_SOURCES` static every dispatch path folds, and the one store-writing ingest. It replaced
// `supervisor/registry.rs`'s `COLLECTORS` and the six hand-written closures in
// `crates/vike-datahub/src/backfill.rs`; see its module doc for which of 0059's four rosters
// collapsed here and which two deliberately did not.
#[cfg(feature = "venue-backfill")]
pub mod kline_source;
#[cfg(feature = "venue-backfill")]
mod klines;
#[cfg(feature = "venue-backfill")]
pub mod okx;
// Load/save the MEASURED pace (`vike_model::rate_limits::PaceBook`) so run N+1 starts from run N's
// measurement instead of a hardcoded constant — see `pace_book.rs`'s module doc. UNGATED, like
// `cli`: it needs only vike-model + serde_json, both unconditional here, and it is the bin-side I/O
// half of a type the pure crate deliberately cannot persist for itself.
pub mod pace_book;
pub mod pmxt;
// ⚠ `poly_backtest_store` STOOD HERE and LEFT on 2026-09-20 — it is `vike_data::backtest_store`
// now, renamed because the `poly` in it was never true of the code: it selects between a
// `DataFusionHist` root and an archive reader, and since that day both of those live in vike-data.
// Holding the SELECTOR here is what stopped the engine choosing its own store, which is why the two
// bins it served lived here too. Both have gone: `poly_ch_backtest` with its cause (the engine
// takes `--archive` itself), `poly_mm_batch` to the `vike-poly-research` crate.
//
// The third flag it once carried, `--clickhouse`, reached the recorder's database through
// `backtest_bridge` and was DELETED on 2026-09-20 under the rule that data is fetched by API or
// from the venue directly. `crates/vike-data/src/backtest_store.rs`'s module doc is where the two
// capabilities that deletion cost are recorded.
// The pure planner behind the `migrate_to_group` bin: decides which stray per-symbol series can
// fold into a family's grouped series, and REFUSES when the target is ambiguous.
pub mod regroup;
#[cfg(feature = "poly-reparse")]
pub mod reparse;
// The ALWAYS-ON collector supervisor (a declarative TOML roster + cadence scheduling + per-pass
// gap-heal + a JSON status surface), driving the EXISTING `backfill_*` fns through a registry —
// see `supervisor/mod.rs`. Purely additive and entirely opt-in: nothing else in this crate calls
// into it, and an empty/absent config makes it a no-op. Consumed by the `collector_supervisor` bin.
#[cfg(feature = "venue-backfill")]
pub mod supervisor;
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

// --- the venue-direct collectors (feature `venue-backfill`) --------------------------------------
// Each of these names a bridge crate; a default build has neither the module nor the dep. The
// feature's rationale is in Cargo.toml — briefly: EVERY bin links the whole lib, so while these five
// bridges were non-optional all 22 bins compiled them, including the 14 that read only archives and
// vendor CSV.
//
// Only the supervisor-shape kline trio is re-exported at the crate root: vike-app's bulk Backfill
// action (its default `fat` feature) and vike-datahub's backfill-on-demand (`backfill-serve`) both
// spell `vike_backfill::backfill_{binance,bybit,okx}_klines`. Everything else — the `_paced`
// variants, `klines_commit_key`, the `VENUE` consts, the dukascopy/hyperliquid/funding-rate
// entrypoints — is reached via its module path, which is what every bin and test already spells;
// the crate-root copies of those names resolved for nobody and were removed (alias audit).
#[cfg(feature = "venue-backfill")]
pub use binance::backfill_binance_klines;
#[cfg(feature = "venue-backfill")]
pub use bybit::backfill_bybit_klines;
#[cfg(feature = "venue-backfill")]
pub use okx::backfill_okx_klines;
// Shared CLI body for the `<venue>_backfill` bins (`crate::klines` itself stays private —
// only this entry point needs to cross the lib→bin crate boundary).
#[cfg(feature = "venue-backfill")]
pub use klines::run_klines_backfill_cli;
