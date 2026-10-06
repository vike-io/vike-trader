//! PR-5 — the Studio run BACKEND: dispatch a Run/Sweep/Walk-Forward to the REMOTE COMPUTE daemon
//! (`vike-backend backtest --addr`) over TCP instead of running it in-process.
//!
//! ⚠ This said `vike-datahub` until 2026-09-20 and the code agreed with it, which is the part that
//! mattered — see the tombstone where `DEFAULT_REMOTE_ADDR` stood. Ruling 7 moved every `Run*` verb
//! onto the compute plane; the data daemon serves the STORE reads a remote slice needs and refuses
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
//! (`wire_studio`). The server — `crates/vike-backtest/src/compute_server.rs`, mounted by
//! `vike-backend backtest --addr`; it was `vike-datahub` behind `serve-datafusion` until ruling 7 —
//! converts wire → engine and
//! runs `vike_studio_core::run::run_slice`; THIS module is the client-side mirror — engine → wire for
//! the request, wire → engine for the answer — reusing the client crate's own DTOs and
//! `WireTrade::{from_trade,to_trade}` rather than inventing a second schema. Native-strategy params
//! ride as their TOML TEXT (`WireSpec::Native.params_toml`), the same idiom the server parses back with
//! `StrategySpec::native_from_toml_str`, so a `toml::Value` never crosses the wire.

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
use vike_node_proto::auth::{NodeKeys, Scope};
use vike_studio_core::{
    DataSlice, ParamscanEntry, RunError, RunOutcome, SliceKind, StrategySpec, StudioParamscan,
    spawn_outcome,
};

// ⚠ **`DEFAULT_REMOTE_ADDR` stood here and is DELETED (2026-09-20).** It was
// `"127.0.0.1:7878"` — the DATA daemon — and it was a guaranteed refusal rather than a stale
// comment: ruling 7 moved every `Run*` verb onto the compute plane, and
// `crates/vike-datahub/src/server.rs`'s `handle_connection` consults
// `crates/vike-datahub-client/src/proto.rs`'s `plane_of` BEFORE the scope check, so a user who
// clicked "Remote" and pressed Run got a wrong-plane error from a daemon that has not served the
// verb since. The address field is editable, so what broke was the DEFAULT, not the path — typing
// 7880 always worked, which is why nothing reported it.
//
// It is not re-pointed under its own name because that would leave TWO names for one value:
// [`DEFAULT_COMPUTE_ADDR`] below is the address for BOTH offloading backends now, since the two
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
    /// Offload the run to the COMPUTE daemon listening at `addr` (`host:port`) —
    /// `vike-backend backtest --addr`, [`DEFAULT_COMPUTE_ADDR`].
    ///
    /// ⚠ **Not the datahub**, which this said until 2026-09-20: the three verbs this variant sends
    /// (`RunSlice`, `RunParamscan`, `RunWalkforward`) are all `Plane::Compute`, and the data daemon
    /// refuses them by plane. See the tombstone above the constant for what the wrong default cost.
    Remote {
        /// The COMPUTE daemon's address, e.g. [`DEFAULT_COMPUTE_ADDR`] — the same daemon
        /// [`Backend::Named`] dials.
        addr: String,
    },
    /// **Run a strategy the SERVER already holds** — the one backend an OBSERVE credential can
    /// reach (`docs/decisions/0064-a-named-run-carries-no-source.md`).
    ///
    /// ⚠ **The same DAEMON as [`Backend::Remote`], and a different REQUEST.** This doc said the two
    /// dialled different daemons until 2026-09-20 — "that variant dials the DATA daemon" — and that
    /// sentence was the whole bug: `Backend::Remote` sends `Plane::Compute` verbs, so it was never
    /// the data daemon's to answer, it just DEFAULTED to its address. Both variants dial
    /// `vike-backend backtest --addr` ([`DEFAULT_COMPUTE_ADDR`]) now.
    ///
    /// What still separates them is the CREDENTIAL and the shape of the ask: a named run is the one
    /// backend an OBSERVE key can reach. The addresses stay separate FIELDS because they are fields
    /// of two enum variants and cannot be otherwise, not because they may differ; a box that moves
    /// its compute daemon moves both.
    ///
    /// What it cannot do, and [`to_named_run_spec`] refuses each BY NAME: a script, a sweep, a
    /// walk-forward, more than one symbol, an open-ended window.
    Named {
        /// The COMPUTE daemon's address, e.g. [`DEFAULT_COMPUTE_ADDR`].
        addr: String,
    },
}

