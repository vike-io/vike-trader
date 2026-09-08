//! [`RemoteHistStore`] — a READ-ONLY [`vike_data::HistStore`] backed by a `vike-datahub` server
//! over the [`crate::proto`] RPC protocol.
//!
//! # What it is
//!
//! The Phase-2 enabler for the DataFusion-free GUI. It implements the GUI-relevant `HistStore` READ
//! verbs — [`HistStore::load_bars`], [`HistStore::scan_quotes`], [`HistStore::scan_trades`],
//! [`HistStore::properties_as_of`], plus the PR-6 store-metadata verbs [`HistStore::list_series`],
//! [`HistStore::inventory`], [`HistStore::series_gaps`] and their §6-Q2 sibling
//! [`HistStore::coverage_report`] (the Data-Manager / Studio catalog) — by
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
//! NOT in the served set (`scan_book_updates`, `scan_depth`, `scan_symbol_properties`, `scan_equity`,
//! `scan_exec_*`, `scan_funding`, and the whole `chain_*` family) returns an `Err` — see
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

use vike_data::{
    ChainRow, CohortRow, DataError, ExecFillRow, ExecOrderRow, FundingRow, HistStore,
    InstrumentCoverage, SeriesCoverage, SeriesId, TsRange,
};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

use crate::client::DatahubClient;
use crate::node_auth::{NodeKeys, Scope};

/// A READ-ONLY [`HistStore`] served by a `vike-datahub` server over the datahub RPC protocol. See
/// the [module docs](crate::remote): only the GUI read verbs + the store-metadata verbs
/// (`list_series`/`inventory`/`series_gaps`/`coverage_report`) are served; every other method
/// returns [`DataError::Query`] with a "not served over RPC" message.
pub struct RemoteHistStore {
    /// The datahub server address (e.g. `"127.0.0.1:7878"`). A fresh connection is dialled per read.
    addr: String,
    /// The node keys every dialled connection authenticates with, or `None` for the unauthenticated
    /// connect this type has always done. See [`RemoteHistStore::with_keys`].
    keys: Option<NodeKeys>,
}

impl RemoteHistStore {
    /// Point at a datahub server `addr` (e.g. `"127.0.0.1:7878"`). No connection is opened here —
    /// each read verb dials a fresh [`DatahubClient`] (see the module docs' connection model).
    ///
    /// Unauthenticated: correct against a key-less server (the default, and every deployment before
    /// `docs/decisions/0025-datahub-remote-posture.md` was adopted). Against a KEYED server every
    /// read fails with the connect error naming the missing keys — use [`Self::with_keys`].
    pub fn new(addr: impl Into<String>) -> Self {
        Self { addr: addr.into(), keys: None }
    }

    /// Point at a datahub server and AUTHENTICATE every dialled connection under
    /// [`Scope::Observe`].
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
        Self { addr: addr.into(), keys: Some(keys) }
    }

    /// Dial a fresh client for one read verb. A connect failure maps to [`DataError::Io`] — a
    /// genuine transport failure (the server is down / unreachable), OR now an authentication
    /// refusal — distinct from the [`DataError::Query`] a *served* verb returns on a server-side
    /// error (mapped in each verb).
    fn client(&self) -> Result<DatahubClient, DataError> {
        match &self.keys {
            Some(keys) => DatahubClient::connect_authed(&self.addr, keys, Scope::Observe),
            None => DatahubClient::connect(&self.addr),
        }
        .map_err(|e| DataError::Io(e.to_string()))
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
    // ---- served read verbs (the four GUI reads, answered over RPC) --------------------------------
    //
    // Each opens a fresh connection, calls the matching client verb, and maps the client's ONE
    // `Err(String)` channel (a post-connect transport failure OR a server-side `Response::Error`) to
    // `DataError::Query`. A connect failure is already `DataError::Io` via `self.client()?`.

    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        self.client()?.load_bars(venue, symbol, interval, range).map_err(DataError::Query)
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        self.client()?.scan_quotes(venue, symbol, range).map_err(DataError::Query)
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        self.client()?.scan_trades(venue, symbol, range).map_err(DataError::Query)
    }

    fn properties_as_of(
        &self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, DataError> {
        self.client()?.properties_as_of(venue, symbol, ts).map_err(DataError::Query)
    }

    // ---- served store-metadata verbs (PR-6: the Data-Manager / Studio catalog over RPC) ----------
    //
    // These OVERRIDE the trait defaults (the catalog pair now REFUSES by default — right for a
    // leaf without the verbs, wrong here, where the SERVER can answer) with real RPC answers —
    // safe to serve whole because the catalog is TINY (a series list + cheap manifest-fold
    // coverage, NO Parquet scan), so unlike
    // a tick SLICE this never risks streaming the tape to the client (the compute-to-data invariant).

    fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
        self.client()?.list_series().map_err(DataError::Query)
    }

    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
        self.client()?.inventory().map_err(DataError::Query)
    }

    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, DataError> {
        self.client()?.series_gaps(id).map_err(DataError::Query)
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
        self.client()?.coverage_report().map_err(DataError::Query)
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

    fn scan_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Err(unsupported("scan_book_updates"))
    }

    // The override PREDATES the trait default that now agrees with it, and stays because the two
    // refusals are not the same fact. Split-plane B12 found `scan_depth` falling through to a
    // default of `Ok(vec![])` — a fabricated "no data", since the server may well hold depth and
    // only the RPC lacks the verb — and I5 then fixed that default for every impl. This override
    // survives that fix: the trait's message says "this store serves no depth lane", which is
    // wrong HERE, and `unsupported` says the true reason instead.
    fn scan_depth(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Err(unsupported("scan_depth"))
    }

    fn scan_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Err(unsupported("scan_symbol_properties"))
    }

    fn scan_equity(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError> {
        Err(unsupported("scan_equity"))
    }

    fn scan_exec_fills(&self, _venue: &str, _symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Err(unsupported("scan_exec_fills"))
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

    /// ⚠ The read half of the same override, and the one that matters more. `scan_cohort` defaults
    /// to `Ok(Vec::new())`, which is the fabricated empty `scan_depth` was corrected for: the
    /// SERVER may hold cohort panels for this asset — only the RPC has no verb to ask with — so
    /// answering "there are none" would be this client inventing the store's answer.
    fn scan_cohort(
        &self,
        _venue: &str,
        _asset: &str,
        _range: TsRange,
    ) -> Result<Vec<CohortRow>, DataError> {
        Err(unsupported("scan_cohort"))
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
