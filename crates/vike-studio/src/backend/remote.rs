//! PR-5 — the Studio run BACKEND: dispatch a Run/Sweep/Walk-Forward to the REMOTE backtesthub
//! (`vike-backend backtesthub --addr`) over TCP instead of running it in-process.
//!
//! ⚠ This said `vike-datahub` until 2026-09-20 and the code agreed with it, which is the part that
//! mattered — see the tombstone where `DEFAULT_REMOTE_ADDR` stood. Ruling 7 moved every `Run*` verb
//! onto the backtest plane; the data daemon serves the STORE reads a remote slice needs and refuses
//! the run itself.
//!
//! The contract that makes this a drop-in for the local path: every `spawn_*_remote` here returns the
//! EXACT SAME `Receiver<T>` its `vike_studio_core::spawn_*` twin returns (`Receiver<RunOutcome>` /
//! `Receiver<Result<StudioParamscan, RunError>>` / `Receiver<Result<WalkForwardReport, RunError>>`), so
//! `StudioState::poll` folds a remote outcome in with ZERO changes and the results pane renders it
//! unchanged. A remote run maps the wire answer back onto the same `BacktestResult` / `StudioParamscan` /
//! `WalkForwardReport` the local engine produces, and every failure — connect fault, server error, or
//! protocol desync — arrives as an `Err(RunError)` on that receiver (never a panic / `unwrap`).
//!
//! # The wire DTOs are the client crate's; the conversions live here
//!
//! The `Wire*` schema is defined ONCE in the light, DataFusion-free `vike-datahub-client`
//! (`wire_studio`). The server — `crates/vike-backtesthub/src/backtesthub_server.rs`, mounted by
//! `vike-backend backtesthub --addr`; it was `vike-datahub` behind `datafusion-serve` until ruling 7 —
//! converts wire → engine and
//! runs `vike_studio_core::run_slice`; THIS module is the client-side mirror — engine → wire for
//! the request, wire → engine for the answer — reusing the client crate's own DTOs and
//! `WireTrade::{from_trade,to_trade}` rather than inventing a second schema. Native-strategy params
//! ride as their TOML TEXT (`WireSpec::Native.params_toml`), the same idiom the server parses back with
//! `StrategySpec::native_from_toml_str`, so no params TABLE type crosses the wire; a sweep axis
//! value and a window's chosen value cross as the `vike_model::params::ParamValue` they are.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use vike_analytics::BacktestResult;
use vike_backtest::walkforward::{WalkForwardReport, WfWindow};
use vike_datahub_client::named_run::{
    NamedParam, NamedRoster, NamedRunOutcome, NamedRunRefusal, NamedRunSpec, validate_named_run,
};
use vike_datahub_client::{
    DatahubClient, WireParamscanEntry, WireTrade, WireWfWindow,
    proto::Plane,
    wire_studio::{
        WireParamscan, WireParamscanResult, WireRunResult, WireSlice, WireSliceKind, WireSpec,
        WireWalkforward, WireWalkforwardResult,
    },
};
use vike_hub_proto::auth::{HubKeys, Scope};
use vike_model::params::{ParamValue, Params};
use vike_studio_core::{
    DataSlice, ParamscanEntry, RunError, RunOutcome, SliceKind, StrategySpec, StudioParamscan,
    spawn_outcome,
};

// ⚠ **`DEFAULT_REMOTE_ADDR` stood here and is DELETED (2026-09-20).** It was
// `"127.0.0.1:7878"` — the DATA daemon — and it was a guaranteed refusal rather than a stale
// comment: ruling 7 moved every `Run*` verb onto the backtest plane, and
// `crates/vike-datahub/src/server/connection.rs`'s `handle_connection` consults
// `crates/vike-datahub-client/src/proto/verbs.rs`'s `plane_of` BEFORE the scope check, so a user who
// clicked "Remote" and pressed Run got a wrong-plane error from a daemon that has not served the
// verb since. The address field is editable, so what broke was the DEFAULT, not the path — typing
// 7880 always worked, which is why nothing reported it.
//
// It is not re-pointed under its own name because that would leave TWO names for one value:
// [`DEFAULT_BACKTESTHUB_ADDR`] below is the address for BOTH offloading backends now, since the two
// differ in what they may ASK for and never in which daemon answers. Two further things the old
// constant got wrong are worth keeping, because each is a shape that recurs: its doc claimed the
// value matched "`vike-cli`'s own default", which the same ruling had made false (`vike-cli
// backtest run` dials `config.backtest_addr` → 7880); and it was a hand-copied literal rather than
// a reference to `vike_config::DEFAULT_DATAHUB_ADDR`, which is why nothing moved it when the
// authority moved.