/// The compute daemon, always.
///
/// ⚠ There used to be a `Local` variant and it was this default: the run happened in-process over
/// the GUI's own store. It is GONE, on the owner's decision - every Studio run goes through the
/// compute daemon now. What that removed is not a second ENGINE (there never was one: the local
/// path called `vike_backtest::hist_replay::replay_ticks`, the same function the daemon reaches
/// through `studio_run_table`) but a second SURFACE - a path on which a run's inputs and answers
/// were never marshalled, beside one on which they always are, with nothing holding the two equal.
///
/// The cost is stated rather than hidden: a Studio with no daemon reachable now runs NOTHING. That
/// is a cliff, not a slowdown, and it is the price of the single surface.
impl Default for Backend {
    fn default() -> Self {
        Backend::Remote { addr: DEFAULT_COMPUTE_ADDR.to_string() }
    }
}

impl Backend {
    /// The DEFAULT Remote backend - `DEFAULT_COMPUTE_ADDR`, nothing typed.
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
        Self::Remote { addr: DEFAULT_COMPUTE_ADDR.to_string() }
    }

    /// The DEFAULT Named backend. Same daemon as [`Backend::remote_default`] —
    /// see [`Backend::Named`] for what differs, which is the credential and the ask, not the
    /// port. Same note as above: the UI's button does not call this.
    #[must_use]
    pub fn named_default() -> Self {
        Self::Named { addr: DEFAULT_COMPUTE_ADDR.to_string() }
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
// the desktop as narrow as the code can make it: one variable, one reader, one type no other dial
// accepts, one place it is signed, and that place refuses the data plane.

/// The environment variable Studio's compute key arrives in — read by [`compute_key_from_vars`]
/// and by nothing else in this workspace.
///
/// ⚠ **A name of its own, NOT the platform's `VIKE_DATAHUB_CONTROL_KEY`, and the difference IS the
/// reach limit.** The desktop hands its one environment sweep WHOLE to every datahub dial it
/// resolves — the store, the chart seed, the venue catalog, the named-run key — and every process
/// it starts inherits the same environment. Under the platform name, a single
/// `vike_node_proto::auth::node_keys_from_vars(env)` at any of those sites (or `vike-cli`'s
/// `datahub_keyring`, whose first rung is the environment) would pick the Write key up without
/// naming it. Under this name nothing but this module can see it.
/// `crates/vike-ops/tests/gui/studio_compute_key_reach_gate.rs` holds the name to one Rust reader.
pub const COMPUTE_KEY_ENV: &str = "VIKE_STUDIO_COMPUTE_KEY";

/// **Studio's compute key** — the datahub CONTROL key, held for ONE purpose: signing Studio's own
/// Run, Sweep and Walk-Forward to the COMPUTE daemon.
///
/// ⚠ **OPAQUE, and that is the reach limit made structural.** The key inside is private to this
/// module and is read in exactly one place, [`connect`], which signs it only through
/// `DatahubClient::connect_authed_on(.., Scope::Write, Plane::Compute)` — the constructor that
/// refuses, before sending a byte of `Auth`, a server whose pre-auth `Welcome` is not the compute
/// daemon's. There is no expression elsewhere that turns this into something another dial accepts:
/// handing it to `RemoteHistStore::with_keys`, a chart-seed or catalog dial, [`Backend::Named`]'s
/// observe dial or the trading-node observer is a TYPE error rather than a review comment.
/// `crates/vike-ops/tests/gui/studio_compute_key_reach_gate.rs` holds the lexical half the compiler
/// cannot: one read of the field, one construction, no derive that could serialise it.
///
/// Its `Debug` prints presence only and it has no `Display`, so a `{:?}` of a `StudioState` or a
/// panic message cannot carry it.
#[derive(Clone)]
pub struct ComputeKey {
    write_only: NodeKeys,
}

impl std::fmt::Debug for ComputeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ComputeKey(<redacted>)")
    }
}

