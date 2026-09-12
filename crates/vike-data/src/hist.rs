//! The unified historical-data seam: ONE store for bars AND ticks (design of record:
//! docs/superpowers/specs/2026-07-05-tickstore-datafusion-spec.md).
//!
//! This module is always compiled — it is just the trait + query types (vike-model only).
//! The concrete DataFusion+Parquet backend ([`crate::datafusion_hist::DataFusionHist`]) is
//! behind the `hist-datafusion` feature so default builds never pull the Arrow/DataFusion tree.
//!
//! Read half: `load_bars` + `scan_quotes`/`scan_trades`. Ingest: idempotent `append_*` (manifest
//! file-index). Derive: `resample_*_to_bars` feeds the parity-tested `vike_model::consolidate_*`
//! (bars-from-ticks, in-store). The tick scans read a bounded range into memory; the DataFusion
//! backend also exposes bounded-memory streaming twins (`DataFusionHist::scan_{trades,quotes}_stream`)
//! for ranges too large to `Vec`. With this the DataFusion+Parquet engine is the SINGLE historical
//! store — the SQLite bar store is retired.

use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

use crate::chain_log::ChainRow;
use crate::cohort_log::CohortRow;
use crate::exec_log::{ExecFillRow, ExecOrderRow};
use crate::funding_log::FundingRow;
use crate::perp_metrics_log::PerpMetricRow;
use crate::series::{SeriesCoverage, SeriesId};

/// Errors from the historical store. Feature-independent — DataFusion/Arrow/Parquet failures are
/// stringified so the always-compiled trait never depends on that tree.
#[derive(Debug)]
pub enum DataError {
    /// DataFusion / Arrow / Parquet query or decode failure.
    Query(String),
    /// Filesystem / manifest IO failure.
    Io(String),
}

impl std::fmt::Display for DataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataError::Query(e) => write!(f, "hist query: {e}"),
            DataError::Io(e) => write!(f, "hist io: {e}"),
        }
    }
}

impl std::error::Error for DataError {}

/// Inclusive epoch-ms range; `None` bound = unbounded on that side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TsRange {
    pub start: Option<i64>,
    pub end: Option<i64>,
}

impl TsRange {
    /// The whole series (no bounds).
    pub fn all() -> Self {
        Self::default()
    }
    /// `[start, end]` inclusive.
    pub fn of(start: i64, end: i64) -> Self {
        Self { start: Some(start), end: Some(end) }
    }
}

/// The one seam over all historical market data. Readers (backtests, bench, the resample
/// bridge) depend on this, never on which engine backs it.
pub trait HistStore {
    /// Derived OHLCV bars for `(venue, symbol, interval)` in `range`, ts-ascending. Bars are
    /// small (one row per bar) so a `Vec` is the right shape.
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<Vec<Bar>, DataError>;

    /// L1 quotes for `(venue, symbol)` in `range`, ts-ascending.
    ///
    /// NOTE: reads the whole range into memory (a `Vec`) — for bounded ranges / fixtures. For tick
    /// ranges too large to `Vec`, the DataFusion backend adds bounded-memory streaming twins
    /// (`DataFusionHist::scan_quotes_stream` / `scan_trades_stream`) that yield in constant memory.
    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError>;

    /// Executed trades for `(venue, symbol)` in `range`, ts-ascending. Same slice-1 caveat as
    /// [`HistStore::scan_quotes`].
    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError>;

    // ---- ingest (idempotent by commit key — NEVER by row value) ----------------------------
    //
    // `commit_key` identifies the SOURCE BATCH (e.g. a venue cursor / "venue:symbol:from-to").
    // If it was already ingested for this series the append is a NO-OP (returns 0) — the spec's
    // must-fix #1: trades carry no id, so idempotency lives at the batch level, never per-row
    // value dedup. `None` = always append (no guard). Each append records the sealed Parquet part
    // in the series manifest (the file index that also drives reads — no directory LIST).

    /// Append a batch of bars for `(venue, symbol, interval)`. Returns rows written (0 if skipped).
    fn append_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        bars: &[Bar],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError>;

