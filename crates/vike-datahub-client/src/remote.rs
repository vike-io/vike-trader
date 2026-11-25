//! [`HistoryStoreDatahub`] — a READ-ONLY [`vike_data::HistoryStore`] backed by a `vike-datahub` server
//! over the [`crate::proto`] RPC protocol.
//!
//! # What it is
//!
//! It implements the `HistoryStore` READ verbs — [`HistoryStore::load_bars`] and a lane's bars
//! ([`HistoryStore::load_bars_sourced_capped`]), [`HistoryStore::scan_quotes`], [`HistoryStore::scan_trades`], [`HistoryStore::properties_as_of`], the
//! store-metadata verbs [`HistoryStore::list_series`], [`HistoryStore::inventory`] (and its
//! execution-history window, [`HistoryStore::execution_series`]), [`HistoryStore::series_gaps`],
//! [`HistoryStore::series_facts`], [`HistoryStore::series_days`] and [`HistoryStore::coverage_report`] (the
//! Data-Manager / Studio catalog), and the SIX tick-level and research reads
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` asked the wire to grow
//! ([`HistoryStore::scan_book_updates`], [`HistoryStore::scan_depth`], [`HistoryStore::scan_cohort`],
//! [`HistoryStore::scan_perp_metrics`], [`HistoryStore::scan_equity`], [`HistoryStore::scan_exec_fills`]),
//! plus the two bounded execution-history reads [`HistoryStore::scan_execution_fills_head`] and
//! [`HistoryStore::scan_execution_orders_head`], the text history [`HistoryStore::scan_texts`]
//! (so [`HistoryStore::texts_as_of`] too), the option-chain read [`HistoryStore::scan_chain`]
//! (with the two point-in-time reads the trait derives from it), the two SEC tables
//! ([`HistoryStore::scan_financial_facts`], [`HistoryStore::scan_institutional_holdings`], so their
//! `_as_of` reads too), the text scores [`HistoryStore::scan_text_scores`] and the live strategies' published forecasts
//! [`HistoryStore::scan_forecasts`] (so [`HistoryStore::forecasts_as_of`] too) — by calling the
//! matching [`DatahubClient`] verb, and the bar and tick reads' columns pages as the datahub sent them
//! ([`HistoryStore::read_columns`], for a reader that keeps the columns: the backtesthub's cache). A
//! consumer can therefore read through the ordinary `Arc<dyn HistoryStore + Send + Sync>` seam
//! WITHOUT linking the Arrow/DataFusion engine, which lives only on the server.
//!
//! # Read-only by design
//!
//! Every WRITE verb (`append_*`, `resample_*_to_bars`) and every read verb NOT in the served set
//! (`scan_symbol_properties`, the WHOLE-series `scan_exec_orders` — its bounded head is served —,
//! `scan_funding`) returns an `Err` (`unsupported`), NOT a defaulted empty
//! `Ok`: an empty `Ok` would let a caller mistake "the RPC store cannot answer this" for "there
//! is genuinely no data".
//!
//! # Connection model
//!
//! A handle keeps a small POOL of connections (the `pool` field's doc). A read takes an idle one, or
//! dials a new one while fewer than [`HistoryStoreDatahub::POOL_CONNECTIONS`] exist, or waits for
//! one to come back; it holds that connection for the whole read, every page of it, and returns it
//! after. So reads of ONE handle from several threads are in flight together (the backtesthub's
//! parallel backtests share one handle: `crates/vike-backtesthub/src/backtest_cli/serve.rs`'s
//! `run_serve`), while one connection never carries two reads at once: the protocol is positional,
//! one frame out and one back. A held connection the datahub has since closed is dialled again once
//! and the read repeated, which is safe because every verb here reads (`with_client` argues it).

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use vike_data::{
    BarDay, ChainRow, CohortRow, EncodedKept, ExecFillRow, ExecForecastRow, ExecOrderRow,
    ExecutionHistoryFundingRow, ExecutionSeries, FinancialFactRow, HistoryStore, HistoryStoreError,
    InstitutionalHoldingRow, InstrumentCoverage, PerpMetricRow, SeriesCoverage, SeriesId,
    SharedColumns, TextRow, TextScoreRow, TimestampRange,
};
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{EquitySample, StoreKindId, SymbolProperties};

use crate::client::DatahubClient;
use crate::proto::{
    BAR_COLUMNS, BOOK_UPDATE_COLUMNS, BarColumnsBody, ColumnRow, ColumnsMagic, FEATURE_COVERAGE,
    FEATURE_SCAN_LIMIT, QUOTE_COLUMNS, Request, TRADE_COLUMNS, check_columns, columns_word,
};
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
    /// The connections, REUSED across reads — see [`HistoryStoreDatahub::lease`].
    ///
    /// ⚠ A connection is LEASED to one read at a time and is out of this pool while a read holds
    /// it: on a POSITIONAL protocol two threads sharing a connection would read each other's
    /// replies. That exclusivity is the correctness property; the pool only decides how many reads
    /// run at once.
    pool: Mutex<Pool>,
    /// Signalled when a lease ends (a connection back idle, or a slot freed), for a read waiting at
    /// the cap.
    freed: Condvar,
    /// The idle reply bound every dialled connection arms, or `None` for the client's per-verb
    /// table. See [`HistoryStoreDatahub::with_reply_timeout`].
    reply_timeout: Option<Duration>,
    /// How a columns read (bars and the range scans) asks for its body: `Plain` (the default), or `Lz4`
    /// for a handle on a thin link. See [`HistoryStoreDatahub::with_compressed_bars`].
    columns_body: BarColumnsBody,
    /// The kept rows and splits of each venue this handle has read, as msgpack, with the datahub's stamp of them: the
    /// STAMPED cache (`Request::KeptRowsOfVenues`, the owner's "yes both" of 2026-11-19). Never answered from without
    /// asking: each read sends the stamps it holds and keeps a venue's bytes only where the datahub answers
    /// "unchanged", so it is exact whoever wrote the lists. `kept_stamped` is its one reader and writer.
    kept: Mutex<BTreeMap<String, Arc<Kept>>>,
}

/// One venue of [`HistoryStoreDatahub`]'s stamped cache: its rows and splits as the datahub sent them, and its stamp.
#[derive(Debug)]
struct Kept {
    stamp: String,
    encoded: EncodedKept,
}

impl HistoryStoreDatahub {
    /// Point at a datahub server `addr` (e.g. `"127.0.0.1:7878"`). No connection is opened here —
    /// the first read dials one and later reads reuse it (the module docs' connection model).
    ///
    /// Unauthenticated: correct against a key-less server. Against a KEYED server every read fails
    /// with the connect error naming the missing keys — use [`Self::with_keys`].
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            keys: None,
            pool: Mutex::default(),
            freed: Condvar::new(),
            reply_timeout: None,
            columns_body: BarColumnsBody::Plain,
            kept: Mutex::default(),
        }
    }

    /// **Bound every reply of this handle by `idle`**: no byte for that long ends the read
    /// ([`DatahubClient::set_reply_timeout`] on each connection it dials). It is an IDLE bound, so
    /// a large answer arriving slowly over a thin link is never cut while bytes still come, and a
    /// peer that went silent is. A caller that knows its verbs are short (the Data Manager's catalog
    /// reads) sets seconds here instead of the client's 300 s default.
    pub fn with_reply_timeout(mut self, idle: Duration) -> Self {
        self.reply_timeout = Some(idle);
        self
    }

    /// **Ask for every columns page LZ4-compressed** ([`BarColumnsBody::Lz4`]), the bars' and the tick-level
    /// scans' alike: for a handle whose datahub is at the far end of a thin link (the desktop, over an ssh
    /// tunnel measured at 13-40 KB/s). The rows are the same; the server compresses each page in memory, so a
    /// same-box reader (the backtesthub) leaves this off and reads plain columns, which cost no CPU to pack.
    pub fn with_compressed_bars(mut self) -> Self {
        self.columns_body = BarColumnsBody::Lz4;
        self
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
        Self { keys: Some(keys), ..Self::new(addr) }
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
        let mut client =
            DatahubClient::connect_with_keys(&self.addr, self.keys.as_ref(), Scope::Read)
                .map_err(|e| HistoryStoreError::Io(e.to_string()))?;
        client.set_reply_timeout(self.reply_timeout);
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

    /// The most connections one handle holds, idle and leased together: a read past it waits for one
    /// to come back. Small because each is a thread on a datahub that caps them
    /// (`vike-datahub`'s `MAX_CONNECTIONS`, 64) beside the desktops and the CLI, and because each read in
    /// flight holds a page of rows in the DATAHUB's memory (its `RANGE_READS_IN_FLIGHT` bounds that for
    /// every client together; this keeps one handle from queueing the rest behind it). Four lets the
    /// backtesthub's parallel runs read together: three lanes measured 2.5x faster through one handle.
    pub const POOL_CONNECTIONS: usize = 4;

    /// The pool, poison or not: nothing panics while holding it, and its two fields stay consistent
    /// at every unlock.
    fn lock_pool(&self) -> MutexGuard<'_, Pool> {
        self.pool.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Take a connection for ONE read: an idle one (the most recently returned, so the fewest stay
    /// warm), else a new dial while fewer than [`Self::POOL_CONNECTIONS`] exist, else wait for a lease
    /// to end. The dial runs OUTSIDE the lock, so one slow connect holds up no other read; its slot is
    /// the lease's from the start, so a failed dial frees it through the lease's drop.
    fn lease(&self) -> Result<Lease<'_>, HistoryStoreError> {
        let mut pool = self.lock_pool();
        loop {
            if let Some(client) = pool.idle.pop() {
                return Ok(Lease { store: self, client: Some(client), reused: true, kept: false });
            }
            if pool.open < Self::POOL_CONNECTIONS {
                pool.open += 1;
                drop(pool);
                let mut lease = Lease { store: self, client: None, reused: false, kept: false };
                lease.client = Some(self.dial()?);
                return Ok(lease);
            }
            pool = self.freed.wait(pool).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// Rows per page when the server advertises [`FEATURE_SCAN_LIMIT`].
    ///
    /// ⚠ **A ROW count cannot bound BYTES, and this constant is the whole of the guard.** 10 000
    /// trades are well under [`vike_hub_protocol::frame::MAX_FRAME_LEN`]; 10 000 full L2 snapshots are not. The
    /// wire cap bounds ROWS because that is what the server can count without serializing first,
    /// so a byte-aware cap is a residual this number only makes unlikely. Conservative rather than
    /// tuned: a smaller page costs round trips, a larger one a refused frame.
    const PAGE_ROWS: u32 = 10_000;

    /// Rows per page of a BAR read ([`HistoryStore::load_bars`], [`HistoryStore::load_bars_sourced_capped`])
    /// answered as COLUMNS ([`crate::proto::FEATURE_BAR_COLUMNS`]), twenty times [`Self::PAGE_ROWS`]: a
    /// 32-month `1m` run over three lanes was ~423 round trips at 10 000, each re-reading the series'
    /// manifest on the server, and is ~21 here. A server that answers only JSON is paged by
    /// [`Self::PAGE_ROWS`] as before: a JSON page is built whole in that datahub's memory (twice, while its
    /// buffer grows), and the big page bought ~7% there (measured by `crates/vike-datahub/tests/datafusion_serve/bar_load_timing.rs`'s probe).
    ///
    /// The tick-level scans page by it too where they come as columns ([`crate::proto::FEATURE_TICK_COLUMNS`]):
    /// 200 000 quotes are ~10 MB of columns, a book page is bounded by its levels first (the server's read
    /// counts levels), and the server streams a plain page into its frame. A page may be this large because
    /// a columns page is cut to the frame EXACTLY: 200 000 bars are ~15 MB of columns against the 64 MiB
    /// frame; the server reads at most a kind's ceiling (`LOAD_BARS_CEILING`, ~493 000, and its siblings) for
    /// one request, and its `fit_to_frame` cuts a page that would still pass the frame on a whole `ts`, which
    /// is a SHORT page (the loop below continues past it), never a refusal. So the rows a whole-range read returns are the same
    /// at any page size; only the number of round trips moves. (A HEAD read's overshoot past its `n`
    /// moves with the page, which the `_head` contract leaves free; every bar read a backtest makes
    /// through here is a whole range.)
    pub const BAR_PAGE_ROWS: u32 = 200_000;

    /// Run one range scan as a SEQUENCE of capped pages over ONE leased connection.
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
        mut fetch: impl FnMut(&mut DatahubClient, TimestampRange, Option<u32>) -> Result<Vec<T>, String>,
    ) -> Result<Vec<T>, HistoryStoreError> {
        self.paged_until(
            range,
            None,
            |_| Self::PAGE_ROWS,
            ts_of,
            |c, w, n, out| append_page(out, fetch(c, w, n)),
        )
    }

    /// Rows per bar page on a THIN-LINK handle ([`Self::with_compressed_bars`]): a tenth of
    /// [`Self::BAR_PAGE_ROWS`]. Each page holds one of the datahub's range-read places while it is read and
    /// compressed, and a page that crosses a 13-40 KB/s tunnel in seconds rather than minutes keeps each
    /// reply's slow write short; the round trips it adds are nothing beside that link.
    pub const THIN_LINK_BAR_PAGE_ROWS: u32 = 20_000;

    /// The page a columns read asks of a server: [`Self::BAR_PAGE_ROWS`] where it `answers` the read's columns
    /// ([`Self::THIN_LINK_BAR_PAGE_ROWS`] when this handle asks them compressed), else [`Self::PAGE_ROWS`].
    fn columns_page(&self, answers: bool) -> u32 {
        match (answers, self.columns_body) {
            (false, _) => Self::PAGE_ROWS,
            (true, BarColumnsBody::Plain) => Self::BAR_PAGE_ROWS,
            (true, BarColumnsBody::Lz4) => Self::THIN_LINK_BAR_PAGE_ROWS,
        }
    }

    /// [`Self::paged`] that may STOP EARLY: with `stop_at = Some(n)` the loop ends once `n` rows are
    /// in, and each page asks for at most `n` — the `HistoryStore` `_head` contract (a complete
    /// prefix of at least `n` rows unless it is the whole range), which every page boundary keeps
    /// because the server cuts pages on a whole `ts`. `None` is [`Self::paged`]: the whole range.
    /// `page_of` is the most rows one page asks of a connection's server: [`Self::PAGE_ROWS`], or
    /// [`Self::columns_page`] for columns; asked per page, like the cap, because a redial may land on another build.
    /// `fetch` APPENDS a page to the read so far (bar columns decode straight into it) and must append
    /// NOTHING when it fails: a failed first page is asked again on a fresh connection.
    fn paged_until<T>(
        &self,
        range: TimestampRange,
        stop_at: Option<usize>,
        page_of: impl Fn(&DatahubClient) -> u32,
        ts_of: impl Fn(&T) -> i64,
        mut fetch: impl FnMut(
            &mut DatahubClient,
            TimestampRange,
            Option<u32>,
            &mut Vec<T>,
        ) -> Result<(), String>,
    ) -> Result<Vec<T>, HistoryStoreError> {
        // A head of nothing reads nothing — and must not reach the wire, where `limit: Some(0)` is
        // read as NO cap.
        if stop_at == Some(0) {
            return Ok(Vec::new());
        }
        let mut lease = self.lease()?;

        let mut out: Vec<T> = Vec::new();
        let mut start = range.start;
        loop {
            // Asked per page, not once: the redial below may land on a datahub that advertises
            // something else (it may have been restarted on a new build).
            let capped = lease.client().features().iter().any(|f| f == FEATURE_SCAN_LIMIT);
            let most = page_of(lease.client());
            let page_rows =
                stop_at.map_or(most, |n| u32::try_from(n).unwrap_or(u32::MAX).min(most));
            let limit = capped.then_some(page_rows);
            let before = out.len();
            match fetch(lease.client(), TimestampRange { start, end: range.end }, limit, &mut out) {
                Ok(()) => {}
                // Closed while it was held (`with_client` argues the rule). Only the FIRST page can
                // meet that: past a page it answered, the connection was in use, not idle.
                Err(_) if lease.reused && !lease.client().is_usable() => {
                    lease.redial()?;
                    continue;
                }
                // The connection is NOT returned to the pool — `with_client` argues why.
                Err(e) => return Err(HistoryStoreError::Query(e)),
            }
            lease.reused = false;
            let Some(last) = out[before..].last().map(&ts_of) else { break };
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
        lease.keep();
        Ok(out)
    }

    /// A tick-level range read (quotes, trades, book updates, depth, perp metrics), paged as a bar read is: as
    /// binary COLUMNS in [`Self::columns_page`] pages where the connection's server advertises
    /// [`crate::proto::FEATURE_TICK_COLUMNS`] (`DatahubClient::scan_columns_into`), else `json`'s JSON in
    /// [`Self::PAGE_ROWS`] pages; decided per page, because a redial may land on another build. `scan(window,
    /// limit, body)` is the scan's request asking for columns. The same rows either way.
    fn tick_paged<R: ColumnRow>(
        &self,
        range: TimestampRange,
        ts_of: impl Fn(&R) -> i64,
        json: impl Fn(&mut DatahubClient, TimestampRange, Option<u32>) -> Result<Vec<R>, String>,
        scan: impl Fn(TimestampRange, Option<u32>, BarColumnsBody) -> Request,
    ) -> Result<Vec<R>, HistoryStoreError> {
        self.paged_until(
            range,
            None,
            |c| self.columns_page(c.serves_tick_columns()),
            ts_of,
            |c, w, n, out| {
                if c.serves_tick_columns() {
                    c.scan_columns_into(&scan(w, n, self.columns_body), out).map(drop)
                } else {
                    append_page(out, json(c, w, n))
                }
            },
        )
    }

    /// One read's columns pages as the server sent them ([`HistoryStore::read_columns`]): paged as the typed read pages
    /// ([`Self::paged_until`] at [`Self::columns_page`]), each `page` the PLAIN body of one `magic` reply of `R`
    /// (`DatahubClient::scan_columns_plain`, `load_bar_columns_plain`) kept whole. ⚠ Each page is checked HERE, where it
    /// arrives, as its decode would check it ([`check_columns`]: its lengths, its flag bits, its runs and the text they
    /// expand to, each row's own fields), so no reader of the answer (the cache, or the sandbox relay reading this handle
    /// with the cache off) meets a row count or a body the decode would refuse. Then its last row's time (every kind's
    /// first word) continues the read; a page of no row ends it, as the typed read's empty page does. `None`, nothing
    /// sent, from a connection whose server does not `serve` the read as columns.
    fn column_pages<R: ColumnRow>(
        &self,
        range: TimestampRange,
        magic: &ColumnsMagic,
        serves: fn(&DatahubClient) -> bool,
        page: impl Fn(&mut DatahubClient, TimestampRange, Option<u32>) -> Result<Vec<u8>, String>,
    ) -> Option<Result<SharedColumns, HistoryStoreError>> {
        let mut rows_only = false;
        let pages = self.paged_until(
            range,
            None,
            |c| self.columns_page(serves(c)),
            |&(_, _, last): &(Vec<u8>, usize, i64)| last,
            |c, w, n, out| {
                if !serves(c) {
                    rows_only = true;
                    return Ok(());
                }
                let body = page(c, w, n)?;
                let rows = check_columns::<R>(&body, magic)?;
                if let Some(last) = rows.checked_sub(1) {
                    let last = columns_word(&body, 0, last).expect("a checked page's row");
                    out.push((body, rows, last as i64));
                }
                Ok(())
            },
        );
        if rows_only {
            return None;
        }
        Some(pages.map(|pages| {
            let mut columns = SharedColumns::default();
            for (body, rows, _) in pages {
                columns.push(Arc::new(body), 0..rows);
            }
            columns
        }))
    }

    /// Run one verb over a REUSED connection of the pool, dialling only when none is idle.
    ///
    /// Reuse matters because this is the seam every reader goes through
    /// (`docs/decisions/0084-only-the-datahub-touches-the-store.md`):
    /// `crates/vike-client/src/account_report/excursions.rs`'s `backfill_excursions` calls `load_bars` once per
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
    /// default, `vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT`) and this pool keeps none alive, so
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
        let mut lease = self.lease()?;
        let mut answer = verb(lease.client());
        if answer.is_err() && lease.reused && !lease.client().is_usable() {
            lease.redial()?;
            answer = verb(lease.client());
        }
        match answer {
            Ok(value) => {
                lease.keep();
                Ok(value)
            }
            // The connection is NOT returned to the pool (the lease's drop closes it) — see the doc above.
            Err(e) => Err(HistoryStoreError::Query(e)),
        }
    }

    /// **The kept rows and splits of `venues` off the STAMPED cache** (the `kept` field): the datahub is asked every
    /// time, with the stamp this handle holds of each venue, and a venue it answers "unchanged" is served from the
    /// bytes held, so a second run of a venue moves no row over the wire. The answer is
    /// [`HistoryStore::kept_instruments_and_splits_of`]'s, encoded: the rows in roster order (the datahub's kept lists'
    /// order), the splits in venue-name order (its settings store's, `vike_secrets::stock_split::read_stock_splits`'s
    /// `ORDER BY v.name`). `None` is a datahub that does not serve the stamped verb, an older one (nothing was sent): the
    /// caller asks the JSON verb.
    fn kept_stamped(&self, venues: &[String]) -> Option<Result<EncodedKept, HistoryStoreError>> {
        let lock = || self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let mut asked: Vec<String> = Vec::with_capacity(venues.len());
        for venue in venues {
            if !asked.contains(venue) {
                asked.push(venue.clone());
            }
        }
        let held: Vec<Option<Arc<Kept>>> = {
            let kept = lock();
            asked.iter().map(|venue| kept.get(venue).cloned()).collect()
        };
        let known: BTreeMap<String, String> = asked
            .iter()
            .zip(&held)
            .filter_map(|(venue, held)| Some((venue.clone(), held.as_ref()?.stamp.clone())))
            .collect();
        let answer = self.with_client(|c| {
            if !c.serves_kept_rows_stamped() {
                return Ok(None);
            }
            c.kept_rows_of_venues(&asked, &known).map(Some)
        });
        let mut answer: BTreeMap<String, crate::proto::KeptRowsOfVenue> = match answer {
            Ok(Some(answer)) => answer.into_iter().map(|a| (a.venue.clone(), a)).collect(),
            Ok(None) => return None,
            Err(e) => return Some(Err(e)),
        };
        let desync = |venue: &str, why: &str| {
            Some(Err(HistoryStoreError::Query(format!(
                "protocol desync: the kept rows of `{venue}` {why}"
            ))))
        };
        let mut fresh: Vec<(String, Arc<Kept>)> = Vec::with_capacity(asked.len());
        for (venue, held) in asked.into_iter().zip(held) {
            let Some(row) = answer.remove(&venue) else {
                return desync(&venue, "were not answered");
            };
            let kept = match (row.changed, held) {
                (Some(encoded), _) => Arc::new(Kept { stamp: row.stamp, encoded }),
                (None, Some(held)) if held.stamp == row.stamp => held,
                (None, _) => {
                    return desync(&venue, "were answered unchanged from a stamp not sent");
                }
            };
            fresh.push((venue, kept));
        }
        let mut kept = lock();
        for (venue, row) in &fresh {
            kept.insert(venue.clone(), Arc::clone(row));
        }
        drop(kept);
        let at = |venue: &str| vike_model::VENUES.iter().position(|v| *v == venue);
        fresh.sort_by_key(|(venue, _)| at(venue).unwrap_or(usize::MAX));
        let rows: Vec<&[u8]> = fresh.iter().map(|(_, k)| k.encoded.rows.as_slice()).collect();
        let rows = crate::proto::join_msgpack_arrays(&rows);
        fresh.sort_by_key(|(venue, _)| venue.clone());
        let splits: Vec<&[u8]> = fresh.iter().map(|(_, k)| k.encoded.splits.as_slice()).collect();
        let splits = crate::proto::join_msgpack_arrays(&splits);
        Some(match (rows, splits) {
            (Ok(rows), Ok(splits)) => Ok(EncodedKept { rows, splits }),
            (Err(e), _) | (_, Err(e)) => Err(HistoryStoreError::Query(e)),
        })
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

    /// **The bars of `interval` over `range` folded by the datahub into bars of `to`** — `Request::LoadBars`'
    /// `resample_to`, for a chart zoomed out past what its own interval can carry. Not a `HistoryStore` verb
    /// (the store holds no such series), so an inherent method for [`Self::history_channels`]' reason. One
    /// reply, the whole range, as this handle's columns ([`Self::with_compressed_bars`]) where the server
    /// answers them.
    ///
    /// `Ok(None)` is a datahub OLDER than the field: it does not advertise
    /// [`crate::proto::FEATURE_BAR_RESAMPLE`] and nothing was sent.
    pub fn load_bars_resampled(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        to: &str,
    ) -> Result<Option<Vec<Bar>>, HistoryStoreError> {
        self.with_client(|client| {
            if !client.serves_bar_resample() {
                return Ok(None);
            }
            let body = Some(self.columns_body);
            client.load_bars_resampled(venue, symbol, interval, range, to, body).map(Some)
        })
    }

    /// **The open split warnings, over this store's connection** — `Request::SplitWarnings`, for the Data Manager's
    /// Overview. An inherent method for [`Self::history_channels`]' reason. `Ok(None)` is a datahub OLDER than the read:
    /// it does not advertise [`crate::proto::FEATURE_SPLIT_WARNINGS`] and nothing was sent.
    pub fn split_warnings(
        &self,
    ) -> Result<Option<Vec<crate::catalog::SplitWarning>>, HistoryStoreError> {
        self.with_client(|client| {
            if !client.serves_split_warnings() {
                return Ok(None);
            }
            client.split_warnings().map(Some)
        })
    }

    /// **Every account's latest position snapshot at or before `at_or_before_ms`**, over this store's connection —
    /// `Request::ExecutionAccountPositions`, what an account's window opens from (`vike-cli report account:`, the
    /// Account window's History Summary). An inherent method for [`Self::history_channels`]' reason. `Ok(None)` is a
    /// datahub that does not advertise [`crate::proto::FEATURE_EXECUTION_STREAM`]: nothing was sent, and it stores
    /// no snapshot. An `Err` is an unreachable store or a read that failed, never "no snapshot".
    pub fn execution_account_positions(
        &self,
        at_or_before_ms: i64,
    ) -> Result<
        Option<Vec<crate::execution_stream::ExecutionAccountPositionsWire>>,
        HistoryStoreError,
    > {
        self.with_client(|client| {
            if !client.features().iter().any(|f| f == crate::proto::FEATURE_EXECUTION_STREAM) {
                return Ok(None);
            }
            client.execution_account_positions(at_or_before_ms).map(Some)
        })
    }

    /// **What this store's datahub advertised in its `Welcome`**, over the reused connection (dialling
    /// when there is none) — for a reader that must say WHY a read is not available rather than
    /// render its refusal as an empty answer: the execution history asks it for
    /// [`crate::proto::FEATURE_SCAN_EXECUTION_ORDERS`], [`crate::proto::FEATURE_SCAN_EXECUTION_FILLS_RANGE`] and
    /// the runtime [`crate::proto::FEATURE_EXECUTIONS_RECORDED`]. An `Err` is an unreachable store.
    pub fn advertised_features(&self) -> Result<Vec<String>, HistoryStoreError> {
        self.with_client(|client| Ok(client.features().to_vec()))
    }

    /// **What this store's datahub advertises NOW**: the `Welcome` of a FRESH dial, closed at once and never pooled. A
    /// pooled connection's `Welcome` ([`Self::advertised_features`]) is the one of the datahub it dialled, which an
    /// upgrade of the datahub outlives; this is for a reader that must name the datahub as it is (the backtesthub's
    /// key of an answer from a saved run, `crate::proto::advertised_build`). An `Err` is an unreachable store.
    pub fn dialled_features(&self) -> Result<Vec<String>, HistoryStoreError> {
        self.dial().map(|client| client.features().to_vec())
    }
}

