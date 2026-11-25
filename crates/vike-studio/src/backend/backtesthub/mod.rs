//! PR-5 — the Studio run BACKEND: dispatch a Run/Sweep/Walk-Forward to the REMOTE backtesthub
//! (`vike-backend backtesthub --addr`) over TCP instead of running it in-process.
//!
//! ⚠ This said `vike-datahub` until 2026-09-20 and the code agreed with it, which is the part that
//! mattered — see the tombstone where `DEFAULT_REMOTE_ADDR` stood. Ruling 7 moved every `Run*` verb
//! onto the backtest plane; the data daemon serves the STORE reads a remote slice needs and refuses
//! the run itself.
//!
//! Every `spawn_backtesthub_*` here answers on a `Receiver` the shell polls (`StudioState::poll`), and
//! every answer is the run's SUMMARY (decision 0078's amendments of 2026-11-16 and 2026-11-18): a Run, a
//! Sweep, a Walk-Forward, each row of the Saved pane's "Compare all" and each AI Copilot candidate; the
//! trades and a single run's whole equity curve stay on the server and are paged on request. Every
//! failure — connect fault, server error, or protocol desync — arrives as an `Err(RunError)` on that
//! receiver (never a panic / `unwrap`).
//!
//! # The wire DTOs are the client crate's; the conversions are NOT this module's
//!
//! The `Wire*` schema is defined ONCE in the light, DataFusion-free `vike-backtesthub-client`
//! (`wire_studio`). Each conversion lives BESIDE its other half, so a field added to one is in front
//! of whoever adds it to the other: `vike_studio_core::wire_run` holds both directions of the spec,
//! the slice, the sweep, the walk-forward and the error line (the server's `to_strategy_spec` beside
//! the client's `to_wire_spec`, …), `vike_backtesthub_client::wire_record` both directions of a run's result,
//! and `vike_backtesthub_client::named_run::NamedRunParam` both directions of a named knob. This module
//! DIALS and names which conversion each verb takes; it decodes nothing of its own.
//!
//! Its children: `named_run` (the Named backend, decision 0064), `strategy_files` (the server's
//! strategy files: the list, Open, Save and Build), `studies` (the server's studies and saved runs:
//! the Research pane's lists, a Run study and an artifact's first page), `sdk_source` (the server's
//! strategy SDKs: the SDK source view's list and file reads) and `build` (a Build's pre-dial refusals). The two dials they share, [`connect`] (the one Write dial) and
//! [`connect_observe`], stay here.

use std::sync::mpsc::Receiver;

use vike_ai::{OutOfSampleRun, OutOfSampleSlice};
use vike_backtesthub_client::proto::{FEATURE_ATTACH_FUNDING, FEATURE_RHAI_PARAMETERS};
use vike_backtesthub_client::summary::{
    ParamscanSummaryWire, RunSummaryWire, TRADES_PAGE_SIZE, TradesPageWire, WalkforwardSummaryWire,
};
use vike_backtesthub_client::{
    BacktesthubClient,
    wire_studio::{ParamscanWire, StrategyEngineConfigWire, StrategySpecWire, WalkforwardWire},
};
// The backtesthub's address, the default for BOTH run backends, [`StudioRunBackend::Source`] and
// [`StudioRunBackend::Named`]: `vike-backend backtesthub --addr`, a DIFFERENT process from the
// datahub, on a different port. ⚠ The two ports are the single most likely thing to get wrong here:
// the `Run*` verbs moved to the backtesthub at 7880 (ruling 7), while the store verbs stayed on the
// datahub at 7878 (`vike_config::DEFAULT_DATAHUB_ADDRESS`, which this module has no use for).
// `docs/decisions/0064-a-named-run-carries-no-source.md`'s consequences name the ADDRESS — not the
// key — as the genuine client-side gap.
use vike_config::DEFAULT_BACKTEST_ADDRESS;
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::call::HubCallError;
use vike_hub_protocol::frame::BodyCodec;
use vike_hub_protocol::plane::Plane;
use vike_model::Trade;
use vike_model::params::ParamValue;
use vike_studio_core::{
    DataSlice, RunError, StrategySpec, run_error_from_wire, spawn_outcome, to_wire_slice,
    to_wire_spec,
};