/// WHERE a Studio Run/Sweep/Walk-Forward executes.
///
/// [`Backend::Remote`] is the default: every run leaves this process. The run happens
/// in-process over the GUI's own `DataFusionHist`. [`Backend::Remote`] offloads it to the COMPUTE
/// daemon over TCP (the compute-to-data path): the strategy + slice DTOs go out, only the rendered
/// answer comes back, and the result is mapped to the SAME `BacktestResult` / `StudioParamscan` /
/// `WalkForwardReport` the local path produces, so the results pane renders unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// Offload the run to the backtesthub listening at `addr` (`host:port`) —
    /// `vike-backend backtesthub --addr`, [`DEFAULT_BACKTESTHUB_ADDR`].
    ///
    /// ⚠ **Not the datahub**, which this said until 2026-09-20: the three verbs this variant sends
    /// (`RunSlice`, `RunParamscan`, `RunWalkforward`) are all `Plane::Backtest`, and the data daemon
    /// refuses them by plane. See the tombstone above the constant for what the wrong default cost.
    Remote {
        /// The backtesthub's address, e.g. [`DEFAULT_BACKTESTHUB_ADDR`] — the same daemon
        /// [`Backend::Named`] dials.
        addr: String,
    },
    /// **Run a strategy the SERVER already holds** — the one backend an OBSERVE credential can
    /// reach (`docs/decisions/0064-a-named-run-carries-no-source.md`).
    ///
    /// ⚠ **The same DAEMON as [`Backend::Remote`], and a different REQUEST.** This doc said the two
    /// dialled different daemons until 2026-09-20 — "that variant dials the DATA daemon" — and that
    /// sentence was the whole bug: `Backend::Remote` sends `Plane::Backtest` verbs, so it was never
    /// the data daemon's to answer, it just DEFAULTED to its address. Both variants dial
    /// `vike-backend backtesthub --addr` ([`DEFAULT_BACKTESTHUB_ADDR`]) now.
    ///
    /// What still separates them is the CREDENTIAL and the shape of the ask: a named run is the one
    /// backend an OBSERVE key can reach. The addresses stay separate FIELDS because they are fields
    /// of two enum variants and cannot be otherwise, not because they may differ; a box that moves
    /// its backtesthub moves both.
    ///
    /// What it cannot do, and [`to_named_run_spec`] refuses each BY NAME: a script, a sweep, a
    /// walk-forward, more than one symbol, an open-ended window.
    Named {
        /// The backtesthub's address, e.g. [`DEFAULT_BACKTESTHUB_ADDR`].
        addr: String,
    },
}

/// The backtesthub, always.
///
/// ⚠ There used to be a `Local` variant and it was this default: the run happened in-process over
/// the GUI's own store. It is GONE, on the owner's decision - every Studio run goes through the
/// backtesthub now. What that removed is not a second ENGINE (there never was one: the local
/// path called `vike_backtest::hist_replay::replay_ticks`, the same function the daemon reaches
/// through the backtesthub's `run_slice_local`) but a second SURFACE - a path on which a run's inputs and answers
/// were never marshalled, beside one on which they always are, with nothing holding the two equal.
///
/// The cost is stated rather than hidden: a Studio with no daemon reachable now runs NOTHING. That
/// is a cliff, not a slowdown, and it is the price of the single surface.
impl Default for Backend {
    fn default() -> Self {
        Backend::Remote { addr: DEFAULT_BACKTESTHUB_ADDR.to_string() }
    }
}

impl Backend {
    /// The DEFAULT Remote backend - `DEFAULT_BACKTESTHUB_ADDR`, nothing typed.
    ///
    /// ⚠ It is NOT what the UI's **Remote** button installs, which this said until the
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
    pub fn remote_default() -> Self {
        Self::Remote { addr: DEFAULT_BACKTESTHUB_ADDR.to_string() }
    }

    /// The DEFAULT Named backend. Same daemon as [`Backend::remote_default`] —
    /// see [`Backend::Named`] for what differs, which is the credential and the ask, not the
    /// port. Same note as above: the UI's button does not call this.
    #[must_use]
    pub fn named_default() -> Self {
        Self::Named { addr: DEFAULT_BACKTESTHUB_ADDR.to_string() }
    }

    /// The address this backend dials.
    ///
    /// ⚠ This returned `Option<&str>` while `Local` existed, because that variant dialled
    /// nothing. Every remaining variant carries an address, so the `None` arm became
    /// unreachable and the only caller was answering it with an `.expect`. Returning `&str`
    /// deletes both.
    #[must_use]
    pub fn addr(&self) -> &str {
        match self {
            Self::Remote { addr } | Self::Named { addr } => addr.as_str(),
        }
    }
}

// ⚠ `remote_store_tick_refusal` STOOD HERE and is DELETED WITH ITS CAUSE, not instead of it. It
// existed for exactly one combination - a REMOTE store, a TICK slice, and the `Local` backend -
// because a local tick replay would have had to pull the raw tick tape over the wire, which the
// compute-to-data rule refuses. With `Local` gone that combination cannot arise, so the guard
// could only ever answer `None`; a guard that cannot fire is worse than no guard, because the
// next reader trusts it. Its three UI call sites went with it.

// ---- the COMPUTE KEY (docs/decisions/0083-the-runtime-plugin-join-lands.md, question 1) --------
//
// The owner ruled on 2026-09-26 (option (a)): the desktop resolves the datahub CONTROL key for
// Studio's COMPUTE dial ONLY. At `VerbScope::Write` that key also authorises compiling Rhai,
// running any artifact in the plugin directory, `Backfill` and `DeleteSeries` — a store-mutation
// authority this process never held before — so everything below exists to keep its reach inside
// the desktop as narrow as the code can make it: one name, one reader, one type no other dial
// accepts, one place it is signed, and that place refuses the data plane.
//
// And on 2026-10-31 ("all in the SQLite, not files"): the key lives ONLY as a row of the PC's own
// settings database — not a variable, not a key file. The launcher files it there with the value on a
// pipe; this module reads it out of the credential map the desktop already loads.

