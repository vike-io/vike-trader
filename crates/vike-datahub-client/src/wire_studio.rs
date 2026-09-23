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
//! - [`WireParamscan`]/[`WireParamscanEntry`]/[`WireParamscanResult`] and
//!   [`WireWalkforward`]/[`WireWfWindow`]/[`WireWalkforwardResult`] are the PR-4 siblings of the
//!   `RunSlice` DTOs — the wire mirrors of `vike_studio_core`'s `run_paramscan_slice` (a ranked
//!   parameter sweep) and `run_walkforward_slice` (anchored out-of-sample windows). They follow the
//!   same "mirror ONLY what is rendered" discipline; [`WireParamscanResult`]'s `dsr`/`pbo` are
//!   `Option<f64>` for a serde reason spelled out on that type, and [`WireWfWindow`]'s
//!   `chosen_params` is the one field that crosses in a DIFFERENT TYPE from the one the engine
//!   holds — a `toml::Value` rendered to TOML TEXT, the first bullet's idiom a second time, argued
//!   at length on that type because the loss it accepts is real.

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
/// The server builds this from a `BacktestResult` at the boundary (`vike_backtest::wire_result`'s `to_wire_result`);
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
    /// **What this run was COSTED under** — see [`WireCostModel`]. `None` on a frame from a server
    /// that predates the stamp, and `None` on the per-entry results inside a
    /// [`WireParamscanResult`], whose stamp rides the CONTAINER once rather than on every row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<WireCostModel>,
}

/// **The COST-MODEL STAMP: which cost model chose this answer, and what it could not model.**
///
/// Ruling: `docs/decisions/0063-the-studio-optimizer-derives-its-cost-model-and-declares-what-it-cannot.md`.
///
/// # Why a report owes this
///
/// Before it existed, a Studio run reported nothing about costs and the wire discarded the fields
/// that would let a reader infer them — so a priced run and an unpriced one were indistinguishable
/// in either direction. And they genuinely differed: `EngineParams`' `Default` is a ZERO fee rate,
/// the GUI sends no engine params on any verb, and nothing said so. A SEARCHING walk makes that
/// worse rather than better, because ranking is where a cost model stops scaling a report and
/// starts CHOOSING its winner.
///
/// # Computed, never restated
///
/// Every field here is produced from the value the run actually used, on the run's own path
/// (`vike_studio_core::wire_run`), not written beside it. That constraint is `0063`'s and it is
/// not stylistic: two doors each DOCUMENTED as unable to disagree about walk-forward annualization,
/// with no test comparing them, turned out to be a factor of `sqrt(24)` apart on hourly bars.
///
/// # Additive
///
/// Every field a server fills is optional or defaulted and the whole struct is
/// `skip_serializing_if`-omitted upstream, so this needed no [`crate::proto::PROTO_VERSION`] bump —
/// the [`WireWfWindow::chosen_params`] shape, whose doc carries why a gratuitous bump is actively
/// harmful (strict-equality version check at connect; the version is folded into the signed auth
/// mac).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WireCostModel {
    /// `"derived"` — resolved from the slice's own venue + symbols; `"override"` — the caller's
    /// flat `fee_rate` displaced the derivation; `"none"` — nothing applied and every fill was
    /// priced at zero.
    ///
    /// These three must stay distinguishable: a reader who cannot separate a derived schedule from
    /// an overridden flat rate is in exactly the position `0047` warned a searching report would
    /// leave them in.
    pub source: String,
    /// The fee LANE that answered, for `source == "derived"` only — e.g. `binance` vs
    /// `binance-perp`, so a reader can see whether the perp `.P` split applied. A bare venue id is
    /// NOT a lane, and keying the schedule off one would charge a perp slice the spot rates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// The schedule's VARIANT name (`PercentMakerTaker`, `PerShareWithFloor`,
    /// `PercentOfUnderlying`, `ProbabilityScaled`, `Free`), or `"none"`.
    ///
    /// ⚠ Load-bearing rather than decorative: several lanes resolve to shapes with NO flat
    /// equivalent, which report a zero maker/taker pair. Without the variant, `maker 0, taker 0`
    /// reads as free trading when it may mean *this schedule is not expressible as a flat pair*.
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
    /// ⚠ **`0063` calls this the highest-value field in the stamp.** A derived maker/taker schedule
    /// that never touched its maker side and one that mattered look identical without it, and the
    /// shipped Studio starters are all market orders — so the realised mix is what stops a reader
    /// believing a split was doing work it could not be doing. It is the engine's OWN
    /// classification (order KIND, not crossing aggressiveness), because that is the one the fee
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
    /// ⚠ **The metric alone is not provenance, and `0063` says so explicitly.** Inside a
    /// walk-forward window every candidate's equity curve starts at the same cash, so ranking by
    /// total return and ranking by final equity are the SAME ordering — a report naming the metric
    /// is telling a reader less than it appears to. Read it beside the realised cost above, which
    /// is why the two ship in one struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_metric: Option<String>,
    /// **Every term `0063` ruled on that this answer did NOT price**, each entry naming itself and
    /// why.
    ///
    /// ⚠ Part of the stamp, not a footnote. A statement that says only what IS modelled invites the
    /// reader to infer that nothing else exists, which is the failure `0063` exists to avoid
    /// shipping. ⚠ It holds TWO CLASSES and the entries say which: the DECLARED RESIDUALS (impact
    /// calibration, the `[risk]` budget, the binary-outcome winners map) can flip a winner and
    /// nothing in a slice implies them, so no wire surface fixes them; the rows marked
    /// `DTO omission` (instrument snapping, perp funding) are ones `0063` ruled REACHABLE by a
    /// boolean whose plumbing is not built — listed because a reader who saw only the residuals
    /// would conclude everything else was priced. `vike_studio_core::cost_model`'s `NOT_MODELLED`
    /// is the one place they are written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_modelled: Vec<String>,
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

