//! The blocking backtesthub client: a [`HubConnection`] that speaks the [`crate::proto`] request/response
//! protocol. The `Run*` family (`run_backtest`, `run_slice`, `run_paramscan`, `run_walkforward`,
//! `run_paramscan_profile`, `run_study`, `run_walkforward_profile`), the strategy-roster / named-run
//! verbs (`list_strategies`, `named_strategies`, `run_named`, `serves_named_run`), the server's studies
//! and saved runs (`list_studies`, `list_runs`, `read_run_artifact`) and its strategy SDKs' source
//! (`list_sdks`, `read_sdk_file`).
//!
//! Connecting, authenticating and bounding a reply are [`HubConnection`]'s, shared with the datahub's client
//! (`crates/vike-hub-protocol/src/conn.rs`'s module doc carries each rule). [`BacktesthubClient`] derefs to
//! it, so `features`, `set_reply_timeout`, `cancel_handle`, `is_usable`, `requires_auth` and
//! `authenticated_scope` are called on the client itself.
//!
//! Every capability check here refuses BEFORE the write, sending nothing
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).

use std::io;
use std::net::ToSocketAddrs;
use std::ops::{Deref, DerefMut};
use std::time::Duration;

use vike_hub_protocol::auth::{BACKTESTHUB_DOMAIN, HubKeys, Scope};
use vike_hub_protocol::call::HubCallError;
use vike_hub_protocol::conn::HubConnection;
use vike_hub_protocol::features::FEATURE_WIRE_MSGPACK;
use vike_hub_protocol::frame::BodyCodec;
use vike_hub_protocol::plane::Plane;

use crate::named_run::{NamedRunOutcome, NamedRunRoster, NamedRunSpec, validate_named_run};
use crate::proto::{
    FEATURE_ATTACH_FUNDING, FEATURE_BUILD_STRATEGY_FILE, FEATURE_NAMED_RUN,
    FEATURE_PARAMSCAN_PROFILE_SUMMARY, FEATURE_RHAI_PARAMETERS, FEATURE_RUN_CURVE,
    FEATURE_RUN_FILES, FEATURE_RUN_FROM_SAVED, FEATURE_RUN_SUMMARY, FEATURE_SDK_SOURCE,
    FEATURE_SEARCH_METHOD, FEATURE_SLIPPAGE_MODEL, FEATURE_STRATEGY_FILES, FEATURE_STRATEGY_SDK,
    FEATURE_STUDY, FEATURE_STUDY_FILES, FEATURE_WALKFORWARD_SEARCH, PROTO_VERSION, Request,
    Response, SearchSelectionWire, StudyRunRequestWire, WireStrategyFile, may_answer_from_saved,
    reply_wait, request_kind,
};
use crate::sdk_source::{WireSdkFile, WireSdkListing};
use crate::study::{WireRunArtifact, WireRunPage, WireStudyListing};
use crate::summary::{
    CURVE_PAGE_SIZE_MAX, CurvePageWire, ParamscanProfileSummaryWire, ParamscanRowsPageWire,
    ParamscanSummaryWire, RunSummaryWire, TradesPageWire, WalkforwardSummaryWire,
};
use crate::wire_studio::{
    DataSliceWire, ParamscanWire, StrategyEngineConfigWire, StrategySpecWire, StudioParamscanWire,
    TradingRecordWire, WalkforwardResultsWire, WalkforwardWindowSearchWire, WalkforwardWire,
};

/// A connected backtesthub client: a [`HubConnection`] speaking this protocol. Drop it to close the
/// connection.
#[derive(Debug)]
pub struct BacktesthubClient {
    conn: HubConnection,
    /// Every run this client sends is asked FRESH ([`Self::set_fresh`]).
    fresh: bool,
}

impl Deref for BacktesthubClient {
    type Target = HubConnection;

    fn deref(&self) -> &HubConnection {
        &self.conn
    }
}

impl DerefMut for BacktesthubClient {
    fn deref_mut(&mut self) -> &mut HubConnection {
        &mut self.conn
    }
}

/// What a profile's parameter search answered ([`BacktesthubClient::run_paramscan_profile_answer`]).
#[derive(Debug)]
pub enum ParamscanProfileAnswer {
    /// The SUMMARIZED search, every page of its rows read.
    Summary(Box<ParamscanProfileSummaryWire>),
    /// The whole report's JSON text: asked whole, from a daemon without the summary, or a data plan.
    Whole(String),
}

/// `why` a page of a summarized search was lost, with where the search itself is: it FINISHED, and the run the daemon
/// saved holds every point whole (a page is lost when the daemon dropped the search from memory, for newer runs).
fn with_the_saved_run(why: String, summary: &ParamscanProfileSummaryWire) -> String {
    match summary.saved_run.as_str() {
        "" => format!("{why} (the search finished; the daemon saved no run of it)"),
        id => format!(
            "{why}. The search finished: its whole report, every point, is the saved run {id} \
             (`vike-cli backtest show {id}`)"
        ),
    }
}

/// The variant NAME of a response (no payload), its wire tag, for protocol-desync error messages, so we
/// never stringify a whole (possibly large) `RunResult` payload just to report "unexpected variant".
fn response_kind(r: &Response) -> &'static str {
    match r {
        Response::Pong => "Pong",
        Response::Report(_) => "Report",
        Response::RunResult(_) => "RunResult",
        Response::ParamscanResult(_) => "ParamscanResult",
        Response::WalkforwardResult(_) => "WalkforwardResult",
        Response::ParamscanReport(_) => "ParamscanReport",
        Response::WalkforwardReport(_) => "WalkforwardReport",
        Response::CombinatorialWalkforwardReport(_) => "CombinatorialWalkforwardReport",
        Response::PermutationReport(_) => "PermutationReport",
        Response::StudyReport(_) => "StudyReport",
        Response::Error(_) => "Error",
        Response::Strategies(_) => "Strategies",
        Response::NamedStrategies(_) => "NamedStrategies",
        Response::NamedRun(_) => "NamedRun",
        Response::StrategyFiles(_) => "StrategyFiles",
        Response::StrategyFileText { .. } => "StrategyFileText",
        Response::StrategyFileSaved { .. } => "StrategyFileSaved",
        Response::StrategyFileBuilt { .. } => "StrategyFileBuilt",
        Response::RunSummary(_) => "RunSummary",
        Response::ParamscanSummary(_) => "ParamscanSummary",
        Response::WalkforwardSummary(_) => "WalkforwardSummary",
        Response::TradesPage(_) => "TradesPage",
        Response::ParamscanProfileSummary(_) => "ParamscanProfileSummary",
        Response::ParamscanRows(_) => "ParamscanRows",
        Response::CurvePage(_) => "CurvePage",
        Response::Studies(_) => "Studies",
        Response::Runs(_) => "Runs",
        Response::RunArtifact(_) => "RunArtifact",
        Response::Sdks(_) => "Sdks",
        Response::SdkFile(_) => "SdkFile",
    }
}

vike_utility::refusal_enum! {
    /// A [`BacktesthubClient::dial`] that reached no backtesthub: ONE sentence for `vike-cli` and `vike-mcp`
    /// (decision 0252: neither links the other), naming the invocation that serves this plane. Its id is the one
    /// `vike-cli`'s flag reference published first.
    #[derive(Debug)]
    pub enum DialRefusal with ids DialRefusalId {
        /// The dial or its handshake failed; `e` is what the call DID, the door's exit rung decides on it.
        CannotConnect { addr: String, e: HubCallError } => "cli.backtest.addr.cannot_connect":
            "cannot connect to the backtest daemon at {addr}: {e} (start it with `vike-backend backtesthub --addr`)",
    }
}