/// A run as the results pane shows it: its summary, and its trades when they arrived whole (a NAMED
/// run, whose bounded window crosses whole and is summarized in this process). A backtesthub run's
/// trades stay on the server, and the Trades tab pages them (`crate::panes::results::StudioResultsTradesPager`).
/// The run's answer, not a pane's, so it lives beside the dials that produce it.
#[derive(Debug, Clone)]
pub struct StudioRunView {
    pub summary: RunSummaryWire,
    pub trades: Option<Vec<Trade>>,
}

/// A run's outcome as the Studio holds it.
pub(crate) type StudioRunViewOutcome = Result<StudioRunView, RunError>;

/// **The engine settings every Studio run sends beside its spec** (the Param scan pane's test conditions): the
/// starting capital (`None`, the engine's default) and the funding switch, OFF unless ticked (the owner's ruling of
/// 2026-11-20: a backtest charges no funding unless he asks). A Run, a scan, a walk-forward, each Compare-all row and
/// each copilot candidate send it through [`Self::wire`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StudioRunEngine {
    pub cash: Option<f64>,
    pub funding: bool,
}

/// A run with funding OFF refused on a server that predates the switch: it would charge a perpetual's funding.
pub(crate) const FUNDING_OFF_NOT_SERVED: &str = "This backtest server cannot switch funding off \
     (it predates the switch), so this run would be charged a perpetual's stored funding: \
     nothing was sent. Update the server, or tick Include funding to run with it.";

/// A plain Run (or a Compare-all row) of a Rhai script with values refused on a server that predates them.
pub(crate) const RHAI_VALUES_NOT_SERVED: &str = "This backtest server predates a Rhai script's \
     parameters, so the values and costs set in the Param scan pane would be dropped: nothing \
     was sent. Update the server, or clear them to run the script's own values.";

impl StudioRunEngine {
    /// The engine knobs for a server advertising `features`: the capital and the funding switch. ⚠ OFF is never
    /// dropped: an older server ([`FEATURE_ATTACH_FUNDING`] not advertised) charges a perpetual's funding on an
    /// absent switch, so OFF there is refused in words ([`FUNDING_OFF_NOT_SERVED`]) and nothing is sent. ON is left
    /// out there, its default already charging it.
    pub(crate) fn wire(
        &self,
        features: &[String],
    ) -> Result<Option<StrategyEngineConfigWire>, RunError> {
        let attach_funding = match (advertises(features, FEATURE_ATTACH_FUNDING), self.funding) {
            (true, funding) => Some(funding),
            (false, true) => None,
            (false, false) => return Err(RunError::Data(FUNDING_OFF_NOT_SERVED.to_string())),
        };
        Ok((self.cash.is_some() || attach_funding.is_some()).then(|| StrategyEngineConfigWire {
            cash: self.cash,
            attach_funding,
            ..StrategyEngineConfigWire::default()
        }))
    }
}

/// Whether a server advertising `features` serves `feature`.
fn advertises(features: &[String], feature: &str) -> bool {
    features.iter().any(|f| f == feature)
}

/// `spec` as a server advertising `features` reads it. A Rhai buffer's parameters on a server that predates them
/// ([`FEATURE_RHAI_PARAMETERS`]): the bare buffer when `axes_hold_values` (a scan's or a walk-forward's grid holds
/// every value and cost as an axis, `StudioState::held_rows`, so nothing is lost), else refused in words
/// ([`RHAI_VALUES_NOT_SERVED`]): a plain Run or a Compare-all row would silently drop them.
pub(crate) fn served_spec(
    spec: StrategySpecWire,
    features: &[String],
    axes_hold_values: bool,
) -> Result<StrategySpecWire, RunError> {
    match spec {
        StrategySpecWire::RhaiWithParameters { source, .. }
            if !advertises(features, FEATURE_RHAI_PARAMETERS) =>
        {
            match axes_hold_values {
                true => Ok(StrategySpecWire::Rhai(source)),
                false => Err(RunError::Data(RHAI_VALUES_NOT_SERVED.to_string())),
            }
        }
        other => Ok(other),
    }
}