/// One sweep grid point's outcome — the wire mirror of `vike_studio_core::ParamscanEntry`.
///
/// Reuses [`WireRunResult`] verbatim for the point's rendered result (the same subset the Studio
/// renders), so a sweep entry carries exactly what a single `RunSlice` would.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireParamscanEntry {
    /// The `(name, value)` overrides that produced this point — one row of the cartesian grid.
    pub overrides: Vec<(String, f64)>,
    /// The rendered backtest answer for this point.
    pub result: WireRunResult,
}

/// The rendered answer of a `RunSweep` — the wire mirror of `vike_studio_core::StudioParamscan` (a ranked
/// parameter sweep plus its deflated Sharpe and PBO across the trial set).
///
/// # Why `dsr`/`pbo` are `Option<f64>` and NOT plain `f64`
///
/// `StudioParamscan`'s `dsr`/`pbo` are plain `f64` that are **`NaN` in NORMAL operation** — a
/// single-point or `< 2`-trial grid leaves PBO unassessable (documented + tested in studio-core as
/// `sweep_pbo_is_nan_for_a_single_point_grid`), and a degenerate/short slice can likewise leave DSR
/// non-finite. `serde_json` serializes a `NaN` `f64` as JSON `null` WITHOUT error, but then FAILS to
/// deserialize `null` back into a plain `f64` — which would turn an ORDINARY successful sweep into a
/// client-side DECODE error. Carrying them as `Option<f64>` (populated via `finite_or_none` at the
/// server boundary — `None` for a non-finite value) makes the round-trip TOTAL: a non-assessable
/// DSR/PBO arrives as `None`, never a broken frame.
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
    /// **What this sweep's winner was chosen under** — see [`WireCostModel`]. Carried on the
    /// CONTAINER rather than on each entry: one cost model priced every point, and repeating it per
    /// row would invite a reader to look for a difference that cannot exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<WireCostModel>,
}

