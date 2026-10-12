//! The Studio compute-to-data run verbs' wire DTOs (`RunSlice` and its sweep / walk-forward
//! siblings): serde mirrors of the `vike-studio-core` Run vocabulary, kept in this LIGHT
//! (DataFusion-free) crate so the wire schema never reaches the engine crates — the #719 precedent:
//! define the DTOs in the client, convert at the server boundary.
//!
//! # Why separate DTO types
//!
//! None of `vike_studio_core::{StrategySpec, DataSlice}`, `vike_sim::EngineParams` or
//! `vike_analytics::BacktestResult` derive serde, and growing serde on them would tie the wire to a
//! crate's internal surface. The compute daemon (`crates/vike-backtest/src/compute_server.rs`)
//! converts these into the engine types at the boundary and runs the slice beside the store
//! (`vike_studio_core::wire_run`), returning just the rendered answer.
//!
//! # What crosses
//!
//! ONLY what the Studio sets or RENDERS: params as **TOML text** ([`WireSpec`], the
//! `RunBacktest(profile_toml)` idiom, so a `toml::Value` never needs serde), a `TsRange` decomposed
//! into `start`/`end` ([`WireSlice`]), the rendered subset of the growing `BacktestResult`
//! ([`WireRunResult`]), and the sweep / walk-forward mirrors of `vike_studio_core`'s
//! `run_paramscan_slice` and `run_walkforward_slice`. Two exceptions are argued on their types:
//! [`WireParamscanResult`]'s `dsr`/`pbo` and [`WireWfWindow`]'s `chosen_params`.
//!
//! A field added after its verb shipped follows
//! `docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`
//! (verdict 3): a REPLY-side addition is `#[serde(default, skip_serializing_if)]` and needs no
//! [`crate::proto::PROTO_VERSION`] bump; a REQUEST-side field an old server would silently ignore
//! is capability-negotiated.

use serde::{Deserialize, Serialize};
use vike_model::Trade;

/// The strategy a `RunSlice` executes — the wire mirror of `vike_studio_core::StrategySpec`. A
/// native strategy's params ride as TOML **text**, parsed at the boundary
/// (`vike_studio_core::StrategySpec::native_from_toml_str`).
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
    /// A runtime-loaded Rust strategy, named by an ALREADY-BUILT artifact's sha256 rather than
    /// compiled from anything this frame carries.
    ///
    /// ⚠ This is NOT a source-carrying variant like [`WireSpec::Rhai`] — see
    /// `crates/vike-backtest/src/compute_server.rs`'s module doc for the scope argument that turns
    /// on exactly this distinction.
    Plugin {
        /// The plugin's declared name.
        name: String,
        /// The sha256 of the built artifact this run names, hex-encoded (64 chars). The full hash
        /// crosses whole — a truncated one would silently name a DIFFERENT artifact.
        sha: String,
        /// The `strategy.params` table serialized as TOML text, same idiom as [`WireSpec::Native`].
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
/// with `vike_data::TsRange` decomposed into `start`/`end`.
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

/// The engine cost/cash knobs a Studio run can set — the wire subset of `vike_sim::EngineParams`.
///
/// A `None` field takes the matching `EngineParams::default()` value at the server boundary, and the
/// whole struct is optional on the wire (`Request::RunSlice.params`: `None` = every field default).
/// A faithful 1:1 mirror of the three `EngineParams` cost fields, in the engine's own names and
/// units (no lossy bps↔fraction step); a new knob is added the day the UI grows it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WireEngineParams {
    /// Starting cash → `EngineParams::cash`. `None` = the engine default.
    pub cash: Option<f64>,
    /// Flat fee fraction (NOT bps) → `EngineParams::fee_rate`. `None` = the engine default.
    pub fee_rate: Option<f64>,
    /// Flat slippage → `EngineParams::slippage`. `None` = the engine default.
    pub slippage: Option<f64>,
}

/// A completed round-trip — a full, lossless mirror of the small, stable `vike_model::Trade`
/// ([`WireTrade::from_trade`] / [`WireTrade::to_trade`] round-trip it exactly), kept a distinct wire
/// type so the wire stays under explicit control if `Trade` grows a field.
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

/// The rendered answer of a `RunSlice` — the fields the Studio RENDERS from a
/// `vike_analytics::BacktestResult`, NOT the whole (growing) result. Built at the boundary by
/// `vike_backtest::wire_result`'s `to_wire_result`; the equity curve is the parity anchor (a local
/// `run_slice` and this remote path must produce a bit-identical curve).
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
    /// **What this run was COSTED under** — see [`WireCostModel`]. `None` on a frame from a server
    /// that predates the stamp, and `None` on the per-entry results inside a
    /// [`WireParamscanResult`], whose stamp rides the CONTAINER once rather than on every row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<WireCostModel>,
}