/// Build Studio's [`ComputeKey`] from an already-swept environment map — [`COMPUTE_KEY_ENV`] and
/// nothing else. A pure map lookup: the BINARY owns the one `std::env::vars()` sweep, per this
/// workspace's "libraries take configuration as parameters" rule, exactly as
/// `vike_strategy_builder::builder::keys_from_vars` does for the builder's key.
///
/// - Only the WRITE slot is filled, so the key cannot be presented at `Scope::Read` even by a
///   caller holding the inner value — which none outside this module can.
/// - A blank or whitespace value is NO key (an empty key signs a mac the daemon refuses, which
///   would trade "no key, here is where it comes from" for `bad mac`).
/// - The platform name `VIKE_DATAHUB_CONTROL_KEY` is NOT consulted, whatever the map holds, and
///   neither is any store: see [`COMPUTE_KEY_ENV`] for why the name is the limit.
pub fn compute_key_from_vars(vars: &HashMap<String, String>) -> Option<ComputeKey> {
    let key = vars.get(COMPUTE_KEY_ENV)?.trim();
    if key.is_empty() {
        return None;
    }
    Some(ComputeKey { write_only: NodeKeys::new(Vec::new(), key.as_bytes().to_vec()) })
}

/// Spawn a REMOTE single-slice Run on a worker thread; the `RunOutcome` arrives on the returned
/// receiver — the remote twin of `vike_studio_core::spawn_run`, so `StudioState::run_rx` accepts it
/// unchanged.
///
/// `key` is Studio's [`ComputeKey`] when the launcher handed one over, and `None` otherwise — see
/// [`connect`] for what each does.
pub fn spawn_run_remote(
    addr: String,
    key: Option<ComputeKey>,
    spec: StrategySpec,
    slice: DataSlice,
) -> Receiver<RunOutcome> {
    spawn_outcome(move || run_slice_remote(&addr, key.as_ref(), &spec, &slice))
}

/// Spawn a REMOTE parameter sweep on a worker thread — the remote twin of
/// `vike_studio_core::spawn_paramscan` (same `Receiver` type, so `StudioState::sweep_rx` accepts it).
pub fn spawn_sweep_remote(
    addr: String,
    key: Option<ComputeKey>,
    spec: StrategySpec,
    slice: DataSlice,
    grid: Vec<(String, Vec<f64>)>,
) -> Receiver<Result<StudioParamscan, RunError>> {
    spawn_outcome(move || run_sweep_remote(&addr, key.as_ref(), &spec, &slice, grid))
}