/// The walk-forward configuration a `RunWalkforward` executes — the wire mirror of
/// `run_walkforward_slice`'s `n_splits` parameter.
///
/// ⚠ **This doc opened "Only `n_splits` crosses" and that stopped being true** when the Studio door
/// grew its search arm (record 0063): [`Self::search`] crosses beside it, optional and omitted when
/// absent. What the paragraph below says about the two knobs that STILL do not cross is unchanged,
/// and the reason the annualization must not is the sharper half of it.
///
/// The `WalkMode` (`Anchored`) is hardcoded inside
/// `vike_studio_core::run_walkforward_slice_searching` and is not wire-configurable. The
/// annualization is DERIVED there from the slice's own interval
/// (`vike_analytics::report::periods_per_year_for_interval`), so it ALREADY crosses — as
/// [`WireSlice::interval`] — and needs no field of its own here.
///
/// ⚠ This doc read "`periods_per_year` (`252`) are HARDCODED", which was an accurate description of
/// the code and was the bug it described: `252` is the count of DAILY observations in a year, so a
/// `1h` slice reported an `oos_sharpe` `sqrt(24) ≈ 4.9x` below the profile-driven door's
/// (`vike_backtest::harness::run_walkforward`, which derives from its profile's interval), and
/// `sqrt(1440) ≈ 37.9x` below it on the `1m` slices the roundtrip fixture uses — while doc comments
/// on the harness side asserted the two planes could not disagree and no test compared them. Adding
/// a `periods_per_year` FIELD here would reopen it from the other end: a caller could then send a
/// scale that contradicts the interval it sent beside it. The scale is a function of the slice, so
/// the slice is the only thing that carries it.
///
/// ⚠ **NOT `Copy` or `Eq` any more**, for the same reason [`WireWfWindow`] stopped being `Copy`:
/// [`Self::search`] owns a grid. Nothing depended on the implicit copy — this rides inside a
/// [`crate::Request`] variant and both conversion sites take it by reference — but a caller
/// expecting one now gets a move, and [`WireWalkforward::fixed`] is the spelling that replaces the
/// old one-field struct literal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireWalkforward {
    /// The number of out-of-sample walk-forward windows.
    pub n_splits: usize,
    /// **What each window does on its own TRAINING half.** `None` — and the absence of the field
    /// on the wire — is the FIXED walk, byte-identical to every frame this verb carried before the
    /// field existed.
    ///
    /// ⚠ `default` + `skip_serializing_if` keep this additive (no [`crate::proto::PROTO_VERSION`]
    /// bump — see [`WireWfWindow::chosen_params`] for why a bump is actively harmful), but on THIS
    /// field they are not a guard, they are the DEFECT. [`crate::Request`] has no
    /// `deny_unknown_fields`, so a daemon predating this field decodes the frame, DROPS the search,
    /// runs the FIXED walk and answers a perfectly well-formed
    /// [`crate::Response::WalkforwardResult`] — a caller cannot tell an optimized walk from a fixed
    /// one by looking at one. That is why it is capability-negotiated through
    /// [`crate::proto::FEATURE_WALKFORWARD_SEARCH`] and refused CLIENT-SIDE, without sending, by
    /// [`crate::DatahubClient::run_walkforward`]. The precedent is
    /// [`crate::proto::FEATURE_SEARCH_METHOD`], copied rather than reinvented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<WireWindowSearch>,
}

impl WireWalkforward {
    /// The FIXED-parameter walk — the shape every caller sent before the search arm existed, and
    /// the spelling that replaces the old `WireWalkforward { n_splits }` struct literal.
    pub fn fixed(n_splits: usize) -> Self {
        WireWalkforward { n_splits, search: None }
    }

    /// A walk whose windows SEARCH. Pairs with [`WireWindowSearch`]'s own doc for what the server
    /// does with it.
    pub fn searching(n_splits: usize, search: WireWindowSearch) -> Self {
        WireWalkforward { n_splits, search: Some(search) }
    }
}

