//! The unified historical-data seam: ONE store for bars AND ticks (design of record:
//! docs/superpowers/specs/2026-07-05-tickstore-datafusion-spec.md).
//!
//! This module is just the trait + query types (vike-model only). The concrete DataFusion+Parquet
//! backend (`HistoryStoreDataFusion`) is in the crate `vike-store`, so a build of this crate never pulls the
//! Arrow/DataFusion tree.
//!
//! Read half: `load_bars` + `scan_quotes`/`scan_trades`. Ingest: idempotent `append_*` (manifest
//! file-index). Derive: `resample_*_to_bars` feeds the parity-tested `vike_model::consolidate_*`
//! (bars-from-ticks, in-store). The tick scans read a bounded range into memory; a range too large
//! to `Vec` is read a page at a time under a row budget (`HistoryStore::scan_quotes_capped` and its
//! siblings). With this the DataFusion+Parquet engine is the SINGLE historical store — the SQLite
//! bar store is retired.

use serde::{Deserialize, Serialize};
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{EquitySample, SymbolProperties};

use crate::chain_log::ChainRow;
use crate::cohort_log::CohortRow;
use crate::exec_log::{ExecFillRow, ExecOrderRow};
use crate::forecast_log::ExecForecastRow;
use crate::funding_log::ExecutionHistoryFundingRow;
use crate::option_trade_log::OptionTradeRow;
use crate::perp_metrics_log::PerpMetricRow;
use crate::sec_log::{FinancialFactRow, InstitutionalHoldingRow};
use crate::store::series::{ExecutionSeries, SeriesCoverage, SeriesId};
use crate::text_log::TextRow;
use crate::text_score_log::TextScoreRow;
use vike_model::StoreKindId;

/// Errors from the historical store. Engine-independent — DataFusion/Arrow/Parquet failures are
/// stringified so the trait never depends on that tree.
///
/// The serde derive is the form an error crosses a process boundary in, variant for variant (a strategy
/// file's sandboxed child, a study file's host): externally tagged by the variant's name, so its bytes are
/// those of the wire copies it replaces (`hist_tests.rs`'s `the_serde_forms_are_the_wire_copies_bytes`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HistoryStoreError {
    /// DataFusion / Arrow / Parquet query or decode failure.
    Query(String),
    /// Filesystem / manifest IO failure.
    Io(String),
    /// **This store does not implement the series kind that was asked for.** A CAPABILITY answer,
    /// not a failure of the operation — the call was well-formed and this backend cannot serve it.
    ///
    /// It exists because the alternative is worse than an error: `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` verdict 6
    /// records that `append_funding` used to DEFAULT to `Ok(0)`, which is the same value a correct
    /// already-ingested batch and a correct empty batch both return. A backfill against a store that
    /// never implemented the kind therefore reported success, wrote nothing, and a later empty `ls`
    /// read as "no data in that window" rather than "nothing was ever stored". Three situations, one
    /// return value, no way for a caller to separate them.
    ///
    /// Carry the KIND and the store in the message, so the reader knows which of the two to change.
    Unsupported(String),
    /// **The store REFUSED a superseding commit** ([`HistoryStoreError::is_supersede_refusal`]): a property of
    /// the KEY, not a failure of the store. Its text is worded and shown as a [`HistoryStoreError::Query`]'s.
    SupersedeRefused(String),
}

impl std::fmt::Display for HistoryStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HistoryStoreError::Query(e) | HistoryStoreError::SupersedeRefused(e) => {
                write!(f, "hist query: {e}")
            }
            HistoryStoreError::Io(e) => write!(f, "hist io: {e}"),
            HistoryStoreError::Unsupported(e) => write!(f, "hist unsupported: {e}"),
        }
    }
}

impl std::error::Error for HistoryStoreError {}

impl HistoryStoreError {
    /// Whether this is the store REFUSING a superseding commit: `append_quotes_superseding`,
    /// `append_bars_superseding` or `resample_quotes_to_bars_superseding` (all on `HistoryStoreDataFusion`)
    /// found the key they were asked to supersede folded into a multi-key part — background
    /// compaction merged a provisional part into a neighbour before its canonical commit arrived —
    /// so the removal cannot be performed exactly and the call is refused whole.
    ///
    /// It is the one store failure that is a property of a KEY rather than of the store: no row is
    /// sealed, no key is spent, and the same call refuses the same way on every retry. Every other
    /// [`HistoryStoreError`] (a disk, a lock, a decode) is the store failing, and says nothing about the next
    /// call's keys — which is why a caller that wants to step over a refused window and go on must
    /// tell the two apart, and must not step over the rest.
    ///
    /// Decided by the variant the store's `plan_supersede` returns ([`HistoryStoreError::SupersedeRefused`]),
    /// never by the words: a `Query` or an `Io` error that quotes them is not the refusal.
    /// `crates/vike-store/tests/store/supersede_refusal.rs`'s
    /// `the_store_refusing_a_supersede_is_recognised_in_both_of_its_message_shapes` builds the real
    /// refusal in both of its shapes and asserts this answers `true`.
    pub fn is_supersede_refusal(&self) -> bool {
        matches!(self, HistoryStoreError::SupersedeRefused(_))
    }
}

/// Inclusive epoch-ms range; `None` bound = unbounded on that side. Its serde form is `{start, end}`, the
/// bytes the process-boundary copies of it carried (see [`HistoryStoreError`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimestampRange {
    pub start: Option<i64>,
    pub end: Option<i64>,
}

impl TimestampRange {
    /// The whole series (no bounds).
    pub fn all() -> Self {
        Self::default()
    }
    /// `[start, end]` inclusive.
    pub fn of(start: i64, end: i64) -> Self {
        Self { start: Some(start), end: Some(end) }
    }
}

/// Parse one bound of a [`TimestampRange`] as an operator writes it: a bare epoch-ms integer first, else a
/// `YYYY-MM-DDTHH` UTC hour label (Howard-Hinnant civil-calendar math, the convention of
/// `vike_model::time::hours_in_range`), else a plain `YYYY-MM-DD` UTC date, that day's midnight, read by
/// [`vike_model::parse_date_label`], the parser the CLI's other date words go through. An hour label with no
/// minutes/seconds means `:00:00`.
///
/// # Errors
/// The sentence naming what was written and the three spellings it takes.
pub fn parse_timestamp(s: &str) -> Result<i64, String> {
    if let Ok(ms) = s.parse::<i64>() {
        return Ok(ms);
    }
    if let Some((y, m, d, h)) = vike_model::time::parse_hour_label(s) {
        return Ok(vike_utility::calendar::days_from_civil(y, m, d) * vike_model::MS_PER_DAY
            + i64::from(h) * vike_model::MS_PER_HOUR);
    }
    vike_model::parse_date_label(s).map_err(|_| {
        format!("invalid timestamp {s:?}: expected epoch-ms, YYYY-MM-DD or YYYY-MM-DDTHH")
    })
}

/// What a bar series holds inside a [`TimestampRange`], described by its two ENDS and its SIZE — the answer
/// to [`HistoryStore::bar_edges`], and what [`HistoryStore::load_bars`]' result says about itself
/// (`first().ts`, `last().ts`, `len()`) with the rows left out.
///
/// A range that holds no bar is ONE state however it is read: both ends `None` and `rows == 0`, which
/// is [`Default`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BarEdges {
    /// The smallest `ts` among the rows in range — `load_bars`' first row. `None` = the range holds
    /// no bar.
    pub first_timestamp: Option<i64>,
    /// The largest `ts` among the rows in range — `load_bars`' last row. `None` = the range holds no
    /// bar.
    pub last_timestamp: Option<i64>,
    /// How many BARS are in range — `load_bars(..).len()`. A timestamp stored twice counts once,
    /// exactly as it is returned once.
    pub rows: u64,
}

/// One UTC day of a stored read, as [`HistoryStore::series_days`] states it: WHAT a read of that day (a bar
/// read, or a tick read's quotes, trades or book updates) reads, folded into a stamp, without the rows.
///
/// Two answers carrying the same `stamp` for a day were given while that day's stored rows were the
/// same, so a read of the day answers the same rows in the same order: what lets a reader keep
/// a day it read and know when it went stale (`vike-backtesthub`'s `bar_cache`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BarDay {
    /// The UTC day: whole days since 1970-01-01, `timestamp.div_euclid(86_400_000)` of each of its rows.
    pub day: i64,
    /// A digest of every stored part a read of this day reads. Equal stamps mean the same rows; a
    /// different stamp means nothing more than "read it again". Stable within one store build only.
    pub stamp: u64,
    /// The day holds a part written under a PROVISIONAL commit key alone: rows a later settled commit is
    /// expected to replace (the superseding appends). A reader keeps no such day.
    pub provisional: bool,
}

/// The kept rows and share splits of some venues as [`HistoryStore::kept_instruments_and_splits_encoded_of`] holds
/// them: each a MSGPACK array, `rmp_serde`'s named encoding of `Vec<vike_model::Instrument>` and
/// `Vec<vike_model::StockSplit>`, in the order [`HistoryStore::kept_instruments_and_splits_of`] answers them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EncodedKept {
    /// The rows, one msgpack array.
    pub rows: Vec<u8>,
    /// The share splits, one msgpack array.
    pub splits: Vec<u8>,
}

/// One read a caller names AHEAD of making it ([`HistoryStore::prefetch`]): the trait's own read verb, with the
/// arguments it will be called with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedRead {
    /// [`HistoryStore::load_bars`].
    Bars { venue: String, symbol: String, interval: String, range: TimestampRange },
    /// [`HistoryStore::load_bars_sourced_capped`].
    BarsSourcedCapped {
        venue: String,
        source: String,
        symbol: String,
        interval: String,
        range: TimestampRange,
        budget: Option<usize>,
    },
    /// [`HistoryStore::list_series`].
    ListSeries,
    /// [`HistoryStore::scan_quotes`].
    Quotes { venue: String, symbol: String, range: TimestampRange },
    /// [`HistoryStore::scan_trades`].
    Trades { venue: String, symbol: String, range: TimestampRange },
    /// [`HistoryStore::scan_book_updates`].
    BookUpdates { venue: String, symbol: String, range: TimestampRange },
}

/// A bar or tick read answered ENCODED, as a store holds its rows so ([`HistoryStore::read_columns`]): runs of rows,
/// each the rows `cut` of one plain COLUMNS body of the read's kind (the datahub's columns codec,
/// `vike_datahub_client::proto`'s table, the body that read's columns reply carries), shared with the store. The
/// answer is every run's rows, in order. This crate cannot read the codec: a store that answers one hands only bodies a
/// decode of them accepts ([`HistoryStore::read_columns`]' contract), and a reader that splices a run still checks the
/// layout it copies.
#[derive(Debug, Clone, Default)]
pub struct SharedColumns {
    runs: Vec<(std::sync::Arc<Vec<u8>>, std::ops::Range<usize>)>,
    len: usize,
}