/// What a run sends to `client`: `spec` and `engine` as that server reads them ([`served_spec`],
/// [`StudioRunEngine::wire`]), or why it cannot be sent there.
fn request_for(
    client: &BacktesthubClient,
    spec: StrategySpecWire,
    engine: StudioRunEngine,
    axes_hold_values: bool,
) -> Result<(StrategySpecWire, Option<StrategyEngineConfigWire>), RunError> {
    let features = client.features();
    Ok((served_spec(spec, features, axes_hold_values)?, engine.wire(features)?))
}

// ⚠ **`DEFAULT_REMOTE_ADDR` stood here and is DELETED (2026-09-20).** It was
// `"127.0.0.1:7878"` — the DATA daemon — and it was a guaranteed refusal rather than a stale
// comment: ruling 7 moved every `Run*` verb onto the backtest plane, and
// `crates/vike-datahub/src/server/connection.rs`'s `handle_connection` consults
// `vike_hub_protocol::plane::foreign_verb` BEFORE the scope check, so a user who
// clicked "Remote" and pressed Run got a wrong-plane error from a daemon that has not served the
// verb since. The address field is editable, so what broke was the DEFAULT, not the path — typing
// 7880 always worked, which is why nothing reported it.
//
// It is not re-pointed under its own name because that would leave TWO names for one value:
// `vike_config::DEFAULT_BACKTEST_ADDRESS` (imported above) is the address for BOTH backends now, since the two
// differ in what they may ASK for and never in which daemon answers. Two further things the old
// constant got wrong are worth keeping, because each is a shape that recurs: its doc claimed the
// value matched "`vike-cli`'s own default", which the same ruling had made false (`vike-cli
// backtest run` dials `config.backtest_addr` → 7880); and it was a hand-copied literal rather than
// a reference to `vike_config::DEFAULT_DATAHUB_ADDRESS`, which is why nothing moved it when the
// authority moved.

/// WHERE a Studio Run/Sweep/Walk-Forward executes.
///
/// [`StudioRunBackend::Source`] is the default: every run leaves this process. The run happens
/// in-process over the GUI's own `HistoryStoreDataFusion`. [`StudioRunBackend::Source`] offloads it to the COMPUTE
/// daemon over TCP (the compute-to-data path): the strategy + slice DTOs go out, only the rendered
/// answer comes back, and the result is mapped to the SAME `TradingRecord` / `StudioParamscan` /
/// `WalkforwardResults` the local path produces, so the results pane renders unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StudioRunBackend {
    /// Offload the run to the backtesthub listening at `address` (`host:port`) —
    /// `vike-backend backtesthub --addr`, [`DEFAULT_BACKTEST_ADDRESS`].
    ///
    /// ⚠ **Not the datahub**, which this said until 2026-09-20: the three verbs this variant sends
    /// (`RunSlice`, `RunParamscan`, `RunWalkforward`) are all `Plane::Backtest`, and the data daemon
    /// refuses them by plane. See the tombstone above the constant for what the wrong default cost.
    Source {
        /// The backtesthub's address, e.g. [`DEFAULT_BACKTEST_ADDRESS`] — the same daemon
        /// [`StudioRunBackend::Named`] dials.
        address: String,
    },
    /// **Run a strategy the SERVER already holds** — the one backend an OBSERVE credential can
    /// reach (`docs/decisions/0064-a-named-run-carries-no-source.md`).
    ///
    /// ⚠ **The same DAEMON as [`StudioRunBackend::Source`], and a different REQUEST.** This doc said the two
    /// dialled different daemons until 2026-09-20 — "that variant dials the DATA daemon" — and that
    /// sentence was the whole bug: `StudioRunBackend::Source` sends `Plane::Backtest` verbs, so it was never
    /// the data daemon's to answer, it just DEFAULTED to its address. Both variants dial
    /// `vike-backend backtesthub --addr` ([`DEFAULT_BACKTEST_ADDRESS`]) now.
    ///
    /// What still separates them is the CREDENTIAL and the shape of the ask: a named run is the one
    /// backend a device at the `observe` role can reach. The addresses stay separate FIELDS because they are fields
    /// of two enum variants and cannot be otherwise, not because they may differ; a box that moves
    /// its backtesthub moves both.
    ///
    /// What it cannot do, and `named_run::to_named_run_spec` refuses each BY NAME: a script, a sweep, a
    /// walk-forward, more than one symbol, an open-ended window.
    Named {
        /// The backtesthub's address, e.g. [`DEFAULT_BACKTEST_ADDRESS`].
        address: String,
    },
}