/// **WHICH search a walk-forward window runs on its own training half, and under what ranking** —
/// the Studio DTO door's spelling of the profile door's `[walkforward].search` /
/// `[walkforward].rank_by` keys.
///
/// ⚠ **The strings are the operator's own TOKENS, not typed scalars**, the same rule
/// [`crate::WireSearch`] states for its own knobs and for the same reason: the authority for what
/// `"sweep"` and `"sharpe"` mean — the accepted set and the refusal text — runs on the SERVER
/// (`vike_backtest::harness::walkforward::WindowSearch::from_str_ci` and
/// `vike_backtest::harness::RankMetric::from_str_ci`). Typed fields here would put a second parser,
/// with a second message, in the light client crate — which carries no `vike-backtest` dependency
/// and must not grow one.
///
/// ⚠ **Not to be confused with [`crate::WireSearch`]**, which names a search METHOD (grid / euler /
/// tpe / genetic) for the PROFILE-shaped paramscan verb. This one names whether a WINDOW searches
/// at all, and carries the grid it searches — the two live on different verbs and answer different
/// questions.
///
/// ⚠ **The sibling objection, answered rather than left standing.** [`crate::WireSearch`]'s doc
/// closes by noting that `Request::RunWalkforwardProfile` carries no selector because "a second
/// place to say 'optimize' is a second place for the two to disagree". That objection is about ONE
/// verb having TWO doors — a wire field beside a `[walkforward]` table in the same profile. This
/// verb has no profile: the Studio DTO door carries no `[walkforward]` table at all, so this field
/// is the only place the question can be asked, not a second one.
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
    /// answer this selection DIFFERENTLY from what it asks for — the predicate
    /// [`crate::DatahubClient::run_walkforward`] refuses on.
    ///
    /// **An explicit `none` with nothing else is `false`**: an old daemon drops the field and runs
    /// the fixed walk, which is exactly what was asked, and refusing it would be a false refusal on
    /// the one request that works against every peer in the field. Anything else is `true` —
    /// including a grid or a `rank_by` written UNDER `none`, because the refusal those deserve
    /// ("a grid is a sweep knob") is one an old daemon cannot produce: it drops the whole field and
    /// reports success.
    pub fn needs_capability(&self) -> bool {
        !self.grid.axes.is_empty()
            || self.rank_by.is_some()
            || !self.method.trim().eq_ignore_ascii_case(NO_WINDOW_SEARCH)
    }
}

/// The window-search method that searches NOTHING — the control, and a first-class value rather
/// than an omitted field.
///
/// It is spelled here as well as parsed on the server because the CLIENT has to answer one question
/// without a `vike-backtest` dependency: "would an old daemon, which drops this field entirely,
/// answer what was asked?" For this one value it would, which is what
/// [`WireWindowSearch::needs_capability`] returns `false` for. Every other spelling — including one
/// this client has never heard of — is the server's to accept or refuse.
pub const NO_WINDOW_SEARCH: &str = "none";

