//! PR-3 — the Studio compute-to-data run verb (`RunSlice`): the serde wire DTOs that mirror the
//! `vike-studio-core` Run vocabulary, kept in the LIGHT (DataFusion-free) client crate so the wire
//! schema never reaches the engine crates. This is the #719 precedent — define the DTOs in the
//! client, convert at the server boundary — applied to the Studio's `run_slice` path.
//!
//! # Why these are separate DTO types
//!
//! None of `vike_studio_core::{StrategySpec, DataSlice}` or `vike_backtest::{EngineParams,
//! BacktestResult}` derive serde — they are `#[derive(Debug, Clone, PartialEq)]` engine types, and
//! growing serde on them would tie the wire schema to a crate's internal surface. #719 established
//! the pattern for `RunBacktest(profile_toml)`: ship a small config as text, parse+convert next to
//! the data. `RunSlice` follows it — these DTOs cross the wire; `vike-datahub` (behind
//! `serve-datafusion`) converts them into the studio/engine types and runs the EXISTING
//! `vike_studio_core::run::run_slice` right beside the store, returning just the rendered answer.
//!
//! # What crosses, and what does NOT
//!
//! - [`WireSpec`] carries a native strategy's params as their **TOML text** (`params_toml`), exactly
//!   the `RunBacktest(profile_toml)` idiom, so a `toml::Value` never needs serde on the wire.
//! - [`WireSlice`] decomposes `vike_data::TsRange` into `start`/`end` `Option<i64>` fields (it is not
//!   serde in vike-data) — the same decomposition the read verbs already use in [`crate::proto`].
//! - [`WireEngineParams`] mirrors ONLY the cost/cash knobs a Studio run sets; every omitted (`None`)
//!   field takes `EngineParams::default()` at the boundary (see [`WireEngineParams`]'s docs).
//! - [`WireRunResult`] carries ONLY the fields the Studio RENDERS — deliberately NOT the whole
//!   `BacktestResult`, which is documented as a growing superset (a wire type that tracked it would
//!   be a permanent maintenance tax).
//! - [`WireTrade`] is a faithful, full mirror of the bounded, stable `vike_model::Trade`.
//! - [`WireRunError`] carries a run failure's `kind` + `message`; the server stringifies it into
//!   `Response::Error` kind-first, so a client can classify the failure without re-parsing.
//! - [`WireSweep`]/[`WireSweepEntry`]/[`WireSweepResult`] and
//!   [`WireWalkforward`]/[`WireWfWindow`]/[`WireWalkforwardResult`] are the PR-4 siblings of the
//!   `RunSlice` DTOs — the wire mirrors of `vike_studio_core`'s `run_sweep_slice` (a ranked
//!   parameter sweep) and `run_walkforward_slice` (anchored out-of-sample windows). They follow the
//!   same "mirror ONLY what is rendered" discipline; [`WireSweepResult`]'s `dsr`/`pbo` are
//!   `Option<f64>` for a serde reason spelled out on that type.

use serde::{Deserialize, Serialize};
use vike_model::Trade;

/// The strategy a `RunSlice` executes — the wire mirror of `vike_studio_core::StrategySpec`.
///
/// A native strategy's params ride as their **TOML text** (`params_toml`), NOT a `toml::Value`: the
/// server parses the text into the params table at the boundary
/// (`vike_studio_core::StrategySpec::native_from_toml_str`), exactly the `RunBacktest(profile_toml)`
/// idiom — so `toml::Value` never needs serde on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireSpec {
    /// Rhai source text, compiled server-side. A compile failure comes back as `Response::Error`
    /// carrying the `"compile"` kind (see [`WireRunError`]).
    Rhai(String),
    /// A native registry strategy `name` plus its `strategy.params` table as TOML **text**
    /// (empty/whitespace = an empty params table — every registry `from_params` tolerates that).
    Native {
        /// The `vike_backtest::harness::registry` strategy name (e.g. `"buy_hold"`).
        name: String,
        /// The `strategy.params` table serialized as TOML text (e.g. `size = 1.0\nsymbol = "BTCUSDT"`).
        params_toml: String,
    },
}

