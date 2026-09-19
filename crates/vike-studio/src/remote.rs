//! PR-5 — the Studio run BACKEND: dispatch a Run/Sweep/Walk-Forward to a REMOTE `vike-datahub`
//! server over TCP instead of running it in-process.
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
//! (`wire_studio`). The server (`vike-datahub`, behind `serve-datafusion`) converts wire → engine and
//! runs `vike_studio_core::run::run_slice`; THIS module is the client-side mirror — engine → wire for
//! the request, wire → engine for the answer — reusing the client crate's own DTOs and
//! `WireTrade::{from_trade,to_trade}` rather than inventing a second schema. Native-strategy params
//! ride as their TOML TEXT (`WireSpec::Native.params_toml`), the same idiom the server parses back with
//! `StrategySpec::native_from_toml_str`, so a `toml::Value` never crosses the wire.

use std::sync::mpsc::Receiver;

use vike_backtest::BacktestResult;
use vike_backtest::walkforward::{WalkForwardReport, WfWindow};
use vike_datahub_client::named_run::{
    NamedParam, NamedRoster, NamedRunOutcome, NamedRunRefusal, NamedRunSpec, validate_named_run,
};
use vike_datahub_client::node_auth::{NodeKeys, Scope};
use vike_datahub_client::{
    DatahubClient, WireParamscan, WireParamscanEntry, WireParamscanResult, WireRunResult,
    WireSlice, WireSliceKind, WireSpec, WireTrade, WireWalkforward, WireWalkforwardResult,
    WireWfWindow,
};
use vike_studio_core::{
    DataSlice, ParamscanEntry, RunError, RunOutcome, SliceKind, StrategySpec, StudioParamscan,
    spawn_outcome,
};

/// The default remote datahub address — the localhost port an SSH-tunnelled `vike-datahub` serves on
/// (`ssh -L 7878:localhost:7878 the CI box`), matching `vike-cli`'s own default and the deploy runbook.
pub const DEFAULT_REMOTE_ADDR: &str = "127.0.0.1:7878";