/// The deployment row Studio's backtest key is filed under in the PC's settings database — read by
/// [`backtest_key_from_credentials`] and by nothing else in this workspace.
///
/// ⚠ **A name of its own, NOT the platform's `VIKE_DATAHUB_CONTROL_KEY`, and the difference IS the
/// reach limit.** The platform name is a HUB key: the `hub_key` table's, resolved by every datahub
/// dial the desktop makes (the store, the chart seed, the venue catalog, the named-run key) and by
/// `vike-cli`'s `datahub_keyring` on this very PC, so a Write key filed there would reach `Backfill`
/// and `DeleteSeries` through any of them. This is a DEPLOYMENT row of the credential table,
/// admitted to `vike-cli secrets set` by name
/// (`vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID`) and read by name from the
/// credential map: no hub-key resolver and no datahub dial ever asks for it, so nothing but this
/// module can see it. `crates/vike-ops/tests/gui/studio_backtest_key_reach_gate.rs` holds the name to
/// one Rust reader.
///
/// ⚠ **It was a process variable of the same spelling until 2026-10-31**, set by `just studio` for
/// the GUI alone; the variable is read by NOTHING now (no fallback, no migration — a retired name
/// just stops being read), and [`no_backtest_key_message`] says where the key is when Run finds none.
pub const BACKTEST_KEY_NAME: &str = "VIKE_STUDIO_BACKTEST_KEY";

/// **Studio's backtest key** — the datahub CONTROL key, held for ONE purpose: signing Studio's own
/// Run, Sweep and Walk-Forward to the backtesthub.
///
/// ⚠ **OPAQUE, and that is the reach limit made structural.** The key inside is private to this
/// module and is read in exactly one place, [`connect`], which signs it only through
/// `DatahubClient::connect_authed_on(.., Scope::Write, Plane::Backtest)` — the constructor that
/// refuses, before sending a byte of `Auth`, a server whose pre-auth `Welcome` is not the compute
/// daemon's. There is no expression elsewhere that turns this into something another dial accepts:
/// handing it to `RemoteHistStore::with_keys`, a chart-seed or catalog dial, [`Backend::Named`]'s
/// observe dial or the trading-node observer is a TYPE error rather than a review comment.
/// `crates/vike-ops/tests/gui/studio_backtest_key_reach_gate.rs` holds the lexical half the compiler
/// cannot: one read of the field, one construction, no derive that could serialise it.
///
/// Its `Debug` prints presence only and it has no `Display`, so a `{:?}` of a `StudioState` or a
/// panic message cannot carry it.
#[derive(Clone)]
pub struct BacktestKey {
    write_only: HubKeys,
}

impl std::fmt::Debug for BacktestKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BacktestKey(<redacted>)")
    }
}

/// Build Studio's [`BacktestKey`] from the CREDENTIAL map the desktop loads from the PC's settings
/// database — [`BACKTEST_KEY_NAME`] and nothing else. A pure map lookup: the BINARY opens the store,
/// per this workspace's "libraries take configuration as parameters" rule — the same shape
/// `vike_strategy_builder::builder::keys_from_credentials` has for the builder's key, the other
/// Write-scope row of the PC's store. It reads no variable and no file.
///
/// - Only the WRITE slot is filled, so the key cannot be presented at `Scope::Read` even by a
///   caller holding the inner value — which none outside this module can.
/// - A blank or whitespace value is NO key (an empty key signs a mac the daemon refuses, which
///   would trade "no key, here is where it comes from" for `bad mac`).
/// - The platform name `VIKE_DATAHUB_CONTROL_KEY` is NOT consulted, whatever the map holds: see
///   [`BACKTEST_KEY_NAME`] for why the name is the limit.
pub fn backtest_key_from_credentials(creds: &HashMap<String, String>) -> Option<BacktestKey> {
    let key = creds.get(BACKTEST_KEY_NAME)?.trim();
    if key.is_empty() {
        return None;
    }
    Some(BacktestKey { write_only: HubKeys::new(Vec::new(), key.as_bytes().to_vec()) })
}

/// Spawn a REMOTE single-slice Run on a worker thread; the `RunOutcome` arrives on the returned
/// receiver — the remote twin of `vike_studio_core::spawn_run`, so `StudioState::run_rx` accepts it
/// unchanged.
///
/// `key` is Studio's [`BacktestKey`] when the launcher handed one over, and `None` otherwise — see
/// [`connect`] for what each does.
pub fn spawn_run_remote(
    addr: String,
    key: Option<BacktestKey>,
    spec: StrategySpec,
    slice: DataSlice,
) -> Receiver<RunOutcome> {
    spawn_outcome(move || run_slice_remote(&addr, key.as_ref(), &spec, &slice))
}

/// Spawn a REMOTE parameter sweep on a worker thread — the remote twin of
/// `vike_studio_core::spawn_paramscan` (same `Receiver` type, so `StudioState::sweep_rx` accepts it).
pub fn spawn_sweep_remote(
    addr: String,
    key: Option<BacktestKey>,
    spec: StrategySpec,
    slice: DataSlice,
    grid: Vec<(String, Vec<ParamValue>)>,
) -> Receiver<Result<StudioParamscan, RunError>> {
    spawn_outcome(move || run_sweep_remote(&addr, key.as_ref(), &spec, &slice, grid))
}

