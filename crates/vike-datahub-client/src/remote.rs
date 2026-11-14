//! [`HistoryStoreDatahub`] — a READ-ONLY [`vike_data::HistoryStore`] backed by a `vike-datahub` server
//! over the [`crate::proto`] RPC protocol.
//!
//! # What it is
//!
//! It implements the `HistoryStore` READ verbs — [`HistoryStore::load_bars`] and a lane's bars
//! ([`HistoryStore::load_bars_sourced_capped`]), [`HistoryStore::scan_quotes`], [`HistoryStore::scan_trades`], [`HistoryStore::properties_as_of`], the
//! store-metadata verbs [`HistoryStore::list_series`], [`HistoryStore::inventory`],
//! [`HistoryStore::series_gaps`], [`HistoryStore::series_facts`] and [`HistoryStore::coverage_report`] (the
//! Data-Manager / Studio catalog), and the SIX tick-level and research reads
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` asked the wire to grow
//! ([`HistoryStore::scan_book_updates`], [`HistoryStore::scan_depth`], [`HistoryStore::scan_cohort`],
//! [`HistoryStore::scan_perp_metrics`], [`HistoryStore::scan_equity`], [`HistoryStore::scan_exec_fills`]),
//! plus the two bounded execution-history reads [`HistoryStore::scan_execution_fills_head`] and
//! [`HistoryStore::scan_execution_orders_head`] — by calling the matching [`DatahubClient`] verb. A
//! consumer can therefore read through the ordinary `Arc<dyn HistoryStore + Send + Sync>` seam
//! WITHOUT linking the Arrow/DataFusion engine, which lives only on the server.
//!
//! # Read-only by design
//!
//! Every WRITE verb (`append_*`, `resample_*_to_bars`) and every read verb NOT in the served set
//! (`scan_symbol_properties`, the WHOLE-series `scan_exec_orders` — its bounded head is served —,
//! `scan_funding`, the `chain_*` family) returns an `Err` (`unsupported`), NOT a defaulted empty
//! `Ok`: an empty `Ok` would let a caller mistake "the RPC store cannot answer this" for "there
//! is genuinely no data".
//!
//! # Connection model
//!
//! `with_client` REUSES one connection per handle (the `conn` field's doc), dialling only when
//! there is none, and SERIALIZES every reader of that handle on it — the protocol is positional,
//! one frame out and one back, so one handle never overlaps two reads. A caller that wants reads in
//! flight together builds several handles, each owning a connection and dialling on first use
//! (`crates/vike-app-core/src/data/stored_load.rs`'s `PROBE_WORKERS` is that caller). A held
//! connection the datahub has since closed is dialled again once and the read repeated, which is
//! safe because every verb here reads (`with_client` argues it).

use std::sync::Mutex;

use vike_data::{
    ChainRow, CohortRow, ExecFillRow, ExecOrderRow, ExecutionHistoryFundingRow, HistoryStore,
    HistoryStoreError, InstrumentCoverage, PerpMetricRow, SeriesCoverage, SeriesId, TimestampRange,
};
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{EquitySample, SymbolProperties};

use crate::client::DatahubClient;
use crate::proto::FEATURE_SCAN_LIMIT;
use vike_hub_protocol::auth::{HubKeys, Scope};

/// A READ-ONLY [`HistoryStore`] served by a `vike-datahub` server over the datahub RPC protocol. See
/// the [module docs](crate::remote): the GUI read verbs, the store-metadata verbs
/// (`list_series`/`inventory`/`series_gaps`/`coverage_report`) and 0084's six tick-level and
/// research reads are served; every other method returns [`HistoryStoreError::Query`] with a "not served
/// over RPC" message.
pub struct HistoryStoreDatahub {
    /// The datahub server address (e.g. `"127.0.0.1:7878"`).
    addr: String,
    /// The hub keys every dialled connection authenticates with, or `None` for the unauthenticated
    /// connect this type has always done. See [`HistoryStoreDatahub::with_keys`].
    keys: Option<HubKeys>,
    /// The live connection, REUSED across reads — see [`HistoryStoreDatahub::with_client`].
    ///
    /// ⚠ A `Mutex` because [`HistoryStore`] methods take `&self` behind
    /// `Arc<dyn HistoryStore + Send + Sync>`. Its SERIALIZATION of concurrent readers is the
    /// correctness property, not a side effect: on a POSITIONAL protocol two threads sharing a
    /// connection would read each other's replies.
    conn: Mutex<Option<DatahubClient>>,
}

impl HistoryStoreDatahub {
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
    pub fn with_keys(addr: impl Into<String>, keys: HubKeys) -> Self {
        Self { addr: addr.into(), keys: Some(keys), conn: Mutex::new(None) }
    }

    /// Dial ONE new connection. A connect failure maps to [`HistoryStoreError::Io`] — a genuine transport
    /// failure (the server is down / unreachable) or an authentication refusal — distinct from the
    /// [`HistoryStoreError::Query`] a *served* verb returns on a server-side error.
    ///
    /// ⚠ **Keys held and a datahub that asked for none prints one stderr line per dial, and is not
    /// refused** ([`Self::with_keys`] argues why that connect works). A keyed datahub that is down and
    /// a key-less one on the same address read as the same store under the same label, and this line
    /// is the only thing that tells them apart. Printed, not logged: a bin that reads history here
    /// starts no subscriber.
    fn dial(&self) -> Result<DatahubClient, HistoryStoreError> {
        let client = DatahubClient::connect_with_keys(&self.addr, self.keys.as_ref(), Scope::Read)
            .map_err(|e| HistoryStoreError::Io(e.to_string()))?;
        if self.keys.is_some() && client.authenticated_scope().is_none() {
            eprintln!(
                "the datahub at {} asked for no key, though this box holds datahub keys, so this \
                 connection is not authenticated. If your keyed datahub is down and another one \
                 answers on this address, everything read here comes from that other store",
                self.addr
            );
        }
        Ok(client)
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
    /// ⚠ **The continuation is `last_timestamp + 1`, and it is only correct because the server's cap is
    /// SOFT.** `crates/vike-datahub/src/server/range_reads.rs`'s `cap_to_whole_timestamp` stops on a
    /// whole-`ts` boundary so this line cannot skip rows: a page cut mid-`ts` would leave the rest
    /// of that timestamp beyond the next `start`, lost with no error. Neither half is safe alone.
    ///
    /// ⚠ **The loop ends on an EMPTY page, not on a short one.** A short page is the ordinary
    /// straddle case (the server stopped early to keep a `ts` whole), so treating it as the end
    /// would truncate silently. The cost is ONE trailing request per scan, the price of having no
    /// cursor in the reply (see [`FEATURE_SCAN_LIMIT`]).
    ///
    /// ⚠ **Stopping on an empty page is safe only because of the STORE's contract**, which this
    /// loop cannot check: `vike_data::HistoryStore::scan_quotes_capped` promises a complete prefix
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
    /// accumulated, because `HistoryStore` returns the whole range.
    ///
    /// Each page runs from `last_timestamp + 1` to the END of the range, so a server that loaded that
    /// remainder and then capped it would make a full read QUADRATIC. Every range verb passes the
    /// page size down as a read budget (`HistoryStore::load_bars_head` for bars), so no page costs the
    /// server more than the page plus at most one storage block of the symbol's rows.
    fn paged<T>(
        &self,
        range: TimestampRange,
        ts_of: impl Fn(&T) -> i64,
        fetch: impl FnMut(&mut DatahubClient, TimestampRange, Option<u32>) -> Result<Vec<T>, String>,
    ) -> Result<Vec<T>, HistoryStoreError> {
        self.paged_until(range, None, ts_of, fetch)
    }

    /// [`Self::paged`] that may STOP EARLY: with `stop_at = Some(n)` the loop ends once `n` rows are
    /// in, and each page asks for at most `n` — the `HistoryStore` `_head` contract (a complete
    /// prefix of at least `n` rows unless it is the whole range), which every page boundary keeps
    /// because the server cuts pages on a whole `ts`. `None` is [`Self::paged`]: the whole range.
    fn paged_until<T>(
        &self,
        range: TimestampRange,
        stop_at: Option<usize>,
        ts_of: impl Fn(&T) -> i64,
        mut fetch: impl FnMut(&mut DatahubClient, TimestampRange, Option<u32>) -> Result<Vec<T>, String>,
    ) -> Result<Vec<T>, HistoryStoreError> {
        // A head of nothing reads nothing — and must not reach the wire, where `limit: Some(0)` is
        // read as NO cap.
        if stop_at == Some(0) {
            return Ok(Vec::new());
        }
        let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let held = guard.take();
        let mut reused = held.is_some();
        let mut client = match held {
            Some(c) => c,
            None => self.dial()?,
        };
        let page_rows = stop_at
            .map_or(Self::PAGE_ROWS, |n| u32::try_from(n).unwrap_or(u32::MAX).min(Self::PAGE_ROWS));

        let mut out: Vec<T> = Vec::new();
        let mut start = range.start;
        loop {
            // Asked per page, not once: the redial below may land on a datahub that advertises
            // something else (it may have been restarted on a new build).
            let capped = client.features().iter().any(|f| f == FEATURE_SCAN_LIMIT);
            let limit = capped.then_some(page_rows);
            let page = match fetch(&mut client, TimestampRange { start, end: range.end }, limit) {
                Ok(page) => page,
                // Closed while it was held (`with_client` argues the rule). Only the FIRST page can
                // meet that: past a page it answered, the connection was in use, not idle.
                Err(_) if reused && !client.is_usable() => {
                    reused = false;
                    client = self.dial()?;
                    continue;
                }
                // The connection is NOT returned to the slot — `with_client` argues why.
                Err(e) => return Err(HistoryStoreError::Query(e)),
            };
            reused = false;
            let Some(last) = page.last().map(&ts_of) else { break };
            out.extend(page);
            if !capped || stop_at.is_some_and(|n| out.len() >= n) {
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
    /// `crates/vike/src/tearsheet_cli/excursions.rs`'s `backfill_excursions` calls `load_bars` once per
    /// SYMBOL, and a dial per read would be a TCP connect plus a handshake (an HMAC round trip on a
    /// keyed server) per call.
    ///
    /// ⚠ **Any error DROPS the connection**, deliberately. The client collapses a post-connect
    /// transport failure, a server-side `Response::Error` and a `protocol desync` into one
    /// `Err(String)`, and only the middle one leaves the socket usable; since they are
    /// indistinguishable HERE, keeping the connection risks answering the NEXT verb with this one's
    /// frame. A redial costs one round trip; a desynced read returns plausible data for the wrong
    /// question.
    ///
    /// ⚠ **A HELD connection that failed in transport is dialled again ONCE and the read repeated.**
    /// The datahub closes a connection idle past `config.node_idle_read_timeout_ms` (300 s by
    /// default, `vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT`) and this slot keeps none alive, so
    /// the first read after a pause lands on a dead socket, and a caller that turns a failed read into
    /// "no data" turns the pause into a wrong answer. `is_usable()` is what tells that from a refusal:
    /// `Response::Error` leaves it `true`, and the datahub's answer is not asked twice. A connection
    /// this read dialled itself was not idle, so it gets no repeat, and neither does the repeat.
    ///
    /// ⚠ **Safe only because every verb through here and [`Self::paged_until`] is a READ**: this type
    /// sends no write (the module docs), so the worst a repeat can cost is the read itself. A verb
    /// that writes may not take this path.
    fn with_client<T>(
        &self,
        mut verb: impl FnMut(&mut DatahubClient) -> Result<T, String>,
    ) -> Result<T, HistoryStoreError> {
        let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let held = guard.take();
        let reused = held.is_some();
        let mut client = match held {
            Some(c) => c,
            None => self.dial()?,
        };
        let mut answer = verb(&mut client);
        if answer.is_err() && reused && !client.is_usable() {
            client = self.dial()?;
            answer = verb(&mut client);
        }
        match answer {
            Ok(value) => {
                *guard = Some(client);
                Ok(value)
            }
            // The connection is NOT returned to the slot — see the doc above.
            Err(e) => Err(HistoryStoreError::Query(e)),
        }
    }

    /// **The history-channels read, over this store's connection** — `Request::HistoryChannels`,
    /// for the Data Manager's HISTORY column. Not a `HistoryStore` verb (not a question about stored
    /// rows), so an inherent method on the handle the GUI already holds, under the same Observe key.
    ///
    /// `Ok(None)` is a datahub OLDER than the read: it does not advertise
    /// [`crate::proto::FEATURE_HISTORY_CHANNELS`], nothing was sent, and the caller renders its own
    /// compiled table instead (`crate::history::compiled_report`). An `Err` is an unreachable store
    /// or a read that failed.
    pub fn history_channels(
        &self,
    ) -> Result<Option<crate::history::HistoryChannelsReport>, HistoryStoreError> {
        self.with_client(|client| {
            if !client.serves_history_channels() {
                return Ok(None);
            }
            client.history_channels().map(Some)
        })
    }

    /// **What this store's datahub advertised in its `Welcome`**, over the reused connection (dialling
    /// when there is none) — for a reader that must say WHY a read is not available rather than
    /// render its refusal as an empty answer: the execution history asks it for
    /// [`crate::proto::FEATURE_SCAN_EXECUTION_ORDERS`], [`crate::proto::FEATURE_SCAN_EXECUTION_FILLS_RANGE`] and
    /// the runtime [`crate::proto::FEATURE_EXECUTION_MATERIALIZE`]. An `Err` is an unreachable store.
    pub fn advertised_features(&self) -> Result<Vec<String>, HistoryStoreError> {
        self.with_client(|client| Ok(client.features().to_vec()))
    }
}

/// The `Err` an unsupported (write / non-served-read) verb returns.
///
/// [`HistoryStoreError::Query`], the nearest "this store cannot answer that" case (`HistoryStoreError` has no
/// "unsupported" variant); `Io` stays reserved for a connect failure, raised by
/// [`HistoryStoreDatahub::dial`]. The message names the method so a log line is self-explanatory.
fn unsupported(method: &str) -> HistoryStoreError {
    HistoryStoreError::Query(format!(
        "HistoryStoreDatahub is read-only over RPC; {method} is not served"
    ))
}

impl HistoryStore for HistoryStoreDatahub {
    // ---- served read verbs, answered over RPC ---------------------------------------------------
    //
    // Range reads go through `paged`, the rest through `with_client`; both REUSE one connection.
    // The client's ONE `Err(String)` channel maps to `HistoryStoreError::Query`; a connect failure is
    // `HistoryStoreError::Io`, raised by `dial`.

    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.paged(
            range,
            |r: &Bar| r.timestamp,
            |c, w, n| c.load_bars(venue, symbol, interval, w, n),
        )
    }

    /// ONE `source=` lane's bars (a `user-<label>` import, `aggtrades`), paged as [`Self::load_bars`] and stopping
    /// once `budget` rows are in. ⚠ An OVERRIDE, and it must be one: the trait default refuses every lane, so a
    /// backtest naming `[data] source` through the datahub could not read it. Refused locally, nothing sent, by a
    /// datahub without [`crate::proto::FEATURE_BAR_SOURCE`], which would answer the venue's candles in the lane's place.
    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.paged_until(
            range,
            budget,
            |r: &Bar| r.timestamp,
            |c, w, n| c.load_bars_sourced_ms(venue, source, symbol, interval, w.start, w.end, n),
        )
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.paged(range, |r: &TickQuote| r.timestamp, |c, w, n| c.scan_quotes(venue, symbol, w, n))
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.paged(range, |r: &TickTrade| r.timestamp, |c, w, n| c.scan_trades(venue, symbol, w, n))
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
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.paged(
            range,
            |r: &L2BookUpdate| r.timestamp,
            |c, w, n| c.scan_book_updates(venue, symbol, w, n),
        )
    }

    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.paged(
            range,
            |r: &L2BookUpdate| r.timestamp,
            |c, w, n| c.scan_depth(venue, symbol, w, n),
        )
    }

    /// ⚠ The second argument is an ASSET, not a symbol — the trait's own spelling, kept here so a
    /// caller cannot pass one where the other belongs.
    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        self.paged(range, |r: &CohortRow| r.ts, |c, w, n| c.scan_cohort(venue, asset, w, n))
    }

    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
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
        range: TimestampRange,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        self.paged(
            range,
            |r: &EquitySample| r.timestamp,
            |c, w, n| c.scan_equity(venue, symbol, w, n),
        )
    }

    /// The WHOLE series: `HistoryStore::scan_exec_fills` takes no range, so this sends the windowless
    /// frame every server answers (refused by name above the server's ceiling).
    fn scan_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        self.with_client(|c| c.scan_exec_fills(venue, symbol))
    }

    /// ⚠ An OVERRIDE, and it must be one: the trait default reads the WHOLE series and filters it
    /// here. Against a server advertising [`crate::proto::FEATURE_SCAN_EXECUTION_FILLS_RANGE`] the head
    /// is read in whole-`ts` pages over `range` and stops once `n` rows are in; against an older one
    /// it falls back to exactly that default — the whole series, filtered to `range` — because such a
    /// server would IGNORE a window and answer everything as if it were the window.
    fn scan_execution_fills_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        if n == 0 {
            return Ok(Vec::new());
        }
        if self.with_client(|c| Ok(c.serves_execution_fills_range()))? {
            return self.paged_until(
                range,
                Some(n),
                |r: &ExecFillRow| r.ts,
                |c, w, l| c.scan_execution_fills_range(venue, symbol, w, l),
            );
        }
        let mut rows = self.scan_exec_fills(venue, symbol)?;
        rows.retain(|r| {
            range.start.is_none_or(|lo| r.ts >= lo) && range.end.is_none_or(|hi| r.ts <= hi)
        });
        Ok(rows)
    }

    /// The order-lifecycle head, in whole-`ts` pages over `range` — served only by a datahub that
    /// advertises [`crate::proto::FEATURE_SCAN_EXECUTION_ORDERS`]; against an older one the client
    /// refuses LOCALLY (nothing is sent) and the refusal names the capability.
    fn scan_execution_orders_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        self.paged_until(
            range,
            Some(n),
            |r: &ExecOrderRow| r.ts,
            |c, w, l| c.scan_exec_orders(venue, symbol, w, l),
        )
    }

    fn properties_as_of(
        &self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, HistoryStoreError> {
        self.with_client(|c| c.properties_as_of(venue, symbol, ts))
    }

    // ---- served store-metadata verbs (the Data-Manager / Studio catalog) ------------------------
    //
    // These OVERRIDE refusing trait defaults with real RPC answers, safe to serve whole because the
    // catalog is TINY (a series list + cheap manifest-fold coverage, NO Parquet scan): unlike a tick
    // SLICE it never streams the tape to the client.

    fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        self.with_client(|c| c.list_series())
    }

    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        self.with_client(|c| c.inventory())
    }

    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, HistoryStoreError> {
        self.with_client(|c| c.series_gaps(id))
    }

    /// ⚠ The trait DEFAULT refuses (a store with no per-series manifest has no answer); this
    /// override stops a routed reader inheriting that refusal about a server which knows.
    fn series_facts(
        &self,
        id: &SeriesId,
    ) -> Result<(SeriesCoverage, Vec<String>), HistoryStoreError> {
        self.with_client(|c| c.series_facts(id))
    }

    // ⚠ The trait DEFAULT is still `Ok(vec![])` for THIS verb (its trait doc carries the
    // vacuous-truth argument and the FRONTS warning this override answers) — right for a store that
    // cannot join its kinds, a LIE here: an inherited empty would render the Data Manager's Partial
    // column blank and say "nothing is partial".
    //
    // An OLD server's refusal arrives through this same `Err`: `DatahubClient::coverage_report`
    // refuses without sending when `FEATURE_COVERAGE` is absent, and
    // `vike_app_core::data::stored_mode` turns that `HistoryStoreError::Query` into its third state.
    fn coverage_report(&self) -> Result<Vec<InstrumentCoverage>, HistoryStoreError> {
        self.with_client(|c| c.coverage_report())
    }

    // The KEPT lists (`Request::InstrumentLists`, a Read verb), every venue's rows; the operator's own lists are
    // not kept rows and stay out. `Ok(None)` is a datahub too old to keep them: nothing was sent.
    fn kept_instruments(&self) -> Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError> {
        self.with_client(|c| {
            if !c.serves_instrument_lists() {
                return Ok(None);
            }
            let lists = c.instrument_lists()?.venues;
            Ok(Some(lists.into_iter().flat_map(|l| l.instruments).collect()))
        })
    }

    // ---- unsupported: write verbs (all append_*, resample_*) ------------------------------------

    fn append_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _bars: &[Bar],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_bars"))
    }

    fn append_quotes(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[TickQuote],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_quotes"))
    }

    fn append_trades(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[TickTrade],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_trades"))
    }

    fn append_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        _updates: &[L2BookUpdate],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_book_updates"))
    }

    fn append_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[(i64, SymbolProperties)],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_symbol_properties"))
    }

    fn append_equity(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[EquitySample],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_equity"))
    }

    fn append_exec_fills(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecFillRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_exec_fills"))
    }

    fn append_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecOrderRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_exec_orders"))
    }

    fn append_funding(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecutionHistoryFundingRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_funding"))
    }

    fn append_chain_snapshot(
        &self,
        _venue: &str,
        _underlying: &str,
        _rows: &[ChainRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_chain_snapshot"))
    }

    /// ⚠ An OVERRIDE of a permissive default, not a stub: `HistoryStore::append_cohort` defaults to
    /// `Ok(0)`, which here would tell a producer its batch was a duplicate rather than never sent.
    fn append_cohort(
        &self,
        _venue: &str,
        _asset: &str,
        _rows: &[CohortRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_cohort"))
    }

    fn resample_quotes_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TimestampRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("resample_quotes_to_bars"))
    }

    fn resample_trades_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TimestampRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
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
        _range: TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, HistoryStoreError> {
        Err(unsupported("scan_symbol_properties"))
    }

    fn scan_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        Err(unsupported("scan_exec_orders"))
    }

    fn scan_funding(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TimestampRange,
    ) -> Result<Vec<ExecutionHistoryFundingRow>, HistoryStoreError> {
        Err(unsupported("scan_funding"))
    }

    fn scan_chain(
        &self,
        _venue: &str,
        _underlying: &str,
        _range: TimestampRange,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        Err(unsupported("scan_chain"))
    }

    fn chain_as_of(
        &self,
        _venue: &str,
        _underlying: &str,
        _ts: i64,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        Err(unsupported("chain_as_of"))
    }

    fn chain_as_of_within(
        &self,
        _venue: &str,
        _underlying: &str,
        _ts: i64,
        _lookback_ms: i64,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        Err(unsupported("chain_as_of_within"))
    }
}