/// Spawn a REMOTE walk-forward validation on a worker thread — the remote twin of
/// `vike_studio_core::spawn_walkforward` (same `Receiver` type, so `StudioState::wf_rx` accepts it).
pub fn spawn_walkforward_remote(
    addr: String,
    key: Option<ComputeKey>,
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
    key: Option<&ComputeKey>,
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
    key: Option<&ComputeKey>,
    spec: &StrategySpec,
    slice: &DataSlice,
    grid: Vec<(String, Vec<f64>)>,
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
    key: Option<&ComputeKey>,
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

/// Connect to the COMPUTE daemon for a Run, Sweep or Walk-Forward. A transport fault, a
/// protocol-version mismatch or a refusal becomes a [`RunError::Data`] naming the address, surfaced
/// in the Studio's existing error banner rather than an `unwrap`.
///
/// - **With Studio's [`ComputeKey`]** — `DatahubClient::connect_authed_on` at `Scope::Write` (every
///   `Run*` verb is Control-scoped: `vike_datahub_client::proto`'s `required_scope`), and ONLY to a
///   server whose pre-auth `Welcome` names the COMPUTE plane. ⚠ This is the ONE place the key is
///   read, and the plane is the reason it is this constructor: one node pair authenticates both
///   daemons, so the same key signed toward the datahub would SUCCEED and open a Write session that
///   carries `Backfill` and `DeleteSeries` — which is exactly what the address regression
///   `crates/vike-desktop/src/main.rs` records (Studio's compute address overwritten with the
///   datahub's) or a hand-typed 7878 in the Backend field would do. Refused there, nothing but the
///   `Hello` has been sent. Against a KEY-LESS compute daemon it degrades to an unauthenticated
///   connect and nothing is signed at all.
/// - **Without one** — the plain connect, unchanged against a key-less daemon. A KEYED one refuses
///   at connect, and the message then says where this key comes from: the datahub client's own
///   wording points at the credential store, which Studio's Run never reads.
fn connect(addr: &str, key: Option<&ComputeKey>) -> Result<DatahubClient, RunError> {
    let dialled = match key {
        Some(key) => {
            DatahubClient::connect_authed_on(addr, &key.write_only, Scope::Write, Plane::Compute)
        }
        None => DatahubClient::connect(addr),
    };
    dialled.map_err(|e| {
        if key.is_none() && e.kind() == std::io::ErrorKind::PermissionDenied {
            return RunError::Data(no_compute_key_message(addr));
        }
        RunError::Data(format!("connect to the compute daemon at {addr} failed: {e}"))
    })
}

/// What a keyed compute daemon's refusal means when this Studio holds no [`ComputeKey`] — pure, so
/// the sentence an operator reads is unit-tested.
///
/// It keeps `REQUIRES authentication` (the phrase every earlier banner used, so a search for it
/// still lands here) and names the ONE place Studio's key comes from. It deliberately does not
/// repeat the datahub client's advice to set keys in the credential store: Studio's Run reads no
/// store, so that advice sends the operator to change something that changes nothing.
fn no_compute_key_message(addr: &str) -> String {
    format!(
        "the compute daemon at {addr} REQUIRES authentication, and this Studio holds no compute key. \
         Studio's Run, Sweep and Walk-Forward sign with the datahub CONTROL key, which this process \
         reads from {COMPUTE_KEY_ENV} and from nowhere else — `just studio` reads it off the server \
         box and hands it over. No credential store is consulted for it, so a key written into one \
         changes nothing here."
    )
}

/// `StrategySpec` → [`WireSpec`]. A native strategy's `toml::Value` params are serialized to TOML TEXT
/// (`params_toml`) — the exact text `StrategySpec::native_from_toml_str` parses back at the server
/// boundary, so a local and a remote native run resolve bit-identical params.
pub fn to_wire_spec(spec: &StrategySpec) -> Result<WireSpec, RunError> {
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
pub fn to_wire_slice(slice: &DataSlice) -> WireSlice {
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
pub fn to_backtest_result(wire: WireRunResult) -> BacktestResult {
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
pub fn to_studio_sweep(wire: WireParamscanResult) -> StudioParamscan {
    let WireParamscanResult { entries, dsr, pbo, best_index, cost_model: _ } = wire;
    StudioParamscan {
        entries: entries.into_iter().map(to_sweep_entry).collect(),
        dsr: dsr.unwrap_or(f64::NAN),
        pbo: pbo.unwrap_or(f64::NAN),
        best_index,
    }
}

/// [`WireWfWindow`] → `WfWindow`, parsing each chosen value's TOML text back into the
/// `toml::Value` the server rendered it from.
///
/// Takes the window BY VALUE. [`WireWfWindow`] stopped being `Copy` when it grew `chosen_params`
/// (an owned `Vec`), and [`to_walkforward_report`] already owns the whole answer — so a move costs
/// nothing here, where a borrow would force a clone of every rendering.
fn to_wf_window(window: WireWfWindow) -> WfWindow {
    let WireWfWindow { test_range, oos_return, chosen_params } = window;
    WfWindow {
        test_range,
        oos_return,
        chosen_params: chosen_params.map(|chosen| {
            chosen.into_iter().map(|(key, rendered)| (key, parse_toml_value(&rendered))).collect()
        }),
    }
}

/// One rendered TOML value — `vike_studio_core::wire_run`'s `to_wire_wf_window` wrote it with
/// `toml::Value`'s `Display` — parsed back into the value it came from.
///
/// TOTAL by construction: a rendering that will not parse back is preserved as a
/// `toml::Value::String` of the raw text rather than dropped, so a window's `(key, value)` pair
/// survives even when its TYPE does not. That fallback is not decoration — [`to_wf_window`] has no
/// error channel (it maps one field of an already-decoded answer), and the peer is a socket, so
/// "the server rendered these with `Display`" is a belief about the other end rather than anything
/// the frame proves.
///
/// ⚠ Parsed as a one-key DOCUMENT (`v = <text>`) rather than `rendered.parse::<toml::Value>()`, and
/// the reason is a distinction this workspace has already written down once:
/// `crates/vike-studio-core/src/spec.rs`'s `parse_scalar` uses the same spelling and says why —
/// a bare `2.5` is not a valid TOML DOCUMENT, and `FromStr` for `Value` has been the document parse
/// in the versions that comment was written against. The document form means the same thing under
/// either flavour, so it cannot quietly change meaning under a `toml` bump.
fn parse_toml_value(rendered: &str) -> toml::Value {
    let document = format!("v = {rendered}");
    toml::from_str::<toml::Table>(&document)
        .ok()
        .and_then(|mut table| table.remove("v"))
        .unwrap_or_else(|| toml::Value::String(rendered.to_string()))
}

/// [`WireWalkforwardResult`] → `WalkForwardReport`: a full mirror (every field crosses the wire, so
/// no `Default` fill is needed).
///
/// "Full" is the field ROSTER, not the field TYPES — a window's chosen values crossed as TOML text
/// and are parsed back here, one value at a time, by [`parse_toml_value`].
pub fn to_walkforward_report(wire: WireWalkforwardResult) -> WalkForwardReport {
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

/// The default COMPUTE-daemon address — `vike-backend backtest --addr`, which is a DIFFERENT
/// process from the datahub, on a different port. It is the default for BOTH offloading backends,
/// [`Backend::Remote`] and [`Backend::Named`]; the datahub's own address has no user in this module
/// and is `vike_config::DEFAULT_DATAHUB_ADDR` where it is needed.
///
/// ⚠ **The two ports are the single most likely thing to get wrong here, and the record says so:**
/// the `Run*` verbs moved to the compute daemon at 7880 (ruling 7), while the store verbs stayed on
/// the datahub at 7878. `docs/decisions/0064-a-named-run-carries-no-source.md`'s consequences name
/// the ADDRESS — not the key — as the genuine client-side gap, because the observe key the shell
/// already resolves authenticates against BOTH daemons under one domain separator.
pub const DEFAULT_COMPUTE_ADDR: &str = vike_config::DEFAULT_BACKTEST_ADDR;

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
pub fn named_backend_search_refusal(backend: &Backend, action: &str) -> Option<String> {
    matches!(backend, Backend::Named { .. }).then(|| {
        format!(
            "the Named backend runs ONE parameter set, so it cannot {action}: a grid, a trial count \
             and a split count are cost terms the request would be naming, and the verb is \
             read-scope precisely because it names none of them. Switch the Backend to Remote, \
             which signs with the CONTROL key `just studio` hands this process ({COMPUTE_KEY_ENV}), \
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
/// alone: it arrives as a [`ComputeKey`], a type no other dial accepts, and only when the launcher
/// handed it over. Without it, Remote against a keyed backend — which by
/// `vike_datahub_client::bind`'s `bind_decision` is every one bound off-box — is refused at
/// connect. This backend is the one that needs no Control key at all: it makes the narrow case not
/// need it, and it is still the right choice on a PC that was never given one.
///
/// What it costs, stated where somebody choosing it will read it: no script, no sweep, no
/// walk-forward, one symbol, one bounded window. [`to_named_run_spec`] refuses each of those BY
/// NAME rather than silently narrowing the run.
pub fn spawn_named_run_remote(
    addr: String,
    keys: Option<NodeKeys>,
    spec: StrategySpec,
    slice: DataSlice,
) -> Receiver<RunOutcome> {
    spawn_outcome(move || run_named_remote(&addr, keys.as_ref(), &spec, &slice))
}

/// Dial the COMPUTE daemon as OBSERVE, ship the named run, and map the answer back onto a
/// `BacktestResult` — so `StudioState::poll` and the results pane are unchanged, exactly as the
/// remote-slice path is.
fn run_named_remote(
    addr: &str,
    keys: Option<&NodeKeys>,
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
                 `just studio` hands this process, or `backtest --script` from `vike-cli`.",
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

/// Ask the compute daemon which strategies it would run, and whether the lane is armed — the roster
/// a picker offers, which by 0064's decision 7 must be the roster the verb actually SERVES.
///
/// ⚠ Not `list_strategies`: that answers the daemon's SIMULATOR roster, which both over- and
/// under-states what a named run can resolve. See `Request::NamedStrategies`.
///
/// ⚠ **BLOCKING — call it from [`spawn_named_roster_remote`], never from a frame.** The verb itself
/// is trivial (a compile-time const plus a build-time generated roster, no store read, no engine),
/// and the first draft of this feature reasoned from exactly that and put the call on the egui
/// update loop. The cost that matters is not the VERB, it is the DIAL:
/// `vike_datahub_client`'s `CONNECT_TIMEOUT` is ten seconds PER RESOLVED ADDRESS, and its own module
/// doc records that name resolution above it is unbounded — so a stale tunnel or a dark IPv6 route
/// is a frozen GUI, not a slow one.
pub fn named_roster_remote(addr: &str, keys: Option<&NodeKeys>) -> Result<NamedRoster, String> {
    connect_observe(addr, keys).map_err(|e| e.to_string())?.named_strategies()
}

/// [`named_roster_remote`] on a worker thread — the shape every other dial in this module takes,
/// and the one the UI must use.
pub fn spawn_named_roster_remote(
    addr: String,
    keys: Option<NodeKeys>,
) -> Receiver<Result<NamedRoster, String>> {
    spawn_outcome(move || named_roster_remote(&addr, keys.as_ref()))
}

/// Dial as OBSERVE when a key is present, unauthenticated when it is not.
///
/// ⚠ The unauthenticated arm is not a fallback that hides a problem: a key-LESS compute daemon
/// authenticates nothing and serves the loopback socket (`docs/decisions/0050`), and a KEYED one
/// answers a connect-time error naming the keys. Both are legible.
fn connect_observe(addr: &str, keys: Option<&NodeKeys>) -> Result<DatahubClient, RunError> {
    match keys {
        Some(k) => DatahubClient::connect_authed(addr, k, Scope::Read),
        None => DatahubClient::connect(addr),
    }
    .map_err(|e| {
        RunError::Data(format!(
            "connect to the compute daemon {addr} failed: {e} — the Run* verbs are served by \
             `vike-backend backtest --addr` (default {DEFAULT_COMPUTE_ADDR}), NOT by the datahub"
        ))
    })
}

/// **`(StrategySpec, DataSlice)` → [`NamedRunSpec`], or the ONE bound that refused it, BY NAME.**
///
/// Pure, so the whole of this backend's refusal surface is testable with no socket and no store —
/// and every arm names the dimension it hit rather than narrowing the run to fit. That is
/// `docs/decisions/0062`'s decision 5 applied here: answering a different question than the one
/// asked, and reporting it as the answer, is the lie these bounds exist to avoid.
pub fn to_named_run_spec(spec: &StrategySpec, slice: &DataSlice) -> Result<NamedRunSpec, RunError> {
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
    let params = named_params_from_toml(params)?;
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

/// A native strategy's `toml::Value` params → the wire's [`NamedParam`] list.
///
/// ⚠ **A STRING param is refused here rather than dropped**, and the reserved source key gets its
/// own sentence. `NamedParam` has no text variant — that is the structural half of 0064's decision
/// 2 — so a string knob cannot cross this wire at all; silently omitting it would run a DIFFERENT
/// strategy configuration than the one on screen and report it as the answer.
fn named_params_from_toml(params: &toml::Value) -> Result<Vec<(String, NamedParam)>, RunError> {
    let Some(table) = params.as_table() else { return Ok(Vec::new()) };
    let mut out = Vec::with_capacity(table.len());
    for (key, value) in table {
        let param = match value {
            toml::Value::Integer(i) => NamedParam::Int(*i),
            toml::Value::Float(x) => NamedParam::Num(*x),
            toml::Value::Boolean(b) => NamedParam::Flag(*b),
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