impl SharedColumns {
    /// The rows `cut` of `body` as the next run; an empty cut adds nothing.
    pub fn push(&mut self, body: std::sync::Arc<Vec<u8>>, cut: std::ops::Range<usize>) {
        if !cut.is_empty() {
            self.len += cut.len();
            self.runs.push((body, cut));
        }
    }

    /// How many rows the answer holds.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the answer holds no row.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Each run's body and the rows of it the answer holds, in order.
    pub fn runs(&self) -> impl Iterator<Item = (&[u8], std::ops::Range<usize>)> {
        self.runs.iter().map(|(body, cut)| (body.as_slice(), cut.clone()))
    }

    /// The runs themselves, in order: for a reader that keeps the bodies, or drops each once it is read.
    pub fn into_runs(self) -> Vec<(std::sync::Arc<Vec<u8>>, std::ops::Range<usize>)> {
        self.runs
    }
}

/// The one seam over all historical market data. Readers (backtests, bench, the resample
/// bridge) depend on this, never on which engine backs it.
pub trait HistoryStore {
    /// Derived OHLCV bars for `(venue, symbol, interval)` in `range`, ts-ascending. Bars are
    /// small (one row per bar) so a `Vec` is the right shape.
    ///
    /// **ONE bar per timestamp, the NEWEST write's.** The ingest is idempotent by commit key, never
    /// by row, so two ingests of one window under two keys (a strategy's start history and a
    /// backfill, say) store a bar time twice; a read answers it once, with the bar written last
    /// (within one append, the later of the batch). Every read of bars keeps this rule —
    /// [`Self::load_bars_head`], [`Self::bar_edges`] and [`Self::load_bars_sourced_capped`] too.
    /// Ticks and every other kind are ROWS and keep every one.
    ///
    /// ⚠ **"Small" is per BAR, not per RANGE.** The whole range is materialised, so a caller that
    /// wants only what the range LOOKS like — its first `ts`, its last, its size — must ask
    /// [`Self::bar_edges`] instead of loading it to read two numbers off the ends, and a caller that
    /// wants only its START — a page, or "more than `n` rows, or all of them" — must ask
    /// [`Self::load_bars_head`] instead of loading it to keep the first few.
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError>;