/// Which recorded series a [`WireSlice`] replays — the wire mirror of `vike_studio_core::SliceKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WireSliceKind {
    /// `kind=bar` series at `interval`, replayed through `StrategyEngine::run`.
    #[default]
    Bars,
    /// The recorded `kind=quote`/`trade`/`book` tick series, replayed through
    /// `vike_backtest::hist_replay::replay_ticks` (`interval` is unused).
    Ticks,
}

/// The data window a `RunSlice` executes over — the wire mirror of `vike_studio_core::DataSlice`,
/// with `vike_data::TsRange` decomposed into `start`/`end` (per the [`crate::proto`] read-verb
/// precedent, because `TsRange` is not serde in vike-data).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireSlice {
    /// Venue partition (e.g. `"binance"`, `"polymarket"`).
    pub venue: String,
    /// One or more symbols, in the order the engine should register them.
    pub symbols: Vec<String>,
    /// Bar step (e.g. `"1m"`). Conventionally empty for [`WireSliceKind::Ticks`].
    pub interval: String,
    /// Inclusive-range start bound (epoch-ms), `None` = unbounded — the low half of a `TsRange`.
    pub start: Option<i64>,
    /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
    pub end: Option<i64>,
    /// Bar series vs recorded tick series.
    pub kind: WireSliceKind,
}

/// The engine cost/cash knobs a Studio run can set — the wire subset of `vike_backtest::EngineParams`.
///
/// Every field is an `Option`: a `None` field takes the corresponding `EngineParams::default()`
/// value at the server boundary, so a run can set just `cash` and leave the rest at their engine
/// defaults. The whole struct is also optional on the wire (`Request::RunSlice.params:
/// Option<WireEngineParams>`); `None` there means "every field default".
///
/// Deliberately a **faithful 1:1 mirror** of the three `EngineParams` cost fields
/// (`cash`/`fee_rate`/`slippage`) rather than a `*_bps` re-encoding: mirroring the engine's own
/// field names and units keeps the conversion a plain field copy with no lossy bps↔fraction step,
/// and matches the harness `EngineCfg` (TOML profile) cost-knob naming. It mirrors ONLY these three
/// today — the fields a Studio run realistically exposes; a new knob is added here the day the UI
/// grows it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WireEngineParams {
    /// Starting cash → `EngineParams::cash`. `None` = the engine default.
    pub cash: Option<f64>,
    /// Flat fee fraction (NOT bps) → `EngineParams::fee_rate`. `None` = the engine default.
    pub fee_rate: Option<f64>,
    /// Flat slippage → `EngineParams::slippage`. `None` = the engine default.
    pub slippage: Option<f64>,
}

/// A completed round-trip — a faithful, full mirror of the bounded, stable `vike_model::Trade`.
///
/// Unlike `BacktestResult` (a documented growing superset, mirrored partially by [`WireRunResult`]),
/// `Trade` is a small stable value type, so a full mirror is cheap and lossless: [`WireTrade::from_trade`]
/// / [`WireTrade::to_trade`] round-trip it exactly. Kept a distinct wire type (rather than reusing
/// `vike_model::Trade` directly) so the wire stays under explicit control if `Trade` ever grows a
/// field — the same discipline the rest of these DTOs apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireTrade {
    pub entry_price: f64,
    pub exit_price: f64,
    pub size: f64,
    pub pnl: f64,
    pub fees: f64,
    /// fill timestamp of the opening order (epoch ms)
    pub entry_ts: i64,
    /// fill timestamp of the closing order (epoch ms)
    pub exit_ts: i64,
    /// originating symbol (`""` for the single-symbol engine)
    pub symbol: String,
    /// max adverse excursion as a fraction of entry price (0.0 = untracked)
    pub mae: f64,
    /// max favorable excursion as a fraction of entry price (0.0 = untracked)
    pub mfe: f64,
    /// true if the opening side of this trade was a buy
    pub is_long: bool,
}

