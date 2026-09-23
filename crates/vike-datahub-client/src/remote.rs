//! [`RemoteHistStore`] — a READ-ONLY [`vike_data::HistStore`] backed by a `vike-datahub` server
//! over the [`crate::proto`] RPC protocol.
//!
//! # What it is
//!
//! The Phase-2 enabler for the DataFusion-free GUI. It implements the GUI-relevant `HistStore` READ
//! verbs — [`HistStore::load_bars`], [`HistStore::scan_quotes`], [`HistStore::scan_trades`],
//! [`HistStore::properties_as_of`], plus the PR-6 store-metadata verbs [`HistStore::list_series`],
//! [`HistStore::inventory`], [`HistStore::series_gaps`] and their §6-Q2 sibling
//! [`HistStore::coverage_report`] (the Data-Manager / Studio catalog), plus the SIX tick-level and
//! research reads `docs/decisions/0084-only-the-datahub-touches-the-store.md` asked the wire to
//! grow ([`HistStore::scan_book_updates`], [`HistStore::scan_depth`], [`HistStore::scan_cohort`],
//! [`HistStore::scan_perp_metrics`], [`HistStore::scan_equity`], [`HistStore::scan_exec_fills`]) — by
//! dialling a datahub server and calling the matching [`DatahubClient`] verb. A consumer that only
//! needs to READ history (a chart) or browse the catalog can therefore point
//! at a remote store through the ordinary `Arc<dyn HistStore + Send + Sync>` seam WITHOUT linking
//! the Arrow/DataFusion engine — that engine lives only on the server. (This crate depends on
//! vike-data with DEFAULT features, i.e. the `HistStore` TRAIT only, so the type is DataFusion-free
//! by construction.)
//!
//! # Read-only by design
//!
//! This is a READ client. Every WRITE verb (`append_*`, `resample_*_to_bars`) and every read verb
//! still NOT in the served set (`scan_symbol_properties`, `scan_exec_orders`, `scan_funding`, and
//! the whole `chain_*` family) returns an `Err` — see
//! [`unsupported`] — rather than succeeding. Returning `Err`, NOT a defaulted empty `Ok`, is
//! deliberate: an empty `Ok` would let a caller mistake "the RPC store cannot answer this" for
//! "there is genuinely no data". In this phase the GUI's writes go through the LOCAL recorder, never
//! this seam; only reads cross the wire.
//!
//! # Connection model
//!
//! Each read verb opens a FRESH [`DatahubClient`] connection: `HistStore` methods take `&self`, and
//! one `DatahubClient` owns one non-`Sync` `TcpStream`, so a connect-per-call keeps
//! `RemoteHistStore` trivially `Send + Sync` (it holds only a `String` address) and drops it into
//! `Arc<dyn HistStore + Send + Sync>` like any other store. The cost is one connect per read; for
//! the GUI's coarse-grained history reads (a chart's visible range) that is the right trade, and a
//! connection-pooled variant can come later if a hot read path ever needs one.

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
    /// ⚠ A `Mutex` rather than a `RefCell` because [`HistStore`] methods take `&self` and this type
    /// lives behind `Arc<dyn HistStore + Send + Sync>`; a `Mutex` is what keeps it `Sync` while
    /// letting a read mutate it. It also SERIALIZES concurrent readers of one handle, which is not
    /// a limitation being accepted quietly: this protocol is POSITIONAL (one frame out, one frame
    /// back on a socket), so two threads sharing a connection would read each other's replies. The
    /// serialization is the correctness property, not a side effect.
    conn: Mutex<Option<DatahubClient>>,
}

impl RemoteHistStore {
    /// Point at a datahub server `addr` (e.g. `"127.0.0.1:7878"`). No connection is opened here —
    /// each read verb dials a fresh [`DatahubClient`] (see the module docs' connection model).
    ///
    /// Unauthenticated: correct against a key-less server (the default, and every deployment before
    /// `docs/decisions/0025-datahub-remote-posture.md` was adopted). Against a KEYED server every
    /// read fails with the connect error naming the missing keys — use [`Self::with_keys`].
    pub fn new(addr: impl Into<String>) -> Self {
        Self { addr: addr.into(), keys: None, conn: Mutex::new(None) }
    }

    /// Point at a datahub server and AUTHENTICATE every dialled connection under
    /// [`Scope::Read`].
    ///
    /// ⚠ Observe is not a parameter, and that is the design: this type is a READ-ONLY
    /// [`HistStore`] — every write verb and every non-served read already returns [`unsupported`] —
    /// so it has no use for a control key and should not carry one. `vike_datahub::server`'s
    /// `required_scope` puts every verb this type sends in the Observe column, so a control-scoped
    /// connection here would grant strictly more than the type can spend. A caller needing the
    /// Control verbs (`Backfill`, the `Run*` compute) reaches for
    /// [`DatahubClient::connect_authed`] directly.
    ///
    /// `keys` needs only its observe key populated; a `NodeKeys` whose observe key is absent will
    /// fail every dial at the handshake (the closed-gate shape), which is the safe direction.
    ///
    /// Against a KEY-LESS server this degrades to a plain unauthenticated connect (see
    /// [`DatahubClient::connect_authed`]), so one configured GUI works against both a keyed
    /// production datahub and a local dev one.
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