/// WHERE a Studio Run/Sweep/Walk-Forward executes.
///
/// [`Backend::Local`] is the default and byte-identical to the pre-PR-5 behavior — the run happens
/// in-process over the GUI's own `DataFusionHist`. [`Backend::Remote`] offloads it to a `vike-datahub`
/// server over TCP (the compute-to-data path): the strategy + slice DTOs go out, only the rendered
/// answer comes back, and the result is mapped to the SAME `BacktestResult` / `StudioParamscan` /
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
    /// **Run a strategy the SERVER already holds** — the one backend an OBSERVE credential can
    /// reach (`docs/decisions/0064-a-named-run-carries-no-source.md`).
    ///
    /// ⚠ **A DIFFERENT DAEMON from [`Backend::Remote`], not a different mode of it.** That variant
    /// dials the DATA daemon; this one dials the COMPUTE daemon (`vike-backend backtest --addr`,
    /// [`DEFAULT_COMPUTE_ADDR`]), which is where every `Run*` verb has been served since ruling 7.
    /// The two addresses are kept as separate fields for that reason — one box commonly runs both,
    /// on different ports, and a single shared field would silently point one of them wrong.
    ///
    /// What it cannot do, and [`to_named_run_spec`] refuses each BY NAME: a script, a sweep, a
    /// walk-forward, more than one symbol, an open-ended window.
    Named {
        /// The COMPUTE daemon's address, e.g. [`DEFAULT_COMPUTE_ADDR`].
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
/// `vike_studio_core::spawn_paramscan` (same `Receiver` type, so `StudioState::sweep_rx` accepts it).
pub fn spawn_sweep_remote(
    addr: String,
    spec: StrategySpec,
    slice: DataSlice,
    grid: Vec<(String, Vec<f64>)>,
) -> Receiver<Result<StudioParamscan, RunError>> {
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

/// Dial `addr`, ship the `RunSweep` grid, and map the ranked answer back onto a `StudioParamscan`.
fn run_sweep_remote(
    addr: &str,
    spec: &StrategySpec,
    slice: &DataSlice,
    grid: Vec<(String, Vec<f64>)>,
) -> Result<StudioParamscan, RunError> {
    let wire_spec = to_wire_spec(spec)?;
    let mut client = connect(addr)?;
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
    spec: &StrategySpec,
    slice: &DataSlice,
    n_splits: usize,
) -> Result<WalkForwardReport, RunError> {
    let wire_spec = to_wire_spec(spec)?;
    let mut client = connect(addr)?;
    // Params `None` — default engine params, matching the local walk-forward path (see the sweep note).
    let wire = client
        .run_walkforward(wire_spec, to_wire_slice(slice), WireWalkforward::fixed(n_splits), None)
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
            cost_model: None,
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
        let wire = WireParamscanResult {
            entries: vec![WireParamscanEntry {
                overrides: vec![("fast".to_string(), 5.0)],
                result: WireRunResult { final_equity: 42.0, ..Default::default() },
            }],
            dsr: None,
            pbo: Some(0.25),
            best_index: 0,
            cost_model: None,
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
            windows: vec![
                WireWfWindow { test_range: (0, 100), oos_return: 0.05, chosen_params: None },
                WireWfWindow {
                    test_range: (100, 200),
                    oos_return: -0.02,
                    chosen_params: Some(vec![("fast".to_string(), "8".to_string())]),
                },
            ],
            oos_equity_curve: vec![10_000.0, 10_500.0],
            oos_return: 0.05,
            oos_sharpe: 1.2,
            wf_consistency: 1.0,
            cost_model: None,
        };
        let r = to_walkforward_report(wire);
        assert_eq!(r.windows.len(), 2);
        assert_eq!(r.windows[0].test_range, (0, 100));
        assert_eq!(r.windows[0].oos_return, 0.05);
        // the fixed-parameter window records no choice; the searched one carries its winner back
        // as a real `toml::Value`, not as the text it crossed the wire in
        assert_eq!(r.windows[0].chosen_params, None);
        assert_eq!(
            r.windows[1].chosen_params,
            Some(vec![("fast".to_string(), toml::Value::Integer(8))])
        );
        assert_eq!(r.oos_equity_curve, vec![10_000.0, 10_500.0]);
        assert_eq!(r.oos_return, 0.05);
        assert_eq!(r.oos_sharpe, 1.2);
        assert_eq!(r.wf_consistency, 1.0);
    }

    /// The render→parse PAIR, end to end. `vike_studio_core::wire_run`'s `to_wire_wf_window` writes
    /// each chosen value with `toml::Value`'s `Display` — the `.to_string()` below IS that call —
    /// and [`to_wf_window`] parses it back. So this asserts the exact property
    /// `WireWfWindow::chosen_params`' doc claims and nothing weaker: across the value shapes a
    /// `[sweep]` axis produces, the text trip is lossless.
    #[test]
    fn a_rendered_toml_value_parses_back_to_itself() {
        let levels = toml::Value::Array(vec![toml::Value::Integer(1), toml::Value::Integer(2)]);
        let originals: Vec<(String, toml::Value)> = vec![
            ("slow".to_string(), toml::Value::Integer(30)),
            ("size".to_string(), toml::Value::Float(1.5)),
            ("symbol".to_string(), toml::Value::String("BTCUSDT".to_string())),
            ("flag".to_string(), toml::Value::Boolean(true)),
            ("levels".to_string(), levels),
        ];
        // exactly what the server puts on the wire
        let rendered: Vec<(String, String)> =
            originals.iter().map(|(k, v)| (k.clone(), v.to_string())).collect();

        let back = to_wf_window(WireWfWindow {
            test_range: (0, 10),
            oos_return: 0.0,
            chosen_params: Some(rendered),
        });
        assert_eq!(
            back.chosen_params,
            Some(originals),
            "every shape a [sweep] axis produces must survive Value -> TOML text -> Value"
        );
    }

    /// A rendering that does NOT parse back is preserved as a `toml::Value::String` of the raw
    /// text — never dropped, and never a panic. This is the residual the wire type's doc names:
    /// the pair survives, the TYPE does not, and a report is read rather than re-executed.
    #[test]
    fn an_unparseable_rendering_is_kept_as_its_own_text() {
        let back = to_wf_window(WireWfWindow {
            test_range: (0, 10),
            oos_return: 0.0,
            chosen_params: Some(vec![("mystery".to_string(), "not a toml value".to_string())]),
        });
        let kept = toml::Value::String("not a toml value".to_string());
        assert_eq!(back.chosen_params, Some(vec![("mystery".to_string(), kept)]));
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

// ---- the NAMED RUN (docs/decisions/0064-a-named-run-carries-no-source.md) -----------------------

/// The default COMPUTE-daemon address — `vike-backend backtest --addr`, which is a DIFFERENT
/// process from the datahub [`DEFAULT_REMOTE_ADDR`] names.
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
             read-scope precisely because it names none of them. Switch the Backend to Remote (which \
             needs a CONTROL key this shell deliberately does not hold) or to Local, or {action} \
             from `vike-cli` on the daemon's own box."
        )
    })
}