/// Spawn a REMOTE walk-forward validation on a worker thread — the remote twin of
/// `vike_studio_core::spawn_walkforward` (same `Receiver` type, so `StudioState::wf_rx` accepts it).
pub fn spawn_walkforward_remote(
    addr: String,
    key: Option<BacktestKey>,
    spec: StrategySpec,
    slice: DataSlice,
    n_splits: usize,
) -> Receiver<Result<WalkForwardReport, RunError>> {
    spawn_outcome(move || run_walkforward_remote(&addr, key.as_ref(), &spec, &slice, n_splits))
}

/// Dial `addr`, ship the `RunSlice` request, and map the answer back onto a `BacktestResult`. Params
/// go out as `None` — the server then resolves every engine field from `EngineParams::default()`,
/// matching the local path's `EngineParams::default()`.
fn run_slice_remote(
    addr: &str,
    key: Option<&BacktestKey>,
    spec: &StrategySpec,
    slice: &DataSlice,
) -> RunOutcome {
    let wire_spec = to_wire_spec(spec)?;
    let mut client = connect(addr, key)?;
    let wire =
        client.run_slice(wire_spec, to_wire_slice(slice), None).map_err(parse_wire_run_error)?;
    Ok(to_backtest_result(wire))
}

/// Dial `addr`, ship the `RunSweep` grid, and map the ranked answer back onto a `StudioParamscan`.
fn run_sweep_remote(
    addr: &str,
    key: Option<&BacktestKey>,
    spec: &StrategySpec,
    slice: &DataSlice,
    grid: Vec<(String, Vec<ParamValue>)>,
) -> Result<StudioParamscan, RunError> {
    let wire_spec = to_wire_spec(spec)?;
    let mut client = connect(addr, key)?;
    // Params go out as `None` — the server resolves every engine field from `EngineParams::default()`,
    // matching the local sweep path. (The proto carries an optional cost/cash override; the Studio GUI
    // does not expose one yet, so it keeps the default-params behavior.)
    let wire = client
        .run_paramscan(wire_spec, to_wire_slice(slice), WireParamscan { axes: grid }, None)
        .map_err(parse_wire_run_error)?;
    Ok(to_studio_sweep(wire))
}

/// Dial `addr`, ship the `RunWalkforward` split-count, and map the stitched answer back onto a
/// `WalkForwardReport`.
fn run_walkforward_remote(
    addr: &str,
    key: Option<&BacktestKey>,
    spec: &StrategySpec,
    slice: &DataSlice,
    n_splits: usize,
) -> Result<WalkForwardReport, RunError> {
    let wire_spec = to_wire_spec(spec)?;
    let mut client = connect(addr, key)?;
    // Params `None` — default engine params, matching the local walk-forward path (see the sweep note).
    let wire = client
        .run_walkforward(wire_spec, to_wire_slice(slice), WireWalkforward::fixed(n_splits), None)
        .map_err(parse_wire_run_error)?;
    Ok(to_walkforward_report(wire))
}

/// Connect to the backtesthub for a Run, Sweep or Walk-Forward. A transport fault, a
/// protocol-version mismatch or a refusal becomes a [`RunError::Data`] naming the address, surfaced
/// in the Studio's existing error banner rather than an `unwrap`.
///
/// - **With Studio's [`BacktestKey`]** — `DatahubClient::connect_authed_on` at `Scope::Write` (every
///   `Run*` verb is Control-scoped: `vike_datahub_client::proto`'s `required_scope`), and ONLY to a
///   server whose pre-auth `Welcome` names the BACKTEST plane. ⚠ This is the ONE place the key is
///   read, and the plane is the reason it is this constructor: one node pair authenticates both
///   daemons, so the same key signed toward the datahub would SUCCEED and open a Write session that
///   carries `Backfill` and `DeleteSeries` — which is exactly what the address regression
///   `crates/vike-desktop/src/main.rs` records (Studio's compute address overwritten with the
///   datahub's) or a hand-typed 7878 in the Backend field would do. Refused there, nothing but the
///   `Hello` has been sent. Against a KEY-LESS backtesthub it degrades to an unauthenticated
///   connect and nothing is signed at all.
/// - **Without one** — the plain connect, unchanged against a key-less daemon. A KEYED one refuses
///   at connect, and the message then says where this key comes from: the datahub client's own
///   wording points at the platform key names, which Studio's Run never reads.
fn connect(addr: &str, key: Option<&BacktestKey>) -> Result<DatahubClient, RunError> {
    let dialled = match key {
        Some(key) => {
            DatahubClient::connect_authed_on(addr, &key.write_only, Scope::Write, Plane::Backtest)
        }
        None => DatahubClient::connect(addr),
    };
    dialled.map_err(|e| {
        if key.is_none() && e.kind() == std::io::ErrorKind::PermissionDenied {
            return RunError::Data(no_backtest_key_message(addr));
        }
        RunError::Data(format!("connect to the backtesthub at {addr} failed: {e}"))
    })
}