/// The backtesthub, always.
///
/// ⚠ There used to be a `Local` variant and it was this default: the run happened in-process over
/// the GUI's own store. It is GONE, on the owner's decision - every Studio run goes through the
/// backtesthub now. What that removed is not a second ENGINE (there never was one: the local
/// path called `vike_backtest::run::tick_replay::replay_ticks`, the same function the daemon reaches
/// through the backtesthub's `run_slice_local`) but a second SURFACE - a path on which a run's inputs and answers
/// were never marshalled, beside one on which they always are, with nothing holding the two equal.
///
/// The cost is stated rather than hidden: a Studio with no daemon reachable now runs NOTHING. That
/// is a cliff, not a slowdown, and it is the price of the single surface.
impl Default for StudioRunBackend {
    fn default() -> Self {
        StudioRunBackend::Source { address: DEFAULT_BACKTEST_ADDRESS.to_string() }
    }
}

impl StudioRunBackend {
    /// The DEFAULT Source backend - `DEFAULT_BACKTEST_ADDRESS`, nothing typed.
    ///
    /// ⚠ It is NOT what the UI's **Source** button installs, which this said until the
    /// button stopped discarding the operator's address. Both constructors hard-code the
    /// default host, so calling one from a mode switch silently reset a hand-entered box
    /// back to 127.0.0.1:7880 - for no reason at all, since both modes dial the SAME
    /// daemon. The button now rebuilds the variant around the CURRENT address and these
    /// two are what `no_offloading_backend_defaults_to_the_datahub` measures.
    ///
    /// ⚠ This exists so the DEFAULT can be tested. While the choice was spelled inline in
    /// `StudioState::ui`, the only test that could see an address was one that PASSED the address
    /// in — so `no_offloading_backend_defaults_to_the_datahub` below could not have caught the
    /// 7878 default it now exists to catch, and did not, for as long as that default stood.
    #[must_use]
    pub fn source_default() -> Self {
        Self::Source { address: DEFAULT_BACKTEST_ADDRESS.to_string() }
    }

    /// The DEFAULT Named backend. Same daemon as [`StudioRunBackend::source_default`] —
    /// see [`StudioRunBackend::Named`] for what differs, which is the credential and the ask, not the
    /// port. Same note as above: the UI's button does not call this.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn named_default() -> Self {
        Self::Named { address: DEFAULT_BACKTEST_ADDRESS.to_string() }
    }

    /// The address this backend dials.
    ///
    /// ⚠ This returned `Option<&str>` while `Local` existed, because that variant dialled
    /// nothing. Every remaining variant carries an address, so the `None` arm became
    /// unreachable and the only caller was answering it with an `.expect`. Returning `&str`
    /// deletes both.
    #[must_use]
    pub fn address(&self) -> &str {
        match self {
            Self::Source { address } | Self::Named { address } => address.as_str(),
        }
    }
}

// ⚠ `remote_store_tick_refusal` STOOD HERE and is DELETED WITH ITS CAUSE, not instead of it. It
// existed for exactly one combination - a REMOTE store, a TICK slice, and the `Local` backend -
// because a local tick replay would have had to pull the raw tick tape over the wire, which the
// compute-to-data rule refuses. With `Local` gone that combination cannot arise, so the guard
// could only ever answer `None`; a guard that cannot fire is worse than no guard, because the
// next reader trusts it. Its three UI call sites went with it.