impl DialRefusal {
    /// What a door shows, under ONE id: a door's refusal of this box keeps its own (`hub.access.denied`, the
    /// daemon answered, so "start it" would mislead); any other failure is this refusal, rendered.
    #[must_use]
    pub fn shown(&self) -> String {
        match self {
            DialRefusal::CannotConnect { addr, e: e @ HubCallError::AccessDenied(_) } => {
                format!("the backtest daemon at {addr} refused this box: {e}")
            }
            DialRefusal::CannotConnect { .. } => self.rendered(),
        }
    }
}

impl BacktesthubClient {
    /// [`HubConnection::connect`], speaking [`PROTO_VERSION`]. Against a KEYED server this fails HERE, with an
    /// actionable message; use [`connect_authed`](Self::connect_authed) for those.
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self, HubCallError> {
        Ok(Self::negotiated(HubConnection::connect(addr, Plane::Backtest, PROTO_VERSION)?))
    }

    /// A freshly handshaken connection as this plane's client: msgpack toward a daemon that advertises
    /// [`FEATURE_WIRE_MSGPACK`], JSON toward an older one ([`prefer_body`](Self::prefer_body)).
    fn negotiated(conn: HubConnection) -> Self {
        let mut client = Self { conn, fresh: false };
        client.prefer_body(BodyCodec::Msgpack);
        client
    }

    /// **Choose the body every later request is written in**, and so every reply (the daemon answers in the
    /// request's codec): `wanted` when the daemon advertises [`FEATURE_WIRE_MSGPACK`], JSON when it does not (an
    /// older daemon reads only JSON). Every constructor asks [`BodyCodec::Msgpack`]; a client on a THIN link
    /// (the desktop over its ssh tunnel) asks [`BodyCodec::MsgpackLz4`], and a caller may ask
    /// [`BodyCodec::Json`] back. Answers the codec now in force. Nothing is sent.
    pub fn prefer_body(&mut self, wanted: BodyCodec) -> BodyCodec {
        let served = self.conn.features().iter().any(|f| f == FEATURE_WIRE_MSGPACK);
        self.conn.set_body_codec(if served { wanted } else { BodyCodec::Json });
        self.conn.body_codec()
    }

    /// [`HubConnection::connect_authed`], speaking [`PROTO_VERSION`]. ⚠ A KEY-LESS server is a successful
    /// unauthenticated connect, not an error; a caller that must be sure it authenticated checks
    /// `authenticated_scope`.
    pub fn connect_authed<A: ToSocketAddrs>(
        addr: A,
        keys: &HubKeys,
        scope: Scope,
    ) -> Result<Self, HubCallError> {
        Ok(Self::negotiated(HubConnection::connect_authed(
            addr,
            keys,
            scope,
            BACKTESTHUB_DOMAIN,
            PROTO_VERSION,
        )?))
    }

    /// [`HubConnection::connect_authed_on`], speaking [`PROTO_VERSION`]: refused before the first byte of
    /// `Auth` unless the `Welcome` names `plane`. One device key is on both daemons' access list, so the
    /// guard names a dial to the wrong port before anything is signed (the signature itself, under
    /// [`BACKTESTHUB_DOMAIN`], would be refused by the data daemon).
    pub fn connect_authed_on<A: ToSocketAddrs>(
        addr: A,
        keys: &HubKeys,
        scope: Scope,
        plane: Plane,
    ) -> Result<Self, HubCallError> {
        Ok(Self::negotiated(HubConnection::connect_authed_on(
            addr,
            keys,
            scope,
            plane,
            BACKTESTHUB_DOMAIN,
            PROTO_VERSION,
        )?))
    }

    /// [`HubConnection::connect_with_keys`], speaking [`PROTO_VERSION`]: `connect_authed` under `scope` when
    /// `keys` is `Some`, plain `connect` when it is `None`. ⚠ UNGUARDED on the plane: a caller holding a
    /// key for ONE plane dials [`connect_authed_on`](Self::connect_authed_on) instead.
    pub fn connect_with_keys<A: ToSocketAddrs>(
        addr: A,
        keys: Option<&HubKeys>,
        scope: Scope,
    ) -> Result<Self, HubCallError> {
        Ok(Self::negotiated(HubConnection::connect_with_keys(
            addr,
            keys,
            scope,
            BACKTESTHUB_DOMAIN,
            PROTO_VERSION,
        )?))
    }

    /// **The one dial `vike-cli` and `vike-mcp` make for a backtesthub verb**: [`connect_with_keys`](Self::connect_with_keys)
    /// at `addr` under `scope` (a run COMPILES client-supplied Rhai, so [`Scope::Write`]; a roster read
    /// [`Scope::Read`]), a failure as [`DialRefusal::CannotConnect`].
    pub fn dial(addr: &str, keys: Option<&HubKeys>, scope: Scope) -> Result<Self, DialRefusal> {
        Self::connect_with_keys(addr, keys, scope)
            .map_err(|e| DialRefusal::CannotConnect { addr: addr.to_string(), e })
    }

    /// The idle reply bound a verb arms for `request` on this client before writing it: the caller's
    /// `set_reply_timeout` value, else the per-verb default ([`crate::proto::reply_wait`]'s class).
    pub fn reply_timeout_for(&self, request: &Request) -> Option<Duration> {
        self.conn.reply_bound(reply_wait(request).default_bound())
    }

    /// **Ask every later run FRESH** (decision 0078's amendment of 2026-11-20): a backtesthub that advertises
    /// [`FEATURE_RUN_FROM_SAVED`] answers a run identical to one it saved from that saved run; `fresh` makes it run each
    /// one again (a measurement, or a strategy known to answer otherwise each time), wrapping it in
    /// [`Request::Fresh`]. Toward an older backtesthub nothing is wrapped: it answers no run from a saved one. Off by
    /// default; nothing is sent.
    pub fn set_fresh(&mut self, fresh: bool) {
        self.fresh = fresh;
    }

    /// **The ONE post-handshake exchange of this protocol**: [`HubConnection::exchange_frame`] under the verb's
    /// wire tag and its [`crate::proto::reply_wait`] class. Every positional verb goes through here; a run goes
    /// in [`Request::Fresh`] when [`Self::set_fresh`] asked for it and the backtesthub reads it.
    fn exchange(&mut self, request: &Request) -> Result<Response, HubCallError> {
        let served = || self.features().iter().any(|f| f == FEATURE_RUN_FROM_SAVED);
        let fresh;
        let request = if self.fresh && may_answer_from_saved(request) && served() {
            fresh = Request::Fresh(Box::new(request.clone()));
            &fresh
        } else {
            request
        };
        self.conn.exchange_frame(
            request_kind(request),
            reply_wait(request).default_bound(),
            request,
        )
    }