/// What a keyed backtesthub's refusal means when this Studio holds no [`BacktestKey`] — pure, so
/// the sentence an operator reads is unit-tested.
///
/// It keeps `REQUIRES authentication` (the phrase every earlier banner used, so a search for it
/// still lands here) and names the ONE place Studio's key comes from — the row, and the two ways to
/// file it. It deliberately does not repeat the datahub client's advice to set the PLATFORM key
/// names in the credential store: Studio's Run reads those names nowhere, so that advice sends the
/// operator to file something that changes nothing here.
fn no_backtest_key_message(addr: &str) -> String {
    format!(
        "the backtesthub at {addr} REQUIRES authentication, and this Studio holds no backtest key. \
         Studio's Run, Sweep and Walk-Forward sign with the datahub CONTROL key, which this process \
         reads from the `{BACKTEST_KEY_NAME}` row of THIS PC's settings database and from nowhere \
         else: there is no key file and no environment variable for it. `just studio` files it \
         there from the server box on every start (the value travels on a pipe and is never \
         printed); by hand, `vike-cli secrets set {BACKTEST_KEY_NAME}` takes the value on stdin, \
         never on the command line."
    )
}

/// `StrategySpec` → [`WireSpec`]. A native or plugin strategy's `Params` are serialized to TOML
/// TEXT (`params_toml`) — the exact text `StrategySpec::native_from_toml_str` parses back at the
/// server boundary, so a local and a remote native run resolve bit-identical params.
pub(crate) fn to_wire_spec(spec: &StrategySpec) -> Result<WireSpec, RunError> {
    match spec {
        StrategySpec::Rhai(src) => Ok(WireSpec::Rhai(src.clone())),
        StrategySpec::Native { name, params } => {
            let params_toml = toml::to_string(params).map_err(|e| {
                RunError::Strategy(format!("serialize native params to TOML failed: {e}"))
            })?;
            Ok(WireSpec::Native { name: name.clone(), params_toml })
        }
        StrategySpec::Plugin { name, sha, params } => {
            let params_toml = toml::to_string(params).map_err(|e| {
                RunError::Strategy(format!("serialize plugin params to TOML failed: {e}"))
            })?;
            Ok(WireSpec::Plugin { name: name.clone(), sha: sha.clone(), params_toml })
        }
    }
}

/// `DataSlice` → [`WireSlice`], decomposing the `TsRange` into `start`/`end` — the inverse of the
/// server's `to_data_slice`.
pub(crate) fn to_wire_slice(slice: &DataSlice) -> WireSlice {
    WireSlice {
        venue: slice.venue.clone(),
        symbols: slice.symbols.clone(),
        interval: slice.interval.clone(),
        start: slice.range.start,
        end: slice.range.end,
        kind: match slice.kind {
            SliceKind::Bars => WireSliceKind::Bars,
            SliceKind::Ticks => WireSliceKind::Ticks,
        },
    }
}

/// [`WireRunResult`] → `BacktestResult`: fill the rendered fields the wire carries and leave the rest
/// (the growing-superset fields the Studio never renders) at their `Default`. `WireTrade::to_trade`
/// reconstructs each closed trade losslessly.
pub(crate) fn to_backtest_result(wire: WireRunResult) -> BacktestResult {
    let WireRunResult {
        equity_curve,
        equity_ts,
        final_equity,
        n_trades,
        per_symbol_pnl,
        trades,
        stale_deferrals,
        session_deferrals,
        // The cost-model STAMP does not survive this mapping, and it cannot: `BacktestResult` has
        // no field for it. The Studio GUI therefore still renders an unstamped result — a
        // DECLARED gap rather than an oversight, since the stamp rides the wire answer that a
        // JSON/CLI reader sees. Carrying it into the GUI means a home for it on the engine type.
        cost_model: _,
    } = wire;
    BacktestResult {
        equity_curve,
        equity_ts,
        final_equity,
        n_trades,
        per_symbol_pnl,
        trades: trades.iter().map(WireTrade::to_trade).collect(),
        stale_deferrals,
        session_deferrals,
        ..Default::default()
    }
}

/// [`WireParamscanEntry`] → `ParamscanEntry`.
fn to_sweep_entry(entry: WireParamscanEntry) -> ParamscanEntry {
    ParamscanEntry { overrides: entry.overrides, result: to_backtest_result(entry.result) }
}

/// [`WireParamscanResult`] → `StudioParamscan`: rebuild each ranked entry and map a `None` `dsr`/`pbo` back to
/// `NaN` (the "not assessable" sentinel the results pane already handles) — the inverse of the
/// server's `finite_or_none`.
pub(crate) fn to_studio_sweep(wire: WireParamscanResult) -> StudioParamscan {
    let WireParamscanResult { entries, dsr, pbo, best_index, cost_model: _ } = wire;
    StudioParamscan {
        entries: entries.into_iter().map(to_sweep_entry).collect(),
        dsr: dsr.unwrap_or(f64::NAN),
        pbo: pbo.unwrap_or(f64::NAN),
        best_index,
    }
}

/// [`WireWfWindow`] → `WfWindow`: the window's range and return, and the `ParamValue`s its search
/// chose, moved across as they crossed the wire.
///
/// Takes the window BY VALUE. [`WireWfWindow`] stopped being `Copy` when it grew `chosen_params`
/// (an owned `Vec`), and [`to_walkforward_report`] already owns the whole answer — so a move costs
/// nothing here, where a borrow would force a clone of every chosen value.
fn to_wf_window(window: WireWfWindow) -> WfWindow {
    let WireWfWindow { test_range, oos_return, chosen_params } = window;
    WfWindow { test_range, oos_return, chosen_params }
}