/// **Run a strategy the SERVER already holds** — the Studio's third backend, and the only one an
/// OBSERVE credential can reach.
///
/// `crates/vike-app-core/src/backend_registry.rs`'s `datahub_observe_key` states why this shell
/// must never hold the datahub CONTROL key: *"the datahub's CONTROL scope also carries the verbs
/// that COMPILE CLIENT-SUPPLIED RHAI, so a desktop that only wants a DOM ladder must never hold
/// that key."* [`Backend::Remote`] above needs exactly that key, so against any keyed backend — which
/// by `vike_datahub_client::bind`'s `bind_decision` is every one bound off-box — it cannot work
/// from this process at all. This backend is the third option that comment does not consider:
/// making the narrow case not need Control.
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
                 source is the Control-scope `backtest --script` path, which this shell \
                 deliberately holds no key for.",
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
        Some(k) => DatahubClient::connect_authed(addr, k, Scope::Observe),
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
                 Strategy source to Native and pick a name off this backend's roster, or run the \
                 script on the Local backend. Shipping source to a server is the Control-scope \
                 path, and this shell deliberately holds no key for it \
                 (docs/decisions/0064-a-named-run-carries-no-source.md)."
                    .to_string(),
            ));
        }
        StrategySpec::Native { name, params } => (name, params),
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
                     named run carries no source; run it on the Local backend, or use \
                     `vike-cli backtest --script`, which is Control-scope for exactly this reason."
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

#[cfg(test)]
mod named_run_tests {
    use super::*;
    use vike_data::TsRange;

    fn bar_slice() -> DataSlice {
        DataSlice {
            venue: "binance".into(),
            symbols: vec!["BTCUSDT".into()],
            interval: "1d".into(),
            range: TsRange { start: Some(0), end: Some(86_400_000 * 3) },
            kind: SliceKind::Bars,
        }
    }

    fn native(name: &str) -> StrategySpec {
        StrategySpec::native_default(name)
    }

    /// The ordinary case: a native strategy over a bounded bar slice converts.
    #[test]
    fn a_native_strategy_over_a_bounded_bar_slice_converts() {
        let named = to_named_run_spec(&native("buy_hold"), &bar_slice()).expect("converts");
        assert_eq!(named.strategy, "buy_hold");
        assert_eq!(named.symbol, "BTCUSDT");
        assert_eq!(named.start, 0);
        assert!(named.params.is_empty());
    }

    /// **THE STRUCTURAL REFUSAL, in the UI's own words.** A Rhai spec cannot become a named run,
    /// and the message says why rather than reporting an empty roster or a compile error.
    #[test]
    fn a_script_cannot_become_a_named_run_and_the_refusal_says_so() {
        let err = to_named_run_spec(&StrategySpec::rhai("fn on_bar(){}"), &bar_slice())
            .expect_err("a script must be refused");
        let msg = err.to_string();
        assert!(msg.contains("carries no source"), "{msg}");
        assert!(msg.contains("Native"), "…and it names the way forward: {msg}");
    }