/// One out-of-sample window's outcome — the wire mirror of `vike_backtest::walkforward::WfWindow`.
///
/// ⚠ **NOT `Copy` any more**, for the same reason the engine type stopped being `Copy`:
/// `chosen_params` owns a `Vec`. Nothing depended on the implicit copy — these live in a
/// [`WireWalkforwardResult`]'s `Vec`, and both conversion sites take the whole result — but a
/// future caller expecting one gets a move instead.
///
/// # Why `chosen_params` crosses as `(String, String)` and not `(String, toml::Value)`
///
/// The engine field is `Option<Vec<(String, toml::Value)>>`, and this crate has **no `toml`
/// dependency** — deliberately. It is the LIGHT, DataFusion-free half the GUI links, and its whole
/// normal-dep list is `vike-data` (trait only) + `vike-model` + serde + the two hash crates the
/// node auth already needed; growing a TOML parser onto it to carry a diagnostic field would spend
/// exactly the weight this crate exists to defend.
///
/// So the value crosses as its **rendered TOML text** — `1.5`, `"BTCUSDT"`, `true`, `[1, 2]` —
/// which is the module doc's first bullet applied one type down: the same reason [`WireSpec`]'s
/// `Native` arm ships its params as `params_toml` rather than a table, so a `toml::Value` never
/// needs serde on the wire.
///
/// **What that costs, stated rather than implied.** This field is no longer a lossless mirror: the
/// server renders it with `toml::Value`'s `Display` (`vike_studio_core::wire_run`'s
/// `to_wire_wf_window`) and the Studio client parses each rendering back
/// (`vike_studio::remote`'s `to_wf_window`), so the pair is a text round trip rather than a copy.
/// For the values a `[sweep]` axis actually produces — the TOML scalars and arrays a grid array
/// holds — that trip is exact, and `vike_studio::remote`'s
/// `a_rendered_toml_value_parses_back_to_itself` is where it is checked; anything that does NOT
/// parse back is preserved as a `toml::Value::String` of the raw text rather than dropped, so the
/// key/value pair survives even when its type does not. A walk-forward report is read, not
/// re-executed, so a value that came back as its own text still says what the window chose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireWfWindow {
    /// The `[start, end)` bar-index range of this OOS window.
    pub test_range: (usize, usize),
    /// The window's out-of-sample return (a fraction, e.g. `0.05` = +5%).
    pub oos_return: f64,
    /// The parameter overrides this window's own search SELECTED, each value rendered as TOML text
    /// — `None` on the fixed-parameter walk.
    ///
    /// ⚠ **This doc said "today that is EVERY run this verb serves" and that stopped being true**
    /// when the Studio door grew its search arm (`0063`). A window populates this whenever
    /// [`WireWalkforward::search`] asked for one; a fixed walk still leaves it `None`. The field was
    /// mirrored ahead of the feature precisely so this day would need no wire change — the
    /// alternative being a field that vanishes silently the moment an optimizing walk reaches the
    /// verb, leaving a stitched number with no way to tell a procedure that found something stable
    /// from one that wandered, which is the whole diagnostic the engine field was added for.
    ///
    /// ⚠ `default` + `skip_serializing_if` are load-bearing, and together they are why this field
    /// did NOT bump [`crate::proto::PROTO_VERSION`]. Omitted when `None`, so every frame this verb
    /// produces today is byte-identical to the pre-field one; defaulted when absent, so an OLD
    /// server's frame still decodes for a NEW client. Both directions decode cleanly, which is
    /// exactly the test `PROTO_VERSION`'s own doc sets for a change that needs no bump — see
    /// [`crate::proto::FEATURE_AUTH`], where the argument (and the reason a gratuitous bump is
    /// actively harmful here, the version being folded into the signed auth mac) is spelled out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_params: Option<Vec<(String, String)>>,
}