/// **The COST-MODEL STAMP: which cost model chose this answer, and what it could not model.**
/// Ruling: `docs/decisions/0063-the-studio-optimizer-derives-its-cost-model-and-declares-what-it-cannot.md`.
///
/// A report owes it because `EngineParams`' `Default` is a ZERO fee rate and the GUI sends no engine
/// params, and a SEARCHING walk is where a cost model starts CHOOSING the winner.
///
/// ⚠ **Computed, never restated:** every field comes from the value the run actually used, on the
/// run's own path (`vike_studio_core::wire_run`), never written beside it (0063's constraint).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WireCostModel {
    /// `"derived"` — resolved from the slice's own venue + symbols; `"override"` — the caller's
    /// flat `fee_rate` displaced the derivation; `"none"` — nothing applied and every fill was
    /// priced at zero. These three must stay distinguishable (`0047`'s warning about a searching
    /// report).
    pub source: String,
    /// The fee LANE that answered, for `source == "derived"` only — e.g. `binance` vs
    /// `binance-perp`, so a reader can see whether the perp `.P` split applied. A bare venue id is
    /// NOT a lane, and keying the schedule off one would charge a perp slice the spot rates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// The schedule's VARIANT name (`PercentMakerTaker`, `PerShareWithFloor`,
    /// `PercentOfUnderlying`, `ProbabilityScaled`, `Free`), or `"none"`.
    ///
    /// ⚠ Load-bearing: several lanes resolve to shapes with NO flat equivalent and report a zero
    /// maker/taker pair, so without the variant `maker 0, taker 0` reads as free trading.
    pub variant: String,
    /// The maker fraction (NOT bps) the engine charged.
    pub maker_rate: f64,
    /// The taker fraction (NOT bps) the engine charged.
    pub taker_rate: f64,
    /// Why nothing was derived, for `source == "none"` only — so a zero-cost run says so in words
    /// instead of leaving a reader to conclude that trading is free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Fills the engine booked as MAKER across the runs this answer reports, and its taker twin —
    /// the REALISED mix.
    ///
    /// ⚠ **`0063`'s highest-value field:** without it a split that never touched its maker side
    /// looks like one that mattered. The engine's OWN classification (order KIND), the one the fee
    /// was charged under.
    pub maker_fills: u64,
    /// Fills the engine booked as TAKER — the other half of [`Self::maker_fills`].
    pub taker_fills: u64,
    /// Total commission charged across those runs, signed (a rebate-bearing schedule contributes
    /// negative terms). Summed from what the engine debited, never recomputed from the rates above.
    pub fees_paid: f64,
    /// Which metric picked the winner, when this answer RANKED anything (`"sharpe"` / `"return"` /
    /// `"max_dd"` / `"equity"`); `None` for an answer that chose nothing.
    ///
    /// ⚠ **The metric alone is not provenance** (`0063`): inside a walk-forward window every
    /// candidate starts at the same cash, so `return` and `equity` are the SAME ordering. Read it
    /// beside the realised cost above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_metric: Option<String>,
    /// **Every term `0063` ruled on that this answer did NOT price**, each entry naming itself and
    /// why: a statement of only what IS modelled invites the inference that nothing else exists.
    /// ⚠ TWO CLASSES, and the entries say which — DECLARED RESIDUALS (no wire surface fixes them)
    /// and `DTO omission` rows (reachable, plumbing not built). `vike_studio_core::cost_model`'s
    /// `NOT_MODELLED` is the one place they are written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_modelled: Vec<String>,
}

/// Why a `RunSlice` could not produce a result — the wire mirror of `vike_studio_core::RunError`.
///
/// `kind` is the machine class (`"compile"` / `"data"` / `"strategy"`); `message` is the detail. It
/// rides the existing `Response::Error(String)` via [`WireRunError::to_error_string`], kind-first,
/// so a client can prefix-match the class without re-parsing.
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
    /// the failure class survives the trip through the string channel.
    pub fn to_error_string(&self) -> String {
        format!("{}: {}", self.kind, self.message)
    }
}

/// The sweep grid a `RunSweep` executes — the wire mirror of `run_paramscan_slice`'s
/// `grid: &[(String, Vec<f64>)]` parameter.
///
/// Each axis is `(param_name, values)`; the SERVER expands the cartesian product next to the data
/// (`vike_studio_core::run_paramscan_slice`), so only this compact grid crosses the wire, never the
/// expanded point set. `axes` is empty for a degenerate no-override sweep (one point).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WireParamscan {
    /// The `(name, values)` axes — each key overrides `strategy.params.<name>` across its value grid.
    pub axes: Vec<(String, Vec<f64>)>,
}

