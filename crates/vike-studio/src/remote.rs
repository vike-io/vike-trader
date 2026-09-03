//! PR-5 — the Studio run BACKEND: dispatch a Run/Sweep/Walk-Forward to a REMOTE `vike-datahub`
//! server over TCP instead of running it in-process.
//!
//! The contract that makes this a drop-in for the local path: every `spawn_*_remote` here returns the
//! EXACT SAME `Receiver<T>` its `vike_studio_core::spawn_*` twin returns (`Receiver<RunOutcome>` /
//! `Receiver<Result<StudioSweep, RunError>>` / `Receiver<Result<WalkForwardReport, RunError>>`), so
//! `StudioState::poll` folds a remote outcome in with ZERO changes and the results pane renders it
//! unchanged. A remote run maps the wire answer back onto the same `BacktestResult` / `StudioSweep` /
//! `WalkForwardReport` the local engine produces, and every failure — connect fault, server error, or
//! protocol desync — arrives as an `Err(RunError)` on that receiver (never a panic / `unwrap`).
//!
//! # The wire DTOs are the client crate's; the conversions live here
//!
//! The `Wire*` schema is defined ONCE in the light, DataFusion-free `vike-datahub-client`
//! (`wire_studio`). The server (`vike-datahub`, behind `serve-datafusion`) converts wire → engine and
//! runs `vike_studio_core::run::run_slice`; THIS module is the client-side mirror — engine → wire for
//! the request, wire → engine for the answer — reusing the client crate's own DTOs and
//! `WireTrade::{from_trade,to_trade}` rather than inventing a second schema. Native-strategy params
//! ride as their TOML TEXT (`WireSpec::Native.params_toml`), the same idiom the server parses back with
//! `StrategySpec::native_from_toml_str`, so a `toml::Value` never crosses the wire.

use std::sync::mpsc::Receiver;

use vike_backtest::walkforward::{WalkForwardReport, WfWindow};
use vike_backtest::BacktestResult;
use vike_datahub_client::{
    DatahubClient, WireRunResult, WireSlice, WireSliceKind, WireSpec, WireSweep, WireSweepEntry,
    WireSweepResult, WireTrade, WireWalkforward, WireWalkforwardResult, WireWfWindow,
};
use vike_studio_core::{
    spawn_outcome, DataSlice, RunError, RunOutcome, SliceKind, StrategySpec, StudioSweep,
    SweepEntry,
};

/// The default remote datahub address — the localhost port an SSH-tunnelled `vike-datahub` serves on
/// (`ssh -L 7878:localhost:7878 the CI box`), matching `vike-cli`'s own default and the deploy runbook.
pub const DEFAULT_REMOTE_ADDR: &str = "127.0.0.1:7878";

/// WHERE a Studio Run/Sweep/Walk-Forward executes.
///
/// [`Backend::Local`] is the default and byte-identical to the pre-PR-5 behavior — the run happens
/// in-process over the GUI's own `DataFusionHist`. [`Backend::Remote`] offloads it to a `vike-datahub`
/// server over TCP (the compute-to-data path): the strategy + slice DTOs go out, only the rendered
/// answer comes back, and the result is mapped to the SAME `BacktestResult` / `StudioSweep` /
/// `WalkForwardReport` the local path produces, so the results pane renders unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Backend {
    /// Run in-process over the local store (the default).
    #[default]
    Local,
    /// Offload the run to a `vike-datahub` server listening at `addr` (`host:port`).
    Remote {
        /// The datahub server address, e.g. [`DEFAULT_REMOTE_ADDR`].
        addr: String,
    },
}

/// The split-plane tick rule (2026-08-18): with a REMOTE store, a TICK slice runs ONLY on
/// [`Backend::Remote`]. A local tick replay would need `scan_book_updates`/`scan_depth` — the tick
/// TAPE — over the wire, which is exactly what the compute-to-data rule refuses (the RPC store
/// keeps both as read-only ERRORS, so the run could only fail after dialling anyway; see
/// `vike_datahub_client::RemoteHistStore`'s module doc). Returns the one-line refusal to show
/// instead of dispatching, `None` when the run may proceed. Pure — the UI guard around every
/// Backend dispatch in `StudioState`, and testable without a store or a socket.
///
/// Never refuses: a local store (any slice, any backend), a bar slice (its `load_bars` read is a
/// served RPC verb — small, not the tape), or the Remote backend itself.
pub fn remote_store_tick_refusal(
    store_is_remote: bool,
    kind: SliceKind,
    backend: &Backend,
) -> Option<&'static str> {
    (store_is_remote && kind == SliceKind::Ticks && matches!(backend, Backend::Local)).then_some(
        "tick slices over a remote store run remotely: switch Backend to Remote — a local run \
         would have to pull the raw tick tape over the wire",
    )
}