    /// Append a batch of quotes for `(venue, symbol)`.
    fn append_quotes(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[QuoteTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError>;

    /// Append a batch of trades for `(venue, symbol)`.
    fn append_trades(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[TradeTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError>;

    /// Append recorded L2 book events for `(venue, symbol)` (`kind=book` series; one part
    /// row per level, regrouped on scan). Returns EVENTS written (0 if commit_key already
    /// ingested). Same batch-level idempotency contract as the other appends.
    fn append_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError>;

    /// Recorded L2 book events for `(venue, symbol)` in `range`, sorted by `(ts, seq)` and
    /// regrouped into [`BookUpdate`]s. Same in-memory `Vec` caveat as [`HistStore::scan_quotes`].
    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError>;

    /// Append recorded L2 DEPTH snapshots for `(venue, symbol)` — the `kind=depth` series.
    ///
    /// ## Why this is not `kind=book`
    ///
    /// Same row type, same codec, DIFFERENT series — and that separation is the whole point.
    /// `kind=book` is the LOSSLESS lane: every delta present, `seq` contiguous, gaps detectable (a
    /// contract `market_data_conformance` machine-checks). `kind=depth` is the CONFLATING lane: a
    /// venue like Binance serves `@depth20@100ms` — a full snapshot every 100 ms with **every
    /// intermediate book state discarded**.
    ///
    /// Both fold correctly: a snapshot is a full-state anchor, so `L2Book::apply_snapshot` rebuilds
    /// from it and no delta is lost — there were none to lose. What differs is what the data can
    /// SUPPORT. On a conflated series the book teleports rather than evolves, so queue-position
    /// modelling and maker-fill simulation are fiction. Writing it into `kind=book` would let a
    /// market-making backtest run on it silently and report fills it could never have got.
    ///
    /// **The path IS the disclosure.** A consumer asking for `book` never receives conflated data,
    /// and `coverage_report` lists `book` and `depth` separately — so a customer sees "book ✗,
    /// depth ✓" and knows exactly what they hold.
    ///
    /// Returns EVENTS written (0 if `commit_key` was already ingested), like
    /// [`append_book_updates`](HistStore::append_book_updates).
    ///
    /// **Defaults to REFUSING**, not to a silent no-op. Most `HistStore` impls in this workspace are
    /// read-only views over one source (an archive file, a ClickHouse table, an RPC seam) and have no
    /// depth to offer; a default `Ok(0)` would let a recorder write into them and report success
    /// while the rows went nowhere. Only [`crate::DataFusionHist`] overrides it.
    fn append_depth(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let (_, _, _, _) = (venue, symbol, updates, commit_key);
        Err(DataError::Query("append_depth: this store serves no depth lane".into()))
    }

    /// Recorded L2 depth snapshots for `(venue, symbol)` — the read half of
    /// [`append_depth`](HistStore::append_depth). Same `(ts, seq)` ordering and both-layouts scan as
    /// [`scan_book_updates`](HistStore::scan_book_updates).
    ///
    /// **Defaults to REFUSING**, symmetrically with its write twin. It used to default to
    /// `Ok(Vec::new())` on the argument that "a reader asking *is there depth here?* of a store
    /// that has none is answering correctly with no" — and that argument confuses two different
    /// facts. A store that HAS the lane and holds nothing in `range` answers `Ok(vec![])` from a
    /// real read; a store that does not implement the verb never looked, and returning the same
    /// empty on its behalf fabricates a confident "no data" out of a capability gap. Every caller
    /// then sees one value for both.
    ///
    /// This is the credential store's rule applied to a read verb; the authority for it is
    /// `crates/vike-secrets/src/dotenv.rs`'s `load_workspace_dotenv`: **an ABSENT thing is the
    /// ordinary state and is silent; a thing that is PRESENT and unusable is an ERROR.** Here the
    /// absent thing is the ROWS (an honest empty `Ok`) and the unusable thing is the VERB.
    ///
    /// Only [`crate::DataFusionHist`] answers it for real. Two shapes for everyone else: a store
    /// with no depth lane INHERITS this refusal (`crate::MemHistStore` does), while a store that
    /// FRONTS another one must not — a delegating wrapper FORWARDS to its inner store, and a seam
    /// that could serve the lane but does not carry it over the wire overrides with its own reason
    /// (`crates/vike-datahub-client/src/remote.rs`'s `RemoteHistStore`, which had to override this
    /// method while the default was empty precisely because the empty claimed the SERVER holds no
    /// depth).
    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        let (_, _, _) = (venue, symbol, range);
        Err(DataError::Query("scan_depth: this store serves no depth lane".into()))
    }

    /// Append observed point-in-time instrument properties for `(venue, symbol)`. Rows are
    /// `(observation_ts_ms, SymbolProperties)`. `commit_key` gives idempotency (recorders use a
    /// per-UTC-day key so at most one row lands per symbol per day). See the PIT properties design.
    fn append_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[(i64, SymbolProperties)],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError>;

    /// Observed properties for `(venue, symbol)` in `range`, ts-ascending.
    fn scan_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError>;

    /// Point-in-time lookup: the most-recent properties observed at or before `ts` (`None` if none).
    fn properties_as_of(
        &self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, DataError> {
        Ok(self
            .scan_symbol_properties(venue, symbol, TsRange::of(i64::MIN, ts))?
            .pop()
            .map(|(_, f)| f))
    }

    // ---- store inventory / metadata (kind-agnostic) -------------------------------------------
    //
    // The catalog verbs the Data-Manager / Studio data-browser render from, promoted onto the TRAIT
    // (they were concrete `DataFusionHist` methods) so a `&dyn HistStore` — notably the RPC-backed
    // `RemoteHistStore` — can answer them, not only the local DataFusion backend. The two
    // ENUMERATION verbs (`list_series`/`inventory`) REFUSE by default — a store that can enumerate
    // implements them consciously; their docs carry the argument. The two DERIVED views
    // (`series_gaps`/`coverage_report`) still default to an empty `Ok`, each stating its own
    // vacuous-truth argument on the method. Metadata is TINY — a series list and
    // a cheap per-series coverage folded straight from the manifest file-index, NO Parquet scan — so,
    // unlike the tick scans, it is always safe to ship whole over the wire (the compute-to-data rule
    // caps SLICES, not catalog metadata).

    /// Enumerate every stored series — the `(kind, venue, symbol, interval)` leaves — sorted.
    ///
    /// **Defaults to REFUSING.** It used to default to `Ok(Vec::new())`, documented as *"a store
    /// that cannot enumerate its inventory"* — the admission and the fabrication in one line: the
    /// default conceded the store CANNOT enumerate, then answered in the voice of a store that HAS
    /// nothing. A store that serves this verb and holds no series answers `Ok(vec![])` from a real
    /// fold; a store that does not serve it never looked, and an empty `Ok` on its behalf hands
    /// every caller — the Studio slice picker, the Data-Manager grid, `vike-datahub`'s catalog
    /// verbs — one value for both facts.
    ///
    /// Same rule and same authority as [`HistStore::scan_depth`]'s default:
    /// `crates/vike-secrets/src/dotenv.rs`'s `load_workspace_dotenv` — **an ABSENT thing is the
    /// ordinary state and is silent; a thing that is PRESENT and unusable is an ERROR.** The
    /// absent thing here is the SERIES (an honest empty listing from a store that looked); the
    /// unusable thing is the VERB.
    ///
    /// The refusal names the verb's absence and claims NOTHING about content — deliberately, so a
    /// leaf that holds data it cannot enumerate (the ClickHouse bridge, the flat Parquet archive
    /// store) inherits a true statement, unlike `scan_depth`'s "serves no depth lane" (which the
    /// RPC seam had to override as false THERE). Three shapes for implementors: a store that CAN
    /// enumerate implements the verb consciously ([`crate::DataFusionHist`]'s manifest walk, and
    /// `crate::MemHistStore`'s in-memory fold — which the old default silently SHADOWED: a seeded
    /// double answered "no series" while holding rows); a delegating wrapper FORWARDS to its inner
    /// store (poly_mm_batch's `CachedHistStore`), because "cannot enumerate" is false for a type
    /// whose whole job is fronting a store that can; and an RPC seam serves it over the wire
    /// (`crates/vike-datahub-client/src/remote.rs`'s `RemoteHistStore`).
    fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
        Err(DataError::Query("list_series: this store cannot enumerate its inventory".into()))
    }

    /// Every stored series paired with its cheap coverage (manifest fold — first/last ts, rows, bytes,
    /// parts, dates; NO scan).
    ///
    /// **Defaults to REFUSING**, for exactly [`HistStore::list_series`]'s reasons — it is the same
    /// enumeration wearing coverage, and the two defaults must agree or one call site could be
    /// refused the series list while a neighbour is handed a fabricated empty inventory of the
    /// same store. The empty-inventory answer belongs to a store that folded its real catalog and
    /// found nothing.
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
        Err(DataError::Query("inventory: this store cannot enumerate its inventory".into()))
    }

    /// The GAP ranges (inclusive epoch-ms) missing within `id`'s recorded day span — the Data-Manager
    /// "where's the hole" view, derived from the manifest `date=` index (NO scan). Default: no gaps —
    /// and unlike its two enumeration siblings above, that empty is NOT a fabrication: a gap is a
    /// hole WITHIN a recorded span, a store that serves no catalog records no span, and a hole in
    /// no span is vacuously absent. The FRONTS rule on [`HistStore::coverage_report`] applies here
    /// unchanged (`RemoteHistStore` overrides with the server's real answer rather than inheriting
    /// "no holes" about data it never asked).
    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, DataError> {
        let _ = id;
        Ok(Vec::new())
    }

    /// The CROSS-KIND coverage report: every instrument with its `trade`/`quote`/`book`/`depth`
    /// series lined up, so a day one kind has and another lacks is a single visible row
    /// ([`crate::InstrumentCoverage::partial_days`] — the Data-Manager's "Partial" column). The
    /// fourth member of this metadata family, promoted onto the trait for the same reason as its
    /// three siblings above: it is a manifest fold (NO Parquet scan) whose only remaining consumer
    /// gap was a `&dyn HistStore` — notably the RPC-backed `RemoteHistStore` — that could not reach
    /// it (split-plane spec §6 Q2).
    ///
    /// Default: an empty report. Its former companions in that convention —
    /// [`HistStore::list_series`] / [`HistStore::inventory`] — have since moved to REFUSING (their
    /// empty was an ENUMERATION fabricated out of a capability gap); this verb keeps the empty
    /// `Ok` because it is a DERIVED view: "nothing partial" is a statement about the JOIN of tick
    /// lanes, and for an impl that serves no tick lanes at all the join is vacuously empty.
    /// ⚠ An impl that FRONTS a store which *can* compute this (the RPC seam does) must override
    /// with an honest `Err` rather than inherit the empty `Ok`: there, "no partial days" and "I
    /// cannot ask" are different facts, and the default states the wrong one. This is the trap
    /// [`HistStore::scan_depth`]'s default USED to carry for EVERY impl, and it took the other
    /// cure — the default itself now refuses, because a store that does not implement that verb
    /// never reads a row and has nothing to be empty ABOUT. One DECLARED residual keeps this
    /// default honest about its edge: a LEAF that holds tick lanes it cannot join inherits
    /// "nothing partial" about lanes it genuinely serves — today that is the flat Parquet archive
    /// store (`crates/vike-backfill/src/archive_store.rs`'s `ArchiveParquetHistStore`), whose
    /// callers read its lanes directly and never this report; a store like it that gains a
    /// Data-Manager surface must override rather than inherit.
    fn coverage_report(&self) -> Result<Vec<crate::coverage::InstrumentCoverage>, DataError> {
        Ok(Vec::new())
    }

    /// One series' INGEST COMMIT KEYS — the store's only record of WHO WROTE it.
    ///
    /// **Defaults to REFUSING**, for [`HistStore::list_series`]'s reasons and one sharper one: the
    /// caller is about to DELETE something, and an empty `Ok` here would read as "this series
    /// records no provenance", which is a real state with real consequences (a `--produced-by`
    /// assertion refuses it; without one it may be deleted). A store that cannot answer must not be
    /// able to impersonate a store that answered "nothing".
    ///
    /// Three shapes, as with the enumeration siblings: a store that CAN answer implements it
    /// (`crate::DataFusionHist`'s manifest read); a delegating wrapper FORWARDS; an RPC seam serves
    /// it over the wire.
    fn series_commits(&self, id: &SeriesId) -> Result<Vec<String>, DataError> {
        let _ = id;
        Err(DataError::Query(
            "series_commits: this store cannot report a series' provenance".into(),
        ))
    }

    /// **Delete one series, IRREVERSIBLY**, optionally asserting that every commit key it records
    /// carries `require_produced_by`.
    ///
    /// **Defaults to REFUSING.** Every other default on this seam answers a READ; this one destroys
    /// data, so the default must be the one that does nothing and says so. A store that does not
    /// implement it would otherwise inherit either a silent success (an operator believing a
    /// cleanup ran) or a silent no-op — and the two are indistinguishable downstream, which is the
    /// exact pairing `crate::datafusion_hist::DataFusionHist`'s `series_dir_of` incident list exists
    /// to warn about.
    ///
    /// Implementors owe the assertion INSIDE their own critical section, not before it: the whole
    /// value of the check is that it holds at the instant the bytes go. `DataFusionHist`'s
    /// `delete_series_checked` is the reference, and its doc carries the lock mechanics.
    fn delete_series_checked(
        &self,
        id: &SeriesId,
        require_produced_by: Option<&str>,
    ) -> Result<(), DataError> {
        let (_, _) = (id, require_produced_by);
        Err(DataError::Query("delete_series: this store serves no delete verb".into()))
    }

    /// Append equity-curve samples for `(venue, symbol)` — the durable store for the vike-core
    /// equity sampler's output (portfolio-observer PR-3). `venue` is the fixed `"portfolio"`
    /// partition namespace at the call site; `symbol` carries the per-exchange venue name or the
    /// cross-venue `"TOTAL"` rollup — the trait stays generic in (venue, symbol) exactly like
    /// [`HistStore::append_symbol_properties`]. Same batch-level `commit_key` idempotency contract as
    /// every other append.
    fn append_equity(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[EquitySample],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError>;

    /// Equity-curve samples for `(venue, symbol)` in `range`, ts-ascending.
    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError>;

    // ---- execution trade-log (kind=exec_fill / kind=exec_order) -----------------------------
    //
    // The ACCOUNT fill/order log (Tier-2): a strategy's OWN executions, NOT market prints. These are
    // DISTINCT kinds from `kind=trade` (market trade ticks), partitioned `venue+symbol+date` like the
    // tick kinds (no interval), so account fills never collide with a symbol's public prints even when
    // `(venue, symbol)` match. Rows are [`crate::ExecFillRow`] / [`crate::ExecOrderRow`]. Same
    // batch-level `commit_key` idempotency contract as every other append (NEVER per-row value dedup).

    /// Append a batch of account fills for `(venue, symbol)` (`kind=exec_fill` series). Returns rows
    /// written (0 if `commit_key` was already ingested or the batch is empty).
    fn append_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecFillRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError>;

    /// Account fills for `(venue, symbol)` (`kind=exec_fill`), ts-ascending. Same in-memory `Vec`
    /// caveat as [`HistStore::scan_quotes`].
    fn scan_exec_fills(&self, venue: &str, symbol: &str) -> Result<Vec<ExecFillRow>, DataError>;

    /// Append a batch of order lifecycle snapshots for `(venue, symbol)` (`kind=exec_order` series).
    /// Returns rows written (0 if `commit_key` was already ingested or the batch is empty).
    fn append_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecOrderRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError>;

    /// Order lifecycle snapshots for `(venue, symbol)` (`kind=exec_order`), ts-ascending. Same
    /// in-memory `Vec` caveat as [`HistStore::scan_quotes`].
    fn scan_exec_orders(&self, venue: &str, symbol: &str) -> Result<Vec<ExecOrderRow>, DataError>;

    // ---- realized perp funding (kind=funding) ----------------------------------------------
    //
    // The ACCOUNT realized-funding series (Tier-2): a strategy's OWN perp funding credits/debits,
    // NOT a market-wide print. A DISTINCT kind from every market series (`kind=trade`/`kind=book`/…),
    // keyed `venue+symbol=<coin>` like the tick kinds (no interval), so account funding never collides
    // with a symbol's public prints even when `(venue, symbol)` match. Rows are [`crate::FundingRow`]
    // (the venue `coin` is the partition `symbol`, dropped from the row); `hash` is the per-row
    // at-most-once identity. Same batch-level `commit_key` idempotency contract as every other append
    // (NEVER per-row value dedup).
    //
    // DEFAULTED (like [`HistStore::properties_as_of`]): a store that does not hold funding (e.g. a
    // bars-only test double) inherits the empty/no-op default; the durable backends override with real
    // behavior. This keeps the seam additive without forcing every existing impl to grow a stub.

    /// Append a batch of realized funding payments for `(venue, symbol=<coin>)` (`kind=funding`
    /// series). Returns rows written (0 if `commit_key` was already ingested, the batch is empty, or
    /// this store does not hold funding). Default = no-op.
    fn append_funding(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[FundingRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let _ = (venue, symbol, rows, commit_key);
        Ok(0)
    }

    /// Realized funding payments for `(venue, symbol=<coin>)` (`kind=funding`) in `range`,
    /// ts-ascending. Same in-memory `Vec` caveat as [`HistStore::scan_quotes`]. Default = empty.
    fn scan_funding(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<FundingRow>, DataError> {
        let _ = (venue, symbol, range);
        Ok(Vec::new())
    }

    // ---- option-chain snapshots (kind=chain) -----------------------------------------------
    //
    // The point-in-time OPTIONS-SURFACE series: one row per instrument per snapshot, all rows of
    // one snapshot sharing `ts` (the chain's asof). Keyed `venue+symbol=<underlying>` like the tick
    // kinds (no interval) — a DISTINCT kind from every market series, so chain snapshots never
    // collide with an underlying's public prints even when `(venue, symbol)` match. Rows are
    // [`crate::ChainRow`]; the opt-in [`crate::ChainRecorder`] (`VIKE_RECORD_CHAINS=1`) is the
    // producer. Same batch-level `commit_key` idempotency contract as every other append (the
    // recorder keys per cadence bucket so a re-fetched chain within one bucket is a no-op).
    //
    // DEFAULTED (like [`HistStore::append_funding`]): a store that does not hold chains (e.g. a
    // bars-only test double) inherits the empty/no-op default; the durable backends override with
    // real behavior. This keeps the seam additive without forcing every existing impl to grow a
    // stub.

    /// Append one chain snapshot's rows for `(venue, symbol=<underlying>)` (`kind=chain` series).
    /// Returns rows written (0 if `commit_key` was already ingested, the batch is empty, or this
    /// store does not hold chains). Default = no-op.
    fn append_chain_snapshot(
        &self,
        venue: &str,
        underlying: &str,
        rows: &[ChainRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let _ = (venue, underlying, rows, commit_key);
        Ok(0)
    }

    /// Chain snapshot rows for `(venue, symbol=<underlying>)` (`kind=chain`) in `range`,
    /// ts-ascending. Same in-memory `Vec` caveat as [`HistStore::scan_quotes`]. Default = empty.
    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TsRange,
    ) -> Result<Vec<ChainRow>, DataError> {
        let _ = (venue, underlying, range);
        Ok(Vec::new())
    }

    // ---- cohort open interest (kind=cohort) --------------------------------------------------
    //
    // The graded POSITIONING panel: for one hour, one asset and one cohort label, the long-side and
    // total notional a grading service reported. Keyed `venue=<exchange>`/`symbol=<asset>` like the
    // tick kinds (no interval) — a DISTINCT kind from every market series, so a cohort panel never
    // collides with that asset's public prints even when `(venue, symbol)` match. Rows are
    // [`crate::CohortRow`]; [`crate::CohortRecorder`] is the producer.
    //
    // ⚠ The row is LONG rather than wide — one row per cohort LABEL, the label in a column — and
    // that shape is a store-layout decision rather than a codec detail:
    // `crate::store_kind::STORE_KINDS`' `cohort` row carries the argument and the arithmetic behind
    // it. What it means for a CALLER of these two verbs is that one append is a whole fetch (every
    // label of one axis over a window), and one scan returns every axis, label, grading and basis
    // recorded for that asset — a caller wanting one of them filters columns, not series.
    //
    // DEFAULTED (like [`HistStore::append_chain_snapshot`]): a store that does not hold cohort
    // panels (e.g. a bars-only test double) inherits the empty/no-op default; the durable backends
    // override with real behavior. This keeps the seam additive without forcing every existing impl
    // to grow a stub.

    /// Append a batch of cohort marginals for `(venue, symbol=<asset>)` (`kind=cohort` series).
    /// Returns rows written (0 if `commit_key` was already ingested, the batch is empty, or this
    /// store does not hold cohort panels). Default = no-op.
    ///
    /// ⚠ The `commit_key` must discriminate the AXIS, the GRADING and the LABEL BASIS as well as
    /// the window — those three are row columns rather than path segments, so two fetches differing
    /// only in one of them address the SAME series, and a key that names only `(venue, asset,
    /// window)` would make the second a silent no-op against the first.
    /// [`crate::CohortRecorder`]'s `commit_key` is the shape that does, and the reason it is a
    /// function rather than a call-site `format!`.
    fn append_cohort(
        &self,
        venue: &str,
        asset: &str,
        rows: &[CohortRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let _ = (venue, asset, rows, commit_key);
        Ok(0)
    }

    /// Cohort marginals for `(venue, symbol=<asset>)` (`kind=cohort`) in `range`, ts-ascending —
    /// EVERY axis, label, grading and label basis recorded for that asset. Same in-memory `Vec`
    /// caveat as [`HistStore::scan_quotes`]. Default = empty.
    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TsRange,
    ) -> Result<Vec<CohortRow>, DataError> {
        let _ = (venue, asset, range);
        Ok(Vec::new())
    }

    // ---- perp market-context metrics (kind=perp_metrics) -------------------------------------
    //
    // The venue's own per-interval numbers ABOUT a perpetual that are not its funding rate: today,
    // the funding PREMIUM. Keyed `venue`/`symbol` like the tick kinds (no interval) — a DISTINCT
    // kind from every market series, so it never collides with that symbol's public prints even
    // when `(venue, symbol)` match. Rows are [`crate::PerpMetricRow`];
    // `crates/vike-backfill/src/funding_rate.rs`'s `backfill_funding_rate` is the producer, and it
    // writes this series from the SAME venue response that fills the funding-rate bars.
    //
    // ⚠ The funding RATE is deliberately NOT one of these columns: it already lives on
    // [`vike_model::Bar::funding`] in the `kind=bar` series under the reserved `interval=funding`
    // label, and a second stored copy is a duplicate that can disagree with the first.
    // [`crate::PerpMetricRow`]'s own doc is the authority on that split and on why open interest is
    // absent.
    //
    // DEFAULTED (like [`HistStore::append_cohort`]): a store that does not hold perp metrics (e.g.
    // a bars-only test double) inherits the empty/no-op default; the durable backends override with
    // real behavior. This keeps the seam additive without forcing every existing impl to grow a
    // stub.

    /// Append a batch of perp market-context rows for `(venue, symbol)` (`kind=perp_metrics`
    /// series). Returns rows written (0 if `commit_key` was already ingested, the batch is empty,
    /// or this store does not hold perp metrics). Default = no-op.
    fn append_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[PerpMetricRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let _ = (venue, symbol, rows, commit_key);
        Ok(0)
    }

    /// Perp market-context rows for `(venue, symbol)` (`kind=perp_metrics`) in `range`,
    /// ts-ascending. Same in-memory `Vec` caveat as [`HistStore::scan_quotes`]. Default = empty.
    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        let _ = (venue, symbol, range);
        Ok(Vec::new())
    }

    /// Point-in-time chain lookup: for each instrument, the most-recent row observed at or before
    /// `ts` — i.e. the latest snapshot state per instrument (the [`HistStore::properties_as_of`]
    /// analog for the options surface, defaulted over [`HistStore::scan_chain`] the same way).
    ///
    /// Per-INSTRUMENT latest (not "rows at the single latest snapshot ts") by design: a full
    /// surface is recorded as one chain per expiry, each with its own `asof_ms`, so the latest-ts
    /// snapshot alone would hold only one expiry. Consequence: an instrument that stopped being
    /// observed (delisted/expired) still surfaces with its LAST recorded row — callers wanting
    /// live-only contracts filter `expiry_ms > ts`. Returned sorted `(expiry_ms, strike,
    /// instrument)` — deterministic; empty if nothing was recorded at or before `ts`.
    ///
    /// ⚠ COST: this is the UNBOUNDED form — it scans and materializes EVERY chain row ever recorded
    /// at or before `ts`, so one lookup grows linearly with archive age. That is fine for the
    /// `kind=properties` shape it is modelled on (~1 row/day/symbol) but NOT for chains: at the
    /// recorder's 1-minute default cadence one underlying is ~hundreds of rows/minute, so a month of
    /// recording is tens of millions of rows decoded PER CALL. Prefer
    /// [`HistStore::chain_as_of_within`], which bounds the scan to a lookback window; reach for this
    /// form only when an arbitrarily stale last-observation genuinely matters (or pair it with
    /// [`crate::hist_maint`] retention so the series stays small).
    fn chain_as_of(
        &self,
        venue: &str,
        underlying: &str,
        ts: i64,
    ) -> Result<Vec<ChainRow>, DataError> {
        let rows = self.scan_chain(venue, underlying, TsRange::of(i64::MIN, ts))?;
        Ok(fold_chain_as_of(rows))
    }

    /// Bounded [`HistStore::chain_as_of`]: identical per-instrument-latest semantics and ordering,
    /// but the scan starts at `ts - lookback_ms` instead of the beginning of time — so the cost is
    /// proportional to the WINDOW, not to how long the store has been recording. This is the form
    /// backtests and surface reads should use.
    ///
    /// Pick `lookback_ms` as a few recorder cadence periods (the recorder's default is 1 minute, so
    /// e.g. 5–15 minutes): an instrument the venue stopped quoting inside the window is stale by the
    /// documented delisting semantics anyway. An instrument NOT observed within the window is simply
    /// absent from the result — the difference from the unbounded form, and the whole point.
    /// A non-positive `lookback_ms` yields the `ts`-instant only; the subtraction saturates, so a
    /// huge lookback degrades to (and matches) [`HistStore::chain_as_of`] rather than overflowing.
    fn chain_as_of_within(
        &self,
        venue: &str,
        underlying: &str,
        ts: i64,
        lookback_ms: i64,
    ) -> Result<Vec<ChainRow>, DataError> {
        let from = ts.saturating_sub(lookback_ms.max(0));
        let rows = self.scan_chain(venue, underlying, TsRange::of(from, ts))?;
        Ok(fold_chain_as_of(rows))
    }

    // ---- derive: resample stored ticks -> bars (in-store) ----------------------------------
    //
    // Read the tick slice for `range` (ts-sorted), feed it to the PARITY-TESTED
    // `vike_model::consolidate_{quotes,trades}` (the same tick->bar math the backtest uses), and
    // `append_bars` the result. `interval` is the bar step ("1m"/"5m"/"1h"/"1d"). Idempotent via
    // `commit_key` like the other appends. Returns bars written.

    /// Resample stored QUOTES → OHLCV bars (bid/ask mid) for `(venue, symbol, interval)`.
    fn resample_quotes_to_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        commit_key: Option<&str>,
    ) -> Result<usize, DataError>;

    /// Resample stored TRADES → OHLCV bars (trade price) for `(venue, symbol, interval)`.
    fn resample_trades_to_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        commit_key: Option<&str>,
    ) -> Result<usize, DataError>;
}

/// The shared per-instrument-latest fold behind [`HistStore::chain_as_of`] and
/// [`HistStore::chain_as_of_within`] — the ONE place the PIT semantics live, so the bounded and
/// unbounded reads can never drift apart (they differ only in the scan range they hand in).
///
/// `rows` must be ts-ASCENDING (every `scan_chain` impl guarantees it): the fold is last-write-wins
/// per instrument, so ordering IS the "freshest observation" rule. Output is sorted
/// `(expiry_ms, strike, instrument)` — deterministic across backends.
fn fold_chain_as_of(rows: Vec<ChainRow>) -> Vec<ChainRow> {
    // BTreeMap (std) for a deterministic fold; no f64 summing happens here, so map order is
    // presentation-only anyway.
    let mut latest: std::collections::BTreeMap<String, ChainRow> =
        std::collections::BTreeMap::new();
    for r in rows {
        latest.insert(r.instrument.clone(), r);
    }
    let mut out: Vec<ChainRow> = latest.into_values().collect();
    out.sort_by(|a, b| {
        a.expiry_ms
            .cmp(&b.expiry_ms)
            .then_with(|| a.strike.total_cmp(&b.strike))
            .then_with(|| a.instrument.cmp(&b.instrument))
    });
    out
}