/// One sweep grid point's outcome — the wire mirror of `vike_studio_core::ParamscanEntry`. Reuses
/// [`WireRunResult`] verbatim, so a sweep entry carries exactly what a single `RunSlice` would.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireParamscanEntry {
    /// The `(name, value)` overrides that produced this point — one row of the cartesian grid.
    pub overrides: Vec<(String, f64)>,
    /// The rendered backtest answer for this point.
    pub result: WireRunResult,
}

/// The rendered answer of a `RunSweep` — the wire mirror of `vike_studio_core::StudioParamscan` (a
/// ranked parameter sweep plus its deflated Sharpe and PBO across the trial set).
///
/// ⚠ **`dsr`/`pbo` are `Option<f64>` because they are `NaN` in NORMAL operation** (a `< 2`-trial
/// grid leaves PBO unassessable — studio-core's `sweep_pbo_is_nan_for_a_single_point_grid` — and a
/// short slice can leave DSR non-finite). serde_json writes a `NaN` as `null` and then FAILS to read
/// `null` back into an `f64`, which would turn an ordinary sweep into a client-side DECODE error;
/// `finite_or_none` at the server boundary maps non-finite to `None`, so the round trip is TOTAL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireParamscanResult {
    /// The grid points, ranked best-first by annualized Sharpe (the `StudioParamscan::entries` order).
    pub entries: Vec<WireParamscanEntry>,
    /// Deflated Sharpe across the trials, or `None` when non-finite (a degenerate/short slice).
    pub dsr: Option<f64>,
    /// Probability of backtest overfitting (CSCV), or `None` when not assessable (`< 2` trials, a
    /// too-short slice, or non-finite returns — the `NaN` case studio-core documents).
    pub pbo: Option<f64>,
    /// Index of the best entry within `entries` — always `0` (entries are pre-ranked), kept explicit
    /// to mirror `StudioParamscan::best_index`.
    pub best_index: usize,
    /// **What this sweep's winner was chosen under** — see [`WireCostModel`]. On the CONTAINER, not
    /// on each entry: one cost model priced every point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<WireCostModel>,
}

/// The walk-forward configuration a `RunWalkforward` executes — the wire mirror of
/// `run_walkforward_slice`'s `n_splits` parameter, plus the optional window [`Self::search`].
///
/// The `WalkMode` (`Anchored`) is hardcoded in `vike_studio_core::run_walkforward_slice_searching`,
/// and the annualization is DERIVED there from [`WireSlice::interval`]
/// (`vike_analytics::report::periods_per_year_for_interval`). ⚠ A `periods_per_year` FIELD would let
/// a caller send a scale contradicting that interval (a hardcoded `252` once put a `1h` slice's
/// `oos_sharpe` `sqrt(24)` low); the scale is a function of the slice, so only the slice carries it.
///
/// Not `Copy`/`Eq`: [`Self::search`] owns a grid. [`WireWalkforward::fixed`] spells the fixed walk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireWalkforward {
    /// The number of out-of-sample walk-forward windows.
    pub n_splits: usize,
    /// **What each window does on its own TRAINING half.** `None` — the field absent on the wire —
    /// is the FIXED walk, byte-identical to every frame before the field existed.
    ///
    /// ⚠ `default` + `skip_serializing_if` keep it additive, but on THIS field they are the DEFECT,
    /// not a guard: [`crate::proto::Request`] has no `deny_unknown_fields`, so a daemon predating it
    /// decodes the frame, DROPS the search, runs the FIXED walk and answers a well-formed
    /// [`crate::proto::Response::WalkforwardResult`]. Hence the capability
    /// [`crate::proto::FEATURE_WALKFORWARD_SEARCH`] and the CLIENT-SIDE refusal, without sending, in
    /// [`crate::DatahubClient::run_walkforward`] (decision 0112, verdict 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<WireWindowSearch>,
}

impl WireWalkforward {
    /// The FIXED-parameter walk: no window search.
    pub fn fixed(n_splits: usize) -> Self {
        WireWalkforward { n_splits, search: None }
    }

    /// A walk whose windows SEARCH — see [`WireWindowSearch`] for what the server does with it.
    pub fn searching(n_splits: usize, search: WireWindowSearch) -> Self {
        WireWalkforward { n_splits, search: Some(search) }
    }
}