/// Spawn a REMOTE single-slice Run on a worker thread; the `RunOutcome` arrives on the returned
/// receiver — the remote twin of `vike_studio_core::spawn_run`, so `StudioState::run_rx` accepts it
/// unchanged.
pub fn spawn_run_remote(
    addr: String,
    spec: StrategySpec,
    slice: DataSlice,
) -> Receiver<RunOutcome> {
    spawn_outcome(move || run_slice_remote(&addr, &spec, &slice))
}

/// Spawn a REMOTE parameter sweep on a worker thread — the remote twin of
/// `vike_studio_core::spawn_sweep` (same `Receiver` type, so `StudioState::sweep_rx` accepts it).
pub fn spawn_sweep_remote(
    addr: String,
    spec: StrategySpec,
    slice: DataSlice,
    grid: Vec<(String, Vec<f64>)>,
) -> Receiver<Result<StudioSweep, RunError>> {
    spawn_outcome(move || run_sweep_remote(&addr, &spec, &slice, grid))
}

/// Spawn a REMOTE walk-forward validation on a worker thread — the remote twin of
/// `vike_studio_core::spawn_walkforward` (same `Receiver` type, so `StudioState::wf_rx` accepts it).
pub fn spawn_walkforward_remote(
    addr: String,
    spec: StrategySpec,
    slice: DataSlice,
    n_splits: usize,
) -> Receiver<Result<WalkForwardReport, RunError>> {
    spawn_outcome(move || run_walkforward_remote(&addr, &spec, &slice, n_splits))
}

/// Dial `addr`, ship the `RunSlice` request, and map the answer back onto a `BacktestResult`. Params
/// go out as `None` — the server then resolves every engine field from `EngineParams::default()`,
/// matching the local path's `EngineParams::default()`.
fn run_slice_remote(addr: &str, spec: &StrategySpec, slice: &DataSlice) -> RunOutcome {
    let wire_spec = to_wire_spec(spec)?;
    let mut client = connect(addr)?;
    let wire =
        client.run_slice(wire_spec, to_wire_slice(slice), None).map_err(parse_wire_run_error)?;
    Ok(to_backtest_result(wire))
}

/// Dial `addr`, ship the `RunSweep` grid, and map the ranked answer back onto a `StudioSweep`.
fn run_sweep_remote(
    addr: &str,
    spec: &StrategySpec,
    slice: &DataSlice,
    grid: Vec<(String, Vec<f64>)>,
) -> Result<StudioSweep, RunError> {
    let wire_spec = to_wire_spec(spec)?;
    let mut client = connect(addr)?;
    // Params go out as `None` — the server resolves every engine field from `EngineParams::default()`,
    // matching the local sweep path. (The proto carries an optional cost/cash override; the Studio GUI
    // does not expose one yet, so it keeps the default-params behavior.)
    let wire = client
        .run_sweep(wire_spec, to_wire_slice(slice), WireSweep { axes: grid }, None)
        .map_err(parse_wire_run_error)?;
    Ok(to_studio_sweep(wire))
}

/// Dial `addr`, ship the `RunWalkforward` split-count, and map the stitched answer back onto a
/// `WalkForwardReport`.
fn run_walkforward_remote(
    addr: &str,
    spec: &StrategySpec,
    slice: &DataSlice,
    n_splits: usize,
) -> Result<WalkForwardReport, RunError> {
    let wire_spec = to_wire_spec(spec)?;
    let mut client = connect(addr)?;
    // Params `None` — default engine params, matching the local walk-forward path (see the sweep note).
    let wire = client
        .run_walkforward(wire_spec, to_wire_slice(slice), WireWalkforward { n_splits }, None)
        .map_err(parse_wire_run_error)?;
    Ok(to_walkforward_report(wire))
}