    /// Run one verb over the REUSED connection, dialling only when there is none.
    ///
    /// ⚠ **This type dialled a FRESH connection per read until 2026-09-22, and the cost was not
    /// theoretical.** `crates/vike-report/src/excursions.rs`'s `backfill_excursions` calls
    /// `load_bars` once per TRADE, so a 500-trade journal was 500 TCP connects plus 500 handshakes
    /// — and on a keyed server, 500 HMAC challenge round trips. A parameter sweep is the same shape
    /// one order of magnitude up: a 200-trial search over three series is 600 dials. The old
    /// connect-per-call was chosen when the only consumer was a GUI chart reading its visible
    /// range, where one connect per coarse read is the right trade; it stopped being right the
    /// moment `docs/decisions/0084-only-the-datahub-touches-the-store.md` made this the seam every
    /// reader goes through.
    ///
    /// ⚠ **Any error DROPS the connection**, and that is deliberate rather than cautious. The
    /// client collapses three outcomes into one `Err(String)` channel — a post-connect transport
    /// failure, a server-side `Response::Error`, and a `protocol desync` — and only the middle one
    /// leaves the socket usable. Since the three are indistinguishable HERE, keeping the connection
    /// after any of them would risk answering the NEXT verb with a frame belonging to this one, on
    /// a protocol whose whole framing is positional. A redial costs one round trip; a desynced
    /// read returns plausible data for the wrong question.
    /// Rows per page when the server advertises [`FEATURE_SCAN_LIMIT`].
    ///
    /// ⚠ **A ROW count cannot bound BYTES, and this constant is the whole of the guard.** A page of
    /// 10 000 trades is well under [`crate::proto::MAX_FRAME_LEN`]; 10 000 full L2 snapshots, each
    /// carrying hundreds of levels, is not. The wire cap the owner chose bounds ROWS because that
    /// is what the server can count without serializing first, so a byte-aware cap — the server
    /// measuring as it encodes — is a residual this number only makes unlikely. Chosen
    /// conservatively for that reason rather than tuned: a smaller page costs round trips, and a
    /// larger one costs a refused frame.
    const PAGE_ROWS: u32 = 10_000;

    /// Run one range scan as a SEQUENCE of capped pages over the reused connection.
    ///
    /// ⚠ **The continuation is `last_ts + 1`, and it is only correct because the server's cap is
    /// SOFT.** `crates/vike-datahub/src/server.rs`'s `cap_to_whole_ts` stops on a whole-`ts`
    /// boundary precisely so this line cannot skip rows: a page cut mid-`ts` would leave the rest
    /// of that timestamp beyond the next `start`, and they would vanish with no error. The two
    /// halves are one design and neither is safe alone.
    ///
    /// ⚠ **The loop ends on an EMPTY page, not on a short one.** A short page is the ordinary
    /// straddle case — the server stopped early to keep a `ts` whole — so treating it as the end
    /// would truncate the answer silently. The cost is ONE trailing request per scan, which is the
    /// price of having no cursor in the reply (see [`FEATURE_SCAN_LIMIT`] for why there is none).
    ///
    /// ⚠ **A server that does NOT advertise the cap is asked ONCE, unbounded** — today's behaviour,
    /// and the only safe choice: such a server IGNORES an unknown `limit` (serde skips unknown
    /// fields) and would answer every page with the whole range, so paging it would multiply one
    /// oversized frame by the number of pages.
    ///
    /// ⚠ What this does NOT bound is the caller's own `Vec`: every page is accumulated, because
    /// `HistStore` returns the whole range. Paging bounds the FRAME and the server's write, not the
    /// answer's size in this process.
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
}

/// The `Err` an unsupported (write / non-served-read) verb returns.
///
/// Uses [`DataError::Query`] — the nearest existing "this store cannot answer that" case, since
/// vike-data's `DataError` has only `Query` and `Io` and no dedicated "unsupported" variant. `Io` is
/// reserved for a real transport failure of a *served* verb (see [`RemoteHistStore::client`]), so
/// `Query` carries the unsupported case; the message names the method so a log line is
/// self-explanatory.
fn unsupported(method: &str) -> DataError {
    DataError::Query(format!("RemoteHistStore is read-only over RPC; {method} is not served"))
}