/// The `Request::ScanQuotes` of one page `w` of a read, asking `body` columns: what `scan_quotes` pages as rows and
/// `read_columns` as they came.
fn quotes_scan(
    venue: &str,
    symbol: &str,
    w: TimestampRange,
    limit: Option<u32>,
    body: BarColumnsBody,
) -> Request {
    let (venue, symbol) = (venue.to_string(), symbol.to_string());
    let (start, end, columns) = (w.start, w.end, Some(body));
    Request::ScanQuotes { venue, symbol, start, end, limit, source: None, columns }
}

/// [`quotes_scan`] of trades.
fn trades_scan(
    venue: &str,
    symbol: &str,
    w: TimestampRange,
    limit: Option<u32>,
    body: BarColumnsBody,
) -> Request {
    let (venue, symbol) = (venue.to_string(), symbol.to_string());
    Request::ScanTrades { venue, symbol, start: w.start, end: w.end, limit, columns: Some(body) }
}

/// [`quotes_scan`] of book updates.
fn books_scan(
    venue: &str,
    symbol: &str,
    w: TimestampRange,
    limit: Option<u32>,
    body: BarColumnsBody,
) -> Request {
    let (venue, symbol) = (venue.to_string(), symbol.to_string());
    Request::ScanBookUpdates {
        venue,
        symbol,
        start: w.start,
        end: w.end,
        limit,
        columns: Some(body),
    }
}