/// Connect to the datahub server (which performs the version handshake). A transport fault or a
/// protocol-version mismatch becomes a [`RunError::Data`] naming the address, surfaced in the Studio's
/// existing error banner rather than an `unwrap`.
fn connect(addr: &str) -> Result<DatahubClient, RunError> {
    DatahubClient::connect(addr)
        .map_err(|e| RunError::Data(format!("connect to datahub {addr} failed: {e}")))
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

/// [`WireSweepEntry`] → `SweepEntry`.
fn to_sweep_entry(entry: WireSweepEntry) -> SweepEntry {
    SweepEntry { overrides: entry.overrides, result: to_backtest_result(entry.result) }
}

/// [`WireSweepResult`] → `StudioSweep`: rebuild each ranked entry and map a `None` `dsr`/`pbo` back to
/// `NaN` (the "not assessable" sentinel the results pane already handles) — the inverse of the
/// server's `finite_or_none`.
pub fn to_studio_sweep(wire: WireSweepResult) -> StudioSweep {
    let WireSweepResult { entries, dsr, pbo, best_index } = wire;
    StudioSweep {
        entries: entries.into_iter().map(to_sweep_entry).collect(),
        dsr: dsr.unwrap_or(f64::NAN),
        pbo: pbo.unwrap_or(f64::NAN),
        best_index,
    }
}

/// [`WireWfWindow`] → `WfWindow`.
fn to_wf_window(window: &WireWfWindow) -> WfWindow {
    WfWindow { test_range: window.test_range, oos_return: window.oos_return }
}

/// [`WireWalkforwardResult`] → `WalkForwardReport`: a full mirror (every field crosses the wire, so no
/// `Default` fill is needed).
pub fn to_walkforward_report(wire: WireWalkforwardResult) -> WalkForwardReport {
    let WireWalkforwardResult { windows, oos_equity_curve, oos_return, oos_sharpe, wf_consistency } =
        wire;
    WalkForwardReport {
        windows: windows.iter().map(to_wf_window).collect(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::TsRange;
    use vike_model::Trade;

    #[test]
    fn backend_defaults_to_local() {
        assert_eq!(Backend::default(), Backend::Local);
    }

    #[test]
    fn to_wire_spec_maps_rhai_verbatim() {
        let w = to_wire_spec(&StrategySpec::rhai("fn on_bar() {}")).unwrap();
        assert_eq!(w, WireSpec::Rhai("fn on_bar() {}".to_string()));
    }

    /// A native spec's params go out as TOML TEXT and round-trip through the SAME parser the server
    /// uses (`native_from_toml_str`), so a remote native run resolves the identical params table.
    #[test]
    fn to_wire_spec_serializes_native_params_to_toml_text() {
        let spec = StrategySpec::native(
            "buy_hold",
            vike_studio_core::params_from_rows(&[
                ("size".to_string(), "2".to_string()),
                ("symbol".to_string(), "BTCUSDT".to_string()),
            ]),
        );
        match to_wire_spec(&spec).unwrap() {
            WireSpec::Native { name, params_toml } => {
                assert_eq!(name, "buy_hold");
                let back = StrategySpec::native_from_toml_str("buy_hold", &params_toml).unwrap();
                assert_eq!(back, spec, "params must survive Value -> TOML text -> Value");
            }
            other => panic!("expected Native, got {other:?}"),
        }
    }

    #[test]
    fn to_wire_slice_maps_fields_and_kind() {
        let bars =
            DataSlice::bars("binance", "BTCUSDT", "1m", TsRange { start: Some(1), end: Some(9) });
        let w = to_wire_slice(&bars);
        assert_eq!(w.venue, "binance");
        assert_eq!(w.symbols, vec!["BTCUSDT".to_string()]);
        assert_eq!(w.interval, "1m");
        assert_eq!(w.start, Some(1));
        assert_eq!(w.end, Some(9));
        assert_eq!(w.kind, WireSliceKind::Bars);

        let ticks = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TsRange::all());
        assert_eq!(to_wire_slice(&ticks).kind, WireSliceKind::Ticks);
    }

    fn sample_trade() -> Trade {
        Trade {
            entry_price: 100.0,
            exit_price: 110.0,
            size: 1.0,
            pnl: 10.0,
            fees: 0.1,
            entry_ts: 1,
            exit_ts: 2,
            symbol: "BTCUSDT".to_string(),
            mae: 0.0,
            mfe: 0.0,
            is_long: true,
        }
    }

    #[test]
    fn to_backtest_result_reconstructs_rendered_fields() {
        let wire = WireRunResult {
            equity_curve: vec![1000.0, 1010.0],
            equity_ts: vec![1, 2],
            final_equity: 1010.0,
            n_trades: 1,
            per_symbol_pnl: vec![("BTCUSDT".to_string(), 10.0)],
            trades: vec![WireTrade::from_trade(&sample_trade())],
            stale_deferrals: 3,
            session_deferrals: 4,
        };
        let r = to_backtest_result(wire);
        assert_eq!(r.equity_curve, vec![1000.0, 1010.0]);
        assert_eq!(r.equity_ts, vec![1, 2]);
        assert_eq!(r.final_equity, 1010.0);
        assert_eq!(r.n_trades, 1);
        assert_eq!(r.per_symbol_pnl, vec![("BTCUSDT".to_string(), 10.0)]);
        assert_eq!(r.trades, vec![sample_trade()]);
        assert_eq!(r.stale_deferrals, 3);
        assert_eq!(r.session_deferrals, 4);
        // fields the wire never carries fall back to Default
        assert_eq!(r.warmup, 0);
        assert!(r.dropped.is_empty());
    }

    #[test]
    fn to_studio_sweep_maps_entries_and_none_scores_to_nan() {
        let wire = WireSweepResult {
            entries: vec![WireSweepEntry {
                overrides: vec![("fast".to_string(), 5.0)],
                result: WireRunResult { final_equity: 42.0, ..Default::default() },
            }],
            dsr: None,
            pbo: Some(0.25),
            best_index: 0,
        };
        let s = to_studio_sweep(wire);
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].overrides, vec![("fast".to_string(), 5.0)]);
        assert_eq!(s.entries[0].result.final_equity, 42.0);
        assert!(s.dsr.is_nan(), "a None dsr becomes NaN (not assessable)");
        assert_eq!(s.pbo, 0.25);
        assert_eq!(s.best_index, 0);
    }

    #[test]
    fn to_walkforward_report_is_a_full_mirror() {
        let wire = WireWalkforwardResult {
            windows: vec![WireWfWindow { test_range: (0, 100), oos_return: 0.05 }],
            oos_equity_curve: vec![10_000.0, 10_500.0],
            oos_return: 0.05,
            oos_sharpe: 1.2,
            wf_consistency: 1.0,
        };
        let r = to_walkforward_report(wire);
        assert_eq!(r.windows.len(), 1);
        assert_eq!(r.windows[0].test_range, (0, 100));
        assert_eq!(r.windows[0].oos_return, 0.05);
        assert_eq!(r.oos_equity_curve, vec![10_000.0, 10_500.0]);
        assert_eq!(r.oos_return, 0.05);
        assert_eq!(r.oos_sharpe, 1.2);
        assert_eq!(r.wf_consistency, 1.0);
    }

    #[test]
    fn parse_wire_run_error_classifies_the_kind_prefix() {
        match parse_wire_run_error("compile: bad".to_string()) {
            RunError::Compile(m) => assert_eq!(m, "bad"),
            other => panic!("expected Compile, got {other:?}"),
        }
        match parse_wire_run_error("data: no bars".to_string()) {
            RunError::Data(m) => assert_eq!(m, "no bars"),
            other => panic!("expected Data, got {other:?}"),
        }
        match parse_wire_run_error("strategy: nope".to_string()) {
            RunError::Strategy(m) => assert_eq!(m, "nope"),
            other => panic!("expected Strategy, got {other:?}"),
        }
        // an unrecognized prefix (a desync / transport line) is surfaced verbatim as Data
        match parse_wire_run_error("protocol desync: x".to_string()) {
            RunError::Data(m) => assert_eq!(m, "protocol desync: x"),
            other => panic!("expected Data, got {other:?}"),
        }
        match parse_wire_run_error("bare".to_string()) {
            RunError::Data(m) => assert_eq!(m, "bare"),
            other => panic!("expected Data, got {other:?}"),
        }
    }

    /// The split-plane tick rule, exhaustively: the ONE refused cell is (remote store, tick
    /// slice, Local backend); every other combination proceeds. `StudioState::start_run`/
    /// `start_sweep`/`start_walkforward` all guard on this function, so the pure truth table IS
    /// the UI behavior.
    #[test]
    fn remote_store_tick_refusal_refuses_exactly_the_local_tick_cell() {
        let remote = Backend::Remote { addr: DEFAULT_REMOTE_ADDR.to_string() };
        // the one refused cell — and the message tells the user the way out
        let msg = remote_store_tick_refusal(true, SliceKind::Ticks, &Backend::Local)
            .expect("remote store + tick slice + Local backend must refuse");
        assert!(msg.contains("switch Backend to Remote"), "names the fix: {msg}");

        // every other cell proceeds
        assert_eq!(remote_store_tick_refusal(true, SliceKind::Ticks, &remote), None);
        assert_eq!(remote_store_tick_refusal(true, SliceKind::Bars, &Backend::Local), None);
        assert_eq!(remote_store_tick_refusal(true, SliceKind::Bars, &remote), None);
        assert_eq!(remote_store_tick_refusal(false, SliceKind::Ticks, &Backend::Local), None);
        assert_eq!(remote_store_tick_refusal(false, SliceKind::Ticks, &remote), None);
        assert_eq!(remote_store_tick_refusal(false, SliceKind::Bars, &Backend::Local), None);
        assert_eq!(remote_store_tick_refusal(false, SliceKind::Bars, &remote), None);
    }
}