// ---- the KEY every backtesthub dial signs with (decision 0288) --------------------------------
//
// This PC's own DEVICE key, in the `HubKeys` the desktop builds around it for every hub dial
// (`vike_app_core::backend::registry::datahub_dial_keys`). The server's access list
// decides what it may do: `observe` admits the Read dials ([`connect_observe`]), `control` the
// Write ones ([`connect`]). Until the cutover a Run signed with a shared datahub CONTROL key the
// launcher filed as the `VIKE_STUDIO_BACKTEST_KEY` row (decision 0083, question 1); no hub reads
// that key any more, and nothing here reads the row.

/// Spawn a REMOTE single-slice Run on a worker thread; its `StudioRunViewOutcome` arrives on the returned
/// receiver (`StudioState::run_rx`).
///
/// `key` is this PC's device key ([`crate::StudioState::hub_keys`]), `None` when it holds none — see
/// `connect` for what each does.
///
/// The request carries no cost: a run's commission and slippage are strategy params, in `spec`'s params
/// (the owner's ruling of 2026-11-18), defaulting to the strategy's own, else none. It carries `engine`, the
/// starting capital and the funding switch, as the server reads them ([`request_for`]).
///
/// The answer is the run's SUMMARY (decision 0078's amendment of 2026-11-16): every metric and the
/// equity curve thinned to about two thousand points; the trades stay on the server for
/// `spawn_backtesthub_trades_page`. `fresh` (the toolbar's Recompute) runs it even when the server saved an identical
/// run it would answer from (decision 0078's amendment of 2026-11-20); the summary says when it did.
pub fn spawn_backtesthub_run(
    address: String,
    key: Option<HubKeys>,
    spec: StrategySpec,
    slice: DataSlice,
    engine: StudioRunEngine,
    fresh: bool,
) -> Receiver<StudioRunViewOutcome> {
    spawn_outcome(move || {
        let wire_spec = to_wire_spec(&spec)?;
        let mut client = connect(&address, key.as_ref())?;
        client.set_fresh(fresh);
        let (wire_spec, params) = request_for(&client, wire_spec, engine, false)?;
        let summary = client
            .run_slice_summary(wire_spec, to_wire_slice(&slice), params)
            .map_err(run_error_from_wire)?;
        Ok(StudioRunView { summary, trades: None })
    })
}

/// Spawn a REMOTE parameter scan on a worker thread: each point's numbers and the best point's
/// summary come back; [`spawn_backtesthub_point`] opens another point. No cost, `engine` and `fresh`, as
/// [`spawn_backtesthub_run`].
pub fn spawn_backtesthub_sweep(
    address: String,
    key: Option<HubKeys>,
    spec: StrategySpec,
    slice: DataSlice,
    grid: Vec<(String, Vec<ParamValue>)>,
    engine: StudioRunEngine,
    fresh: bool,
) -> Receiver<Result<ParamscanSummaryWire, RunError>> {
    spawn_outcome(move || {
        let wire_spec = to_wire_spec(&spec)?;
        let mut client = connect(&address, key.as_ref())?;
        client.set_fresh(fresh);
        // The grid holds a Rhai script's values and costs as axes (`StudioState::held_rows`).
        let (wire_spec, params) = request_for(&client, wire_spec, engine, true)?;
        // The server prices every point itself, by the strategy's commission and slippage params.
        client
            .run_paramscan_summary(
                wire_spec,
                to_wire_slice(&slice),
                ParamscanWire { axes: grid },
                params,
            )
            .map_err(run_error_from_wire)
    })
}

/// Spawn a REMOTE walk-forward validation on a worker thread: the windows and the stitched curve,
/// thinned. `walk` is the split count and, when the Sweep pane asks for one, the grid each window
/// searches. `fresh` as [`spawn_backtesthub_run`]'s.
pub fn spawn_backtesthub_walkforward(
    address: String,
    key: Option<HubKeys>,
    spec: StrategySpec,
    slice: DataSlice,
    walk: WalkforwardWire,
    engine: StudioRunEngine,
    fresh: bool,
) -> Receiver<Result<WalkforwardSummaryWire, RunError>> {
    spawn_outcome(move || {
        let wire_spec = to_wire_spec(&spec)?;
        let mut client = connect(&address, key.as_ref())?;
        client.set_fresh(fresh);
        // As the scan's: the walk's grid holds the script's values as axes.
        let (wire_spec, params) = request_for(&client, wire_spec, engine, true)?;
        // The server prices every window itself (see the scan's).
        client
            .run_walkforward_summary(wire_spec, to_wire_slice(&slice), walk, params)
            .map_err(run_error_from_wire)
    })
}