/// [`WireWalkforwardResult`] → `WalkForwardReport`: a full mirror (every field crosses the wire, so
/// no `Default` fill is needed), a window's chosen values included, by [`to_wf_window`].
pub(crate) fn to_walkforward_report(wire: WireWalkforwardResult) -> WalkForwardReport {
    let WireWalkforwardResult {
        windows,
        oos_equity_curve,
        oos_return,
        oos_sharpe,
        wf_consistency,
        cost_model: _,
    } = wire;
    WalkForwardReport {
        windows: windows.into_iter().map(to_wf_window).collect(),
        oos_equity_curve,
        oos_return,
        oos_sharpe,
        wf_consistency,
        // The Studio's walk-forward runs on the dollar ledger only (its picker refuses a foreign
        // quote), so there is never a currency to carry back.
        account_ccy: None,
    }
}

/// The server stringifies a run failure kind-first (`WireRunError::to_error_string` →
/// `"{kind}: {message}"`), so recover the `kind` prefix and rebuild the matching [`RunError`] variant —
/// a remote failure then renders IDENTICALLY to the same failure run locally. Anything without a known
/// kind prefix (a transport / protocol-desync line) is surfaced verbatim as [`RunError::Data`].
fn parse_wire_run_error(s: String) -> RunError {
    if let Some((kind, rest)) = s.split_once(": ") {
        match kind {
            "compile" => return RunError::Compile(rest.to_string()),
            "data" => return RunError::Data(rest.to_string()),
            "strategy" => return RunError::Strategy(rest.to_string()),
            _ => {}
        }
    }
    RunError::Data(s)
}

#[path = "remote_tests.rs"]
#[cfg(test)]
mod remote_tests;

// ---- the NAMED RUN (docs/decisions/0064-a-named-run-carries-no-source.md) -----------------------

/// The default COMPUTE-daemon address — `vike-backend backtesthub --addr`, which is a DIFFERENT
/// process from the datahub, on a different port. It is the default for BOTH offloading backends,
/// [`Backend::Remote`] and [`Backend::Named`]; the datahub's own address has no user in this module
/// and is `vike_config::DEFAULT_DATAHUB_ADDR` where it is needed.
///
/// ⚠ **The two ports are the single most likely thing to get wrong here, and the record says so:**
/// the `Run*` verbs moved to the backtesthub at 7880 (ruling 7), while the store verbs stayed on
/// the datahub at 7878. `docs/decisions/0064-a-named-run-carries-no-source.md`'s consequences name
/// the ADDRESS — not the key — as the genuine client-side gap, because the observe key the shell
/// already resolves authenticates against BOTH daemons under one domain separator.
pub(crate) const DEFAULT_BACKTESTHUB_ADDR: &str = vike_config::DEFAULT_BACKTEST_ADDR;

/// The refusal a SEARCH-shaped action gets on [`Backend::Named`], or `None` when it may proceed.
///
/// ⚠ **This is a BOUND, not a missing feature, and the sentence says so** —
/// `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 3 makes the single-point shape
/// the largest of that verb's bounds and a STRUCTURAL one: `NamedRunSpec` has no field a grid, a
/// trial count or a split count could occupy, which is what makes
/// `vike_backtest::harness::sweep`'s unchecked `product()` over client-supplied arrays unreachable
/// from an Observe credential. Growing a search dimension is that record's FIRST reopener.
///
/// Pure, so the Studio's whole named-backend refusal surface is testable with no socket.
pub(crate) fn named_backend_search_refusal(backend: &Backend, action: &str) -> Option<String> {
    matches!(backend, Backend::Named { .. }).then(|| {
        format!(
            "the Named backend runs ONE parameter set, so it cannot {action}: a grid, a trial count \
             and a split count are cost terms the request would be naming, and the verb is \
             read-scope precisely because it names none of them. Switch the Backend to Remote, \
             which signs with the CONTROL key `just studio` files in this PC's settings database \
             (the {BACKTEST_KEY_NAME} row), \
             or {action} from `vike-cli` on the daemon's own box."
        )
    })
}

/// **Run a strategy the SERVER already holds** — the Studio's second backend, and the only one an
/// OBSERVE credential can reach.
///
/// [`Backend::Remote`] needs the datahub CONTROL key, because every `Run*` verb it sends is
/// Control-scoped. ⚠ Until 2026-09-26 this doc said this process could never hold that key — the
/// observer posture `crates/vike-app-core/src/backend/backend_registry.rs` states for every dial the
/// desktop makes to the datahub. The owner then ruled (`docs/decisions/0083-the-runtime-plugin-
/// join-lands.md`, question 1, option (a)) that the desktop resolves it for Studio's COMPUTE dial
/// alone: it arrives as a [`BacktestKey`], a type no other dial accepts, and only when the launcher
/// handed it over. Without it, Remote against a keyed backend — which by
/// `vike_datahub_client::bind`'s `bind_decision` is every one bound off-box — is refused at
/// connect. This backend is the one that needs no Control key at all: it makes the narrow case not
/// need it, and it is still the right choice on a PC that was never given one.
///
/// What it costs, stated where somebody choosing it will read it: no script, no sweep, no
/// walk-forward, one symbol, one bounded window. [`to_named_run_spec`] refuses each of those BY
/// NAME rather than silently narrowing the run.
pub(crate) fn spawn_named_run_remote(
    addr: String,
    keys: Option<HubKeys>,
    spec: StrategySpec,
    slice: DataSlice,
) -> Receiver<RunOutcome> {
    spawn_outcome(move || run_named_remote(&addr, keys.as_ref(), &spec, &slice))
}