/// Append one page a verb answered as a `Vec` to the read so far, MOVING the first page in (no copy) and
/// appending nothing when the verb failed: [`HistoryStoreDatahub`]'s pager's rule for a page.
fn append_page<T>(out: &mut Vec<T>, page: Result<Vec<T>, String>) -> Result<(), String> {
    let page = page?;
    if out.is_empty() {
        *out = page;
    } else {
        out.extend(page);
    }
    Ok(())
}

/// [`HistoryStoreDatahub`]'s connections: the ones no read holds, and how many exist, idle and leased
/// together (never more than [`HistoryStoreDatahub::POOL_CONNECTIONS`]).
#[derive(Default)]
struct Pool {
    idle: Vec<DatahubClient>,
    open: usize,
}

/// One read's connection, from [`HistoryStoreDatahub::lease`]. **Dropped, it is CLOSED and its slot
/// freed**; only [`Lease::keep`] returns it to the pool, so an error, an early `?` and a panic all
/// close the connection by default — `with_client` argues why an errored one may not be reused.
struct Lease<'a> {
    store: &'a HistoryStoreDatahub,
    /// `Some` from the lease's first moment it is handed out; `None` only inside [`Self::redial`]
    /// and after [`Self::keep`].
    client: Option<DatahubClient>,
    /// Taken idle from the pool (it may have been closed by the datahub while it sat there), not
    /// dialled for this read.
    reused: bool,
    kept: bool,
}