/// **WHICH search a walk-forward window runs on its own training half, and under what ranking** —
/// the Studio DTO door's spelling of the profile door's `[walkforward].search` /
/// `[walkforward].rank_by` keys.
///
/// ⚠ **The strings are the operator's own TOKENS, not typed scalars** (as [`crate::WireSearch`]'s
/// knobs are): their authority runs on the SERVER
/// (`vike_backtest::walkforward::runner::WindowSearch::from_str_ci`,
/// `vike_backtest::harness::RankMetric::from_str_ci`), and typed fields would put a second parser
/// in this crate, which must not grow a `vike-backtest` dependency.
///
/// ⚠ **Not [`crate::WireSearch`]**, which names a search METHOD for the PROFILE-shaped paramscan
/// verb; this one says whether a WINDOW searches at all, and carries its grid. Its "second place to
/// say 'optimize'" objection is about one verb with TWO doors; this verb has no profile, so this
/// field is the only place the question can be asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireWindowSearch {
    /// `"none"` (the control) or `"sweep"`. Parsed on the SERVER; anything else is refused there
    /// BY NAME rather than falling through to the fixed walk.
    pub method: String,
    /// The `(name, values)` axes each window scores on its training half — the same grid
    /// [`WireParamscan`] carries for the sweep verb, reused rather than re-spelled.
    pub grid: WireParamscan,
    /// Which metric picks each window's winner (`"sharpe"` / `"return"` / `"max_dd"` / `"equity"`,
    /// case-insensitive). `None` = the server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_by: Option<String>,
}

impl WireWindowSearch {
    /// Whether a daemon that does not advertise [`crate::proto::FEATURE_WALKFORWARD_SEARCH`] would
    /// answer this selection DIFFERENTLY from what it asks — the predicate
    /// [`crate::DatahubClient::run_walkforward`] refuses on.
    ///
    /// **An explicit `none` with nothing else is `false`:** an old daemon drops the field and runs
    /// the fixed walk, exactly what was asked. Anything else is `true` — including a grid or a
    /// `rank_by` written UNDER `none`, whose proper refusal ("a grid is a sweep knob") an old daemon
    /// cannot produce.
    pub fn needs_capability(&self) -> bool {
        !self.grid.axes.is_empty()
            || self.rank_by.is_some()
            || !self.method.trim().eq_ignore_ascii_case(NO_WINDOW_SEARCH)
    }
}

/// The window-search method that searches NOTHING — the control, a first-class value rather than an
/// omitted field. Spelled here as well as parsed on the server because the CLIENT must answer, with
/// no `vike-backtest` dependency, "would an old daemon, which drops this field, answer what was
/// asked?" — yes for this one value ([`WireWindowSearch::needs_capability`] is `false`); every other
/// spelling, known or not, is the server's to accept or refuse.
pub const NO_WINDOW_SEARCH: &str = "none";

/// One out-of-sample window's outcome — the wire mirror of `vike_backtest::walkforward::WfWindow`.
/// Not `Copy`: `chosen_params` owns a `Vec`.
///
/// # Why `chosen_params` crosses as `(String, String)` and not `(String, toml::Value)`
///
/// This LIGHT crate has **no `toml` dependency**, deliberately, so each value crosses as its
/// **rendered TOML text** (`1.5`, `"BTCUSDT"`, `[1, 2]`) — [`WireSpec`]'s `params_toml` idiom.
///
/// **What that costs:** a text round trip, not a lossless mirror (`vike_studio_core::wire_run`'s
/// `to_wire_wf_window` renders, `vike_studio::backend::remote`'s `to_wf_window` parses back). For
/// the scalars and arrays a `[sweep]` axis produces it is exact
/// (`a_rendered_toml_value_parses_back_to_itself`); anything else is kept as a `toml::Value::String`
/// of the raw text rather than dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireWfWindow {
    /// The `[start, end)` bar-index range of this OOS window.
    pub test_range: (usize, usize),
    /// The window's out-of-sample return (a fraction, e.g. `0.05` = +5%).
    pub oos_return: f64,
    /// The parameter overrides this window's own search SELECTED, each value rendered as TOML text
    /// — `None` on the fixed walk, populated whenever [`WireWalkforward::search`] asked for one.
    ///
    /// ⚠ `default` + `skip_serializing_if` are load-bearing: a fixed walk's frame stays
    /// byte-identical to the pre-field one, and an OLD server's frame still decodes for a NEW client
    /// (decision 0112, verdict 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_params: Option<Vec<(String, String)>>,
}

/// The rendered answer of a `RunWalkforward` — a FULL mirror of
/// `vike_backtest::walkforward::WalkForwardReport` (the stitched OOS equity curve and its summary
/// stats). Every scalar is finite in normal operation, so none needs [`WireParamscanResult`]'s
/// `Option` guard. "Full" is the field ROSTER: a window's chosen parameters cross as TOML text (see
/// [`WireWfWindow`]).
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
    /// **What this walk's windows were costed under, and — when they SEARCHED — what chose each
    /// winner.** See [`WireCostModel`]. The realised mix folds the OUT-OF-SAMPLE windows only: a
    /// search's training scores are in no number this report carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<WireCostModel>,
}

#[path = "wire_studio_tests.rs"]
#[cfg(test)]
mod wire_studio_tests;