/// Dial the backtesthub as OBSERVE, ship the named run, and map the answer back onto a
/// `BacktestResult` — so `StudioState::poll` and the results pane are unchanged, exactly as the
/// remote-slice path is.
fn run_named_remote(
    addr: &str,
    keys: Option<&HubKeys>,
    spec: &StrategySpec,
    slice: &DataSlice,
) -> RunOutcome {
    let named = to_named_run_spec(spec, slice)?;
    let mut client = connect_observe(addr, keys)?;
    match client.run_named(&named).map_err(RunError::Data)? {
        // ⚠ `report_json` is DISCARDED here, and deliberately. The wire carries it beside the curve
        // so a client need not re-implement sharpe/return/max_dd — but the Studio's results pane
        // already derives its own metrics from a `BacktestResult`, and does so identically for
        // `Backend::Remote`, whose `RunSlice` answer carries no report at all. Taking the server's
        // numbers here and the pane's numbers there would make the two backends disagree about the
        // same run for no reason the operator could see. The field is the right shape for a
        // consumer that has no metrics of its own (`vike-cli`), which is why it is on the wire.
        NamedRunOutcome::Ran { result, .. } => Ok(to_backtest_result(*result)),
        // ⚠ NOT an error on the wire, and it must not become a silent empty result here either: the
        // operator of THAT box has not armed the lane, and the fix is on their side. The note names
        // the variable and the command — the teaching-refusal shape.
        NamedRunOutcome::NotArmed => Err(RunError::Data(NamedRoster::unarmed_note().to_string())),
        NamedRunOutcome::Refused(NamedRunRefusal::UnknownStrategy { known }) => {
            Err(RunError::Strategy(format!(
                "this backend does not hold a strategy called {:?}. It can run: {}. \
                 A named run resolves only strategies compiled into the SERVER — shipping your own \
                 source is the Control-scope path: the Remote backend here, signed with the key \
                 `just studio` files in this PC's settings database, or `backtest --script` from \
                 `vike-cli`.",
                named.strategy,
                known.join(", ")
            )))
        }
        NamedRunOutcome::Refused(NamedRunRefusal::NoSlot { limit }) => {
            Err(RunError::Data(format!(
                "this backend is already running its maximum of {limit} named runs and REFUSES rather \
             than queueing — a queued run is a connection held open for an unbounded time. Try \
             again in a moment."
            )))
        }
    }
}

/// Ask the backtesthub which strategies it would run, and whether the lane is armed — the roster
/// a picker offers, which by 0064's decision 7 must be the roster the verb actually SERVES.
///
/// ⚠ Not `list_strategies`: that answers the daemon's SIMULATOR roster, which both over- and
/// under-states what a named run can resolve. See `Request::NamedStrategies`.
///
/// ⚠ **BLOCKING — call it from [`spawn_named_roster_remote`], never from a frame.** The verb itself
/// is trivial (a compile-time const plus a build-time generated roster, no store read, no engine),
/// and the first draft of this feature reasoned from exactly that and put the call on the egui
/// update loop. The cost that matters is not the VERB, it is the DIAL:
/// `vike_hub_proto::limits::CLIENT_CONNECT_TIMEOUT` (which `vike_datahub_client` dials with) is ten seconds PER RESOLVED ADDRESS, and its own module
/// doc records that name resolution above it is unbounded — so a stale tunnel or a dark IPv6 route
/// is a frozen GUI, not a slow one.
pub(crate) fn named_roster_remote(
    addr: &str,
    keys: Option<&HubKeys>,
) -> Result<NamedRoster, String> {
    connect_observe(addr, keys).map_err(|e| e.to_string())?.named_strategies()
}

/// [`named_roster_remote`] on a worker thread — the shape every other dial in this module takes,
/// and the one the UI must use.
pub(crate) fn spawn_named_roster_remote(
    addr: String,
    keys: Option<HubKeys>,
) -> Receiver<Result<NamedRoster, String>> {
    spawn_outcome(move || named_roster_remote(&addr, keys.as_ref()))
}

/// Dial as OBSERVE when a key is present, unauthenticated when it is not.
///
/// ⚠ The unauthenticated arm is not a fallback that hides a problem: a key-LESS backtesthub
/// authenticates nothing and serves the loopback socket (`docs/decisions/0050`), and a KEYED one
/// answers a connect-time error naming the keys. Both are legible.
fn connect_observe(addr: &str, keys: Option<&HubKeys>) -> Result<DatahubClient, RunError> {
    DatahubClient::connect_with_keys(addr, keys, Scope::Read).map_err(|e| {
        RunError::Data(format!(
            "connect to the backtesthub {addr} failed: {e} — the Run* verbs are served by \
             `vike-backend backtesthub --addr` (default {DEFAULT_BACKTESTHUB_ADDR}), NOT by the datahub"
        ))
    })
}