    /// The first `ts`, last `ts` and row count of `(venue, symbol, interval)`'s bars in `range` —
    /// [`Self::load_bars`]' answer about itself, without the bars.
    ///
    /// ⚠ **This exists because the alternative is UNBOUNDED, and the alternative is what the data
    /// daemon's `Backfill` verb did.** That verb reads its requested range back through the served
    /// store as a write-through proof and reports two numbers from it. `load_bars` builds every row
    /// of the range — once as Arrow batches and once more as decoded `Vec<Bar>` — so a multi-year
    /// window of a fine interval (two decades of 5-second candles is tens of millions of bars,
    /// gigabytes) sits in RAM inside a daemon whose memory cap it shares with the market-data and
    /// recorder planes. One long request could OOM-kill the daemon and every plane in it.
    ///
    /// ⚠ **The DEFAULT falls back to `load_bars` and is exactly as unbounded as it.** That is honest
    /// rather than lazy, and it is what keeps this method additive: every implementor answers
    /// correctly the day it lands, and only the ones that can do better have to change. A store
    /// that can grow large and can fold its ends without decoding rows MUST override —
    /// `HistoryStoreDataFusion` does, reading the `ts` column alone. The others inherit for a reason
    /// each: `crate::HistoryStoreMemory` is memory-resident and the flat archive store holds no bars at
    /// all, so there is nothing for them to bound; `HistoryStoreDatahub` (an RPC seam) has no edges verb
    /// to forward to, and adding one is a wire change for a caller that today holds the concrete
    /// store. A wrapper that FRONTS another store must FORWARD — inheriting here would make it pay
    /// the fallback while its inner store could do better, the same rule the catalog verbs below
    /// state.
    ///
    /// ⚠ **The answer is `load_bars`', not an approximation of it**, and an override is tested
    /// against exactly that: `first_timestamp`/`last_timestamp` are the smallest and largest `ts` among the rows
    /// `load_bars` returns for the same arguments (the range is inclusive, a `None` bound is open),
    /// and `rows` is their count, one per timestamp as `load_bars` answers it.
    ///
    /// ⚠ **What an override does NOT prove.** `load_bars` decodes every column of every row, so a
    /// part whose price columns cannot be decoded fails it. An edges read decodes the `ts` column
    /// and no other, and such a part still answers. A caller using this as a write-through proof is
    /// proving the rows are committed and readable through this handle — not that every column of
    /// them decodes.
    fn bar_edges(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<BarEdges, HistoryStoreError> {
        let bars = self.load_bars(venue, symbol, interval, range)?;
        Ok(BarEdges {
            first_timestamp: bars.first().map(|b| b.timestamp),
            last_timestamp: bars.last().map(|b| b.timestamp),
            rows: bars.len() as u64,
        })
    }

    /// The START of [`Self::load_bars`]' answer, read without reading the rest: a ts-ascending
    /// COMPLETE PREFIX of `load_bars(range)` — every row of `range` with `ts <= c` for some cut `c`,
    /// and none past `c` — holding AT LEAST `n` rows unless it is the whole range.
    ///
    /// ⚠ **This exists because a capped read of a range used to cost the whole range.** The data
    /// daemon's `LoadBars` verb answers with at most a client's `limit` rows, and it found them by
    /// loading the client's ENTIRE range and cutting the reply afterwards — so the FRAME was bounded
    /// and the ALLOCATION was the range. On a series of tens of millions of `5s` bars that is
    /// gigabytes for a 10,000-row page, and a paging reader asks it again for every page.
    /// `docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md` carries the measurements
    /// and the argument.
    ///
    /// ⚠ **Both halves of the contract are load-bearing, and neither of them is "about `n` rows".**
    ///
    /// * **COMPLETE.** The cut `c` never splits a timestamp: every row at `c` is in the answer. So a
    ///   caller that cuts the answer on a whole-`ts` boundary of its own gets EXACTLY what cutting
    ///   `load_bars` would have given it — which is what lets the data daemon's `cap_to_whole_timestamp`
    ///   keep every capped reply byte-identical to the one it sent before this method existed.
    /// * **AT LEAST `n`.** Fewer than `n` rows means the answer IS the whole range. That is what lets
    ///   one read of `n = C + 1` decide "more than `C` rows, or all of them" — a short answer from a
    ///   range that goes on and the end of the range look the same from outside. The loop that keeps
    ///   reading until the count is met therefore lives INSIDE the store, where the point a read
    ///   stopped at is known. The four `_capped` tick reads promise this half too (see
    ///   [`Self::scan_quotes_capped`]): they promised only a complete prefix of unstated size until
    ///   a pager, which stops on an EMPTY answer, was shown to lose a grouped series' rows without
    ///   it, and `HistoryStoreDataFusion` now answers both families from one block walk.
    ///
    /// The answer may hold MORE than `n` rows — an implementation reads whole storage blocks — and
    /// trimming it is the caller's job, on a whole-`ts` boundary. `n == 0` asks for nothing: an empty
    /// answer is a complete prefix (its cut lies before the first row), and so is the whole range.
    ///
    /// ⚠ **The DEFAULT is `load_bars` itself: correct, and exactly as unbounded.** The whole range is
    /// a complete prefix holding every row it has, so it meets the contract for every `n`; that is
    /// honest rather than lazy, and it is what keeps this method additive — the shape
    /// [`Self::bar_edges`] has, for the same reasons. A store that can grow large and can stop
    /// reading early MUST override — `HistoryStoreDataFusion` does, walking its manifest's parts in
    /// `ts` order and stopping once `n` rows of the range are in. `crate::HistoryStoreMemory` is
    /// memory-resident and the flat archive store holds no bars, so they inherit. A wrapper that
    /// FRONTS another store must forward.
    ///
    /// ⚠ **`HistoryStoreDatahub` inherits, and must NOT forward to a capped `LoadBars`.** That verb's
    /// reply is cut on a whole-`ts` boundary and may be SHORT of its `limit` while the range goes on,
    /// so forwarding would break the at-least half — the very half this method exists to add. The
    /// default pages the whole range instead, which is correct, and nothing calls this method over
    /// that seam.
    fn load_bars_head(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        let _ = n;
        self.load_bars(venue, symbol, interval, range)
    }

    /// L1 quotes for `(venue, symbol)` in `range`, ts-ascending.
    ///
    /// NOTE: reads the whole range into memory (a `Vec`) — for bounded ranges / fixtures. For a
    /// range too large to `Vec`, read it a page at a time under a row budget
    /// ([`Self::scan_quotes_capped`]).
    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError>;

    /// Executed trades for `(venue, symbol)` in `range`, ts-ascending. Same slice-1 caveat as
    /// [`HistoryStore::scan_quotes`].
    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError>;

    // ---- ingest (idempotent by commit key — NEVER by row value) ----------------------------
    //
    // `commit_key` identifies the SOURCE BATCH (e.g. a venue cursor / "venue:symbol:from-to").
    // If it was already ingested for this series the append is a NO-OP (returns 0) — the spec's
    // must-fix #1: trades carry no id, so idempotency lives at the batch level, never per-row
    // value dedup. `None` = always append (no guard). Each append records the sealed Parquet part
    // in the series manifest (the file index that also drives reads — no directory LIST). A bar
    // time two batches both carry is stored twice and READ once, the newest (`load_bars`' doc).

    /// Append a batch of bars for `(venue, symbol, interval)`. Returns rows written (0 if skipped).
    fn append_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        bars: &[Bar],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError>;

    /// Append a batch of quotes for `(venue, symbol)`.
    fn append_quotes(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[TickQuote],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError>;

    /// Append a batch of trades for `(venue, symbol)`.
    fn append_trades(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[TickTrade],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError>;

    /// Append recorded L2 book events for `(venue, symbol)` (`kind=book` series; one part
    /// row per level, regrouped on scan). Returns EVENTS written (0 if commit_key already
    /// ingested). Same batch-level idempotency contract as the other appends.
    fn append_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[L2BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError>;

    /// [`Self::scan_quotes`] under a ROW BUDGET — and the doc for every capped read
    /// ([`Self::scan_trades_capped`], [`Self::scan_book_updates_capped`],
    /// [`Self::scan_depth_capped`], and the three research reads [`Self::scan_cohort_capped`],
    /// [`Self::scan_perp_metrics_capped`] and [`Self::scan_equity_capped`] point here).
    ///
    /// ⚠ **`budget` bounds the READ, not the ANSWER, and only an impl that overrides this can
    /// honour it.** `crates/vike-datahub-client/src/proto/features.rs`'s `FEATURE_SCAN_LIMIT` caps the wire
    /// FRAME by truncating a result the store has already built; that leaves the ALLOCATION
    /// unbounded, which on the live store is 3.5 billion level-rows for one symbol's `kind=depth`
    /// series — about 27 GB for its worst single day, in the data daemon beside a live
    /// order-signing daemon. An impl that can narrow what it READS takes the budget here;
    /// `HistoryStoreDataFusion` selects whole manifest parts under it and CLAMPS the range to match.
    ///
    /// ⚠ **The contract has TWO halves, and the paging reader needs both** — the one
    /// [`Self::load_bars_head`] gives bars:
    ///
    /// * **COMPLETE.** An impl honouring the budget returns a ts-ascending COMPLETE PREFIX of the
    ///   unbudgeted answer — every row of `range` with `ts <= c` for some cut `c`, none past it, and
    ///   never a sample of the asked-for range. The caller pages by continuing past the last row's
    ///   `ts`, so a gap inside the answered span is lost in silence. That holds for the UNION of
    ///   every place the symbol's rows live (a per-symbol series and any `group=` directory), not for
    ///   each separately: two layouts each complete to their own cut make a page complete only to
    ///   the smaller one.
    /// * **AT LEAST `budget` rows, unless the answer is the whole range.** The caller stops on the
    ///   first EMPTY answer, so an empty — or merely short — answer from a range that goes on is
    ///   read as its end. A grouped part counts every symbol it holds, so a read sized by part rows
    ///   alone can select a block holding none of this symbol's rows; the impl must keep reading.
    ///
    /// Both halves were broken for grouped series from v0.1.34, when budgets landed, until the fix
    /// `docs/superpowers/specs/2026-10-01-tick-scan-paging-silent-loss-design.md` designs, and a
    /// paged read lost their rows with no error; that page carries the reproduction. `HistoryStoreDataFusion` keeps both in one block walk shared with
    /// `load_bars_head`. The answer may hold MORE than `budget` rows (whole storage blocks), and
    /// trimming it is the caller's job, on a whole-`ts` boundary. `None` — or a zero budget — is the
    /// whole range.
    ///
    /// ⚠ **The DEFAULT ignores the budget and delegates**, which bounds nothing. That is honest
    /// rather than lazy: a store with no part index cannot narrow a read, and pretending otherwise
    /// by truncating the result would claim a bound it does not have. `crate::HistoryStoreMemory` and
    /// the archive store inherit it, and both are small by construction.
    ///
    /// ⚠ **Seven verbs, not thirteen — and the reason the family grew is the reason it exists.** It
    /// started as four (the tick kinds), on the argument that `kind=cohort` and `kind=perp_metrics`
    /// held thousands of rows per series and `scan_equity`/`scan_exec_fills` held none, so a budget
    /// bought nothing there. That argument measured SIZE, and the data daemon serves every one of
    /// these kinds to a client who chooses the range: each page of a paged cohort read decoded the
    /// whole rest of the range, which is quadratic, and is tolerable only while nobody's series is
    /// large. So [`Self::scan_cohort_capped`], [`Self::scan_perp_metrics_capped`] and
    /// [`Self::scan_equity_capped`] joined, under this contract unchanged
    /// (`docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md`, section 2).
    /// Exec fills joined as a HEAD rather than a budget, [`Self::scan_execution_fills_head`], because its
    /// verb has no range on the wire to page with.
    ///
    /// `kind=bar` is not in this family either and is not small: the OANDA history lane put tens of
    /// millions of rows into a single `5s` series. It has a bounded read of its own,
    /// [`Self::load_bars_head`], whose `n` is a count to REACH rather than an optional budget,
    /// because the data daemon's `LoadBars` needs one read of `n = C + 1` to decide whether to refuse
    /// an oversized request by name. Both families answer under the same contract, from the same
    /// block walk in `HistoryStoreDataFusion`.
    fn scan_quotes_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        let _ = budget;
        self.scan_quotes(venue, symbol, range)
    }

    /// [`Self::scan_quotes_capped`] of ONE `source=` lane ([`SeriesId::source`]), and the only read that
    /// reaches a lane: every other verb reads the sourceless series, so a caller gets a lane's rows by
    /// naming it and in no other way.
    ///
    /// **Defaults to REFUSING**, for [`Self::list_series`]'s reason: a store with no lanes answering an
    /// empty tape would read as a recorder that recorded nothing. A lane the store does not hold is the
    /// missing series' answer, empty, from a store that has lanes.
    fn scan_quotes_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        let _ = (venue, symbol, range, budget);
        Err(HistoryStoreError::Query(format!("this store cannot read a lane (`source={source}`)")))
    }

    /// [`Self::load_bars`] of ONE `source=` lane under a row budget, the bar twin of
    /// [`Self::scan_quotes_sourced_capped`], and the only read that reaches a bar lane: `load_bars` and
    /// its siblings read the sourceless series, so a chart or a backtest asking for a venue's candles
    /// never meets a lane's bars. Defaults to REFUSING, for the same reason.
    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        let _ = (venue, symbol, interval, range, budget);
        Err(HistoryStoreError::Query(format!("this store cannot read a lane (`source={source}`)")))
    }

    /// **The UTC days a read of `range` reads**, each with a stamp of what it holds: every day that holds a
    /// stored part overlapping `range`, ascending. A day not listed held no row of the range when this answered.
    /// `kind` names the read:
    ///
    /// - [`StoreKindId::Bar`]: [`Self::load_bars`] (`source: None`) or [`Self::load_bars_sourced_capped`]
    ///   (`Some(lane)`, no budget) of `interval`, which a bar read must name.
    /// - [`StoreKindId::Quote`], [`StoreKindId::Trade`], [`StoreKindId::Book`]: [`Self::scan_quotes`],
    ///   [`Self::scan_trades`] or [`Self::scan_book_updates`], stamped over EVERY layout that read reads (the
    ///   symbol's own series and each `group=` series of the kind and venue), so a write to any of them that could
    ///   change a day's rows changes its stamp. A tick read names no `interval` and no `source`.
    ///
    /// Any other kind, or a bar read without its interval, or a tick read with one or a source, is refused.
    ///
    /// It exists so a reader can keep the rows of past days and serve them again for exactly as long as
    /// each day's stored parts are unchanged: a row appended to a LATER day changes no earlier day's
    /// stamp (`vike-backtesthub`'s `bar_cache` is that reader). It reads no row.
    ///
    /// **Defaults to `Ok(None)`: this store cannot say**, and a reader then keeps nothing. Never an
    /// empty `Some`, which is the positive claim "the range holds no row". `HistoryStoreDataFusion`
    /// answers from its manifests. A wrapper that FRONTS another store must forward.
    fn series_days(
        &self,
        kind: StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: TimestampRange,
    ) -> Result<Option<Vec<BarDay>>, HistoryStoreError> {
        let _ = (kind, venue, source, symbol, interval, range);
        Ok(None)
    }

    /// **A bar or tick read's rows ENCODED, where this store HOLDS them so** ([`SharedColumns`]): what
    /// [`Self::load_bars`] (`kind` [`StoreKindId::Bar`], `source: None`), an unbudgeted
    /// [`Self::load_bars_sourced_capped`] (`Some(lane)`), [`Self::scan_quotes`], [`Self::scan_trades`] or
    /// [`Self::scan_book_updates`] (`Quote`, `Trade`, `Book`, with no source and no interval) answer, the same rows
    /// in the same order, as that read's columns bodies; `kind`, `source` and `interval` name the read as
    /// [`Self::series_days`] does. A reader that only relays the rows (the backtesthub's relay hands them to its
    /// sandboxed child as the columns parts they already are) is spared their decode and their encode, and one that
    /// keeps them (the backtesthub's cache, of its datahub's pages) their decode and its own encode.
    ///
    /// `None` is a store that holds no columns, the default: the caller asks the typed read. A store that keeps
    /// them overrides it (`vike-backtesthub`'s `bar_cache` does), and so does one that is SENT them
    /// (`vike-datahub-client`'s `HistoryStoreDatahub`, its pages as the datahub sent them where the datahub answers the
    /// read as columns); a wrapper that fronts one need not forward it, it is only slower.
    ///
    /// ⚠ **An override answers only bodies a decode of them accepts**, so its readers may keep or relay them as they
    /// are: a store that is SENT them checks each where it arrives, every check the decode makes
    /// (`vike_datahub_client::proto::check_columns`); a row count no body holds would otherwise reach a reader that
    /// sizes by it (the sandbox relay's count frame, which its child reserves).
    fn read_columns(
        &self,
        kind: StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: TimestampRange,
    ) -> Option<Result<SharedColumns, HistoryStoreError>> {
        let _ = (kind, venue, source, symbol, interval, range);
        None
    }

    /// [`Self::scan_trades`] under a row budget — see [`Self::scan_quotes_capped`].
    fn scan_trades_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        let _ = budget;
        self.scan_trades(venue, symbol, range)
    }