impl WireTrade {
    /// Mirror a `vike_model::Trade` onto the wire, field-for-field (lossless).
    pub fn from_trade(t: &Trade) -> Self {
        WireTrade {
            entry_price: t.entry_price,
            exit_price: t.exit_price,
            size: t.size,
            pnl: t.pnl,
            fees: t.fees,
            entry_ts: t.entry_ts,
            exit_ts: t.exit_ts,
            symbol: t.symbol.clone(),
            mae: t.mae,
            mfe: t.mfe,
            is_long: t.is_long,
        }
    }

    /// Rebuild the `vike_model::Trade` (the inverse of [`from_trade`](Self::from_trade)) — for a
    /// consumer (e.g. the Studio results pane) that reconstructs a `BacktestResult` to render.
    pub fn to_trade(&self) -> Trade {
        Trade {
            entry_price: self.entry_price,
            exit_price: self.exit_price,
            size: self.size,
            pnl: self.pnl,
            fees: self.fees,
            entry_ts: self.entry_ts,
            exit_ts: self.exit_ts,
            symbol: self.symbol.clone(),
            mae: self.mae,
            mfe: self.mfe,
            is_long: self.is_long,
        }
    }
}

/// The rendered answer of a `RunSlice` — the fields the Studio actually RENDERS from a
/// `vike_backtest::BacktestResult`, NOT the whole result (which is a documented growing superset).
///
/// The server builds this from a `BacktestResult` at the boundary (`vike_datahub::to_wire_result`);
/// the equity curve is the parity anchor — a local `run_slice` and this remote path must produce a
/// bit-identical curve (the PR-3 parity gate).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WireRunResult {
    /// Equity after each step — the curve the Equity/Performance/Validation tabs render.
    pub equity_curve: Vec<f64>,
    /// Step timestamps aligned to `equity_curve` (empty when not tracked).
    pub equity_ts: Vec<i64>,
    pub final_equity: f64,
    pub n_trades: usize,
    /// Multi-symbol event runs only; cumulative PnL per symbol (empty for single-symbol/vector).
    pub per_symbol_pnl: Vec<(String, f64)>,
    /// Closed trades (empty when the kernel ran with `build_trades = false`).
    pub trades: Vec<WireTrade>,
    /// How many times the stale-price wait discipline DEFERRED a market order (0 when off).
    pub stale_deferrals: u64,
    /// How many times the session gate refused a fill because the venue was CLOSED (0 when off).
    pub session_deferrals: u64,
}

/// Why a `RunSlice` could not produce a result — the wire mirror of `vike_studio_core::RunError`.
///
/// `kind` is the machine class (`"compile"` / `"data"` / `"strategy"`); `message` is the detail.
/// The server does not add a structured error variant to `Response` — it stringifies this into the
/// existing `Response::Error(String)` via [`WireRunError::to_error_string`] (kind-first), so a client
/// can prefix-match the class without re-parsing the whole message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRunError {
    /// The failure class: `"compile"` (bad Rhai), `"data"` (bad/empty slice), or `"strategy"`
    /// (unknown native name / bad params table).
    pub kind: String,
    /// The human-readable detail (the underlying compile/data/strategy message).
    pub message: String,
}

impl WireRunError {
    /// Build a `WireRunError` from its class + detail.
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        WireRunError { kind: kind.into(), message: message.into() }
    }

    /// The single-line string carried by `Response::Error` — `"{kind}: {message}"`, KIND FIRST so
    /// the failure class survives the trip through the string channel and a client can prefix-match
    /// it without re-parsing.
    pub fn to_error_string(&self) -> String {
        format!("{}: {}", self.kind, self.message)
    }
}