/// **`(StrategySpec, DataSlice)` → [`NamedRunSpec`], or the ONE bound that refused it, BY NAME.**
///
/// Pure, so the whole of this backend's refusal surface is testable with no socket and no store —
/// and every arm names the dimension it hit rather than narrowing the run to fit. That is
/// `docs/decisions/0062`'s decision 5 applied here: answering a different question than the one
/// asked, and reporting it as the answer, is the lie these bounds exist to avoid.
pub(crate) fn to_named_run_spec(
    spec: &StrategySpec,
    slice: &DataSlice,
) -> Result<NamedRunSpec, RunError> {
    let (name, params) = match spec {
        // ⚠ THE RECORD, in one arm. A named run has no field a script could occupy — this is not a
        // check that could be relaxed, it is the shape of `NamedRunSpec`.
        StrategySpec::Rhai(_) => {
            return Err(RunError::Strategy(
                "the Named backend runs strategies the SERVER already holds, so it carries no \
                 source — there is no field on the request a script could occupy. Switch the \
                 Strategy source to Native and pick a name off this backend's roster, or send \
                 the script through the Remote backend. Shipping source to a server is the \
                 Control-scope path, which Remote takes with the key `just studio` hands this \
                 process (docs/decisions/0064-a-named-run-carries-no-source.md is why a named run \
                 carries none)."
                    .to_string(),
            ));
        }
        StrategySpec::Native { name, params } => (name, params),
        // A plugin resolves through the LOADER (an already-built artifact named by sha), not
        // through this daemon's compiled-in roster (`vike_backtest::harness::STRATEGIES` plus the
        // operator's own `vike-user-strategies`) — the exact roster `Request::NamedStrategies`
        // enumerates. So a plugin's `name` cannot be looked up here any more than a Rhai script's
        // can, and for the same reason: the Named backend runs strategies the SERVER already
        // holds, and a runtime-loaded plugin is not one of them.
        StrategySpec::Plugin { .. } => {
            return Err(RunError::Strategy(
                "the Named backend runs strategies the SERVER already holds by NAME, and a \
                 runtime-loaded plugin is not on that roster — it is resolved by sha from an \
                 artifact this daemon was never handed. Switch the Strategy source to Native and \
                 pick a name off this backend's roster, or use the Remote backend once the plugin \
                 has been built."
                    .to_string(),
            ));
        }
    };
    if slice.kind != SliceKind::Bars {
        return Err(RunError::Data(
            "the Named backend runs the BAR lane only: its window ceiling is counted in BARS, so a \
             tick slice is a dimension that bound cannot see. Pick a bar slice, or use the Remote \
             backend."
                .to_string(),
        ));
    }
    let [symbol] = slice.symbols.as_slice() else {
        return Err(RunError::Data(format!(
            "the Named backend runs ONE symbol and this slice names {}. A symbol list is a cost \
             term the request would be naming, which is the thing this verb's classification rests \
             on not doing.",
            slice.symbols.len()
        )));
    };
    // ⚠ BOTH bounds required, and an absent one is the shape that means "the whole store"
    // everywhere else on this wire — which is precisely the unbounded window 0064's decision 3
    // found nothing bounding on the verbs that do carry it.
    let (Some(start), Some(end)) = (slice.range.start, slice.range.end) else {
        return Err(RunError::Data(
            "the Named backend needs BOTH ends of its window: an open bound means `the whole \
             store`, and an unbounded window is the cost term this verb may not carry. Set a From \
             and a To on the slice."
                .to_string(),
        ));
    };
    let params = named_params(params)?;
    let named = NamedRunSpec {
        strategy: name.clone(),
        params,
        venue: slice.venue.clone(),
        symbol: symbol.clone(),
        interval: slice.interval.clone(),
        start,
        end,
    };
    // The SAME validator the server runs at its own door, so the sentence an operator reads here is
    // the sentence the server would have answered with — and no frame is written for a request that
    // cannot be served.
    validate_named_run(&named).map_err(RunError::Data)?;
    Ok(named)
}

/// A native strategy's `Params` → the wire's [`NamedParam`] list.
///
/// ⚠ **A STRING param is refused here rather than dropped**, and the reserved source key gets its
/// own sentence. `NamedParam` has no text variant — that is the structural half of 0064's decision
/// 2 — so a string knob cannot cross this wire at all; silently omitting it would run a DIFFERENT
/// strategy configuration than the one on screen and report it as the answer.
fn named_params(params: &Params) -> Result<Vec<(String, NamedParam)>, RunError> {
    let mut out = Vec::with_capacity(params.len());
    for (key, value) in params {
        let param = match value {
            ParamValue::Int(i) => NamedParam::Int(*i),
            ParamValue::Float(x) => NamedParam::Num(*x),
            ParamValue::Bool(b) => NamedParam::Flag(*b),
            _ if key == vike_model::RESERVED_SRC_KEY => {
                return Err(RunError::Strategy(
                    "this strategy carries a `src` param, which is a SCRIPT rather than a knob. A \
                     named run carries no source. Use `vike-cli backtest run --script`, which \
                     is Control-scope for exactly this reason."
                        .to_string(),
                ));
            }
            other => {
                return Err(RunError::Strategy(format!(
                    "the param {key:?} is a {}, and a named run carries numbers and flags only — \
                     its carrier has no text variant. Dropping it silently would run a different \
                     configuration than the one on screen.",
                    other.type_str()
                )));
            }
        };
        out.push((key.clone(), param));
    }
    Ok(out)
}

#[path = "named_run_tests.rs"]
#[cfg(test)]
mod named_run_tests;