impl HistStore for RemoteHistStore {
    // ---- served read verbs: the GUI reads, answered over RPC -------------------------------------
    //
    // ⚠ This header said "the four GUI reads" and stopped being the whole served set when 0084's
    // six landed in the block below it. A reader who counts a header and stops is the failure this
    // file's own docs warn about one layer up, so the number is gone rather than corrected.
    //
    // Each calls the matching client verb through `with_client`, which REUSES one connection and
    // dials only when there is none. The client's ONE `Err(String)` channel (a post-connect
    // transport failure, a server-side `Response::Error`, or a protocol desync) maps to
    // `DataError::Query`; a connect failure is `DataError::Io`, raised by `dial`.

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

    // ---- 0084's six: the tick-level and research reads, answered over RPC since 2026-09-22 --------
    //
    // ⚠ **All six were REFUSALS in this file until the wire grew verbs for them**, and refusing was
    // the right answer for a client that could not ask — an empty `Ok` would let a caller mistake
    // "the RPC store cannot answer this" for "there is genuinely no data".
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md` measured what the refusals cost:
    // four crates opened `DataFusionHist` directly rather than go through the server, BECAUSE the
    // server had no verb to go through. Its verdict is that the store has ONE reader; these impls
    // are what makes obeying it possible, and the refusals they replace are why it was not.
    //
    // ⚠ Two of them carry history worth keeping, because each was once wrong in the QUIETER
    // direction — not refusing, but answering:
    //
    //   * `scan_depth` fell through to a trait default of `Ok(vec![])` (split-plane B12) — a
    //     fabricated "no data" from a client that merely had no verb. I5 fixed that default for
    //     every impl, and the override here outlived the fix because the trait's message ("this
    //     store serves no depth lane") was wrong HERE in a different way.
    //   * `scan_perp_metrics` was ABSENT from this file entirely, so it inherited the same
    //     `Ok(Vec::new())` default and a thin client asking a remote datahub for open interest got
    //     a confident EMPTY answer, indistinguishable from a store that genuinely holds none. It
    //     was caught one method away from the `scan_cohort` override written to prevent exactly it.
    //
    // Both now answer from the SERVER's own rows, which is the one shape that cannot be mistaken
    // for an absence or for a refusal.

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

    // ---- served store-metadata verbs (PR-6: the Data-Manager / Studio catalog over RPC) ----------
    //
    // These OVERRIDE the trait defaults (the catalog pair now REFUSES by default — right for a
    // leaf without the verbs, wrong here, where the SERVER can answer) with real RPC answers —
    // safe to serve whole because the catalog is TINY (a series list + cheap manifest-fold
    // coverage, NO Parquet scan), so unlike
    // a tick SLICE this never risks streaming the tape to the client (the compute-to-data invariant).

    fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
        self.with_client(|c| c.list_series())
    }

    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
        self.with_client(|c| c.inventory())
    }

    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, DataError> {
        self.with_client(|c| c.series_gaps(id))
    }

    /// ⚠ The trait DEFAULT refuses (a store with no per-series manifest has no answer), and this
    /// override is what stops a routed reader inheriting that refusal about a server which knows
    /// perfectly well — the `scan_depth` shape, one family over.
    fn series_facts(&self, id: &SeriesId) -> Result<(SeriesCoverage, Vec<String>), DataError> {
        self.with_client(|c| c.series_facts(id))
    }

    // ⚠ The trap `scan_depth` below used to share, one family up: the trait DEFAULT is `Ok(vec![])`
    // for THIS verb still (the depth read — I5 — and the catalog enumeration pair have each moved
    // to refusing defaults since; this DERIVED view keeps the empty, and its trait doc carries the
    // vacuous-truth argument plus the FRONTS warning this override answers) — right
    // for a store that genuinely cannot join its kinds, and a LIE here, because the SERVER's store
    // usually can. An inherited empty would render the Data Manager's Partial column blank and say
    // "nothing is partial"; the override says either the real answer or why not.
    //
    // The refusal an OLD server produces arrives through this same `Err`: `DatahubClient::
    // coverage_report` checks the `FEATURE_COVERAGE` advertisement and refuses without sending, so
    // "this peer predates the verb" is a `DataError::Query` naming the capability — which is
    // exactly what `vike_app_core::stored_mode` turns into its third state.
    fn coverage_report(&self) -> Result<Vec<InstrumentCoverage>, DataError> {
        self.with_client(|c| c.coverage_report())
    }

    // ---- unsupported: write verbs (all append_*, resample_*) -------------------------------------

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

    /// ⚠ An OVERRIDE of a permissive default, not a stub. `HistStore::append_cohort` defaults to
    /// `Ok(0)` — right for a store that holds no cohort panels, wrong here: this store holds
    /// nothing at all and writes nothing anywhere, so a silent `Ok(0)` would tell a producer its
    /// batch was a duplicate rather than that it was never sent.
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

    // ---- unsupported: read verbs NOT in the served set -------------------------------------------
    //
    // These OVERRIDE trait defaults that would otherwise return an empty `Ok` (funding/chain) or
    // scan an empty store — a `RemoteHistStore` cannot answer them, so it says so with `Err` rather
    // than fabricate "no data" (see the module docs' read-only rationale).

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