/// The sweep grid a `RunSweep` executes — the wire mirror of `run_sweep_slice`'s
/// `grid: &[(String, Vec<f64>)]` parameter.
///
/// Each axis is `(param_name, values)`; the SERVER expands the cartesian product next to the data
/// (`vike_studio_core::run_sweep_slice`), so only this compact grid crosses the wire, never the
/// expanded point set. `axes` is empty for a degenerate no-override sweep (one point).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WireSweep {
    /// The `(name, values)` axes — each key overrides `strategy.params.<name>` across its value grid.
    pub axes: Vec<(String, Vec<f64>)>,
}

/// One sweep grid point's outcome — the wire mirror of `vike_studio_core::SweepEntry`.
///
/// Reuses [`WireRunResult`] verbatim for the point's rendered result (the same subset the Studio
/// renders), so a sweep entry carries exactly what a single `RunSlice` would.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireSweepEntry {
    /// The `(name, value)` overrides that produced this point — one row of the cartesian grid.
    pub overrides: Vec<(String, f64)>,
    /// The rendered backtest answer for this point.
    pub result: WireRunResult,
}

/// The rendered answer of a `RunSweep` — the wire mirror of `vike_studio_core::StudioSweep` (a ranked
/// parameter sweep plus its deflated Sharpe and PBO across the trial set).
///
/// # Why `dsr`/`pbo` are `Option<f64>` and NOT plain `f64`
///
/// `StudioSweep`'s `dsr`/`pbo` are plain `f64` that are **`NaN` in NORMAL operation** — a
/// single-point or `< 2`-trial grid leaves PBO unassessable (documented + tested in studio-core as
/// `sweep_pbo_is_nan_for_a_single_point_grid`), and a degenerate/short slice can likewise leave DSR
/// non-finite. `serde_json` serializes a `NaN` `f64` as JSON `null` WITHOUT error, but then FAILS to
/// deserialize `null` back into a plain `f64` — which would turn an ORDINARY successful sweep into a
/// client-side DECODE error. Carrying them as `Option<f64>` (populated via `finite_or_none` at the
/// server boundary — `None` for a non-finite value) makes the round-trip TOTAL: a non-assessable
/// DSR/PBO arrives as `None`, never a broken frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireSweepResult {
    /// The grid points, ranked best-first by annualized Sharpe (the `StudioSweep::entries` order).
    pub entries: Vec<WireSweepEntry>,
    /// Deflated Sharpe across the trials, or `None` when non-finite (a degenerate/short slice).
    pub dsr: Option<f64>,
    /// Probability of backtest overfitting (CSCV), or `None` when not assessable (`< 2` trials, a
    /// too-short slice, or non-finite returns — the `NaN` case studio-core documents).
    pub pbo: Option<f64>,
    /// Index of the best entry within `entries` — always `0` (entries are pre-ranked), kept explicit
    /// to mirror `StudioSweep::best_index`.
    pub best_index: usize,
}

/// The walk-forward configuration a `RunWalkforward` executes — the wire mirror of
/// `run_walkforward_slice`'s `n_splits` parameter.
///
/// Only `n_splits` crosses: the `WalkMode` (`Anchored`) and `periods_per_year` (`252`) are HARDCODED
/// inside `vike_studio_core::run_walkforward_slice`, NOT wire-configurable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireWalkforward {
    /// The number of out-of-sample walk-forward windows.
    pub n_splits: usize,
}

/// One out-of-sample window's outcome — the wire mirror of `vike_backtest::walkforward::WfWindow`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WireWfWindow {
    /// The `[start, end)` bar-index range of this OOS window.
    pub test_range: (usize, usize),
    /// The window's out-of-sample return (a fraction, e.g. `0.05` = +5%).
    pub oos_return: f64,
}