    /// Every OTHER bound refuses BY NAME too — one row per dimension the request would have named.
    #[test]
    fn every_bound_refuses_by_name_rather_than_narrowing_the_run() {
        // a tick slice — the window ceiling is counted in bars
        let mut ticks = bar_slice();
        ticks.kind = SliceKind::Ticks;
        assert!(
            to_named_run_spec(&native("buy_hold"), &ticks).unwrap_err().to_string().contains("BAR"),
        );
        // a symbol LIST
        let mut many = bar_slice();
        many.symbols.push("ETHUSDT".into());
        assert!(
            to_named_run_spec(&native("buy_hold"), &many)
                .unwrap_err()
                .to_string()
                .contains("ONE symbol")
        );
        // an OPEN window — the shape that means "the whole store"
        let mut open = bar_slice();
        open.range.end = None;
        assert!(
            to_named_run_spec(&native("buy_hold"), &open)
                .unwrap_err()
                .to_string()
                .contains("BOTH ends")
        );
        // ...and an over-wide one, which comes back from the SHARED validator naming its constant
        let mut wide = bar_slice();
        wide.interval = "1s".into();
        wide.range.end = Some(86_400_000 * 30);
        assert!(
            to_named_run_spec(&native("buy_hold"), &wide)
                .unwrap_err()
                .to_string()
                .contains("NAMED_RUN_MAX_BARS")
        );
    }

    /// A STRING param is refused rather than dropped, and the reserved source key gets its own
    /// sentence — the belt, saying what it is.
    #[test]
    fn a_string_param_is_refused_and_a_src_param_says_what_it_is() {
        let mut table = toml::map::Map::new();
        table.insert("mode".into(), toml::Value::String("aggressive".into()));
        let err = to_named_run_spec(
            &StrategySpec::native("buy_hold", toml::Value::Table(table)),
            &bar_slice(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("numbers and flags only"), "{err}");

        let mut src = toml::map::Map::new();
        src.insert(
            vike_model::RESERVED_SRC_KEY.into(),
            toml::Value::String("fn on_bar(){}".into()),
        );
        let err = to_named_run_spec(
            &StrategySpec::native("buy_hold", toml::Value::Table(src)),
            &bar_slice(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("carries no source"), "{err}");
    }

    /// Numeric and boolean knobs DO cross, and an integer stays an integer — the reason
    /// `NamedParam` splits `Int` from `Num` at all (`Grid::from_params`' `rungs` reads
    /// `Value::as_integer`, which answers `None` for a TOML float).
    #[test]
    fn an_integer_knob_stays_an_integer_across_the_conversion() {
        let mut table = toml::map::Map::new();
        table.insert("rungs".into(), toml::Value::Integer(4));
        table.insert("size".into(), toml::Value::Float(2.5));
        table.insert("live".into(), toml::Value::Boolean(true));
        let named = to_named_run_spec(
            &StrategySpec::native("grid", toml::Value::Table(table)),
            &bar_slice(),
        )
        .expect("converts");
        let by_key = |k: &str| named.params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(by_key("rungs"), Some(NamedParam::Int(4)));
        assert_eq!(by_key("size"), Some(NamedParam::Num(2.5)));
        assert_eq!(by_key("live"), Some(NamedParam::Flag(true)));
    }

    /// The compute daemon is a DIFFERENT process from the datahub, and the two defaults must not
    /// collapse into one — pointing a named run at 7878 reaches a daemon that refuses the verb by
    /// PLANE and sends the reader looking for a key they do not need.
    #[test]
    fn the_compute_default_is_not_the_datahub_default() {
        assert_ne!(DEFAULT_COMPUTE_ADDR, DEFAULT_REMOTE_ADDR);
    }

    /// **The SEARCH refusal — 0064's decision 3, bound 1, which is the largest of that verb's
    /// bounds and the only structural one.** It fires on the Named backend and on NOTHING else, so
    /// adding it could not have narrowed what Local or Remote may do.
    #[test]
    fn a_search_is_refused_on_the_named_backend_and_nowhere_else() {
        let named = Backend::Named { addr: DEFAULT_COMPUTE_ADDR.to_string() };
        for action in ["sweep", "walk-forward"] {
            let msg = named_backend_search_refusal(&named, action)
                .unwrap_or_else(|| panic!("a {action} must be refused on the Named backend"));
            assert!(msg.contains(action), "the refusal names the action it stopped: {msg}");
            assert!(
                msg.contains("ONE parameter set"),
                "…and says what the bound IS, rather than reading as a missing feature: {msg}"
            );
        }
        // ...and the two backends that DO carry a search are untouched, which is what makes this a
        // bound on the new backend rather than a regression on the old ones.
        for other in [Backend::Local, Backend::Remote { addr: DEFAULT_REMOTE_ADDR.to_string() }] {
            assert_eq!(named_backend_search_refusal(&other, "sweep"), None, "{other:?}");
        }
    }
}