    /// [`Self::scan_book_updates`] under a row budget — see [`Self::scan_quotes_capped`].
    ///
    /// ⚠ The row count here is a MULTIPLE of the event count: the write side explodes one
    /// `L2BookUpdate` into one row per price level, so a budget expressed in ROWS bounds the read
    /// while the answer's event count is smaller and data-dependent.
    fn scan_book_updates_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        let _ = budget;
        self.scan_book_updates(venue, symbol, range)
    }

    /// [`Self::scan_depth`] under a row budget — see [`Self::scan_quotes_capped`]. Same
    /// level-rows-per-event caveat as [`Self::scan_book_updates_capped`].
    fn scan_depth_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        let _ = budget;
        self.scan_depth(venue, symbol, range)
    }

    /// Recorded L2 book events for `(venue, symbol)` in `range`, sorted by `(ts, seq)` and
    /// regrouped into [`L2BookUpdate`]s. Same in-memory `Vec` caveat as [`HistoryStore::scan_quotes`].
    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError>;

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
    /// [`append_book_updates`](HistoryStore::append_book_updates).
    ///
    /// **Defaults to REFUSING**, not to a silent no-op. Most `HistoryStore` impls in this workspace are
    /// read-only views over one source (an archive file, a ClickHouse table, an RPC seam) and have no
    /// depth to offer; a default `Ok(0)` would let a recorder write into them and report success
    /// while the rows went nowhere. Only `HistoryStoreDataFusion` overrides it.
    fn append_depth(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[L2BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let (_, _, _, _) = (venue, symbol, updates, commit_key);
        Err(HistoryStoreError::Query("append_depth: this store serves no depth lane".into()))
    }

    /// Recorded L2 depth snapshots for `(venue, symbol)` — the read half of
    /// [`append_depth`](HistoryStore::append_depth). Same `(ts, seq)` ordering and both-layouts scan as
    /// [`scan_book_updates`](HistoryStore::scan_book_updates).
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
    /// `crates/vike-secrets/src/store/backend.rs`'s `CredentialStore`: **an ABSENT thing is the
    /// ordinary state and is silent; a thing that is PRESENT and unusable is an ERROR.** Here the
    /// absent thing is the ROWS (an honest empty `Ok`) and the unusable thing is the VERB.
    ///
    /// Two impls answer it for real: `HistoryStoreDataFusion`, and
    /// `crates/vike-datahub-client/src/remote.rs`'s `HistoryStoreDatahub`, which FORWARDS it to the
    /// server. Everyone else takes one of two shapes: a store with no depth lane INHERITS this
    /// refusal (`crate::HistoryStoreMemory` does), while a store that FRONTS another one must not — a
    /// delegating wrapper FORWARDS to its inner store.
    ///
    /// ⚠ **`HistoryStoreDatahub` was this doc's worked example of a THIRD shape — a seam that could
    /// serve the lane but does not carry it over the wire, overriding with its own reason — and it
    /// is not one any more.** `docs/decisions/0084-only-the-datahub-touches-the-store.md` put the
    /// verb on the wire, so that seam forwards like any other wrapper. The shape it illustrated is
    /// real and still available to a future impl; it just has no instance in this tree, and citing
    /// a forwarding store as the model for refusing would reproduce by hand the exact gap 0084 was
    /// written to close. Why that override existed at all is the part worth keeping: while the
    /// default was an empty `Ok`, inheriting it would have claimed the SERVER holds no depth.
    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        let (_, _, _) = (venue, symbol, range);
        Err(HistoryStoreError::Query("scan_depth: this store serves no depth lane".into()))
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
    ) -> Result<usize, HistoryStoreError>;

    /// Observed properties for `(venue, symbol)` in `range`, ts-ascending.
    fn scan_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, HistoryStoreError>;

    /// Point-in-time lookup: the most-recent properties observed at or before `ts` (`None` if none).
    fn properties_as_of(
        &self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, HistoryStoreError> {
        Ok(self
            .scan_symbol_properties(venue, symbol, TimestampRange::of(i64::MIN, ts))?
            .pop()
            .map(|(_, f)| f))
    }

    // ---- store inventory / metadata (kind-agnostic) -------------------------------------------
    //
    // The catalog verbs the Data-Manager / Studio data-browser render from, promoted onto the TRAIT
    // (they were concrete `HistoryStoreDataFusion` methods) so a `&dyn HistoryStore` — notably the RPC-backed
    // `HistoryStoreDatahub` — can answer them, not only the local DataFusion backend. The two
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
    /// Same rule and same authority as [`HistoryStore::scan_depth`]'s default:
    /// `crates/vike-secrets/src/store/backend.rs`'s `CredentialStore` — **an ABSENT thing is the
    /// ordinary state and is silent; a thing that is PRESENT and unusable is an ERROR.** The
    /// absent thing here is the SERIES (an honest empty listing from a store that looked); the
    /// unusable thing is the VERB.
    ///
    /// The refusal names the verb's absence and claims NOTHING about content — deliberately, so a
    /// leaf that holds data it cannot enumerate (the ClickHouse bridge, the flat Parquet archive
    /// store) inherits a true statement, unlike `scan_depth`'s "serves no depth lane" (which the
    /// RPC seam had to override as false THERE). Three shapes for implementors: a store that CAN
    /// enumerate implements the verb consciously (`HistoryStoreDataFusion`'s manifest walk, and
    /// `crate::HistoryStoreMemory`'s in-memory fold — which the old default silently SHADOWED: a seeded
    /// double answered "no series" while holding rows); a delegating wrapper FORWARDS to its inner
    /// store (poly_mm_batch's `CachedHistStore` was one, until #2046 deleted it on 2026-09-20),
    /// because "cannot enumerate" is false for a type
    /// whose whole job is fronting a store that can; and an RPC seam serves it over the wire
    /// (`crates/vike-datahub-client/src/remote.rs`'s `HistoryStoreDatahub`).
    fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        Err(HistoryStoreError::Query(
            "list_series: this store cannot enumerate its inventory".into(),
        ))
    }

    /// **A HINT: the caller is about to make `reads`, each through its own verb, in this order.** A store whose
    /// reads cross one channel to a far side (`vike-backtesthub`'s sandboxed child, its `PipeHistStore`) may ask for
    /// all of them NOW, back to back, so the far side fetches them at once and each verb finds its answer waiting.
    /// It answers and refuses nothing: every read is still made, and judged, by its verb in the caller's order, so a
    /// store that ignores the hint (this default) answers exactly what one that acts on it answers. A read named here
    /// and never made costs its fetch and nothing else. A wrapper that FRONTS another store forwards it.
    fn prefetch(&self, reads: &[PlannedRead]) {
        let _ = reads;
    }

    /// Every stored series paired with its cheap coverage (manifest fold — first/last ts, rows, bytes,
    /// parts, dates; NO scan).
    ///
    /// **Defaults to REFUSING**, for exactly [`HistoryStore::list_series`]'s reasons — it is the same
    /// enumeration wearing coverage, and the two defaults must agree or one call site could be
    /// refused the series list while a neighbour is handed a fabricated empty inventory of the
    /// same store. The empty-inventory answer belongs to a store that folded its real catalog and
    /// found nothing.
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        Err(HistoryStoreError::Query("inventory: this store cannot enumerate its inventory".into()))
    }

    /// The execution-history series of `venue` and `symbol` (`None` = every one) whose span reaches
    /// `range`, with the filters' choices: [`ExecutionSeries::of`] over [`Self::inventory`], so it
    /// refuses where that does and `HistoryStoreDataFusion` answers from its inventory memo.
    /// `HistoryStoreDatahub` overrides it with the datahub's own answer, a few KB instead of the whole
    /// inventory.
    fn execution_series(
        &self,
        venue: Option<&str>,
        symbol: Option<&str>,
        range: TimestampRange,
    ) -> Result<ExecutionSeries, HistoryStoreError> {
        Ok(ExecutionSeries::of(self.inventory()?, venue, symbol, range))
    }

    /// The GAP ranges (inclusive epoch-ms) missing within `id`'s recorded day span — the Data-Manager
    /// "where's the hole" view, derived from the manifest `date=` index (NO scan). Default: no gaps —
    /// and unlike its two enumeration siblings above, that empty is NOT a fabrication: a gap is a
    /// hole WITHIN a recorded span, a store that serves no catalog records no span, and a hole in
    /// no span is vacuously absent. The FRONTS rule on [`HistoryStore::coverage_report`] applies here
    /// unchanged (`HistoryStoreDatahub` overrides with the server's real answer rather than inheriting
    /// "no holes" about data it never asked).
    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, HistoryStoreError> {
        let _ = id;
        Ok(Vec::new())
    }

    /// One series' COVERAGE plus the commit keys that produced it — the pair a run fingerprint is
    /// made of.
    ///
    /// ⚠ **This was INHERENT to `HistoryStoreDataFusion` and is on the trait now**, because
    /// `docs/decisions/0084-only-the-datahub-touches-the-store.md` routes readers through the
    /// datahub and a method that exists only on the concrete store is one a routed reader cannot
    /// call. `crates/vike-backtesthub/src/backtest_cli/run_record.rs`'s `collect_data_fingerprint`
    /// took `Option<&HistoryStoreDataFusion>` for exactly that reason; the consequence was that a
    /// wire-routed run would lose its reproducibility record while still reading green — a
    /// witness that stops witnessing and says nothing.
    ///
    /// ⚠ **The default REFUSES rather than answering empty**, which is the opposite of
    /// [`HistoryStore::series_gaps`] above and deliberately so. A missing gap is vacuously true — a
    /// hole in no recorded span is absent. Missing FACTS are not: an empty coverage and an empty
    /// commit list are a positive claim that the store holds this series and knows nothing about
    /// it, and a fingerprint built from that would record "nothing was read" about a run that read
    /// plenty. A store with no per-series manifest has no answer, and says so.
    fn series_facts(
        &self,
        id: &SeriesId,
    ) -> Result<(SeriesCoverage, Vec<String>), HistoryStoreError> {
        let _ = id;
        Err(HistoryStoreError::Query(
            "series_facts: this store keeps no per-series manifest, so it cannot report coverage \
             or commit keys"
                .into(),
        ))
    }

    /// **The venues' KEPT instrument rows**: the lists only the datahub keeps and reads
    /// (`<project>/instruments/<venue>.json`, `docs/decisions/0084-only-the-datahub-touches-the-store.md`; its
    /// `Request::InstrumentLists`), every venue's rows in one answer. What a backtest reads a market's session
    /// class, fiat quote and contract face from (`vike_backtest::run::KeptRows`).
    ///
    /// Not a question about stored series. It is on the trait for [`HistoryStore::series_facts`]'s reason: a
    /// routed reader holds `Arc<dyn HistoryStore>` and cannot call a method of the concrete datahub handle.
    /// `Ok(None)` (the default) is a store that serves no kept list: every store but the datahub's handle
    /// (`crates/vike-datahub-client/src/remote.rs`'s `HistoryStoreDatahub`, and a datahub too old to keep them),
    /// so a run over it reads every symbol by its name, as before the lists existed. A sandboxed backtest's
    /// piped store asks its parent (`crates/vike-backtesthub/src/strategy_sandbox/pipe_store.rs`'s `PipeHistStore`).
    fn kept_instruments(&self) -> Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError> {
        Ok(None)
    }

    /// **The share splits the operator entered** (`vike_secrets::stock_split`, `vike-cli data splits`), every series'
    /// in one answer, served by the datahub with its kept lists (`Request::InstrumentLists`). What a backtest adjusts a
    /// series' prices by (`vike_backtest::run::KeptRows::splits`). It sits beside [`HistoryStore::kept_instruments`]
    /// for that method's reason, and its default is the same: `Ok(None)`, a store that serves none, so a run over it
    /// reads every series unadjusted, as before splits existed. A sandboxed backtest's piped store asks its parent.
    fn stock_splits(&self) -> Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError> {
        Ok(None)
    }

    /// [`HistoryStore::kept_instruments`] and [`HistoryStore::stock_splits`] in ONE ask, what a backtest reads
    /// before it runs. The default asks the two; the datahub's handle answers both off its one
    /// `Request::InstrumentLists` reply, which carries both: asked as two, every run took the whole lists twice
    /// (14 MB of JSON each on the CI box, 2026-11-17).
    #[expect(clippy::type_complexity, reason = "the two answers above, side by side")]
    fn kept_instruments_and_splits(
        &self,
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
    ) {
        (self.kept_instruments(), self.stock_splits())
    }

    /// [`HistoryStore::kept_instruments_and_splits`] of `venues` alone: their kept rows and their series' share
    /// splits, each as the whole answer carries it, every other venue's left out. What a backtest asks once it knows
    /// the venues it reads (`vike_backtest::run::profile::ProfileBacktest::read_kept_rows`), so a run is not handed
    /// every venue's ~24k rows. The default is the whole answer, filtered here; the datahub's handle asks its datahub
    /// for those venues alone (`Request::InstrumentListsOfVenues`), and a sandboxed backtest's piped store asks its
    /// parent (`crates/vike-backtesthub/src/strategy_sandbox/pipe_store.rs`'s `PipeHistStore`).
    #[expect(clippy::type_complexity, reason = "the two answers above, side by side")]
    fn kept_instruments_and_splits_of(
        &self,
        venues: &[String],
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
    ) {
        let named = |venue: &str| venues.iter().any(|v| v == venue);
        let (rows, splits) = self.kept_instruments_and_splits();
        (
            rows.map(|rows| rows.map(|r| r.into_iter().filter(|i| named(&i.venue)).collect())),
            splits.map(|all| all.map(|s| s.into_iter().filter(|x| named(&x.venue)).collect())),
        )
    }

    /// [`HistoryStore::kept_instruments_and_splits_of`]'s two `Ok(Some(..))` answers, already ENCODED
    /// ([`EncodedKept`]), so a sandboxed backtest's parent hands them to its child as they are
    /// (`crates/vike-backtesthub/src/strategy_sandbox/relay.rs`'s `answer`). `None` is a store that holds no encoded
    /// form, the default: the caller asks the typed method. The datahub's handle answers it off its stamped cache
    /// (`Request::KeptRowsOfVenues`, the owner's "yes both" of 2026-11-19) whenever its datahub serves that verb.
    fn kept_instruments_and_splits_encoded_of(
        &self,
        _venues: &[String],
    ) -> Option<Result<EncodedKept, HistoryStoreError>> {
        None
    }

    /// The CROSS-KIND coverage report: every instrument with its `trade`/`quote`/`book`/`depth`
    /// series lined up, so a day one kind has and another lacks is a single visible row
    /// ([`crate::InstrumentCoverage::partial_days`] — the Data-Manager's "Partial" column). The
    /// fourth member of this metadata family, promoted onto the trait for the same reason as its
    /// three siblings above: it is a manifest fold (NO Parquet scan) whose only remaining consumer
    /// gap was a `&dyn HistoryStore` — notably the RPC-backed `HistoryStoreDatahub` — that could not reach
    /// it (split-plane spec §6 Q2).
    ///
    /// Default: an empty report. Its former companions in that convention —
    /// [`HistoryStore::list_series`] / [`HistoryStore::inventory`] — have since moved to REFUSING (their
    /// empty was an ENUMERATION fabricated out of a capability gap); this verb keeps the empty
    /// `Ok` because it is a DERIVED view: "nothing partial" is a statement about the JOIN of tick
    /// lanes, and for an impl that serves no tick lanes at all the join is vacuously empty.
    /// ⚠ An impl that FRONTS a store which *can* compute this (the RPC seam does) must override
    /// with an honest `Err` rather than inherit the empty `Ok`: there, "no partial days" and "I
    /// cannot ask" are different facts, and the default states the wrong one. This is the trap
    /// [`HistoryStore::scan_depth`]'s default USED to carry for EVERY impl, and it took the other
    /// cure — the default itself now refuses, because a store that does not implement that verb
    /// never reads a row and has nothing to be empty ABOUT. One DECLARED residual keeps this
    /// default honest about its edge: a LEAF that holds tick lanes it cannot join inherits
    /// "nothing partial" about lanes it genuinely serves. No store in this tree is one today (the
    /// flat Parquet archive store was, and is deleted); a store like it that gains a Data-Manager
    /// surface must override rather than inherit.
    fn coverage_report(
        &self,
    ) -> Result<Vec<crate::store::coverage::InstrumentCoverage>, HistoryStoreError> {
        Ok(Vec::new())
    }

    /// One series' INGEST COMMIT KEYS — the store's only record of WHO WROTE it.
    ///
    /// **Defaults to REFUSING**, for [`HistoryStore::list_series`]'s reasons and one sharper one: the
    /// caller is about to DELETE something, and an empty `Ok` here would read as "this series
    /// records no provenance", which is a real state with real consequences (a `--produced-by`
    /// assertion refuses it; without one it may be deleted). A store that cannot answer must not be
    /// able to impersonate a store that answered "nothing".
    ///
    /// Three shapes, as with the enumeration siblings: a store that CAN answer implements it
    /// (`HistoryStoreDataFusion`'s manifest read); a delegating wrapper FORWARDS; an RPC seam serves
    /// it over the wire.
    fn series_commits(&self, id: &SeriesId) -> Result<Vec<String>, HistoryStoreError> {
        let _ = id;
        Err(HistoryStoreError::Query(
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
    /// exact pairing `HistoryStoreDataFusion`'s `series_dir_of` incident list exists
    /// to warn about.
    ///
    /// Implementors owe the assertion INSIDE their own critical section, not before it: the whole
    /// value of the check is that it holds at the instant the bytes go. `HistoryStoreDataFusion`'s
    /// `delete_series_checked` is the reference, and its doc carries the lock mechanics.
    fn delete_series_checked(
        &self,
        id: &SeriesId,
        require_produced_by: Option<&str>,
    ) -> Result<(), HistoryStoreError> {
        let (_, _) = (id, require_produced_by);
        Err(HistoryStoreError::Query("delete_series: this store serves no delete verb".into()))
    }

    /// Append equity-curve samples for `(venue, symbol)` — the durable store for the vike-core
    /// equity sampler's output (portfolio-observer PR-3). `venue` is the fixed `"portfolio"`
    /// partition namespace at the call site; `symbol` carries the per-exchange venue name or the
    /// cross-venue `"TOTAL"` rollup — the trait stays generic in (venue, symbol) exactly like
    /// [`HistoryStore::append_symbol_properties`]. Same batch-level `commit_key` idempotency contract as
    /// every other append.
    fn append_equity(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[EquitySample],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError>;

    /// Equity-curve samples for `(venue, symbol)` in `range`, ts-ascending.
    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<EquitySample>, HistoryStoreError>;

    /// [`Self::scan_equity`] under a row budget — the contract, both halves of it, and why the
    /// DEFAULT delegating to the unbudgeted read is honest: [`Self::scan_quotes_capped`]. A
    /// `kind=equity` series is per-symbol (`symbol` carries the per-exchange name or `"TOTAL"`), so
    /// there is one layout to walk.
    fn scan_equity_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        let _ = budget;
        self.scan_equity(venue, symbol, range)
    }

    // ---- execution trade-log (kind=exec_fill / kind=execution_order) -----------------------------
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
    ) -> Result<usize, HistoryStoreError>;

    /// Account fills for `(venue, symbol)` (`kind=exec_fill`), ts-ascending. Same in-memory `Vec`
    /// caveat as [`HistoryStore::scan_quotes`].
    fn scan_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError>;

    /// The START of [`Self::scan_exec_fills`] over `range`: a ts-ascending COMPLETE PREFIX of the
    /// series' rows in `range` (never splitting a timestamp) holding AT LEAST `n` rows unless it is
    /// all of them — the contract [`Self::load_bars_head`] states. `n` is a count to reach rather
    /// than an optional budget for the reason bars have one: the data daemon's `ScanExecFills`
    /// answers a request with no `limit` by one read of `n = C + 1` that decides "all of it, or
    /// refused by name", and a request WITH a range and a `limit` (the `scan_execution_fills_range`
    /// capability) by one read of `n = min(limit, C)` over that range. `n == 0` asks for nothing.
    ///
    /// ⚠ **It TAKES a range, and that is the family's rule rather than decoration**: every
    /// `scan_*`/`load_*` series read takes a `TimestampRange` except `scan_exec_fills` and
    /// `scan_exec_orders`, a claim the `exec_fill` row of `crate::store::store_kind` makes and
    /// `crates/vike-ops/tests/venues/store_kind_gate/reverse_direction.rs`'s
    /// `only_the_execution_reads_take_no_range` holds. A bounded read joins the rule rather than
    /// widening the exception. The wire range this read needed no change for is the reopened Q1 of
    /// `docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md`: reopened for the
    /// GUI's time windows (the owner's ruling of 2026-10-11), not because a series neared its
    /// ceiling.
    ///
    /// ⚠ **The DEFAULT is `scan_exec_fills` itself, filtered to `range`: correct, and exactly as
    /// unbounded** — the whole range meets the contract for every `n`. `HistoryStoreDataFusion`
    /// overrides it with the block walk; a wrapper that FRONTS another store must forward.
    fn scan_execution_fills_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        let _ = n;
        let mut rows = self.scan_exec_fills(venue, symbol)?;
        rows.retain(|r| {
            range.start.is_none_or(|lo| r.ts >= lo) && range.end.is_none_or(|hi| r.ts <= hi)
        });
        Ok(rows)
    }

    /// Append a batch of order lifecycle snapshots for `(venue, symbol)` (`kind=execution_order` series).
    /// Returns rows written (0 if `commit_key` was already ingested or the batch is empty).
    fn append_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecOrderRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError>;

    /// Order lifecycle snapshots for `(venue, symbol)` (`kind=execution_order`), ts-ascending. Same
    /// in-memory `Vec` caveat as [`HistoryStore::scan_quotes`].
    fn scan_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError>;

    /// The START of [`Self::scan_exec_orders`] over `range`: [`Self::scan_execution_fills_head`]'s
    /// contract over the order-lifecycle series — a ts-ascending COMPLETE PREFIX of the rows in
    /// `range` (never splitting a timestamp) holding AT LEAST `n` rows unless it is all of them;
    /// `n == 0` asks for nothing. The data daemon's `ScanExecOrders` is its one caller, with
    /// `n = min(limit, C)` or `C + 1`.
    ///
    /// ⚠ **The DEFAULT is `scan_exec_orders` itself, filtered to `range`: correct, and exactly as
    /// unbounded** — the twin of [`Self::scan_execution_fills_head`]'s default. `HistoryStoreDataFusion`
    /// overrides it with the block walk; a wrapper that FRONTS another store must forward.
    fn scan_execution_orders_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let mut rows = self.scan_exec_orders(venue, symbol)?;
        rows.retain(|r| {
            range.start.is_none_or(|lo| r.ts >= lo) && range.end.is_none_or(|hi| r.ts <= hi)
        });
        Ok(rows)
    }

    // ---- realized perp funding (kind=exec_funding) -----------------------------------------
    //
    // The ACCOUNT realized-funding series (Tier-2): a strategy's OWN perp funding credits/debits,
    // NOT a market-wide print. A DISTINCT kind from every market series (`kind=trade`/`kind=book`/…),
    // keyed `venue+symbol=<coin>` like the tick kinds (no interval), so account funding never collides
    // with a symbol's public prints even when `(venue, symbol)` match. Rows are [`crate::ExecutionHistoryFundingRow`]
    // (the venue `coin` is the partition `symbol`, dropped from the row); `hash` is the per-row
    // at-most-once identity. Same batch-level `commit_key` idempotency contract as every other append
    // (NEVER per-row value dedup).
    //
    // ⚠ DEFAULTED, BUT THE DEFAULT REFUSES — and this paragraph said the opposite until
    // `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` verdict 6. It read: "a store that does not hold funding inherits the
    // empty/no-op default … This keeps the seam additive without forcing every existing impl to grow
    // a stub." The additive property was real and the cost was hidden: `Ok(0)` is ALSO what a correct
    // already-ingested batch and a correct empty batch return, so a store that never implemented the
    // kind was indistinguishable from one that had nothing to do. The seam stays additive — no impl
    // is forced to grow a stub — but the default now answers `HistoryStoreError::Unsupported` instead of
    // pretending to have worked. The three durable backends (`HistoryStoreDataFusion`, `HistoryStoreMemory` and
    // the remote `DatahubClient`) all override both methods, so this default is reached only by a
    // store that genuinely cannot serve the kind.

    /// Append a batch of realized funding payments for `(venue, symbol=<coin>)`
    /// (`kind=exec_funding` series). Returns rows written — `0` meaning **`commit_key` was already
    /// ingested, or the batch was empty**, and those two alone.
    ///
    /// ⚠ **The default REFUSES rather than returning `Ok(0)`**, and that is the whole point of
    /// `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` verdict 6. It used to be a no-op returning `Ok(0)`, which collided with
    /// both legitimate zero answers above: a backfill against a store that never implemented this
    /// kind reported success, wrote nothing, and a later empty `ls` read as *"no funding in that
    /// window"* rather than *"nothing was ever stored"*. A backend that holds this kind overrides
    /// this method; one that does not now says so.
    fn append_funding(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecutionHistoryFundingRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let _ = (rows, commit_key);
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=exec_funding (asked to append {venue}:{symbol}) — \
             nothing was written; use a store backend that implements append_funding"
        )))
    }

    /// Realized funding payments for `(venue, symbol=<coin>)` (`kind=exec_funding`) in `range`,
    /// ts-ascending. Same in-memory `Vec` caveat as [`HistoryStore::scan_quotes`].
    ///
    /// ⚠ **The default REFUSES rather than returning an empty `Vec`** — the read twin of
    /// `append_funding` above, and for the identical reason: an empty scan from a store that CANNOT
    /// hold the kind is not the same answer as an empty scan from one that can, and a caller handed
    /// `Ok(vec![])` cannot tell which it got.
    fn scan_funding(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<ExecutionHistoryFundingRow>, HistoryStoreError> {
        let _ = range;
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=exec_funding (asked to scan {venue}:{symbol}) — \
             an empty result would be indistinguishable from a store that holds none"
        )))
    }

    // ---- a live mount's published forecasts (kind=exec_forecast, book W35) ---------------------------
    //
    // The account-plane series `vike_datahub::execution_stream` writes from the stream's `Forecast` records
    // ([`crate::ExecForecastRow`]), partitioned `venue+symbol+date` like the other account kinds. Both defaults
    // REFUSE, for `append_funding`'s reason: a store that cannot hold the kind must not answer as one that holds
    // none.

    /// Append a batch of published forecasts for `(venue, symbol)` (`kind=exec_forecast`). Returns rows written
    /// (0 if `commit_key` was already ingested or the batch is empty). The default REFUSES.
    fn append_forecasts(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecForecastRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let _ = (rows, commit_key);
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=exec_forecast (asked to append {venue}:{symbol}) — \n             nothing was written; use a store backend that implements append_forecasts"
        )))
    }

    /// Published forecasts for `(venue, symbol)` (`kind=exec_forecast`) in `range`, ts-ascending. Same in-memory
    /// `Vec` caveat as [`HistoryStore::scan_quotes`]. The default REFUSES, as [`HistoryStore::scan_funding`]'s does.
    fn scan_forecasts(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        let _ = range;
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=exec_forecast (asked to scan {venue}:{symbol}) — \n             an empty result would be indistinguishable from a store that holds none"
        )))
    }

    /// [`Self::scan_forecasts`] under a row budget — see [`Self::scan_quotes_capped`] for the contract and
    /// for why the DEFAULT delegates (it carries `scan_forecasts`' refusal). The datahub's
    /// `StrategyForecasts` (the strategy lane) and `ScanForecasts` (the data plane) reads go through it.
    fn scan_forecasts_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        let _ = budget;
        self.scan_forecasts(venue, symbol, range)
    }

    /// **The point-in-time read**: every mount's forecasts of `(venue, symbol)` DECIDED at or before
    /// `decision_ms` (`ts <= decision_ms`), from `start_ms` when given (`None` = the series' start),
    /// ts-ascending — what a decision taken at `decision_ms` could have known of the live models, and
    /// nothing decided after it. Derived over [`HistoryStore::scan_forecasts`], so it carries that
    /// verb's refusal; a `start_ms` after `decision_ms` is an empty window.
    fn forecasts_as_of(
        &self,
        venue: &str,
        symbol: &str,
        decision_ms: i64,
        start_ms: Option<i64>,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        self.scan_forecasts(
            venue,
            symbol,
            TimestampRange { start: start_ms, end: Some(decision_ms) },
        )
    }

    // ---- option-chain snapshots (kind=chain) -----------------------------------------------
    //
    // The point-in-time OPTIONS-SURFACE series: one row per instrument per snapshot, all rows of
    // one snapshot sharing `ts` (the chain's asof). Keyed `venue+symbol=<underlying>` like the tick
    // kinds (no interval) — a DISTINCT kind from every market series, so chain snapshots never
    // collide with an underlying's public prints even when `(venue, symbol)` match. Rows are
    // [`crate::ChainRow`]; the opt-in [`crate::ChainRecorder`] (`flags.record_chains`) is the
    // producer. Same batch-level `commit_key` idempotency contract as every other append (the
    // recorder keys per cadence bucket so a re-fetched chain within one bucket is a no-op).
    //
    // ⚠ DEFAULTED, BUT BOTH DEFAULTS REFUSE — and this paragraph said the opposite until the
    // sweep that carried `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md`
    // verdict 6 onto this kind's pair. It read: "a store that does not hold chains (e.g. a
    // bars-only test double) inherits the empty/no-op default … This keeps the seam additive
    // without forcing every existing impl to grow a stub." The additive property was real and the
    // cost was the funding collision wearing a second kind's name: `Ok(0)` is ALSO what a correct
    // already-ingested batch and a correct empty batch return, and `Ok(vec![])` is ALSO what a
    // store that HOLDS chains and has none in `range` returns. The seam stays additive — no impl
    // is forced to grow a stub — but a store that never implemented the kind now says so instead
    // of answering in the voice of one that looked.

    /// Append one chain snapshot's rows for `(venue, symbol=<underlying>)` (`kind=chain` series).
    /// Returns rows written — `0` meaning **`commit_key` was already ingested, or the batch was
    /// empty**, and those two alone.
    ///
    /// ⚠ **The default REFUSES rather than returning `Ok(0)`**, for
    /// `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` verdict 6's
    /// reasons applied to this kind. The write half matters as much as the read half and in one
    /// respect more: [`crate::ChainRecorder`]'s `record` logs a store error and drops the batch, so
    /// under the old `Ok(0)` a recorder aimed at a chain-less store had NOTHING to log — it
    /// recorded for hours, reported success every cadence bucket, and the first sign was an empty
    /// series. A backend that holds this kind overrides this method; one that does not now says so.
    fn append_chain_snapshot(
        &self,
        venue: &str,
        underlying: &str,
        rows: &[ChainRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let _ = (rows, commit_key);
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=chain (asked to append {venue}:{underlying}) — \
             nothing was written; use a store backend that implements append_chain_snapshot"
        )))
    }

    /// Chain snapshot rows for `(venue, symbol=<underlying>)` (`kind=chain`) in `range`,
    /// ts-ascending. Same in-memory `Vec` caveat as [`HistoryStore::scan_quotes`].
    ///
    /// ⚠ **The default REFUSES rather than returning an empty `Vec`** — the read twin of
    /// `append_chain_snapshot` above, and the half a caller is likelier to be misled by: an empty
    /// scan from a store that CANNOT hold the kind is not the same answer as an empty scan from one
    /// that can, and a caller handed `Ok(vec![])` cannot tell which it got. Same rule and same
    /// authority as [`HistoryStore::scan_depth`]'s and [`HistoryStore::list_series`]' defaults:
    /// `crates/vike-secrets/src/store/backend.rs`'s `CredentialStore` — **an ABSENT thing is the
    /// ordinary state and is silent; a thing that is PRESENT and unusable is an ERROR.** The absent
    /// thing here is the ROWS (an honest empty `Ok` from a store that looked); the unusable thing
    /// is the VERB.
    ///
    /// ⚠ **THE DEFAULT is what changed, so read this before inheriting it.** Two shapes, exactly as
    /// [`HistoryStore::scan_depth`] spells them: a store with no chain lane INHERITS this refusal, and
    /// that is right for it; a store that FRONTS another one must NOT — a delegating wrapper
    /// FORWARDS to its inner store, because "I hold no chains" is false for a type whose whole job
    /// is fronting a store that does. An RPC seam that cannot carry the kind over the wire
    /// overrides with its own reason (`crates/vike-datahub-client/src/remote.rs`'s
    /// `HistoryStoreDatahub` already does, and had to while this default was empty precisely because
    /// the empty claimed the SERVER holds no chains).
    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        let _ = range;
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=chain (asked to scan {venue}:{underlying}) — \
             an empty result would be indistinguishable from a store that holds none"
        )))
    }

    /// [`Self::scan_chain`] under a row budget — see [`Self::scan_quotes_capped`] for the contract
    /// and for why the DEFAULT delegates (and so inherits [`Self::scan_chain`]'s refusal). The data
    /// daemon's `ScanChain` reads through it: a chain series holds hundreds of rows per snapshot, so
    /// a page that decoded the whole rest of the range would be the quadratic read the three research
    /// reads were capped against. A chain series is per-underlying, never grouped, so there is one
    /// layout to walk.
    fn scan_chain_capped(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        let _ = budget;
        self.scan_chain(venue, underlying, range)
    }

    // ---- option trade log (kind=option_trade) ---------------------------------------------------
    //
    // An options venue's PUBLIC trade log with each trade's implied volatility, mark and index price
    // (`crate::option_trade_log`'s module doc), keyed `venue=…`/`symbol=<currency>`. DISTINCT from
    // `kind=trade`. Both defaults REFUSE, the `chain` pair's shape above and for its reasons.

    /// Append a batch of option trades for `(venue, symbol=<currency>)` (`kind=option_trade` series).
    /// Returns rows written — `0` meaning `commit_key` was already ingested, or the batch was empty.
    /// The default REFUSES: a store that does not hold this kind says so.
    fn append_option_trades(
        &self,
        venue: &str,
        currency: &str,
        rows: &[OptionTradeRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let _ = (rows, commit_key);
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=option_trade (asked to append {venue}:{currency}) — \
             nothing was written"
        )))
    }

    /// Option trades for `(venue, symbol=<currency>)` (`kind=option_trade`) in `range`, ts-ascending.
    /// Same in-memory `Vec` caveat as [`HistoryStore::scan_quotes`]. The default REFUSES, as
    /// [`HistoryStore::scan_chain`]'s does.
    fn scan_option_trades(
        &self,
        venue: &str,
        currency: &str,
        range: TimestampRange,
    ) -> Result<Vec<OptionTradeRow>, HistoryStoreError> {
        let _ = range;
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=option_trade (asked to scan {venue}:{currency}) — \
             an empty result would be indistinguishable from a store that holds none"
        )))
    }

    /// The first and last `ts` and the row count of [`Self::scan_option_trades`]' answer — the
    /// [`Self::bar_edges`] contract for this kind, and what the data daemon's `Backfill` reads back after
    /// the option-trades lane wrote. The default is that scan's; a store that can answer from the `ts`
    /// column alone overrides it, because a range can hold years of trades.
    fn option_trade_edges(
        &self,
        venue: &str,
        currency: &str,
        range: TimestampRange,
    ) -> Result<BarEdges, HistoryStoreError> {
        let rows = self.scan_option_trades(venue, currency, range)?;
        Ok(BarEdges {
            first_timestamp: rows.first().map(|r| r.ts),
            last_timestamp: rows.last().map(|r| r.ts),
            rows: rows.len() as u64,
        })
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
    // `crate::store::store_kind::STORE_KINDS`' `cohort` row carries the argument and the arithmetic behind
    // it. What it means for a CALLER of these two verbs is that one append is a whole fetch (every
    // label of one axis over a window), and one scan returns every axis, label, grading and basis
    // recorded for that asset — a caller wanting one of them filters columns, not series.
    //
    // ⚠ DEFAULTED, AND THESE DEFAULTS STILL ANSWER EMPTY — `cohort` and `perp_metrics` are the LAST
    // pair on this seam that do, and that is a DECLARED residual rather than an oversight. Every
    // other default that once answered in the voice of a store that had LOOKED now refuses:
    // `scan_depth`, `list_series`, `inventory` and `series_commits` first, then `exec_funding`'s
    // pair under `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md`
    // verdict 6, then `chain`'s pair above. The collision is IDENTICAL here: an empty `Vec` means
    // the series holds nothing in `range`, or the series does not exist, or this store cannot
    // answer the question at all.
    //
    // They were left because flipping them is a coordinated edit with its own argument rather than
    // the same edit again. `crates/vike-user-research/src/contract/context.rs`'s `cohort` and `perp_metrics`
    // verbs FORWARD these two straight to a sanctioned, published study surface, so the refusal
    // reaches user study code written against an `Ok(vec![])` it has always been given — a different
    // blast radius from the `chain` pair, which refused before any study could read it
    // (`StudyContext::chain` came later), so no caller was written against an empty answer.
    // `crates/vike-datahub-client/src/remote.rs`'s comment above its tick-level overrides argues from
    // this permissive default (an inherited `Ok(vec![])` reads as a confident empty), so the flip is
    // a two-crate edit. Do it as its own record, on the evidence of what those study callers do with an `Err`, not as a drive-by.
    // `crates/vike-data/src/test_support/stubs_tests.rs`'s `the_last_two_kinds_still_answer_empty_rather_than_refusing`
    // PINS the residual so it fails the day somebody closes it.

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
    ) -> Result<usize, HistoryStoreError> {
        let _ = (venue, asset, rows, commit_key);
        Ok(0)
    }

    /// Cohort marginals for `(venue, symbol=<asset>)` (`kind=cohort`) in `range`, ts-ascending —
    /// EVERY axis, label, grading and label basis recorded for that asset. Same in-memory `Vec`
    /// caveat as [`HistoryStore::scan_quotes`]. Default = empty.
    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        let _ = (venue, asset, range);
        Ok(Vec::new())
    }

    /// [`Self::scan_cohort`] under a row budget — see [`Self::scan_quotes_capped`] for the contract
    /// and for why the DEFAULT delegates. A cohort series is per-asset, never grouped, so there is
    /// one layout to walk; the budget counts rows, and one hour of one asset is many rows (one per
    /// axis, label, grading and label basis), every one of which a complete prefix keeps together.
    fn scan_cohort_capped(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        let _ = budget;
        self.scan_cohort(venue, asset, range)
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
    // [`vike_marketdata::Bar::funding`] in the `kind=bar` series under the reserved `interval=funding`
    // label, and a second stored copy is a duplicate that can disagree with the first.
    // [`crate::PerpMetricRow`]'s own doc is the authority on that split and on why open interest is
    // absent.
    //
    // DEFAULTED (like [`HistoryStore::append_cohort`]): a store that does not hold perp metrics (e.g.
    // a bars-only test double) inherits the empty/no-op default; the durable backends override with
    // real behavior. ⚠ It is the SECOND half of the declared residual the cohort block above argues
    // — read that argument there rather than inferring from this line that the empty is correct.

    /// Append a batch of perp market-context rows for `(venue, symbol)` (`kind=perp_metrics`
    /// series). Returns rows written (0 if `commit_key` was already ingested, the batch is empty,
    /// or this store does not hold perp metrics). Default = no-op.
    fn append_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[PerpMetricRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let _ = (venue, symbol, rows, commit_key);
        Ok(0)
    }

    /// Perp market-context rows for `(venue, symbol)` (`kind=perp_metrics`) in `range`,
    /// ts-ascending. Same in-memory `Vec` caveat as [`HistoryStore::scan_quotes`]. Default = empty.
    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        let _ = (venue, symbol, range);
        Ok(Vec::new())
    }

    /// [`Self::scan_perp_metrics`] under a row budget — see [`Self::scan_quotes_capped`] for the
    /// contract and for why the DEFAULT delegates. A perp-metrics series is per-symbol, never
    /// grouped, so there is one layout to walk.
    fn scan_perp_metrics_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        let _ = budget;
        self.scan_perp_metrics(venue, symbol, range)
    }

    // ---- text history (kind=text) ------------------------------------------------------------
    //
    // News items and economic-calendar events as this box RECEIVED them ([`crate::TextRow`]; its
    // module doc is the contract). Keyed `venue=<text kind>`/`symbol=<collector>`, and the stored `ts`
    // is the RECEIVED time, so the point-in-time read [`HistoryStore::texts_as_of`] is a plain range.
    // The datahub's text recorder (`flags.record_texts`) is the producer.
    //
    // ⚠ DEFAULTED, AND BOTH DEFAULTS REFUSE — born under the rule the `chain` pair above states,
    // never the `cohort` pair's declared residual: an empty `Ok` from a store that cannot hold texts
    // would read as "no news was received", which is a claim about the world.

    /// Append a batch of texts for `(text_kind, collector)` (`kind=text` series). Returns rows written
    /// — `0` meaning **`commit_key` was already ingested, or the batch was empty**, and those two
    /// alone. The default REFUSES (the `chain` pair's reason).
    fn append_texts(
        &self,
        text_kind: &str,
        collector: &str,
        rows: &[TextRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let _ = (rows, commit_key);
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=text (asked to append {text_kind}:{collector}) — \
             nothing was written; use a store backend that implements append_texts"
        )))
    }

    /// Texts of `(text_kind, collector)` (`kind=text`) RECEIVED in `range`, received-ascending; the
    /// rows of one append keep their written order. Same in-memory `Vec` caveat as
    /// [`HistoryStore::scan_quotes`]. The default REFUSES rather than answering empty.
    fn scan_texts(
        &self,
        text_kind: &str,
        collector: &str,
        range: TimestampRange,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        let _ = range;
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=text (asked to scan {text_kind}:{collector}) — \
             an empty result would be indistinguishable from a store that holds none"
        )))
    }

    /// [`Self::scan_texts`] under a row budget — see [`Self::scan_quotes_capped`] for the contract
    /// and for why the DEFAULT delegates (it carries `scan_texts`' refusal). A text series is one
    /// per `(text_kind, collector)`, never grouped, so there is one layout to walk; the rows of one
    /// append share a received time, and a complete prefix keeps them together.
    fn scan_texts_capped(
        &self,
        text_kind: &str,
        collector: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        let _ = budget;
        self.scan_texts(text_kind, collector, range)
    }

    /// **The point-in-time read**: every row of `(text_kind, collector)` RECEIVED in
    /// `[decision_ms - lookback_ms, decision_ms]` — what a decision taken at `decision_ms` could
    /// have read, and nothing received after it, whatever the row's `published_ms` says. Every
    /// revision of a calendar event is returned; the last row per [`crate::TextRow::text_id`] is the
    /// version known at `decision_ms`. A non-positive `lookback_ms` is the `decision_ms` instant only,
    /// and the subtraction saturates. Derived over [`HistoryStore::scan_texts`], so it carries that
    /// verb's refusal.
    fn texts_as_of(
        &self,
        text_kind: &str,
        collector: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        let from = decision_ms.saturating_sub(lookback_ms.max(0));
        self.scan_texts(text_kind, collector, TimestampRange::of(from, decision_ms))
    }

    // ---- SEC tables (kind=financial_fact, kind=institutional_holding) ------------------------
    //
    // Reported XBRL values and Form 13F lines ([`crate::FinancialFactRow`],
    // [`crate::InstitutionalHoldingRow`]; `crate::sec_log`'s module doc is the contract). Keyed
    // `venue=sec`/`symbol=<CIK>`, and the stored `ts` is the instant a row became KNOWABLE, so each
    // point-in-time read is a plain range. The SEC import (`vike-cli data history import`) is the
    // producer.
    //
    // ⚠ DEFAULTED, AND EVERY DEFAULT REFUSES — the `text` pair's rule: an empty `Ok` from a store that
    // cannot hold the kind would read as "this company reported nothing".

    /// Append a batch of reported values for `(venue, symbol)` (`kind=financial_fact`). Returns rows
    /// written — `0` meaning **`commit_key` was already ingested, or the batch was empty**. The default
    /// REFUSES.
    fn append_financial_facts(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[FinancialFactRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let _ = (rows, commit_key);
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=financial_fact (asked to append {venue}:{symbol}) — \
             nothing was written; use a store backend that implements append_financial_facts"
        )))
    }

    /// Reported values of `(venue, symbol)` (`kind=financial_fact`) KNOWABLE in `range`, ascending; the
    /// rows of one append keep their written order. The default REFUSES rather than answering empty.
    fn scan_financial_facts(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        let _ = range;
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=financial_fact (asked to scan {venue}:{symbol}) — \
             an empty result would be indistinguishable from a store that holds none"
        )))
    }

    /// [`Self::scan_financial_facts`] under a row budget — see [`Self::scan_quotes_capped`] for the
    /// contract; the DEFAULT delegates (it carries the scan's refusal).
    fn scan_financial_facts_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        let _ = budget;
        self.scan_financial_facts(venue, symbol, range)
    }

    /// **The point-in-time read** of reported values: every row of `(venue, symbol)` KNOWABLE in
    /// `[decision_ms - lookback_ms, decision_ms]` — nothing filed after the decision, whatever period
    /// it describes. A restated value is a later row of the same concept and period: the last one per
    /// `(concept, unit, period_start, period_end)` is the value known at `decision_ms`. A
    /// non-positive `lookback_ms` is the `decision_ms` instant only. Derived over
    /// [`HistoryStore::scan_financial_facts`], so it carries that verb's refusal.
    fn financial_facts_as_of(
        &self,
        venue: &str,
        symbol: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        let from = decision_ms.saturating_sub(lookback_ms.max(0));
        self.scan_financial_facts(venue, symbol, TimestampRange::of(from, decision_ms))
    }

    /// Append a batch of Form 13F lines for `(venue, symbol)` (`kind=institutional_holding`). Returns
    /// rows written — `0` meaning **`commit_key` was already ingested, or the batch was empty**. The
    /// default REFUSES.
    fn append_institutional_holdings(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[InstitutionalHoldingRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let _ = (rows, commit_key);
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=institutional_holding (asked to append {venue}:{symbol}) \
             — nothing was written; use a store backend that implements \
             append_institutional_holdings"
        )))
    }

    /// Form 13F lines of `(venue, symbol)` (`kind=institutional_holding`) KNOWABLE in `range`,
    /// ascending; the rows of one append keep their written order. The default REFUSES rather than
    /// answering empty.
    fn scan_institutional_holdings(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        let _ = range;
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=institutional_holding (asked to scan {venue}:{symbol}) \
             — an empty result would be indistinguishable from a store that holds none"
        )))
    }

    /// [`Self::scan_institutional_holdings`] under a row budget — see [`Self::scan_quotes_capped`]
    /// for the contract; the DEFAULT delegates (it carries the scan's refusal).
    fn scan_institutional_holdings_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        let _ = budget;
        self.scan_institutional_holdings(venue, symbol, range)
    }

    /// **The point-in-time read** of a manager's 13F lines: every row of `(venue, symbol)` KNOWABLE
    /// in `[decision_ms - lookback_ms, decision_ms]`. The manager's latest report known at
    /// `decision_ms` is the rows of its latest `period`, with a `RESTATEMENT` amendment replacing the
    /// original's lines and a `NEW HOLDINGS` amendment adding to them. A non-positive `lookback_ms` is
    /// the `decision_ms` instant only. Derived over [`HistoryStore::scan_institutional_holdings`], so
    /// it carries that verb's refusal.
    fn institutional_holdings_as_of(
        &self,
        venue: &str,
        symbol: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        let from = decision_ms.saturating_sub(lookback_ms.max(0));
        self.scan_institutional_holdings(venue, symbol, TimestampRange::of(from, decision_ms))
    }

    // ---- text scores (kind=text_score) -------------------------------------------------------
    //
    // A host-side model's numbers for the stored texts ([`crate::TextScoreRow`]; its module doc is the
    // contract). Keyed `venue=<text kind>`/`symbol=<model name>`, and the stored `ts` is the SCORED
    // TEXT's received time, so [`HistoryStore::text_scores_as_of`] selects on the clock
    // [`HistoryStore::texts_as_of`] does. Both defaults REFUSE, for the reason the text pair above gives.

    /// Append a batch of scores for `(text_kind, model_name)` (`kind=text_score` series). Returns rows
    /// written — `0` meaning **`commit_key` was already ingested, or the batch was empty**, and those
    /// two alone. The default REFUSES.
    fn append_text_scores(
        &self,
        text_kind: &str,
        model_name: &str,
        rows: &[TextScoreRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let _ = (rows, commit_key);
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=text_score (asked to append {text_kind}:{model_name}) — \
             nothing was written; use a store backend that implements append_text_scores"
        )))
    }

    /// Scores of `(text_kind, model_name)` (`kind=text_score`) whose TEXT was received in `range`,
    /// received-ascending; the rows of one append keep their written order. Same in-memory `Vec`
    /// caveat as [`HistoryStore::scan_quotes`]. The default REFUSES rather than answering empty.
    fn scan_text_scores(
        &self,
        text_kind: &str,
        model_name: &str,
        range: TimestampRange,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        let _ = range;
        Err(HistoryStoreError::Unsupported(format!(
            "this store does not hold kind=text_score (asked to scan {text_kind}:{model_name}) — \
             an empty result would be indistinguishable from a store that holds none"
        )))
    }

    /// [`Self::scan_text_scores`] under a row budget — [`Self::scan_texts_capped`]'s twin, and its
    /// default delegates for the same reason (it carries `scan_text_scores`' refusal).
    fn scan_text_scores_capped(
        &self,
        text_kind: &str,
        model_name: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        let _ = budget;
        self.scan_text_scores(text_kind, model_name, range)
    }

    /// **The point-in-time read of the scores**: every score of `(text_kind, model_name)` whose text
    /// was RECEIVED in `[decision_ms - lookback_ms, decision_ms]`, the window
    /// [`HistoryStore::texts_as_of`] reads for the texts themselves. Same lookback rules; derived over
    /// [`HistoryStore::scan_text_scores`], so it carries that verb's refusal.
    fn text_scores_as_of(
        &self,
        text_kind: &str,
        model_name: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        let from = decision_ms.saturating_sub(lookback_ms.max(0));
        self.scan_text_scores(text_kind, model_name, TimestampRange::of(from, decision_ms))
    }

    /// Point-in-time chain lookup: for each instrument, the most-recent row observed at or before
    /// `ts` — i.e. the latest snapshot state per instrument (the [`HistoryStore::properties_as_of`]
    /// analog for the options surface, defaulted over [`HistoryStore::scan_chain`] the same way).
    ///
    /// Per-INSTRUMENT latest (not "rows at the single latest snapshot ts") by design: a full
    /// surface is recorded as one chain per expiry, each with its own `asof_ms`, so the latest-ts
    /// snapshot alone would hold only one expiry. Consequence: an instrument that stopped being
    /// observed (delisted/expired) still surfaces with its LAST recorded row — callers wanting
    /// live-only contracts filter `expiry_ms > ts`. Returned sorted `(expiry_ms, strike,
    /// instrument)` — deterministic; empty if nothing was recorded at or before `ts`.
    ///
    /// ⚠ **An empty `Ok` here now means what it says.** This verb is derived over
    /// [`HistoryStore::scan_chain`], whose default REFUSES, so a store that cannot hold chains
    /// propagates `HistoryStoreError::Unsupported` instead of an empty surface. It used to inherit the
    /// empty and hand a caller a confident "nothing was recorded" built out of a capability gap.
    ///
    /// ⚠ COST: this is the UNBOUNDED form — it scans and materializes EVERY chain row ever recorded
    /// at or before `ts`, so one lookup grows linearly with archive age. That is fine for the
    /// `kind=properties` shape it is modelled on (~1 row/day/symbol) but NOT for chains: at the
    /// recorder's 1-minute default cadence one underlying is ~hundreds of rows/minute, so a month of
    /// recording is tens of millions of rows decoded PER CALL. Prefer
    /// [`HistoryStore::chain_as_of_within`], which bounds the scan to a lookback window; reach for this
    /// form only when an arbitrarily stale last-observation genuinely matters (or pair it with
    /// `vike-store`'s `hist_maint` retention so the series stays small).
    fn chain_as_of(
        &self,
        venue: &str,
        underlying: &str,
        ts: i64,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        let rows = self.scan_chain(venue, underlying, TimestampRange::of(i64::MIN, ts))?;
        Ok(fold_chain_as_of(rows))
    }

    /// Bounded [`HistoryStore::chain_as_of`]: identical per-instrument-latest semantics and ordering,
    /// but the scan starts at `ts - lookback_ms` instead of the beginning of time — so the cost is
    /// proportional to the WINDOW, not to how long the store has been recording. This is the form
    /// backtests and surface reads should use.
    ///
    /// Pick `lookback_ms` as a few recorder cadence periods (the recorder's default is 1 minute, so
    /// e.g. 5–15 minutes): an instrument the venue stopped quoting inside the window is stale by the
    /// documented delisting semantics anyway. An instrument NOT observed within the window is simply
    /// absent from the result — the difference from the unbounded form, and the whole point.
    /// A non-positive `lookback_ms` yields the `ts`-instant only; the subtraction saturates, so a
    /// huge lookback degrades to (and matches) [`HistoryStore::chain_as_of`] rather than overflowing.
    /// It carries [`HistoryStore::chain_as_of`]'s refusal note unchanged — the two differ only in the
    /// scan range they hand [`HistoryStore::scan_chain`], so both propagate its default.
    fn chain_as_of_within(
        &self,
        venue: &str,
        underlying: &str,
        ts: i64,
        lookback_ms: i64,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        let from = ts.saturating_sub(lookback_ms.max(0));
        let rows = self.scan_chain(venue, underlying, TimestampRange::of(from, ts))?;
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
        range: TimestampRange,
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError>;

    /// Resample stored TRADES → OHLCV bars (trade price) for `(venue, symbol, interval)`.
    fn resample_trades_to_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError>;
}

/// The shared per-instrument-latest fold behind [`HistoryStore::chain_as_of`] and
/// [`HistoryStore::chain_as_of_within`] — the ONE place the PIT semantics live, so the bounded and
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

#[path = "hist_tests.rs"]
#[cfg(test)]
mod hist_tests;