/// The rendered answer of a `RunWalkforward` — a FULL mirror of
/// `vike_backtest::walkforward::WalkForwardReport` (the stitched OOS equity curve + its summary
/// stats). Unlike [`WireSweepResult`], every scalar here is provably finite in normal operation, so
/// none needs an `Option` guard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireWalkforwardResult {
    /// Per-window outcomes, in split order.
    pub windows: Vec<WireWfWindow>,
    /// The stitched out-of-sample equity curve across all windows.
    pub oos_equity_curve: Vec<f64>,
    /// Total OOS return across the stitched curve (a fraction).
    pub oos_return: f64,
    /// Annualized Sharpe of the stitched OOS returns.
    pub oos_sharpe: f64,
    /// Fraction of windows profitable out-of-sample (the `overfit_verdict` input).
    pub wf_consistency: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use std::fmt::Debug;

    /// Serialize `v` to JSON and back, asserting the value survives unchanged — the wire contract
    /// every DTO must hold (the fast-lane counterpart of the datahub `frame_round_trips_*` tests).
    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(v: &T) {
        let json = serde_json::to_string(v).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*v, back, "value must survive a serde round-trip");
    }

    #[test]
    fn wire_spec_rhai_round_trips() {
        round_trip(&WireSpec::Rhai("fn on_bar() {}".to_string()));
    }

    /// A `WireSpec::Native` carrying its params as TOML TEXT round-trips — the params never become a
    /// `toml::Value` on the wire (the `RunBacktest(profile_toml)` idiom).
    #[test]
    fn wire_spec_native_params_toml_round_trips() {
        let spec = WireSpec::Native {
            name: "buy_hold".to_string(),
            params_toml: "size = 1.0\nsymbol = \"BTCUSDT\"\n".to_string(),
        };
        round_trip(&spec);
        // and the params text is carried verbatim (not re-encoded)
        let json = serde_json::to_string(&spec).unwrap();
        let back: WireSpec = serde_json::from_str(&json).unwrap();
        match back {
            WireSpec::Native { name, params_toml } => {
                assert_eq!(name, "buy_hold");
                assert!(params_toml.contains("symbol = \"BTCUSDT\""));
            }
            other => panic!("expected Native, got {other:?}"),
        }
    }

    #[test]
    fn wire_slice_round_trips_both_kinds() {
        round_trip(&WireSlice {
            venue: "binance".to_string(),
            symbols: vec!["BTCUSDT".to_string()],
            interval: "1m".to_string(),
            start: Some(0),
            end: Some(100_000),
            kind: WireSliceKind::Bars,
        });
        round_trip(&WireSlice {
            venue: "polymarket".to_string(),
            symbols: vec!["TKN".to_string(), "TKN2".to_string()],
            interval: String::new(),
            start: None,
            end: None,
            kind: WireSliceKind::Ticks,
        });
    }

    #[test]
    fn wire_engine_params_round_trips_default_and_set() {
        round_trip(&WireEngineParams::default());
        round_trip(&WireEngineParams {
            cash: Some(5000.0),
            fee_rate: Some(0.001),
            slippage: Some(0.0),
        });
    }

    #[test]
    fn wire_run_result_round_trips() {
        round_trip(&WireRunResult {
            equity_curve: vec![1000.0, 1010.5, 995.25],
            equity_ts: vec![1, 2, 3],
            final_equity: 995.25,
            n_trades: 1,
            per_symbol_pnl: vec![("BTCUSDT".to_string(), -4.75)],
            trades: vec![sample_wire_trade()],
            stale_deferrals: 2,
            session_deferrals: 0,
        });
    }

    fn sample_wire_trade() -> WireTrade {
        WireTrade {
            entry_price: 100.0,
            exit_price: 95.25,
            size: 1.0,
            pnl: -4.75,
            fees: 0.2,
            entry_ts: 60_000,
            exit_ts: 120_000,
            symbol: "BTCUSDT".to_string(),
            mae: 0.05,
            mfe: 0.02,
            is_long: true,
        }
    }

    #[test]
    fn wire_trade_round_trips_through_serde() {
        round_trip(&sample_wire_trade());
    }

    /// `Trade -> WireTrade -> Trade` reconstructs the original exactly — the full-mirror losslessness
    /// the server relies on when building [`WireRunResult`] from a `BacktestResult`.
    #[test]
    fn wire_trade_mirrors_vike_model_trade_losslessly() {
        let t = Trade {
            entry_price: 100.0,
            exit_price: 95.25,
            size: 1.5,
            pnl: -7.125,
            fees: 0.3,
            entry_ts: 60_000,
            exit_ts: 120_000,
            symbol: "ETHUSDT".to_string(),
            mae: 0.05,
            mfe: 0.02,
            is_long: false,
        };
        let wire = WireTrade::from_trade(&t);
        assert_eq!(wire.to_trade(), t, "round-trips back to the exact Trade");
    }

    #[test]
    fn wire_run_error_round_trips_and_formats_kind_first() {
        let e = WireRunError::new("compile", "expected `}` at line 1");
        round_trip(&e);
        assert_eq!(e.to_error_string(), "compile: expected `}` at line 1");
    }

    #[test]
    fn wire_sweep_round_trips() {
        round_trip(&WireSweep::default());
        round_trip(&WireSweep {
            axes: vec![
                ("fast".to_string(), vec![3.0, 5.0, 8.0]),
                ("slow".to_string(), vec![20.0, 30.0]),
            ],
        });
    }

    fn sample_wire_run_result() -> WireRunResult {
        WireRunResult {
            equity_curve: vec![1000.0, 1010.5, 995.25],
            equity_ts: vec![1, 2, 3],
            final_equity: 995.25,
            n_trades: 1,
            per_symbol_pnl: vec![("BTCUSDT".to_string(), -4.75)],
            trades: vec![sample_wire_trade()],
            stale_deferrals: 2,
            session_deferrals: 0,
        }
    }

    #[test]
    fn wire_sweep_entry_round_trips() {
        round_trip(&WireSweepEntry {
            overrides: vec![("fast".to_string(), 5.0)],
            result: sample_wire_run_result(),
        });
    }

    /// A sweep result round-trips with `dsr`/`pbo` as BOTH `Some(finite)` and `None` — the `None`
    /// case is the load-bearing one: a `NaN` `f64` would serialize to `null` and then FAIL to decode
    /// into a plain `f64`, so `Option<f64>` (mapping `null` -> `None`) is what keeps an ordinary
    /// single-point sweep from becoming a client-side decode error.
    #[test]
    fn wire_sweep_result_round_trips_including_none_dsr_pbo() {
        round_trip(&WireSweepResult {
            entries: vec![WireSweepEntry {
                overrides: vec![("fast".to_string(), 5.0)],
                result: sample_wire_run_result(),
            }],
            dsr: Some(1.25),
            pbo: Some(0.5),
            best_index: 0,
        });
        round_trip(&WireSweepResult { entries: vec![], dsr: None, pbo: None, best_index: 0 });
    }

    #[test]
    fn wire_walkforward_round_trips() {
        round_trip(&WireWalkforward { n_splits: 4 });
    }

    #[test]
    fn wire_wf_window_round_trips() {
        round_trip(&WireWfWindow { test_range: (100, 200), oos_return: 0.05 });
    }

    #[test]
    fn wire_walkforward_result_round_trips() {
        round_trip(&WireWalkforwardResult {
            windows: vec![
                WireWfWindow { test_range: (0, 100), oos_return: 0.05 },
                WireWfWindow { test_range: (100, 200), oos_return: -0.02 },
            ],
            oos_equity_curve: vec![10_000.0, 10_500.0, 10_290.0],
            oos_return: 0.029,
            oos_sharpe: 1.1,
            wf_consistency: 0.5,
        });
    }
}