/// Spawn the fetch of page `page` of a summarized run's trades (`run`, and `point` of a scan): what
/// the Trades tab shows, two hundred a page.
pub(crate) fn spawn_backtesthub_trades_page(
    address: String,
    key: Option<HubKeys>,
    run: String,
    point: Option<usize>,
    page: usize,
) -> Receiver<Result<TradesPageWire, RunError>> {
    spawn_outcome(move || {
        connect(&address, key.as_ref())?
            .read_trades(&run, point, page, TRADES_PAGE_SIZE)
            .map_err(run_error_from_wire)
    })
}

/// Spawn the fetch of point `point` of a summarized param scan `run`: what the Studio shows when
/// the user opens a point other than the best.
pub(crate) fn spawn_backtesthub_point(
    address: String,
    key: Option<HubKeys>,
    run: String,
    point: usize,
) -> Receiver<StudioRunViewOutcome> {
    spawn_outcome(move || {
        let summary = connect(&address, key.as_ref())?
            .read_point(&run, point)
            .map_err(run_error_from_wire)?;
        Ok(StudioRunView { summary, trades: None })
    })
}

/// One saved row's outcome from the Saved pane's "Compare all", by row name: the run's SUMMARY, or its
/// [`RunError`] already a sentence, so the pane's `saved_compare_rows` needs no error type.
pub(crate) type StudioSavedCompareOutcome = Vec<(String, Result<RunSummaryWire, String>)>;

/// Spawn "Compare all" on a worker thread: ONE hub `Summarized` `RunSlice` per saved row, the very
/// request a Run of that row sends ([`run_backtesthub_slice_summary`]), so a row's numbers in the compare
/// table are the numbers a Run of it shows — priced by the hub, never by an in-process simulator — and its
/// answer is a summary whatever the slice's length (decision 0078's amendment of 2026-11-18). Every row runs
/// under `engine`, the test conditions' capital and funding switch, as a Run does.
pub(crate) fn spawn_backtesthub_compare(
    address: String,
    key: Option<HubKeys>,
    rows: Vec<(String, StrategySpec)>,
    slice: DataSlice,
    engine: StudioRunEngine,
) -> Receiver<StudioSavedCompareOutcome> {
    spawn_outcome(move || {
        compare_each(rows, |spec| {
            let (_, summary) =
                run_backtesthub_slice_summary(&address, key.as_ref(), spec, &slice, engine)?;
            Ok(summary)
        })
    })
}