    /// Liveness probe: send [`Request::Ping`] and assert the server answers [`Response::Pong`].
    /// A non-`Pong` reply is an [`io::ErrorKind::InvalidData`] protocol error.
    pub fn ping(&mut self) -> io::Result<()> {
        match self.exchange(&Request::Ping)? {
            Response::Pong => Ok(()),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected Pong, got {}", response_kind(&other)),
            )),
        }
    }

    /// Ship a backtest profile (its TOML text) to the server, run it there, and return the report
    /// as JSON text — the same JSON the `backtest --json` bin emits, which
    /// `serde_json::from_str::<TradingResults>` parses.
    ///
    /// Both a transport failure and a server-side [`Response::Error`] surface through the ONE
    /// `Err(String)` channel, so a caller has a single place to handle "no report". Any other reply
    /// (a protocol desync) is likewise reported as `Err`.
    pub fn run_backtest(&mut self, profile_toml: &str) -> Result<String, String> {
        let request = Request::RunBacktest(profile_toml.to_string());
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::Report(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Report, got {}", response_kind(&other)))
            }
        }
    }

    /// Ship a Studio `RunSlice`: resolve `spec`, load `slice`, backtest it server-side, and
    /// return the rendered [`TradingRecordWire`] — the answer, NOT the bars/ticks. `params` is the
    /// optional cost/cash override (`None` = every engine field takes `StrategyEngineConfig::default()`).
    ///
    /// Both a transport failure and a server-side [`Response::Error`] (a bad script/slice/params, or
    /// a lean server that lacks `datafusion-serve`) surface through the ONE `Err(String)` channel —
    /// the error string is kind-first (`"compile: …"` / `"data: …"` / `"strategy: …"`), so a caller
    /// can classify the failure. Any other reply (a protocol desync) is likewise `Err`.
    pub fn run_slice(
        &mut self,
        spec: StrategySpecWire,
        slice: DataSliceWire,
        params: Option<StrategyEngineConfigWire>,
    ) -> Result<TradingRecordWire, String> {
        self.refuse_unserved_slippage(params.as_ref())?;
        self.refuse_unserved_sdk(spec.sdk())?;
        self.refuse_unserved_run_fields(&spec, params.as_ref())?;
        // `slice` is boxed in the variant to keep the enum small (serde-transparent — see the proto).
        let request = Request::RunSlice { spec, slice: Box::new(slice), params };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::RunResult(result) => Ok(result),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected RunResult, got {}", response_kind(&other)))
            }
        }
    }

    /// Ship a Studio `RunSweep`: resolve `spec`, load `slice`, run the parameter `sweep` grid
    /// server-side (`vike_backtesthub::studio_run::run_paramscan_slice`, next to the data), and return the
    /// ranked [`StudioParamscanWire`] — the answer, NOT the bars/ticks. `params` is the optional
    /// cost/cash override applied to EVERY grid point (`None` = `StrategyEngineConfig::default()`).
    ///
    /// Errors as [`run_slice`](Self::run_slice): ONE kind-first `Err(String)` channel.
    pub fn run_paramscan(
        &mut self,
        spec: StrategySpecWire,
        slice: DataSliceWire,
        sweep: ParamscanWire,
        params: Option<StrategyEngineConfigWire>,
    ) -> Result<StudioParamscanWire, String> {
        self.refuse_unserved_slippage(params.as_ref())?;
        self.refuse_unserved_sdk(spec.sdk())?;
        self.refuse_unserved_run_fields(&spec, params.as_ref())?;
        let request =
            Request::RunParamscan { spec, slice: Box::new(slice), paramscan: sweep, params };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::ParamscanResult(result) => Ok(result),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SweepResult, got {}", response_kind(&other)))
            }
        }
    }

    /// Ship a Studio `RunWalkforward`: resolve `spec`, load `slice`, walk it forward over
    /// `walkforward.n_splits` anchored OOS windows server-side
    /// (`vike_backtesthub::studio_run::run_walkforward_slice`), and return the stitched
    /// [`WalkforwardResultsWire`]. `params` is the optional cost/cash override applied to every OOS
    /// window (`None` = `StrategyEngineConfig::default()`). Errors as [`run_slice`](Self::run_slice) (a
    /// tick/multi-symbol slice is one).
    ///
    /// ⚠ **A per-window SEARCH an older daemon would silently DOWNGRADE is refused here, WITHOUT
    /// SENDING.** [`Request`] has no `deny_unknown_fields`, so a daemon predating
    /// `WalkforwardWire::search` drops the field, runs the FIXED walk and answers a well-formed
    /// [`Response::WalkforwardResult`] — an answer to a different question, with no reply to
    /// inspect. The refusal names [`crate::proto::FEATURE_WALKFORWARD_SEARCH`]. A fixed walk — no
    /// `search`, or an explicit `none` carrying no grid — is SENT unchanged to every peer.
    pub fn run_walkforward(
        &mut self,
        spec: StrategySpecWire,
        slice: DataSliceWire,
        walkforward: WalkforwardWire,
        params: Option<StrategyEngineConfigWire>,
    ) -> Result<WalkforwardResultsWire, String> {
        // ⚠ BEFORE THE WRITE, AND IT HAS TO BE — see this method's doc.
        self.refuse_unserved_walkforward_search(&walkforward)?;
        self.refuse_unserved_slippage(params.as_ref())?;
        self.refuse_unserved_sdk(spec.sdk())?;
        self.refuse_unserved_run_fields(&spec, params.as_ref())?;
        let request = Request::RunWalkforward { spec, slice: Box::new(slice), walkforward, params };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::WalkforwardResult(result) => Ok(*result),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected WalkforwardResult, got {}",
                response_kind(&other)
            )),
        }
    }

    /// [`run_walkforward`](Self::run_walkforward)'s refusal of a per-window search an older daemon
    /// would silently downgrade, shared with [`run_walkforward_summary`](Self::run_walkforward_summary).
    fn refuse_unserved_walkforward_search(
        &self,
        walkforward: &WalkforwardWire,
    ) -> Result<(), String> {
        let needs_capability =
            walkforward.search.as_ref().is_some_and(WalkforwardWindowSearchWire::needs_capability);
        if !needs_capability || self.features().iter().any(|f| f == FEATURE_WALKFORWARD_SEARCH) {
            return Ok(());
        }
        Err(format!(
            "backtest daemon does not advertise `{FEATURE_WALKFORWARD_SEARCH}` (advertised: \
             {:?}) — nothing was sent. A per-window walk-forward SEARCH needs a newer \
             `vike-backend backtesthub --addr` with the Studio runners mounted; this verb is \
             capability-negotiated, not version-gated. An older daemon would DROP the search, \
             run the FIXED walk and report success — which is not a coarser answer to the same \
             question but an answer to a different one, and that silent downgrade is what this \
             refusal exists to prevent. Upgrade the daemon, or ask for the fixed walk.",
            self.features()
        ))
    }

    /// [`run_slice`](Self::run_slice), answered as its SUMMARY ([`Request::Summarized`]): every metric the
    /// Studio shows and the equity curve thinned to about [`crate::summary::CURVE_POINTS`] points, the
    /// trades held on the daemon for [`read_trades`](Self::read_trades). Refused LOCALLY, nothing sent,
    /// by a daemon that does not advertise [`FEATURE_RUN_SUMMARY`]; errors otherwise as `run_slice`'s.
    pub fn run_slice_summary(
        &mut self,
        spec: StrategySpecWire,
        slice: DataSliceWire,
        params: Option<StrategyEngineConfigWire>,
    ) -> Result<RunSummaryWire, String> {
        self.refuse_unadvertised_run_summary()?;
        self.refuse_unserved_slippage(params.as_ref())?;
        self.refuse_unserved_sdk(spec.sdk())?;
        self.refuse_unserved_run_fields(&spec, params.as_ref())?;
        let run = Request::RunSlice { spec, slice: Box::new(slice), params };
        match self.exchange(&Request::Summarized(Box::new(run))).map_err(|e| e.to_string())? {
            Response::RunSummary(summary) => Ok(*summary),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected RunSummary, got {}", response_kind(&other)))
            }
        }
    }

    /// [`run_paramscan`](Self::run_paramscan), answered as its SUMMARY: each point's numbers and the
    /// best point's summary; [`read_point`](Self::read_point) asks for another point's. Refused as
    /// [`run_slice_summary`](Self::run_slice_summary) is.
    pub fn run_paramscan_summary(
        &mut self,
        spec: StrategySpecWire,
        slice: DataSliceWire,
        sweep: ParamscanWire,
        params: Option<StrategyEngineConfigWire>,
    ) -> Result<ParamscanSummaryWire, String> {
        self.refuse_unadvertised_run_summary()?;
        self.refuse_unserved_slippage(params.as_ref())?;
        self.refuse_unserved_sdk(spec.sdk())?;
        self.refuse_unserved_run_fields(&spec, params.as_ref())?;
        let run = Request::RunParamscan { spec, slice: Box::new(slice), paramscan: sweep, params };
        match self.exchange(&Request::Summarized(Box::new(run))).map_err(|e| e.to_string())? {
            Response::ParamscanSummary(summary) => Ok(*summary),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected ParamscanSummary, got {}",
                response_kind(&other)
            )),
        }
    }

    /// [`run_walkforward`](Self::run_walkforward), answered as its SUMMARY: the walk with its stitched
    /// curve thinned. Refused as [`run_slice_summary`](Self::run_slice_summary) is, and, like
    /// `run_walkforward`, for a per-window search an older daemon would downgrade.
    pub fn run_walkforward_summary(
        &mut self,
        spec: StrategySpecWire,
        slice: DataSliceWire,
        walkforward: WalkforwardWire,
        params: Option<StrategyEngineConfigWire>,
    ) -> Result<WalkforwardSummaryWire, String> {
        self.refuse_unadvertised_run_summary()?;
        self.refuse_unserved_walkforward_search(&walkforward)?;
        self.refuse_unserved_slippage(params.as_ref())?;
        self.refuse_unserved_sdk(spec.sdk())?;
        self.refuse_unserved_run_fields(&spec, params.as_ref())?;
        let run = Request::RunWalkforward { spec, slice: Box::new(slice), walkforward, params };
        match self.exchange(&Request::Summarized(Box::new(run))).map_err(|e| e.to_string())? {
            Response::WalkforwardSummary(summary) => Ok(*summary),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected WalkforwardSummary, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Page `page` of a summarized run's trades ([`Request::ReadTrades`]), `page_size` a page. `run`
    /// and `point` are the summary's own. Refused as [`run_slice_summary`](Self::run_slice_summary) is.
    pub fn read_trades(
        &mut self,
        run: &str,
        point: Option<usize>,
        page: usize,
        page_size: usize,
    ) -> Result<TradesPageWire, String> {
        self.refuse_unadvertised_run_summary()?;
        let request = Request::ReadTrades { run: run.to_string(), point, page, page_size };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::TradesPage(page) => Ok(page),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected TradesPage, got {}", response_kind(&other)))
            }
        }
    }

    /// The summary of point `point` of a summarized param scan `run` ([`Request::ReadPoint`]).
    /// Refused as [`run_slice_summary`](Self::run_slice_summary) is.
    pub fn read_point(&mut self, run: &str, point: usize) -> Result<RunSummaryWire, String> {
        self.refuse_unadvertised_run_summary()?;
        let request = Request::ReadPoint { run: run.to_string(), point };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::RunSummary(summary) => Ok(*summary),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected RunSummary, got {}", response_kind(&other)))
            }
        }
    }

    /// [`run_backtest`](Self::run_backtest), answered as its SUMMARY ([`Request::Summarized`] around
    /// [`Request::RunBacktest`], decision 0078's amendment of 2026-11-18): the summarizer a Studio Run
    /// gets, the trades and the whole equity curve held on the daemon for
    /// [`read_trades`](Self::read_trades) and [`read_curve`](Self::read_curve). Refused LOCALLY, nothing
    /// sent, by a daemon that does not advertise [`FEATURE_RUN_CURVE`]; errors otherwise as
    /// `run_backtest`'s. A profile that asks for its data plan has no run to summarize: send it whole.
    pub fn run_backtest_summary(&mut self, profile_toml: &str) -> Result<RunSummaryWire, String> {
        self.refuse_unadvertised_run_curve()?;
        let run = Request::RunBacktest(profile_toml.to_string());
        match self.exchange(&Request::Summarized(Box::new(run))).map_err(|e| e.to_string())? {
            Response::RunSummary(summary) => Ok(*summary),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected RunSummary, got {}", response_kind(&other)))
            }
        }
    }

    /// Page `page` of a summarized single run's WHOLE equity curve ([`Request::ReadCurve`]),
    /// `page_size` steps a page. `run` is the summary's [`RunSummaryWire::run`] (held in the daemon's
    /// memory) or its [`RunSummaryWire::saved_run`] (read from the run it saved). Refused LOCALLY,
    /// nothing sent, by a daemon that does not advertise [`FEATURE_RUN_CURVE`].
    pub fn read_curve(
        &mut self,
        run: &str,
        page: usize,
        page_size: usize,
    ) -> Result<CurvePageWire, String> {
        self.refuse_unadvertised_run_curve()?;
        let request = Request::ReadCurve { run: run.to_string(), page, page_size };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::CurvePage(page) => Ok(page),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected CurvePage, got {}", response_kind(&other)))
            }
        }
    }

    /// A summarized single run's WHOLE equity curve, `(equity, timestamp)`, read page by page with
    /// [`read_curve`](Self::read_curve) at [`CURVE_PAGE_SIZE_MAX`] steps a page: no frame carries the
    /// whole curve, so a run of any length reads back, bit for bit. Refused as `read_curve` is.
    pub fn read_whole_curve(&mut self, run: &str) -> Result<(Vec<f64>, Vec<i64>), String> {
        let (mut equity, mut timestamp) = (Vec::new(), Vec::new());
        for page in 0.. {
            let part = self.read_curve(run, page, CURVE_PAGE_SIZE_MAX)?;
            if part.equity.is_empty() && equity.len() < part.total {
                return Err(format!(
                    "run {run}'s curve page {page} came back empty at step {} of {}",
                    equity.len(),
                    part.total
                ));
            }
            equity.extend(part.equity);
            timestamp.extend(part.timestamp);
            if equity.len() >= part.total {
                break;
            }
        }
        Ok((equity, timestamp))
    }

    /// The whole curve's one local refusal: a daemon that does not advertise [`FEATURE_RUN_CURVE`]
    /// holds no run's whole curve and summarizes no `RunBacktest`.
    fn refuse_unadvertised_run_curve(&self) -> Result<(), String> {
        if self.features().iter().any(|f| f == FEATURE_RUN_CURVE) {
            return Ok(());
        }
        Err(format!(
            "this backtesthub does not advertise `{FEATURE_RUN_CURVE}` (advertised: {:?}) — nothing \
             was sent. It is older than this client, which asks for a backtest's summary and reads its \
             whole equity curve in pages: deploy the backtesthub of this client's release, or ask \
             for the whole answer (`vike-cli backtest run --whole`).",
            self.features()
        ))
    }

    /// The summarized runs' one local refusal: a daemon that does not advertise
    /// [`FEATURE_RUN_SUMMARY`] is older than this desktop, which ships with its backtesthub.
    fn refuse_unadvertised_run_summary(&self) -> Result<(), String> {
        if self.features().iter().any(|f| f == FEATURE_RUN_SUMMARY) {
            return Ok(());
        }
        Err(format!(
            "this backtesthub does not advertise `{FEATURE_RUN_SUMMARY}` (advertised: {:?}) — nothing \
             was sent. It is older than this desktop, which asks for a run's summary and pages its \
             trades: the desktop and the backtesthub ship together, so deploy the backtesthub of this \
             desktop's release (or run the desktop of the server's).",
            self.features()
        ))
    }

    /// Ship a profile (its TOML text) to be run as a PARAMETER SWEEP over its own `[sweep]` table
    /// and return the RANKED report as JSON text — the sweep sibling of
    /// [`run_backtest`](Self::run_backtest).
    ///
    /// `rank_by` names the server-side ranking (`"sharpe"` / `"return"` / `"max_dd"` / `"equity"`,
    /// case-insensitive, or `"multi"` — the composite objective); `None` = the `sharpe` default.
    /// Rows come back ALREADY ordered best-first, each carrying its own `TradingResults`, so a
    /// caller renders server-computed stats rather than re-implementing a metric.
    ///
    /// `search` names WHICH METHOD walks the grid, and that method's own knobs, as the TOKENS the
    /// operator typed — see [`crate::proto::SearchSelectionWire`] for why they are strings rather than typed
    /// scalars (one parser, `vike_backtest::search::select`, on whichever side runs).
    ///
    /// ⚠ **A selector an older daemon would silently DOWNGRADE is refused here, WITHOUT SENDING.**
    /// [`Request`] has no `deny_unknown_fields`, so a daemon predating the field drops it, runs the
    /// exhaustive grid and answers a normal report — there is no reply to inspect. The refusal
    /// names [`crate::proto::FEATURE_SEARCH_METHOD`]. An ordinary grid search — no selector, or an
    /// explicit `grid` — is SENT unchanged to every peer.
    ///
    /// Unlike [`run_paramscan`](Self::run_paramscan) (the Studio DTO verb) this carries the WHOLE
    /// profile, so the whole `[engine]` applies — the `fee` schedule included — and a LEAN server
    /// serves it too. Both a transport failure and a server-side [`Response::Error`] surface
    /// through the ONE `Err(String)` channel.
    pub fn run_paramscan_profile(
        &mut self,
        profile_toml: &str,
        rank_by: Option<&str>,
        search: Option<&SearchSelectionWire>,
    ) -> Result<String, String> {
        let request = self.paramscan_profile_request(profile_toml, rank_by, search)?;
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::ParamscanReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SweepReport, got {}", response_kind(&other)))
            }
        }
    }

    /// [`run_paramscan_profile`](Self::run_paramscan_profile)'s request, or its refusal before the write.
    fn paramscan_profile_request(
        &self,
        profile_toml: &str,
        rank_by: Option<&str>,
        search: Option<&SearchSelectionWire>,
    ) -> Result<Request, String> {
        // ⚠ THE REFUSAL IS BEFORE THE WRITE (`run_paramscan_profile`'s doc). `rank_by == "multi"` joins it
        // for a different reason: an old daemon resolves that string through `RankMetric::from_str_ci`,
        // whose four arms have no `multi`, and answers an error naming a set this client offers
        // five of.
        let needs_capability = search.is_some_and(SearchSelectionWire::needs_capability)
            || rank_by.is_some_and(|r| r.eq_ignore_ascii_case("multi"));
        if needs_capability && !self.features().iter().any(|f| f == FEATURE_SEARCH_METHOD) {
            return Err(format!(
                "backtest daemon does not advertise `{FEATURE_SEARCH_METHOD}` (advertised: {:?}) \
                 — nothing was sent. A search METHOD (--optimizer/--euler-depth/--trials/--seed) \
                 and --rank-by multi need a newer `vike-backend backtesthub --addr`; this verb is \
                 capability-negotiated, not version-gated. An older daemon would DROP the field, \
                 run the exhaustive grid and report success, which is the silent downgrade this \
                 refusal exists to prevent — upgrade the daemon, or drop the flag to search the \
                 grid on the server.",
                self.features()
            ));
        }
        Ok(Request::RunParamscanProfile {
            profile_toml: profile_toml.to_string(),
            rank_by: rank_by.map(str::to_string),
            // An EMPTY selector serializes as `None`: an ordinary grid search's frame is
            // byte-identical to one from before the field existed.
            search: search.filter(|s| !s.is_empty()).cloned(),
        })
    }

    /// **A profile's parameter search, SUMMARIZED where the daemon can summarize it** (decision 0078's amendment of
    /// 2026-11-19): [`Request::Summarized`] around [`run_paramscan_profile`](Self::run_paramscan_profile)'s request,
    /// toward a daemon that advertises [`FEATURE_PARAMSCAN_PROFILE_SUMMARY`], answered by each point's overrides and
    /// metric table and every page after the first read here ([`read_paramscan_rows`](Self::read_paramscan_rows)),
    /// so no frame carries more than one page however many points the search has; the whole report stays in the
    /// run the daemon saved ([`ParamscanProfileSummaryWire::saved_run`]).
    ///
    /// THE ONE FALLBACK, both doors' (`vike-cli backtest run`, the MCP `run_sweep`): `whole`, or a daemon that does
    /// not advertise the summary, sends the plain verb and answers its whole report as before; so does a profile that
    /// asks for its data plan, which the daemon answers as it does alone. Refusals and errors as
    /// `run_paramscan_profile`'s.
    pub fn run_paramscan_profile_answer(
        &mut self,
        profile_toml: &str,
        rank_by: Option<&str>,
        search: Option<&SearchSelectionWire>,
        whole: bool,
    ) -> Result<ParamscanProfileAnswer, String> {
        if whole || !self.features().iter().any(|f| f == FEATURE_PARAMSCAN_PROFILE_SUMMARY) {
            return self
                .run_paramscan_profile(profile_toml, rank_by, search)
                .map(ParamscanProfileAnswer::Whole);
        }
        let plain = self.paramscan_profile_request(profile_toml, rank_by, search)?;
        let mut summary = match self
            .exchange(&Request::Summarized(Box::new(plain)))
            .map_err(|e| e.to_string())?
        {
            Response::ParamscanProfileSummary(summary) => summary,
            Response::Report(plan) => return Ok(ParamscanProfileAnswer::Whole(plan)),
            Response::Error(msg) => return Err(msg),
            other => {
                return Err(format!(
                    "protocol desync: expected ParamscanProfileSummary, got {}",
                    response_kind(&other)
                ));
            }
        };
        for page in 1.. {
            if summary.rows.len() >= summary.total {
                break;
            }
            let part = self
                .read_paramscan_rows(&summary.run, page, summary.page_size)
                .map_err(|why| with_the_saved_run(why, &summary))?;
            if part.rows.is_empty() {
                let why = format!(
                    "search {}'s rows page {page} came back empty at row {} of {}",
                    summary.run,
                    summary.rows.len(),
                    summary.total
                );
                return Err(with_the_saved_run(why, &summary));
            }
            summary.rows.extend(part.rows);
        }
        Ok(ParamscanProfileAnswer::Summary(summary))
    }

    /// Page `page` of a summarized profile search's rows ([`Request::ReadParamscanRows`]), `page_size` a page; `run`
    /// is the summary's own. Refused LOCALLY, nothing sent, by a daemon that does not advertise
    /// [`FEATURE_PARAMSCAN_PROFILE_SUMMARY`].
    pub fn read_paramscan_rows(
        &mut self,
        run: &str,
        page: usize,
        page_size: usize,
    ) -> Result<ParamscanRowsPageWire, String> {
        if !self.features().iter().any(|f| f == FEATURE_PARAMSCAN_PROFILE_SUMMARY) {
            return Err(format!(
                "this backtesthub does not advertise `{FEATURE_PARAMSCAN_PROFILE_SUMMARY}` (advertised: \
                 {:?}) — nothing was sent. It holds no summarized search's rows.",
                self.features()
            ));
        }
        let request = Request::ReadParamscanRows { run: run.to_string(), page, page_size };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::ParamscanRows(page) => Ok(page),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected ParamscanRows, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Ask the backtesthub to run one study FILE (built and run sandboxed on its box) over the store
    /// IT holds, and return the
    /// minted run as JSON text ([`Response::StudyReport`]).
    ///
    /// ⚠ `crates/vike-cli/src/cmd/research/study.rs` checks the same advertisement first and prints the
    /// richer refusal (the `vike-backend study` escape hatch with this run's arguments filled in);
    /// this one exists for every OTHER caller, so the rule lives with the protocol.
    ///
    /// ⚠ [`crate::proto::FEATURE_STUDY`] is MOUNT-conditional on an OLDER serving side: such a
    /// backtesthub advertised it only when a study runner was injected into it. See that constant.
    ///
    /// A request that names a tier or a recipe FILE ([`StudyRunRequestWire::needs_study_files`]) is
    /// refused too, nothing sent, by a server that does not advertise [`FEATURE_STUDY_FILES`]: it
    /// would drop both and run the Rust study of that name.
    pub fn run_study(&mut self, study: &StudyRunRequestWire) -> Result<String, String> {
        if !self.features().iter().any(|f| f == FEATURE_STUDY) {
            return Err(format!(
                "backtest daemon does not advertise `{FEATURE_STUDY}` (advertised: {:?}) — nothing \
                 was sent. A study is served by `vike-backend backtesthub --addr` with the study \
                 runner MOUNTED; this verb is capability-negotiated, not version-gated. Run it on \
                 the box that holds the store instead: `vike-backend study`.",
                self.features()
            ));
        }
        if study.needs_study_files() {
            self.refuse_unadvertised_server_files(FEATURE_STUDY_FILES)?;
        }
        match self
            .exchange(&Request::RunStudy(Box::new(study.clone())))
            .map_err(|e| e.to_string())?
        {
            Response::StudyReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected StudyReport, got {}", response_kind(&other)))
            }
        }
    }

    /// The studies of the server's study folder, both tiers, with their recipe file names
    /// ([`Request::ListStudies`]). Refused LOCALLY, nothing sent, by a server that does not advertise
    /// [`FEATURE_STUDY_FILES`].
    pub fn list_studies(&mut self) -> Result<WireStudyListing, String> {
        self.refuse_unadvertised_server_files(FEATURE_STUDY_FILES)?;
        match self.exchange(&Request::ListStudies).map_err(|e| e.to_string())? {
            Response::Studies(listing) => Ok(listing),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Studies, got {}", response_kind(&other)))
            }
        }
    }

    /// Page `page` of the server's saved runs, newest page first ([`Request::ListRuns`]), `page_size`
    /// a page (0 is [`crate::study::RUNS_PAGE_SIZE`]). Refused LOCALLY, nothing sent, by a server that
    /// does not advertise [`FEATURE_RUN_FILES`].
    pub fn list_runs(&mut self, page: usize, page_size: usize) -> Result<WireRunPage, String> {
        self.refuse_unadvertised_server_files(FEATURE_RUN_FILES)?;
        match self.exchange(&Request::ListRuns { page, page_size }).map_err(|e| e.to_string())? {
            Response::Runs(runs) => Ok(runs),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Runs, got {}", response_kind(&other))),
        }
    }

    /// `length` bytes from `offset` of the file `file` of the saved run `run_id`
    /// ([`Request::ReadRunArtifact`]; 0 and anything above it is [`crate::study::RUN_ARTIFACT_PAGE`]).
    /// Refused LOCALLY, nothing sent, by a server that does not advertise [`FEATURE_RUN_FILES`].
    pub fn read_run_artifact(
        &mut self,
        run_id: &str,
        file: &str,
        offset: u64,
        length: u64,
    ) -> Result<WireRunArtifact, String> {
        self.refuse_unadvertised_server_files(FEATURE_RUN_FILES)?;
        let request = Request::ReadRunArtifact {
            run_id: run_id.to_string(),
            file: file.to_string(),
            offset,
            length,
        };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::RunArtifact(artifact) => Ok(artifact),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected RunArtifact, got {}", response_kind(&other)))
            }
        }
    }

    /// The versioned strategy SDKs the server holds, newest first ([`Request::ListSdks`]). Refused
    /// LOCALLY, nothing sent, by a server that does not advertise [`FEATURE_SDK_SOURCE`].
    pub fn list_sdks(&mut self) -> Result<WireSdkListing, String> {
        self.refuse_unadvertised_sdk_source()?;
        match self.exchange(&Request::ListSdks).map_err(|e| e.to_string())? {
            Response::Sdks(listing) => Ok(listing),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Sdks, got {}", response_kind(&other))),
        }
    }

    /// The file or folder `path` (under the SDK's root, `crates/…`) of the server's SDK `sdk`
    /// ([`Request::ReadSdkFile`]): a file's text, or a folder's entries. Refused LOCALLY, nothing sent,
    /// by a server that does not advertise [`FEATURE_SDK_SOURCE`].
    pub fn read_sdk_file(&mut self, sdk: &str, path: &str) -> Result<WireSdkFile, String> {
        self.refuse_unadvertised_sdk_source()?;
        let request = Request::ReadSdkFile { sdk: sdk.to_string(), path: path.to_string() };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::SdkFile(file) => Ok(file),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SdkFile, got {}", response_kind(&other)))
            }
        }
    }

    /// The SDK source verbs' one local refusal: a server that does not advertise
    /// [`FEATURE_SDK_SOURCE`] is older than the versioned strategy SDK.
    fn refuse_unadvertised_sdk_source(&self) -> Result<(), String> {
        if self.features().iter().any(|f| f == FEATURE_SDK_SOURCE) {
            return Ok(());
        }
        let address = self.stream().peer_addr().map_or_else(|_| "?".to_string(), |a| a.to_string());
        Err(format!(
            "the backtest server at {address} does not advertise `{FEATURE_SDK_SOURCE}` (it predates \
             the versioned strategy SDK): deploy the server. Nothing was sent."
        ))
    }

    /// The server's studies' and runs' one local refusal ([`FEATURE_STUDY_FILES`],
    /// [`FEATURE_RUN_FILES`]): a server that does not advertise `feature` predates server-side studies.
    fn refuse_unadvertised_server_files(&self, feature: &str) -> Result<(), String> {
        if self.features().iter().any(|f| f == feature) {
            return Ok(());
        }
        let address = self.stream().peer_addr().map_or_else(|_| "?".to_string(), |a| a.to_string());
        Err(format!(
            "the backtest server at {address} does not advertise `{feature}` (it predates \
             server-side studies): deploy the server. Nothing was sent."
        ))
    }

    /// Ship a profile (its TOML text) to be WALKED FORWARD over its own `[walkforward].n_splits`
    /// anchored out-of-sample windows and return the stitched results as JSON text
    /// (`vike_analytics::walkforward::WalkforwardResults`) — the walk-forward sibling of
    /// [`run_backtest`](Self::run_backtest).
    ///
    /// The split count lives IN the profile (there is no wire override), so one file describes the
    /// whole run. Server-side this is bar-mode + single-series only; a tick or multi-symbol profile
    /// comes back as a clean [`Response::Error`] on the ONE `Err(String)` channel, like every other
    /// failure.
    pub fn run_walkforward_profile(&mut self, profile_toml: &str) -> Result<String, String> {
        let request = Request::RunWalkforwardProfile { profile_toml: profile_toml.to_string() };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::WalkforwardReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected WalkforwardReport, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Ship a profile whose `[walkforward]` declares the COMBINATORIAL form (`groups`/
    /// `test_groups`) and return its paths as JSON text — the same verb as
    /// [`run_walkforward_profile`](Self::run_walkforward_profile), which the profile turns into the
    /// combinatorial walk on the far side, answered by [`Response::CombinatorialWalkforwardReport`].
    pub fn run_combinatorial_walkforward_profile(
        &mut self,
        profile_toml: &str,
    ) -> Result<String, String> {
        let request = Request::RunWalkforwardProfile { profile_toml: profile_toml.to_string() };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::CombinatorialWalkforwardReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected CombinatorialWalkforwardReport, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Ship a profile that declares a `[permutation]` table and return its permutation test as
    /// JSON text — the same verb as [`run_backtest`](Self::run_backtest), which the table turns into
    /// the test on the far side, answered by [`Response::PermutationReport`].
    pub fn run_permutation_profile(&mut self, profile_toml: &str) -> Result<String, String> {
        let request = Request::RunBacktest(profile_toml.to_string());
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::PermutationReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected PermutationReport, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Enumerate the strategy files the daemon's `strategies/` folder holds — the paths a profile's
    /// `[strategy] file` can name. A transport failure or a server-side [`Response::Error`] surfaces
    /// through the ONE `Err(String)` channel; any other reply (a protocol desync) is likewise `Err`.
    pub fn list_strategies(&mut self) -> Result<Vec<String>, String> {
        match self.exchange(&Request::ListStrategies).map_err(|e| e.to_string())? {
            Response::Strategies(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Strategies, got {}", response_kind(&other)))
            }
        }
    }

    /// The strategy files of the daemon's `strategies/` folder with their states
    /// ([`Request::ListStrategyFiles`]). Refused LOCALLY, nothing sent, by a daemon that does not
    /// advertise [`FEATURE_STRATEGY_FILES`].
    pub fn list_strategy_files(&mut self) -> Result<Vec<WireStrategyFile>, String> {
        self.refuse_unadvertised_strategy_files()?;
        match self.exchange(&Request::ListStrategyFiles).map_err(|e| e.to_string())? {
            Response::StrategyFiles(files) => Ok(files),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected StrategyFiles, got {}",
                response_kind(&other)
            )),
        }
    }

    /// One strategy file's text, by its path under the daemon's `strategies/` folder
    /// ([`Request::ReadStrategyFile`]). Refused LOCALLY, nothing sent, by a daemon that does not
    /// advertise [`FEATURE_STRATEGY_FILES`].
    pub fn read_strategy_file(&mut self, file: &str) -> Result<String, String> {
        self.refuse_unadvertised_strategy_files()?;
        let request = Request::ReadStrategyFile { file: file.to_string() };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::StrategyFileText { text, .. } => Ok(text),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected StrategyFileText, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Write one strategy file of the daemon's `strategies/` folder ([`Request::SaveStrategyFile`]):
    /// the sha256 of the text written and the file's states as the list now shows them. Needs a
    /// connection authenticated at the Control scope; refused LOCALLY, nothing sent, by a daemon
    /// that does not advertise [`FEATURE_STRATEGY_FILES`].
    pub fn save_strategy_file(
        &mut self,
        file: &str,
        text: &str,
    ) -> Result<(String, WireStrategyFile), String> {
        self.refuse_unadvertised_strategy_files()?;
        let request = Request::SaveStrategyFile { file: file.to_string(), text: text.to_string() };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::StrategyFileSaved { sha256, file } => Ok((sha256, file)),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected StrategyFileSaved, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Build `text` as the Rust strategy file `file` ([`Request::BuildStrategyFile`]): the daemon forwards
    /// it to the strategy builder and answers the text's sha256, which names the artifact. `Err` is the
    /// compiler's diagnostics verbatim, or why the builder did not answer. BLOCKING for as long as the
    /// compile takes (the reply wait is unbounded). Needs a connection authenticated at the Control
    /// scope; refused LOCALLY, nothing sent, by a daemon that does not advertise
    /// [`FEATURE_BUILD_STRATEGY_FILE`]. `sdk` builds against that SDK over the text's own pin (`None`: as
    /// the text says), refused locally by a daemon without [`FEATURE_STRATEGY_SDK`].
    pub fn build_strategy_file(
        &mut self,
        file: &str,
        text: &str,
        sdk: Option<u32>,
    ) -> Result<String, String> {
        self.refuse_unserved_sdk(sdk)?;
        if !self.features().iter().any(|f| f == FEATURE_BUILD_STRATEGY_FILE) {
            return Err(format!(
                "this backtest daemon does not advertise `{FEATURE_BUILD_STRATEGY_FILE}` (advertised: \
                 {:?}) — nothing was sent. It is older than the Build it forwards to the strategy \
                 builder: run a newer `vike-backend backtesthub --addr`.",
                self.features()
            ));
        }
        let request =
            Request::BuildStrategyFile { file: file.to_string(), text: text.to_string(), sdk };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::StrategyFileBuilt { sha256 } => Ok(sha256),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected StrategyFileBuilt, got {}",
                response_kind(&other)
            )),
        }
    }

    /// The strategy-file verbs' one local refusal: a daemon that does not advertise
    /// [`FEATURE_STRATEGY_FILES`] is older than them.
    fn refuse_unadvertised_strategy_files(&self) -> Result<(), String> {
        if self.features().iter().any(|f| f == FEATURE_STRATEGY_FILES) {
            return Ok(());
        }
        Err(format!(
            "this backtest daemon does not advertise `{FEATURE_STRATEGY_FILES}` (advertised: {:?}) — \
             nothing was sent. It is older than the strategy-file list: run a newer \
             `vike-backend backtesthub --addr`.",
            self.features()
        ))
    }

    /// **Enumerate the roster a NAMED RUN would resolve, and whether the lane is armed** —
    /// [`Request::NamedStrategies`].
    ///
    /// ⚠ **Not [`Self::list_strategies`], and the two answer different rosters.** That one returns
    /// the strategy files of the daemon's folder; this one returns what a named run
    /// can actually resolve, which since plan 2b is nothing (no strategy is compiled into the
    /// server). A picker built on the wrong one offers names that will be refused.
    ///
    /// ⚠ **`armed: false` is a SUCCESS, and it arrives with an EMPTY roster.** The arming gates the
    /// NAMES as well as the run (`docs/decisions/0064`'s decision 8 leg 3: publishing the
    /// operator's compiled-in strategy names is itself the disclosure arming makes an operator's
    /// act). Render [`NamedRunRoster::unarmed_note`], never the bare empty list: the note separates
    /// "withheld" from "this daemon holds none", which is also why
    /// [`crate::proto::FEATURE_NAMED_RUN`] is a BUILD fact rather than a per-lane one.
    ///
    /// ONE local refusal, before a frame: the capability check.
    pub fn named_strategies(&mut self) -> Result<NamedRunRoster, String> {
        self.refuse_unadvertised_named_run()?;
        match self.exchange(&Request::NamedStrategies).map_err(|e| e.to_string())? {
            Response::NamedStrategies(roster) => Ok(roster),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected NamedStrategies, got {}",
                response_kind(&other)
            )),
        }
    }

    /// **Run ONE strategy the server already holds** — [`Request::RunNamed`],
    /// `docs/decisions/0064-a-named-run-carries-no-source.md`.
    ///
    /// The one `Run*` verb reachable from a connection at the OBSERVE scope, which is the point of
    /// the record: a device whose role is `observe` (decision 0288) runs what the server holds and
    /// compiles nothing it sends.
    ///
    /// **Two local refusals, both before a frame is sent**, so an operator sees the rule rather than
    /// a round trip:
    ///
    /// 1. the capability check — its message distinguishes an OLD server from an unarmed one,
    ///    which this verb can do and `vike_datahub_client::DatahubClient::seed_series` cannot, because
    ///    [`crate::proto::FEATURE_NAMED_RUN`] is a build fact and the arming rides in the answer;
    /// 2. [`validate_named_run`] — the SAME function the server's door calls, duplicated here for
    ///    the MESSAGE and never for the enforcement: the server re-checks every bound before it
    ///    touches the store.
    ///
    /// ⚠ The capability check is LOAD-BEARING, not polite: never send a bounded request to a peer
    /// that has not said it understands the bound (a future server that grew a field would drop
    /// it silently, the [`Self::run_paramscan_profile`] failure).
    ///
    /// ⚠ **`Ok(NamedRunOutcome::NotArmed)` is a SUCCESS and must not be rendered as a failure** —
    /// it means the operator has not armed the lane; nothing ran.
    pub fn run_named(&mut self, spec: &NamedRunSpec) -> Result<NamedRunOutcome, String> {
        self.refuse_unadvertised_named_run()?;
        validate_named_run(spec)?;
        match self
            .exchange(&Request::RunNamed(Box::new(spec.clone())))
            .map_err(|e| e.to_string())?
        {
            Response::NamedRun(outcome) => Ok(*outcome),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected NamedRun, got {}", response_kind(&other)))
            }
        }
    }

    /// Whether this server advertised the named-run capability — the question a picker asks BEFORE
    /// it offers a Run control, so an old server produces a sentence rather than a button that
    /// always fails (`vike_datahub_client::DatahubClient::serves_venue_catalog`'s leg: an un-advertised capability must not
    /// render as an EMPTY roster).
    pub fn serves_named_run(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_NAMED_RUN)
    }

    /// The ONE capability refusal both named-run verbs share.
    fn refuse_unadvertised_named_run(&self) -> Result<(), String> {
        if self.serves_named_run() {
            return Ok(());
        }
        Err(format!(
            "this backtesthub does not advertise `{FEATURE_NAMED_RUN}` (advertised: {:?}) — \
             nothing was sent. Unlike the data daemon's armed lanes, this string is a BUILD fact, \
             so its absence means the server PREDATES the named-run verb rather than that its \
             operator declined to arm the lane: an unarmed server advertises it and answers \
             `NotArmed`. Check the address too — the `Run*` verbs are served by \
             `vike-backend backtesthub --addr` (default 127.0.0.1:7880), not by the datahub.",
            self.features()
        ))
    }

    /// `Err`, with nothing sent, when a request names an SDK (`sdk`) and this daemon does not advertise
    /// [`FEATURE_STRATEGY_SDK`]: an older daemon drops the field and builds the file's own SDK.
    fn refuse_unserved_sdk(&self, sdk: Option<u32>) -> Result<(), String> {
        match sdk {
            Some(sdk) if !self.features().iter().any(|f| f == FEATURE_STRATEGY_SDK) => {
                Err(format!(
                    "backtest daemon does not advertise `{FEATURE_STRATEGY_SDK}` (advertised: {:?}) — \
                 nothing was sent. Building against SDK {sdk} needs a newer `vike-backend backtesthub \
                 --addr`: an older daemon would drop the SDK and build the file's own.",
                    self.features()
                ))
            }
            _ => Ok(()),
        }
    }

    /// `Err`, with nothing sent, when a Studio run carries a field this daemon does not advertise (2026-11-20): a
    /// funding switch without [`FEATURE_ATTACH_FUNDING`] (an older daemon drops it and charges a perpetual's funding),
    /// or a Rhai buffer's parameters without [`FEATURE_RHAI_PARAMETERS`] (an older daemon cannot decode the variant).
    /// A request that carries neither is an older client's bytes and passes.
    fn refuse_unserved_run_fields(
        &self,
        spec: &StrategySpecWire,
        params: Option<&StrategyEngineConfigWire>,
    ) -> Result<(), String> {
        let advertised = |feature: &str| self.features().iter().any(|f| f == feature);
        let (feature, what) = if params.is_some_and(StrategyEngineConfigWire::needs_attach_funding)
            && !advertised(FEATURE_ATTACH_FUNDING)
        {
            (FEATURE_ATTACH_FUNDING, "The funding switch (attach_funding)")
        } else if matches!(spec, StrategySpecWire::RhaiWithParameters { .. })
            && !advertised(FEATURE_RHAI_PARAMETERS)
        {
            (FEATURE_RHAI_PARAMETERS, "A Rhai script's parameters")
        } else {
            return Ok(());
        };
        Err(format!(
            "backtest daemon does not advertise `{feature}` (advertised: {:?}) — nothing was sent. \
             {what} needs a newer `vike-backend backtesthub --addr`: an older daemon would drop it or \
             refuse the frame. Upgrade the daemon, or run without it.",
            self.features()
        ))
    }

    /// `Err`, with nothing sent, when `params` name a slippage MODEL and this daemon does not
    /// advertise [`FEATURE_SLIPPAGE_MODEL`]: an older daemon drops the model and runs the value as
    /// a flat fraction of the price, so two ticks would be charged as 200%.
    fn refuse_unserved_slippage(
        &self,
        params: Option<&StrategyEngineConfigWire>,
    ) -> Result<(), String> {
        if !params.is_some_and(StrategyEngineConfigWire::needs_capability)
            || self.features().iter().any(|f| f == FEATURE_SLIPPAGE_MODEL)
        {
            return Ok(());
        }
        Err(format!(
            "backtest daemon does not advertise `{FEATURE_SLIPPAGE_MODEL}` (advertised: {:?}) — \
             nothing was sent. A slippage model (ticks, market impact, or an explicit none) needs \
             a newer `vike-backend backtesthub --addr`: an older daemon would drop the model and \
             charge the value as a fraction of the price. Upgrade the daemon, or run with the \
             percent model's value alone.",
            self.features()
        ))
    }
}

#[path = "client_tests.rs"]
#[cfg(test)]
mod client_tests;