impl Lease<'_> {
    fn client(&mut self) -> &mut DatahubClient {
        self.client.as_mut().expect("a handed-out lease holds a connection")
    }

    /// Close this lease's connection and dial a fresh one in the same slot.
    fn redial(&mut self) -> Result<(), HistoryStoreError> {
        self.client = None;
        self.client = Some(self.store.dial()?);
        self.reused = false;
        Ok(())
    }

    /// Return the connection to the pool for the next read.
    fn keep(mut self) {
        if let Some(client) = self.client.take() {
            self.store.lock_pool().idle.push(client);
            self.kept = true;
            self.store.freed.notify_one();
        }
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if !self.kept {
            self.client = None;
            self.store.lock_pool().open -= 1;
            self.store.freed.notify_one();
        }
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
    // Range reads go through `paged`/`paged_until`, the rest through `with_client`; both lease a
    // connection from the pool for the read.
    // The client's ONE `Err(String)` channel maps to `HistoryStoreError::Query`; a connect failure is
    // `HistoryStoreError::Io`, raised by `dial`.

    /// Paged by [`Self::BAR_PAGE_ROWS`], and each page asked as binary COLUMNS where the connection's server
    /// advertises [`crate::proto::FEATURE_BAR_COLUMNS`] (`DatahubClient::load_bar_columns`), else as JSON: the
    /// same bars either way, decided per page because a redial may land on another build.
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.paged_until(
            range,
            None,
            |c| self.columns_page(c.serves_bar_columns()),
            |r: &Bar| r.timestamp,
            |c, w, n, out| {
                if c.serves_bar_columns() {
                    let body = self.columns_body;
                    c.load_bar_columns_into(venue, None, symbol, interval, w, n, body, out)
                        .map(drop)
                } else {
                    append_page(out, c.load_bars(venue, symbol, interval, w, n))
                }
            },
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
            |c| self.columns_page(c.serves_bar_columns()),
            |r: &Bar| r.timestamp,
            |c, w, n, out| {
                if c.serves_bar_columns() {
                    let body = self.columns_body;
                    c.load_bar_columns_into(venue, Some(source), symbol, interval, w, n, body, out)
                        .map(drop)
                } else {
                    let page = c.load_bars_sourced(venue, source, symbol, interval, w, n);
                    append_page(out, page)
                }
            },
        )
    }

    /// Paged as columns where the server answers them ([`Self::tick_paged`]), else JSON.
    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.tick_paged(
            range,
            |r: &TickQuote| r.timestamp,
            |c, w, n| c.scan_quotes(venue, symbol, w, n),
            |w, limit, body| quotes_scan(venue, symbol, w, limit, body),
        )
    }

    /// Paged as columns where the server answers them ([`Self::tick_paged`]), else JSON.
    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.tick_paged(
            range,
            |r: &TickTrade| r.timestamp,
            |c, w, n| c.scan_trades(venue, symbol, w, n),
            |w, limit, body| trades_scan(venue, symbol, w, limit, body),
        )
    }

    /// The bar and tick reads' pages AS THE DATAHUB SENT THEM ([`Self::column_pages`]), where the connection's server
    /// answers that read as columns ([`crate::proto::FEATURE_BAR_COLUMNS`], [`crate::proto::FEATURE_TICK_COLUMNS`]):
    /// the rows the typed read answers, in its pages, each checked as its decode would check it and decoded by nothing,
    /// for a reader that keeps or relays the columns (the backtesthub's cache and its sandbox relay). `None`, nothing
    /// sent, from a server that answers only rows, and for a read this does not name.
    fn read_columns(
        &self,
        kind: StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: TimestampRange,
    ) -> Option<Result<SharedColumns, HistoryStoreError>> {
        let body = self.columns_body;
        let ticks = DatahubClient::serves_tick_columns;
        match (kind, source, interval) {
            (StoreKindId::Bar, source, Some(interval)) => {
                let bars = DatahubClient::serves_bar_columns;
                self.column_pages::<Bar>(range, &BAR_COLUMNS, bars, |c, w, n| {
                    c.load_bar_columns_plain(venue, source, symbol, interval, w, n, body)
                })
            }
            (StoreKindId::Quote, None, None) => {
                self.column_pages::<TickQuote>(range, &QUOTE_COLUMNS, ticks, |c, w, n| {
                    c.scan_columns_plain(&quotes_scan(venue, symbol, w, n, body))
                })
            }
            (StoreKindId::Trade, None, None) => {
                self.column_pages::<TickTrade>(range, &TRADE_COLUMNS, ticks, |c, w, n| {
                    c.scan_columns_plain(&trades_scan(venue, symbol, w, n, body))
                })
            }
            (StoreKindId::Book, None, None) => {
                let magic = &BOOK_UPDATE_COLUMNS;
                self.column_pages::<L2BookUpdate>(range, magic, ticks, |c, w, n| {
                    c.scan_columns_plain(&books_scan(venue, symbol, w, n, body))
                })
            }
            _ => None,
        }
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
        self.tick_paged(
            range,
            |r: &L2BookUpdate| r.timestamp,
            |c, w, n| c.scan_book_updates(venue, symbol, w, n),
            |w, limit, body| books_scan(venue, symbol, w, limit, body),
        )
    }

    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.tick_paged(
            range,
            |r: &L2BookUpdate| r.timestamp,
            |c, w, n| c.scan_depth(venue, symbol, w, n),
            |w, limit, body| Request::ScanDepth {
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                start: w.start,
                end: w.end,
                limit,
                columns: Some(body),
            },
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
        self.tick_paged(
            range,
            |r: &PerpMetricRow| r.ts,
            |c, w, n| c.scan_perp_metrics(venue, symbol, w, n),
            |w, limit, body| Request::ScanPerpMetrics {
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                start: w.start,
                end: w.end,
                limit,
                columns: Some(body),
            },
        )
    }

    /// The text history, paged over the RECEIVED time; `texts_as_of` is derived over it, so a
    /// study's point-in-time read crosses as these pages. A datahub without `scan_texts` is refused
    /// on this side, by name (`DatahubClient::scan_texts`).
    fn scan_texts(
        &self,
        text_kind: &str,
        collector: &str,
        range: TimestampRange,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        self.paged(
            range,
            |r: &TextRow| r.received_ms,
            |c, w, n| c.scan_texts(text_kind, collector, w, n),
        )
    }

    /// Paged like the others: the server cuts a page only between two timestamps, so a snapshot's
    /// rows (one `ts`, hundreds of rows) never straddle two pages. `chain_as_of` and
    /// `chain_as_of_within` are NOT overridden: the trait derives both from this read, so the
    /// point-in-time fold runs here, over the rows the server sent.
    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        self.paged(range, |r: &ChainRow| r.ts, |c, w, n| c.scan_chain(venue, underlying, w, n))
    }

    /// A company's XBRL values, paged over the KNOWABLE time; `financial_facts_as_of` is derived over
    /// it. A datahub without `scan_sec_tables` is refused on this side, by name.
    fn scan_financial_facts(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        self.paged(
            range,
            |r: &FinancialFactRow| r.filed_ms,
            |c, w, n| c.scan_financial_facts(venue, symbol, w, n),
        )
    }

    /// A manager's Form 13F lines, paged over the KNOWABLE time; `institutional_holdings_as_of` is
    /// derived over it.
    fn scan_institutional_holdings(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        self.paged(
            range,
            |r: &InstitutionalHoldingRow| r.filed_ms,
            |c, w, n| c.scan_institutional_holdings(venue, symbol, w, n),
        )
    }

    /// The text scores, paged over their texts' RECEIVED time; `text_scores_as_of` is derived over
    /// it, as `texts_as_of` is over [`Self::scan_texts`].
    fn scan_text_scores(
        &self,
        text_kind: &str,
        model_name: &str,
        range: TimestampRange,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        self.paged(
            range,
            |r: &TextScoreRow| r.received_ms,
            |c, w, n| c.scan_text_scores(text_kind, model_name, w, n),
        )
    }

    /// The live strategies' published forecasts, paged over the DECISION time; `forecasts_as_of` is
    /// derived over it, so a study's point-in-time read crosses as these pages. A datahub without
    /// `scan_forecasts` is refused on this side, by name (`DatahubClient::scan_forecasts`).
    fn scan_forecasts(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        self.paged(
            range,
            |r: &ExecForecastRow| r.ts,
            |c, w, n| c.scan_forecasts(venue, symbol, w, n),
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
                |_| Self::PAGE_ROWS,
                |r: &ExecFillRow| r.ts,
                |c, w, l, out| append_page(out, c.scan_execution_fills_range(venue, symbol, w, l)),
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
            |_| Self::PAGE_ROWS,
            |r: &ExecOrderRow| r.ts,
            |c, w, l, out| append_page(out, c.scan_exec_orders(venue, symbol, w, l)),
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

    /// ⚠ An OVERRIDE: a datahub advertising [`crate::proto::FEATURE_EXECUTION_SERIES`] answers the window's
    /// series itself, a few KB; an older one is asked the WHOLE inventory and it is folded here, the trait
    /// default's answer.
    fn execution_series(
        &self,
        venue: Option<&str>,
        symbol: Option<&str>,
        range: TimestampRange,
    ) -> Result<ExecutionSeries, HistoryStoreError> {
        let served = self.with_client(|c| {
            if !c.serves_execution_series() {
                return Ok(None);
            }
            c.execution_series(venue, symbol, range).map(Some)
        })?;
        match served {
            Some(answer) => Ok(answer),
            None => Ok(ExecutionSeries::of(self.inventory()?, venue, symbol, range)),
        }
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

    /// ⚠ An OVERRIDE: the trait default (`None`) would leave the backtesthub's bar and tick cache keeping nothing about
    /// a server that knows its days. `None` here is a datahub too old for `SeriesDays`, or for its tick kinds
    /// (nothing was sent).
    fn series_days(
        &self,
        kind: vike_model::StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: TimestampRange,
    ) -> Result<Option<Vec<BarDay>>, HistoryStoreError> {
        self.with_client(|c| c.series_days(kind, venue, source, symbol, interval, range))
    }

    // ⚠ The trait DEFAULT is still `Ok(vec![])` for THIS verb (its trait doc carries the
    // vacuous-truth argument and the FRONTS warning this override answers) — right for a store that
    // cannot join its kinds, a LIE here: an inherited empty would render the Data Manager's Partial
    // column blank and say "nothing is partial".
    //
    // An OLD server is told apart from a failed read: when it does not advertise `FEATURE_COVERAGE`
    // nothing is sent and the answer is `HistoryStoreError::Unsupported` (a capability answer), which
    // `vike_app_core::data::data_manager::mode` words as a server older than the desktop. Every other
    // failure stays `Query` (or `Io` for a dial), as on every verb here.
    fn coverage_report(&self) -> Result<Vec<InstrumentCoverage>, HistoryStoreError> {
        self.with_client(|c| {
            if !c.features().iter().any(|f| f == FEATURE_COVERAGE) {
                return Ok(None);
            }
            c.coverage_report().map(Some)
        })?
        .ok_or_else(|| {
            HistoryStoreError::Unsupported(format!(
                "this datahub predates the cross-kind coverage report (it does not advertise \
                 `{FEATURE_COVERAGE}`); nothing was sent"
            ))
        })
    }

    // The KEPT lists (`Request::InstrumentLists`, a Read verb), every venue's rows; the operator's own lists are
    // not kept rows and stay out. `Ok(None)` is a datahub that keeps none (no project folder) or is too old to: nothing
    // was sent.
    fn kept_instruments(&self) -> Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError> {
        self.with_client(|c| {
            if !c.serves_instrument_lists() {
                return Ok(None);
            }
            let lists = c.instrument_lists()?.venues;
            Ok(Some(lists.into_iter().flat_map(|l| l.instruments).collect()))
        })
    }

    // The operator's share splits, carried by the same `Request::InstrumentLists` answer. `Ok(None)` is a datahub too
    // old to keep the lists (nothing was sent); one too old to serve splits answers none.
    fn stock_splits(&self) -> Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError> {
        self.with_client(|c| {
            if !c.serves_instrument_lists() {
                return Ok(None);
            }
            Ok(Some(c.instrument_lists()?.splits))
        })
    }

    // Both of the above off ONE `Request::InstrumentLists` reply.
    fn kept_instruments_and_splits(
        &self,
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
    ) {
        kept_rows_and_splits(self.with_client(|c| {
            if !c.serves_instrument_lists() {
                return Ok(None);
            }
            Ok(Some(c.instrument_lists()?))
        }))
    }

    // Both of the above for `venues` alone: off the stamped cache (`kept_stamped`) where the datahub serves it, its
    // msgpack decoded here; else off ONE `Request::InstrumentListsOfVenues` reply (a datahub too old to filter answers
    // the whole lists, filtered by `DatahubClient::instrument_lists_of_venues`).
    fn kept_instruments_and_splits_of(
        &self,
        venues: &[String],
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
    ) {
        if let Some(encoded) = self.kept_stamped(venues) {
            fn decoded<T: serde::de::DeserializeOwned>(
                bytes: &[u8],
            ) -> Result<Option<T>, HistoryStoreError> {
                rmp_serde::from_slice(bytes).map(Some).map_err(|e| {
                    HistoryStoreError::Query(format!("the kept rows could not be decoded: {e}"))
                })
            }
            return match encoded {
                Ok(kept) => (decoded(&kept.rows), decoded(&kept.splits)),
                Err(e) => (Err(e.clone()), Err(e)),
            };
        }
        kept_rows_and_splits(self.with_client(|c| {
            if !c.serves_instrument_lists() {
                return Ok(None);
            }
            Ok(Some(c.instrument_lists_of_venues(venues)?))
        }))
    }

    // The same answer as msgpack, off the stamped cache as it is held: what a sandboxed child is handed with no
    // conversion. `None` from a datahub that does not serve the stamped verb.
    fn kept_instruments_and_splits_encoded_of(
        &self,
        venues: &[String],
    ) -> Option<Result<EncodedKept, HistoryStoreError>> {
        self.kept_stamped(venues)
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

    // ⚠ The text history's WRITE is the datahub's own recorder's, never a client's: "not served",
    // rather than the seam's "this store does not hold kind=text", which would be false of the server.

    fn append_texts(
        &self,
        _text_kind: &str,
        _collector: &str,
        _rows: &[TextRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_texts"))
    }

    // The SEC tables' WRITE is the datahub's own import's, never a client's: "not served" as well.

    fn append_financial_facts(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[FinancialFactRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_financial_facts"))
    }

    fn append_institutional_holdings(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[InstitutionalHoldingRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_institutional_holdings"))
    }

    // ⚠ The text scores' WRITE is the datahub's `text-scores` import, never a client's.

    fn append_text_scores(
        &self,
        _text_kind: &str,
        _model_name: &str,
        _rows: &[TextScoreRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(unsupported("append_text_scores"))
    }
}

/// One kept-lists reply as the trait's two answers: the fetched lists' rows, every venue's in order, and the splits;
/// `None` (a datahub that keeps no list) is both `None`, and a failure is both.
#[expect(clippy::type_complexity, reason = "the trait's two answers, side by side")]
fn kept_rows_and_splits(
    reply: Result<Option<crate::catalog::InstrumentListsReply>, HistoryStoreError>,
) -> (
    Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
    Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
) {
    match reply {
        Ok(Some(reply)) => (
            Ok(Some(reply.venues.into_iter().flat_map(|l| l.instruments).collect())),
            Ok(Some(reply.splits)),
        ),
        Ok(None) => (Ok(None), Ok(None)),
        Err(e) => (Err(e.clone()), Err(e)),
    }
}