/// The AI Copilot's candidate backtester (the `backtest` `vike_ai::develop_strategy_with_ledger`
/// takes): each candidate the loop compiled is ONE hub `Summarized` `RunSlice` over its held-out slice,
/// the very request a Run sends ([`run_backtesthub_slice_summary`], signed with the same key), so a
/// candidate is computed on the backtest server and charged as a Run of the same bars is. Nothing runs in
/// this process (the owner's ruling of 2026-11-09; decision 0083's amendment of that day).
///
/// The candidate answers its trade count from the summary, and its WHOLE out-of-sample curve is read only
/// when the loop keeps it (`vike_ai::OutOfSampleRun::read_equity_curve`): page by page over the same
/// connection (`BacktesthubClient::read_whole_curve`, decision 0078's amendment of 2026-11-18), because the
/// deflation needs every point and no frame need carry them all.
pub(crate) fn copilot_backtester(
    address: String,
    key: Option<HubKeys>,
) -> impl Fn(&str, OutOfSampleSlice<'_>) -> Result<OutOfSampleRun, String> + Send + 'static {
    move |code, out_of_sample| {
        let slice = DataSlice::bars(
            out_of_sample.venue,
            out_of_sample.symbol,
            out_of_sample.interval,
            out_of_sample.range,
        );
        // At the engine's default capital (the loop ranks a candidate by its Sharpe and trade count), and with no
        // funding, the default.
        let (mut client, summary) = run_backtesthub_slice_summary(
            &address,
            key.as_ref(),
            &StrategySpec::rhai(code),
            &slice,
            StudioRunEngine::default(),
        )
        .map_err(|e| e.to_string())?;
        let run = summary.run;
        let read_equity_curve =
            Box::new(move || client.read_whole_curve(&run).map(|(equity, _)| equity));
        Ok(OutOfSampleRun { n_trades: summary.n_trades, read_equity_curve })
    }
}

/// Every row through `run`, in order, a failed row kept as its sentence rather than dropped.
fn compare_each(
    rows: Vec<(String, StrategySpec)>,
    run: impl Fn(&StrategySpec) -> Result<RunSummaryWire, RunError>,
) -> StudioSavedCompareOutcome {
    rows.into_iter().map(|(name, spec)| (name, run(&spec).map_err(|e| e.to_string()))).collect()
}

/// Dial `address`, ship the `RunSlice` a Run sends, summarized ([`spawn_backtesthub_run`]'s request), and
/// answer the connection with the run's summary, so a caller that keeps the run can read its whole curve
/// over the same connection. The request carries no cost, and the spec and `engine` exactly as a Run sends them
/// ([`request_for`]).
///
/// The Saved pane's "Compare all" and the AI Copilot asked for the WHOLE answer until decision 0078's amendment
/// of 2026-11-18: the compare table ranks by the summary's Sharpe, which is computed over the whole curve, and
/// the Copilot reads a kept candidate's whole curve on request ([`copilot_backtester`]).
fn run_backtesthub_slice_summary(
    address: &str,
    key: Option<&HubKeys>,
    spec: &StrategySpec,
    slice: &DataSlice,
    engine: StudioRunEngine,
) -> Result<(BacktesthubClient, RunSummaryWire), RunError> {
    let wire_spec = to_wire_spec(spec)?;
    let mut client = connect(address, key)?;
    let (wire_spec, params) = request_for(&client, wire_spec, engine, false)?;
    let summary = client
        .run_slice_summary(wire_spec, to_wire_slice(slice), params)
        .map_err(run_error_from_wire)?;
    Ok((client, summary))
}

/// Connect to the backtesthub for a Run, Sweep, Walk-Forward, a strategy file's Save or a Build. A
/// transport fault, a protocol-version mismatch or a refusal becomes a [`RunError::Data`] naming the
/// address, surfaced in the Studio's existing error banner rather than an `unwrap`.
///
/// - **With this PC's device key** (`keys`, decision 0288) — `BacktesthubClient::connect_authed_on` at
///   `Scope::Write` (every `Run*` verb, the Save and the Build are Control-scoped), and ONLY to a server
///   whose pre-auth `Welcome` names the BACKTEST plane. ⚠ The plane is the reason it is this constructor:
///   the same device at `control` on the datahub's list would open a Write session that carries
///   `Backfill` and `DeleteSeries`, which is exactly what the address regression
///   `crates/vike-desktop/src/main.rs` records (Studio's compute address overwritten with the datahub's)
///   or a hand-typed 7878 in the Backend field would do. Refused there, nothing but the `Hello` has been
///   sent. The server's access list decides the rest: the device must be listed at `control`.
/// - **Without one** — the plain connect, unchanged against a key-less daemon.
///
/// A KEYED backtesthub's refusal is [`device_refusal_message`]: how this PC's device gets on the list.
fn connect(address: &str, keys: Option<&HubKeys>) -> Result<BacktesthubClient, RunError> {
    let dialled = match keys {
        Some(keys) => {
            BacktesthubClient::connect_authed_on(address, keys, Scope::Write, Plane::Backtest)
        }
        None => BacktesthubClient::connect(address),
    };
    dialled.map(thin_link).map_err(|e| {
        if matches!(e, HubCallError::AccessDenied(_)) {
            return RunError::Data(device_refusal_message(address, keys.is_some(), &e.to_string()));
        }
        RunError::Data(format!("connect to the backtesthub at {address} failed: {e}"))
    })
}

/// Both dials' body: msgpack in LZ4 toward a backtesthub that speaks msgpack ("JSON only at the edges", 2026-11-19),
/// since the Studio reaches it over the ssh tunnel, a thin link; JSON toward an older one. A reply LZ4 cannot shrink
/// comes back plain.
fn thin_link(mut client: BacktesthubClient) -> BacktesthubClient {
    client.prefer_body(BodyCodec::MsgpackLz4);
    client
}

/// What a keyed backtesthub's refusal of this Studio means — pure, so the sentence an operator reads is
/// unit-tested. Studio signs with this PC's own device key (decision 0288), so the cure is always the
/// server's access list: register the device, or raise its role to `control`. Both cures are named as
/// the desktop around Studio offers them (`crates/vike-app-core/src/ui/windows/backend/mod.rs`'s
/// `server_section` and `access_section`, the group `registry::no_device_key` points at), never
/// as a `vike-cli` verb a desktop user does not have. `held` is whether this PC holds a device key at
/// all; `why` is the server's own word. No restart is asked for: the desktop seeds
/// `StudioState::hub_keys` again when a connect makes or changes the key (`StudioState::hub_keys_due`).
fn device_refusal_message(address: &str, held: bool, why: &str) -> String {
    let what = if held {
        format!("refused this PC's device key ({why})")
    } else {
        format!("REQUIRES authentication, and this PC holds no device key ({why})")
    };
    format!(
        "the backtesthub at {address} {what}. Studio's Run, Sweep, Walk-Forward, Save and Build sign \
         with this PC's device key, and the server's access list must hold this device at the role \
         `control` or `admin`. Register it in Data Manager → Backend → Server: press Connect over \
         ssh or Connect the server on this computer (a guest pastes the owner's invitation and \
         presses Join). A device listed as `observe` is raised by the server's owner in Data \
         Manager → Backend → Access: the person/device, the role `control`, Set role."
    )
}

#[cfg(test)]
mod tests;

/// **Does the backtest server at `address` answer this PC?** One Read dial ([`connect_observe`]) and a
/// `Ping`, on a worker: the toolbar's server status. `Ok` is "connected", with the features the server
/// advertises (what the panes may offer: the funding switch, a Rhai buffer's parameters); the `Err` is why
/// not, in the dial's own words (unreachable, or the server's refusal of this PC's device key).
pub(crate) fn spawn_backtesthub_check(
    address: String,
    keys: Option<HubKeys>,
) -> Receiver<Result<Vec<String>, String>> {
    spawn_outcome(move || {
        let mut client = connect_observe(&address, keys.as_ref()).map_err(|e| e.to_string())?;
        client.ping().map_err(|e| format!("the backtesthub at {address} did not answer: {e}"))?;
        Ok(client.features().to_vec())
    })
}

/// Dial as OBSERVE when a key is present, unauthenticated when it is not.
///
/// ⚠ The unauthenticated arm is not a fallback that hides a problem: a key-LESS backtesthub
/// authenticates nothing and serves the loopback socket (`docs/decisions/0050`), and a KEYED one
/// answers a connect-time error naming the keys. Both are legible.
fn connect_observe(address: &str, keys: Option<&HubKeys>) -> Result<BacktesthubClient, RunError> {
    BacktesthubClient::connect_with_keys(address, keys, Scope::Read).map(thin_link).map_err(|e| {
        RunError::Data(format!(
            "connect to the backtesthub {address} failed: {e} — the Run* verbs are served by \
             `vike-backend backtesthub --addr` (default {DEFAULT_BACKTEST_ADDRESS}), NOT by the datahub"
        ))
    })
}

// The Named backend (decision 0064): the run, its Compare all, the roster, the spec conversion.
pub(crate) mod named_run;
// The backtest server's strategy files: the Strategy pane's list, Open and Save, and a Build.
pub(crate) mod strategy_files;
// The backtest server's studies and saved runs: the Research pane is their thin client.
pub(crate) mod studies;
// The backtest server's strategy SDKs: the SDK source view's list and its file reads (decision 0296).
pub(crate) mod sdk_source;
// Flow step 2 of the runtime-strategy-file design: a Build's pre-dial refusals.
mod build;

#[cfg(test)]
mod named_run_tests;