/// The rendered answer of a `RunWalkforward` — a FULL mirror of
/// `vike_backtest::walkforward::WalkForwardReport` (the stitched OOS equity curve + its summary
/// stats). Unlike [`WireParamscanResult`], every scalar here is provably finite in normal operation, so
/// none needs an `Option` guard.
///
/// "Full" is the field ROSTER, not the field TYPES: every `WalkForwardReport` field crosses, but a
/// window's chosen parameters cross as rendered TOML TEXT rather than as `toml::Value` — argued,
/// with what it costs, on [`WireWfWindow`].
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
    /// search's training scores are in no number this report carries, so counting their fills would
    /// describe work the report does not contain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<WireCostModel>,
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

    /// A `WireSpec::Plugin` round-trips, and the sha crosses WHOLE — a truncated one would name a
    /// different artifact.
    #[test]
    fn wire_spec_plugin_round_trips_and_keeps_the_sha_whole() {
        let spec = WireSpec::Plugin {
            name: "my_strat".to_string(),
            sha: "a".repeat(64),
            params_toml: "fast = 5\n".to_string(),
        };
        round_trip(&spec);
        let json = serde_json::to_string(&spec).unwrap();
        let back: WireSpec = serde_json::from_str(&json).unwrap();
        match back {
            WireSpec::Plugin { name, sha, params_toml } => {
                assert_eq!(name, "my_strat");
                assert_eq!(sha.len(), 64);
                assert_eq!(sha, "a".repeat(64));
                assert!(params_toml.contains("fast"));
            }
            other => panic!("expected Plugin, got {other:?}"),
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
            cost_model: None,
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
        round_trip(&WireParamscan::default());
        round_trip(&WireParamscan {
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
            cost_model: None,
        }
    }

    #[test]
    fn wire_sweep_entry_round_trips() {
        round_trip(&WireParamscanEntry {
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
        round_trip(&WireParamscanResult {
            entries: vec![WireParamscanEntry {
                overrides: vec![("fast".to_string(), 5.0)],
                result: sample_wire_run_result(),
            }],
            dsr: Some(1.25),
            pbo: Some(0.5),
            best_index: 0,
            cost_model: None,
        });
        round_trip(&WireParamscanResult {
            entries: vec![],
            dsr: None,
            pbo: None,
            best_index: 0,
            cost_model: None,
        });
    }

    #[test]
    fn wire_walkforward_round_trips() {
        round_trip(&WireWalkforward::fixed(4));
        round_trip(&WireWalkforward::searching(
            4,
            WireWindowSearch {
                method: "sweep".to_string(),
                grid: WireParamscan { axes: vec![("fast".to_string(), vec![3.0, 5.0])] },
                rank_by: Some("max_dd".to_string()),
            },
        ));
    }

    /// `needs_capability` is what stands between a client and a daemon that would silently run the
    /// FIXED walk. Its two directions are both load-bearing, and the FALSE one is the easier to get
    /// wrong: refusing an ordinary fixed walk would break every peer in the field.
    #[test]
    fn needs_capability_refuses_a_search_and_admits_the_bare_control() {
        let control = WireWindowSearch {
            method: NO_WINDOW_SEARCH.to_string(),
            grid: WireParamscan::default(),
            rank_by: None,
        };
        assert!(!control.needs_capability(), "an explicit control runs correctly on any peer");
        assert!(
            WireWindowSearch { method: "sweep".to_string(), ..control.clone() }.needs_capability(),
            "a sweep an old daemon would DROP must be refused before it is sent"
        );
        assert!(
            WireWindowSearch {
                grid: WireParamscan { axes: vec![("fast".to_string(), vec![1.0])] },
                ..control.clone()
            }
            .needs_capability(),
            "a grid written under the control needs it too — an old daemon drops the whole field \
             and reports success, so the refusal it deserves is one only this side can give"
        );
        assert!(
            WireWindowSearch { rank_by: Some("equity".to_string()), ..control.clone() }
                .needs_capability(),
            "…and so does a rank_by"
        );
        // A method this client has never heard of is the SERVER's to refuse by name — but it is
        // still a search an old daemon would drop, so it does not leave this side.
        assert!(WireWindowSearch { method: "bayesian".to_string(), ..control }.needs_capability());
    }

    /// The STAMP round-trips, including the two shapes a reader must be able to tell apart: a
    /// derived schedule naming its lane, and an overridden flat rate naming none.
    #[test]
    fn wire_cost_model_round_trips_both_sources() {
        round_trip(&WireCostModel {
            source: "derived".to_string(),
            lane: Some("binance-perp".to_string()),
            variant: "PercentMakerTaker".to_string(),
            maker_rate: 0.0002,
            taker_rate: 0.0005,
            reason: None,
            maker_fills: 0,
            taker_fills: 42,
            fees_paid: 12.5,
            rank_metric: Some("sharpe".to_string()),
            not_modelled: vec!["impact — …".to_string()],
        });
        round_trip(&WireCostModel {
            source: "override".to_string(),
            variant: "none".to_string(),
            maker_rate: 0.001,
            taker_rate: 0.001,
            ..WireCostModel::default()
        });
    }

    /// The stamp is ADDITIVE: an answer without one does not carry the key, and a frame that omits
    /// it — an OLD server answering a NEW client — still decodes. That pair is the whole reason no
    /// [`crate::proto::PROTO_VERSION`] bump was needed.
    #[test]
    fn an_absent_stamp_is_omitted_from_the_frame_and_an_old_frame_decodes() {
        let unstamped = WireWalkforwardResult {
            windows: vec![],
            oos_equity_curve: vec![],
            oos_return: 0.0,
            oos_sharpe: 0.0,
            wf_consistency: 0.0,
            cost_model: None,
        };
        let json = serde_json::to_string(&unstamped).expect("serialize");
        assert!(!json.contains("cost_model"), "an absent stamp must not reach the frame: {json}");

        let old_frame = r#"{"windows":[],"oos_equity_curve":[],"oos_return":0.0,"oos_sharpe":0.0,"wf_consistency":0.0}"#;
        let decoded: WireWalkforwardResult =
            serde_json::from_str(old_frame).expect("an old frame decodes for a new client");
        assert_eq!(decoded, unstamped);
    }

    #[test]
    fn wire_wf_window_round_trips() {
        // the fixed-parameter walk's shape — every window a walk that did NOT search serves
        let fixed = WireWfWindow { test_range: (100, 200), oos_return: 0.05, chosen_params: None };
        round_trip(&fixed);
        // ...and the optimizing walk's: each chosen value as its rendered TOML text, one pair
        // per swept axis. The quoting on the string is the point — it is what tells the client's
        // parse a `toml::Value::String` from a bare value.
        let chosen = vec![
            ("fast".to_string(), "5".to_string()),
            ("symbol".to_string(), "\"BTCUSDT\"".to_string()),
        ];
        round_trip(&WireWfWindow { chosen_params: Some(chosen), ..fixed });
    }

    /// The two properties that let `chosen_params` join the schema without bumping
    /// [`crate::proto::PROTO_VERSION`], asserted rather than argued: a `None` choice does not
    /// appear in the frame AT ALL (so every frame the `RunWalkforward` verb produces today is
    /// byte-identical to the pre-field one), and a frame that omits the key — i.e. an OLD server
    /// answering a NEW client — still decodes, as `None`.
    #[test]
    fn a_none_choice_is_omitted_from_the_frame_and_an_absent_key_decodes_as_none() {
        let w = WireWfWindow { test_range: (0, 10), oos_return: 0.01, chosen_params: None };
        let json = serde_json::to_string(&w).expect("serialize");
        assert!(
            !json.contains("chosen_params"),
            "a None choice must not reach the frame — that omission is what keeps today's \
             walk-forward frames byte-identical to the pre-field ones: {json}"
        );

        let old_frame = r#"{"test_range":[0,10],"oos_return":0.01}"#;
        let decoded: WireWfWindow = serde_json::from_str(old_frame).expect("deserialize");
        assert_eq!(decoded, w, "an absent key must default to None, not fail the frame");
    }

    #[test]
    fn wire_walkforward_result_round_trips() {
        round_trip(&WireWalkforwardResult {
            windows: vec![
                WireWfWindow { test_range: (0, 100), oos_return: 0.05, chosen_params: None },
                WireWfWindow {
                    test_range: (100, 200),
                    oos_return: -0.02,
                    chosen_params: Some(vec![("fast".to_string(), "8".to_string())]),
                },
            ],
            oos_equity_curve: vec![10_000.0, 10_500.0, 10_290.0],
            oos_return: 0.029,
            oos_sharpe: 1.1,
            wf_consistency: 0.5,
            cost_model: None,
        });
    }
}
