//! [`RemoteHistStore`] — a READ-ONLY [`vike_data::HistStore`] backed by a `vike-datahub` server
//! over the [`crate::proto`] RPC protocol.
//!
//! # What it is
//!
//! It implements the `HistStore` READ verbs — [`HistStore::load_bars`],
//! [`HistStore::scan_quotes`], [`HistStore::scan_trades`], [`HistStore::properties_as_of`], the
//! store-metadata verbs [`HistStore::list_series`], [`HistStore::inventory`],
//! [`HistStore::series_gaps`], [`HistStore::series_facts`] and [`HistStore::coverage_report`] (the
//! Data-Manager / Studio catalog), and the SIX tick-level and research reads
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` asked the wire to grow
//! ([`HistStore::scan_book_updates`], [`HistStore::scan_depth`], [`HistStore::scan_cohort`],
//! [`HistStore::scan_perp_metrics`], [`HistStore::scan_equity`], [`HistStore::scan_exec_fills`]) —
//! by calling the matching [`DatahubClient`] verb. A consumer can therefore read through the
//! ordinary `Arc<dyn HistStore + Send + Sync>` seam WITHOUT linking the Arrow/DataFusion engine,
//! which lives only on the server.
//!
//! # Read-only by design
//!
//! Every WRITE verb (`append_*`, `resample_*_to_bars`) and every read verb NOT in the served set
//! (`scan_symbol_properties`, `scan_exec_orders`, `scan_funding`, the `chain_*` family) returns an
//! `Err` (`unsupported`), NOT a defaulted empty `Ok`: an empty `Ok` would let a caller mistake
//! "the RPC store cannot answer this" for "there is genuinely no data".
//!
//! # Connection model
//!
//! `with_client` REUSES one connection per handle (the `conn` field's doc), dialling only when
//! there is none, and SERIALIZES every reader of that handle on it — the protocol is positional,
//! one frame out and one back, so one handle never overlaps two reads. A caller that wants reads in
//! flight together builds several handles, each owning a connection and dialling on first use
//! (`crates/vike-app-core/src/data/stored_load.rs`'s `PROBE_WORKERS` is that caller).

use std::sync::Mutex;

use vike_data::{
    ChainRow, CohortRow, DataError, ExecFillRow, ExecOrderRow, FundingRow, HistStore,
    InstrumentCoverage, PerpMetricRow, SeriesCoverage, SeriesId, TsRange,
};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

use crate::client::DatahubClient;
use crate::proto::FEATURE_SCAN_LIMIT;
use vike_node_proto::auth::{NodeKeys, Scope};

/// A READ-ONLY [`HistStore`] served by a `vike-datahub` server over the datahub RPC protocol. See
/// the [module docs](crate::remote): the GUI read verbs, the store-metadata verbs
/// (`list_series`/`inventory`/`series_gaps`/`coverage_report`) and 0084's six tick-level and
/// research reads are served; every other method returns [`DataError::Query`] with a "not served
/// over RPC" message.
pub struct RemoteHistStore {
    /// The datahub server address (e.g. `"127.0.0.1:7878"`).
    addr: String,
    /// The node keys every dialled connection authenticates with, or `None` for the unauthenticated
    /// connect this type has always done. See [`RemoteHistStore::with_keys`].
    keys: Option<NodeKeys>,
    /// The live connection, REUSED across reads — see [`RemoteHistStore::with_client`].
    ///
    /// ⚠ A `Mutex` because [`HistStore`] methods take `&self` behind
    /// `Arc<dyn HistStore + Send + Sync>`. Its SERIALIZATION of concurrent readers is the
    /// correctness property, not a side effect: on a POSITIONAL protocol two threads sharing a
    /// connection would read each other's replies.
    conn: Mutex<Option<DatahubClient>>,
}

impl RemoteHistStore {
    /// Point at a datahub server `addr` (e.g. `"127.0.0.1:7878"`). No connection is opened here —
    /// the first read dials one and later reads reuse it (the module docs' connection model).
    ///
    /// Unauthenticated: correct against a key-less server. Against a KEYED server every read fails
    /// with the connect error naming the missing keys — use [`Self::with_keys`].
    pub fn new(addr: impl Into<String>) -> Self {
        Self { addr: addr.into(), keys: None, conn: Mutex::new(None) }
    }

    /// Point at a datahub server and AUTHENTICATE every dialled connection under
    /// [`Scope::Read`].
    ///
    /// ⚠ The scope is not a parameter, by design: this type is READ-ONLY, and
    /// [`crate::proto::required_scope`] puts every verb it sends in the Observe column, so a
    /// control-scoped connection would grant strictly more than the type can spend. A caller
    /// needing the Control verbs (`Backfill`, the `Run*` compute) uses
    /// [`DatahubClient::connect_authed`] directly.
    ///
    /// `keys` needs only its observe key; one whose observe key is absent fails every dial at the
    /// handshake, the safe direction. Against a KEY-LESS server this degrades to a plain
    /// unauthenticated connect, so one configured GUI works against a keyed production datahub and
    /// a local dev one.
    pub fn with_keys(addr: impl Into<String>, keys: NodeKeys) -> Self {
        Self { addr: addr.into(), keys: Some(keys), conn: Mutex::new(None) }
    }

    /// Dial ONE new connection. A connect failure maps to [`DataError::Io`] — a genuine transport
    /// failure (the server is down / unreachable) or an authentication refusal — distinct from the
    /// [`DataError::Query`] a *served* verb returns on a server-side error.
    fn dial(&self) -> Result<DatahubClient, DataError> {
        match &self.keys {
            Some(keys) => DatahubClient::connect_authed(&self.addr, keys, Scope::Read),
            None => DatahubClient::connect(&self.addr),
        }
        .map_err(|e| DataError::Io(e.to_string()))
    }

    /// Rows per page when the server advertises [`FEATURE_SCAN_LIMIT`].
    ///
    /// ⚠ **A ROW count cannot bound BYTES, and this constant is the whole of the guard.** 10 000
    /// trades are well under [`crate::proto::MAX_FRAME_LEN`]; 10 000 full L2 snapshots are not. The
    /// wire cap bounds ROWS because that is what the server can count without serializing first,
    /// so a byte-aware cap is a residual this number only makes unlikely. Conservative rather than
    /// tuned: a smaller page costs round trips, a larger one a refused frame.
    const PAGE_ROWS: u32 = 10_000;

    /// Run one range scan as a SEQUENCE of capped pages over the reused connection.
    ///
    /// ⚠ **The continuation is `last_ts + 1`, and it is only correct because the server's cap is
    /// SOFT.** `crates/vike-datahub/src/server/range_reads.rs`'s `cap_to_whole_ts` stops on a
    /// whole-`ts` boundary so this line cannot skip rows: a page cut mid-`ts` would leave the rest
    /// of that timestamp beyond the next `start`, lost with no error. Neither half is safe alone.
    ///
    /// ⚠ **The loop ends on an EMPTY page, not on a short one.** A short page is the ordinary
    /// straddle case (the server stopped early to keep a `ts` whole), so treating it as the end
    /// would truncate silently. The cost is ONE trailing request per scan, the price of having no
    /// cursor in the reply (see [`FEATURE_SCAN_LIMIT`]).
    ///
    /// ⚠ **Stopping on an empty page is safe only because of the STORE's contract**, which this
    /// loop cannot check: `vike_data::HistStore::scan_quotes_capped` promises a complete prefix
    /// holding AT LEAST the budget's rows unless it is the whole range. A datahub older than that
    /// promise could end a GROUPED series early or step past rows with no error, and no client can
    /// detect it (`docs/superpowers/specs/2026-10-01-tick-scan-paging-silent-loss-design.md`) —
    /// upgrading the datahub is the whole repair.
    ///
    /// ⚠ **A server that does NOT advertise the cap is asked ONCE, unbounded**: it IGNORES an
    /// unknown `limit` (serde skips unknown fields), so paging it would multiply one oversized
    /// frame by the number of pages.
    ///
    /// ⚠ Paging bounds the FRAME and the server's write, not the caller's `Vec`: every page is
    /// accumulated, because `HistStore` returns the whole range.
    ///
    /// Each page runs from `last_ts + 1` to the END of the range, so a server that loaded that
    /// remainder and then capped it would make a full read QUADRATIC. Every range verb passes the
    /// page size down as a read budget (`HistStore::load_bars_head` for bars), so no page costs the
    /// server more than the page plus at most one storage block of the symbol's rows.
    fn paged<T>(
        &self,
        range: TsRange,
        ts_of: impl Fn(&T) -> i64,
        mut fetch: impl FnMut(&mut DatahubClient, TsRange, Option<u32>) -> Result<Vec<T>, String>,
    ) -> Result<Vec<T>, DataError> {
        let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut client = match guard.take() {
            Some(c) => c,
            None => self.dial()?,
        };
        let capped = client.features().iter().any(|f| f == FEATURE_SCAN_LIMIT);

        let mut out: Vec<T> = Vec::new();
        let mut start = range.start;
        loop {
            let limit = capped.then_some(Self::PAGE_ROWS);
            let page = match fetch(&mut client, TsRange { start, end: range.end }, limit) {
                Ok(page) => page,
                // The connection is NOT returned to the slot — `with_client` argues why.
                Err(e) => return Err(DataError::Query(e)),
            };
            let Some(last) = page.last().map(&ts_of) else { break };
            out.extend(page);
            if !capped {
                break;
            }
            if range.end.is_some_and(|e| last >= e) {
                break;
            }
            // A `ts` at the very top of the range has nowhere to continue to, and `last + 1` would
            // wrap. Stop rather than re-ask from `i64::MIN`.
            let Some(next) = last.checked_add(1) else { break };
            start = Some(next);
        }
        *guard = Some(client);
        Ok(out)
    }

    /// Run one verb over the REUSED connection, dialling only when there is none.
    ///
    /// Reuse matters because this is the seam every reader goes through
    /// (`docs/decisions/0084-only-the-datahub-touches-the-store.md`):
    /// `crates/vike-report/src/excursions.rs`'s `backfill_excursions` calls `load_bars` once per
    /// TRADE, and a dial per read would be a TCP connect plus a handshake (an HMAC round trip on a
    /// keyed server) per call.
    ///
    /// ⚠ **Any error DROPS the connection**, deliberately. The client collapses a post-connect
    /// transport failure, a server-side `Response::Error` and a `protocol desync` into one
    /// `Err(String)`, and only the middle one leaves the socket usable; since they are
    /// indistinguishable HERE, keeping the connection risks answering the NEXT verb with this one's
    /// frame. A redial costs one round trip; a desynced read returns plausible data for the wrong
    /// question.
    fn with_client<T>(
        &self,
        verb: impl FnOnce(&mut DatahubClient) -> Result<T, String>,
    ) -> Result<T, DataError> {
        let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut client = match guard.take() {
            Some(c) => c,
            None => self.dial()?,
        };
        match verb(&mut client) {
            Ok(value) => {
                *guard = Some(client);
                Ok(value)
            }
            // The connection is NOT returned to the slot — see the doc above.
            Err(e) => Err(DataError::Query(e)),
        }
    }

    /// **The history-channels read, over this store's connection** — `Request::HistoryChannels`,
    /// for the Data Manager's HISTORY column. Not a `HistStore` verb (not a question about stored
    /// rows), so an inherent method on the handle the GUI already holds, under the same Observe key.
    ///
    /// `Ok(None)` is a datahub OLDER than the read: it does not advertise
    /// [`crate::proto::FEATURE_HISTORY_CHANNELS`], nothing was sent, and the caller renders its own
    /// compiled table instead (`crate::history::compiled_report`). An `Err` is an unreachable store
    /// or a read that failed.
    pub fn history_channels(
        &self,
    ) -> Result<Option<crate::history::HistoryChannelsReport>, DataError> {
        self.with_client(|client| {
            if !client.serves_history_channels() {
                return Ok(None);
            }
            client.history_channels().map(Some)
        })
    }
}

/// The `Err` an unsupported (write / non-served-read) verb returns.
///
/// [`DataError::Query`], the nearest "this store cannot answer that" case (`DataError` has no
/// "unsupported" variant); `Io` stays reserved for a connect failure, raised by
/// [`RemoteHistStore::dial`]. The message names the method so a log line is self-explanatory.
fn unsupported(method: &str) -> DataError {
    DataError::Query(format!("RemoteHistStore is read-only over RPC; {method} is not served"))
}

impl HistStore for RemoteHistStore {
    // ---- served read verbs, answered over RPC ---------------------------------------------------
    //
    // Range reads go through `paged`, the rest through `with_client`; both REUSE one connection.
    // The client's ONE `Err(String)` channel maps to `DataError::Query`; a connect failure is
    // `DataError::Io`, raised by `dial`.

    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        self.paged(range, |r: &Bar| r.ts, |c, w, n| c.load_bars(venue, symbol, interval, w, n))
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        self.paged(range, |r: &QuoteTick| r.ts, |c, w, n| c.scan_quotes(venue, symbol, w, n))
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        self.paged(range, |r: &TradeTick| r.ts, |c, w, n| c.scan_trades(venue, symbol, w, n))
    }

    // ---- 0084's six: the tick-level and research reads ------------------------------------------
    //
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md` gives the store ONE reader; these
    // impls make obeying it possible. ⚠ Each must be an explicit override answered from the
    // SERVER's rows: an inherited trait default that answers `Ok(vec![])` is a confident EMPTY
    // indistinguishable from a store that holds none (`scan_perp_metrics` once was exactly that).

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        self.paged(range, |r: &BookUpdate| r.ts, |c, w, n| c.scan_book_updates(venue, symbol, w, n))
    }

    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        self.paged(range, |r: &BookUpdate| r.ts, |c, w, n| c.scan_depth(venue, symbol, w, n))
    }

    /// ⚠ The second argument is an ASSET, not a symbol — the trait's own spelling, kept here so a
    /// caller cannot pass one where the other belongs.
    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TsRange,
    ) -> Result<Vec<CohortRow>, DataError> {
        self.paged(range, |r: &CohortRow| r.ts, |c, w, n| c.scan_cohort(venue, asset, w, n))
    }

    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        self.paged(
            range,
            |r: &PerpMetricRow| r.ts,
            |c, w, n| c.scan_perp_metrics(venue, symbol, w, n),
        )
    }

    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError> {
        self.paged(range, |r: &EquitySample| r.ts, |c, w, n| c.scan_equity(venue, symbol, w, n))
    }

    /// ⚠ No range, because `HistStore::scan_exec_fills` takes none — see
    /// [`crate::proto::Request::ScanExecFills`].
    fn scan_exec_fills(&self, venue: &str, symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        self.with_client(|c| c.scan_exec_fills(venue, symbol))
    }

    fn properties_as_of(
        &self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, DataError> {
        self.with_client(|c| c.properties_as_of(venue, symbol, ts))
    }

    // ---- served store-metadata verbs (the Data-Manager / Studio catalog) ------------------------
    //
    // These OVERRIDE refusing trait defaults with real RPC answers, safe to serve whole because the
    // catalog is TINY (a series list + cheap manifest-fold coverage, NO Parquet scan): unlike a tick
    // SLICE it never streams the tape to the client.

    fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
        self.with_client(|c| c.list_series())
    }

    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
        self.with_client(|c| c.inventory())
    }

    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, DataError> {
        self.with_client(|c| c.series_gaps(id))
    }

    /// ⚠ The trait DEFAULT refuses (a store with no per-series manifest has no answer); this
    /// override stops a routed reader inheriting that refusal about a server which knows.
    fn series_facts(&self, id: &SeriesId) -> Result<(SeriesCoverage, Vec<String>), DataError> {
        self.with_client(|c| c.series_facts(id))
    }

    // ⚠ The trait DEFAULT is still `Ok(vec![])` for THIS verb (its trait doc carries the
    // vacuous-truth argument and the FRONTS warning this override answers) — right for a store that
    // cannot join its kinds, a LIE here: an inherited empty would render the Data Manager's Partial
    // column blank and say "nothing is partial".
    //
    // An OLD server's refusal arrives through this same `Err`: `DatahubClient::coverage_report`
    // refuses without sending when `FEATURE_COVERAGE` is absent, and
    // `vike_app_core::data::stored_mode` turns that `DataError::Query` into its third state.
    fn coverage_report(&self) -> Result<Vec<InstrumentCoverage>, DataError> {
        self.with_client(|c| c.coverage_report())
    }

    // ---- unsupported: write verbs (all append_*, resample_*) ------------------------------------

    fn append_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _bars: &[Bar],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("append_bars"))
    }

    fn append_quotes(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[QuoteTick],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("append_quotes"))
    }

    fn append_trades(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[TradeTick],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("append_trades"))
    }

    fn append_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        _updates: &[BookUpdate],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("append_book_updates"))
    }

    fn append_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[(i64, SymbolProperties)],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("append_symbol_properties"))
    }

    fn append_equity(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[EquitySample],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("append_equity"))
    }

    fn append_exec_fills(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecFillRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("append_exec_fills"))
    }

    fn append_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecOrderRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("append_exec_orders"))
    }

    fn append_funding(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[FundingRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("append_funding"))
    }

    fn append_chain_snapshot(
        &self,
        _venue: &str,
        _underlying: &str,
        _rows: &[ChainRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("append_chain_snapshot"))
    }

    /// ⚠ An OVERRIDE of a permissive default, not a stub: `HistStore::append_cohort` defaults to
    /// `Ok(0)`, which here would tell a producer its batch was a duplicate rather than never sent.
    fn append_cohort(
        &self,
        _venue: &str,
        _asset: &str,
        _rows: &[CohortRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("append_cohort"))
    }

    fn resample_quotes_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("resample_quotes_to_bars"))
    }

    fn resample_trades_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(unsupported("resample_trades_to_bars"))
    }

    // ---- unsupported: read verbs NOT in the served set ------------------------------------------
    //
    // These OVERRIDE trait defaults that would answer an empty `Ok`: `Err` rather than a
    // fabricated "no data" (the module docs' read-only rationale).

    fn scan_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Err(unsupported("scan_symbol_properties"))
    }

    fn scan_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, DataError> {
        Err(unsupported("scan_exec_orders"))
    }

    fn scan_funding(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<FundingRow>, DataError> {
        Err(unsupported("scan_funding"))
    }

    fn scan_chain(
        &self,
        _venue: &str,
        _underlying: &str,
        _range: TsRange,
    ) -> Result<Vec<ChainRow>, DataError> {
        Err(unsupported("scan_chain"))
    }

    fn chain_as_of(
        &self,
        _venue: &str,
        _underlying: &str,
        _ts: i64,
    ) -> Result<Vec<ChainRow>, DataError> {
        Err(unsupported("chain_as_of"))
    }

    fn chain_as_of_within(
        &self,
        _venue: &str,
        _underlying: &str,
        _ts: i64,
        _lookback_ms: i64,
    ) -> Result<Vec<ChainRow>, DataError> {
        Err(unsupported("chain_as_of_within"))
    }
}
