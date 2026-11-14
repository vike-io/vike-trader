//! The Studio compute-to-data run verbs' wire DTOs (`RunSlice` and its sweep / walk-forward
//! siblings): serde mirrors of the `vike-studio-core` Run vocabulary, kept in this LIGHT
//! (DataFusion-free) crate so the wire schema never reaches the engine crates — the #719 precedent:
//! define the DTOs in the client, convert at the server boundary.
//!
//! # Why separate DTO types
//!
//! None of `vike_studio_core::{StrategySpec, DataSlice}`, `vike_simulator::StrategyEngineConfig` or
//! `vike_analytics::BacktestResult` derive serde, and growing serde on them would tie the wire to a
//! crate's internal surface. The backtesthub (`crates/vike-backtesthub/src/backtesthub_server.rs`)
//! converts these into the engine types at the boundary and runs the slice beside the store
//! (`vike_studio_core::wire_run`), returning just the rendered answer.
//!
//! ⚠ **A domain type whose serde writes the wire's bytes crosses AS ITSELF, with no mirror:** a
//! closed trade is a `vike_model::Trade` ([`BacktestResultWire::trades`]) and a walk-forward window a
//! `vike_analytics::result::WalkforwardWindow` ([`WalkforwardReportWire::windows`]). Both are frozen by
//! `tests/backtest_plane_wire_pin.rs`, so a field either type grows fails there first. A failure
//! crosses as the one line `vike_studio_core::RunError::wire_line` writes.
//!
//! # What crosses
//!
//! ONLY what the Studio sets or RENDERS: a strategy's params table as **TOML text** ([`StrategySpecWire`],
//! the `RunBacktest(profile_toml)` idiom), a `TimestampRange` decomposed into `start`/`end`
//! ([`DataSliceWire`]), the rendered subset of the growing `BacktestResult` ([`BacktestResultWire`]), and the
//! sweep / walk-forward mirrors of `vike_studio_core`'s `run_paramscan_slice` and
//! `run_walkforward_slice`. A sweep axis value, a sweep point's override and a window's chosen value
//! each cross as a `vike_model::params::ParamValue` ([`ParamscanWire`], [`ParamscanEntryWire`],
//! [`WalkforwardWindow`]), so an integer knob stays an `Int` from the axis that names it to the window
//! that chose it. [`StudioParamscanWire`]'s `dsr`/`pbo` are argued on their type.
//!
//! # Why a `ParamValue` on the wire is compatible with the `f64` it replaced
//!
//! `ParamValue`'s serde is untagged (`crates/vike-model/src/params.rs`): a `Float(5.0)` is written
//! `5.0` and an `Int(5)` is written `5`, exactly what an `f64` and an `i64` write. A peer that still
//! reads the sweep axes and overrides as `f64` accepts both, and a `Float` axis is the same bytes
//! the `f64` axis was: the committed `fixtures/datahub_wire/v7_paramscan_rename_requests.frames`
//! still decodes and re-encodes (`crates/vike-backtesthub-client/tests/wire_tag_fixtures.rs`).
//! [`WalkforwardWindow`]'s `chosen_params` argues its own case.
//!
//! A field added after its verb shipped follows
//! `docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`
//! (verdict 3): a REPLY-side addition is `#[serde(default, skip_serializing_if)]` and needs no
//! [`crate::proto::PROTO_VERSION`] bump; a REQUEST-side field an old server would silently ignore
//! is capability-negotiated.

use serde::{Deserialize, Serialize};
use vike_analytics::result::WalkforwardWindow;
use vike_model::params::ParamValue;
use vike_model::{OrderDrop, Trade};

/// The strategy a `RunSlice` executes — the wire mirror of `vike_studio_core::StrategySpec`. A
/// strategy file's params ride as TOML **text**, parsed at the boundary
/// (`vike_studio_core::StrategySpec::file_from_toml_str`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StrategySpecWire {
    /// Rhai source text, compiled server-side. A compile failure comes back as `Response::Error`
    /// carrying the `"compile"` kind (`vike_studio_core::RunError::wire_line`).
    Rhai(String),
    /// **Retired: a registry NAME.** No strategy is compiled into the server, so nothing resolves a
    /// name; the variant still DECODES so a peer that sends it is answered with the fix
    /// (`vike_studio_core::wire_run::to_strategy_spec` refuses it, naming [`StrategySpecWire::File`]) rather
    /// than with an undecodable frame.
    Native {
        /// The retired registry name (e.g. `"buy_hold"`).
        name: String,
        /// The `strategy.params` table serialized as TOML text (e.g. `size = 1.0\nsymbol = "BTCUSDT"`).
        params_toml: String,
    },
    /// A runtime-loaded Rust strategy, named by an ALREADY-BUILT artifact's sha256 rather than
    /// compiled from anything this frame carries.
    ///
    /// ⚠ This is NOT a source-carrying variant like [`StrategySpecWire::Rhai`] — see
    /// `crates/vike-backtesthub/src/backtesthub_server.rs`'s module doc for the scope argument that turns
    /// on exactly this distinction.
    Plugin {
        /// The strategy file's declared name.
        name: String,
        /// The sha256 of the built artifact this run names, hex-encoded (64 chars). The full hash
        /// crosses whole — a truncated one would silently name a DIFFERENT artifact.
        sha: String,
        /// The `strategy.params` table serialized as TOML text, same idiom as [`StrategySpecWire::File`].
        params_toml: String,
    },
    /// A strategy FILE of the server's own `strategies/` folder, by its path under that folder
    /// (`trend/momentum.rs`), plus its `strategy.params` table as TOML **text** (empty/whitespace =
    /// an empty table). The server reads the file; nothing of it crosses the wire.
    File {
        /// The file's path under the server's `strategies/` folder.
        file: String,
        /// The `strategy.params` table serialized as TOML text.
        params_toml: String,
    },
}

/// Which recorded series a [`DataSliceWire`] replays — the wire twin of
/// `vike_model::run_request::replay::ReplayKind`, keeping its own bytes (`Bars` / `Ticks`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReplayKindWire {
    /// `kind=bar` series at `interval`, replayed through `StrategyEngine::run`.
    #[default]
    Bars,
    /// The recorded `kind=quote`/`trade`/`book` tick series, replayed through
    /// `vike_backtest::tick_replay::replay_ticks` (`interval` is unused).
    Ticks,
}

/// The data window a `RunSlice` executes over — the wire mirror of `vike_studio_core::DataSlice`,
/// with `vike_data::TimestampRange` decomposed into `start`/`end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataSliceWire {
    /// Venue partition (e.g. `"binance"`, `"polymarket"`).
    pub venue: String,
    /// One or more symbols, in the order the engine should register them.
    pub symbols: Vec<String>,
    /// Bar step (e.g. `"1m"`). Conventionally empty for [`ReplayKindWire::Ticks`].
    pub interval: String,
    /// Inclusive-range start bound (epoch-ms), `None` = unbounded — the low half of a `TimestampRange`.
    pub start: Option<i64>,
    /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
    pub end: Option<i64>,
    /// Bar series vs recorded tick series.
    pub kind: ReplayKindWire,
}

/// The engine cost/cash knobs a Studio run can set — the wire subset of `vike_simulator::StrategyEngineConfig`.
///
/// A `None` field takes the matching `StrategyEngineConfig::default()` value at the server boundary, and the
/// whole struct is optional on the wire (`Request::RunSlice.params`: `None` = every field default).
/// In the engine's own names and units (no lossy bps↔fraction step); a new knob is added the day
/// the UI grows it.
///
/// The slippage is the Studio's switch and its value field, and the server writes them into the ONE
/// profile table, `[engine.slippage]` (`vike_backtest::harness::ProfileBacktest::for_slice`), so a
/// Studio slippage is a profile's: [`Self::slippage_model`] the model, [`Self::slippage`] its value,
/// [`Self::slippage_tick`] the tick of the `ticks` model.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StrategyEngineConfigWire {
    /// Starting cash → `StrategyEngineConfig::cash`. `None` = the engine default.
    pub cash: Option<f64>,
    /// Flat fee fraction (NOT bps) → `StrategyEngineConfig::fee_rate`. `None` = the engine default.
    pub fee_rate: Option<f64>,
    /// The slippage VALUE of [`Self::slippage_model`]: the fraction of the price (`percent`), the
    /// number of ticks (`ticks`) or the constant fraction beside the impact (`market_impact`). With no
    /// model it is the flat fraction, `percent`, what this field meant before the model existed.
    /// `None` = no slippage.
    pub slippage: Option<f64>,
    /// The slippage model, `[engine.slippage]`'s `model`: `none`, `percent`, `ticks` or
    /// `market_impact`. `None` = `percent` when [`Self::slippage`] is set, else no slippage.
    ///
    /// ⚠ A daemon predating this field would DROP it and price the value as a flat fraction, so a
    /// request naming it needs [`crate::proto::FEATURE_SLIPPAGE_MODEL`] ([`Self::needs_capability`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slippage_model: Option<String>,
    /// The `ticks` model's tick: the price one tick is (`0.01`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slippage_tick: Option<f64>,
}

impl StrategyEngineConfigWire {
    /// Whether a daemon that does not advertise [`crate::proto::FEATURE_SLIPPAGE_MODEL`] would
    /// mis-serve these params: it drops the model and the tick, and runs the value as a fraction.
    pub fn needs_capability(&self) -> bool {
        self.slippage_model.is_some() || self.slippage_tick.is_some()
    }
}

/// The rendered answer of a `RunSlice` — the fields the Studio RENDERS from a
/// `vike_analytics::BacktestResult`, NOT the whole (growing) result. Built at the boundary by
/// `crate::wire_result::to_wire_result`; the equity curve is the parity anchor (a local
/// `run_slice` and this remote path must produce a bit-identical curve).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BacktestResultWire {
    /// Equity after each step — the curve the Equity/Performance/Validation tabs render.
    pub equity_curve: Vec<f64>,
    /// Step timestamps aligned to `equity_curve` (empty when not tracked).
    pub equity_timestamp: Vec<i64>,
    pub final_equity: f64,
    pub n_trades: usize,
    /// Multi-symbol event runs only; cumulative PnL per symbol (empty for single-symbol/vector).
    pub per_symbol_pnl: Vec<(String, f64)>,
    /// Closed trades (empty when the kernel ran with `build_trades = false`), each the model's own
    /// `Trade`: its serde writes the eleven keys the wire always carried, in their order.
    ///
    /// ⚠ The DECODE is `Trade`'s, which defaults seven of them, so a trade missing a key reads with
    /// that key at its default where the old mirror refused the frame. No peer writes such a trade.
    pub trades: Vec<Trade>,
    /// How many times the stale-price wait discipline DEFERRED a market order (0 when off).
    pub stale_deferrals: u64,
    /// How many times the session gate refused a fill because the venue was CLOSED (0 when off).
    pub session_deferrals: u64,
    /// **What this run was COSTED under** — see [`CostModelStampWire`]. `None` on a frame from a server
    /// that predates the stamp, and `None` on the per-entry results inside a
    /// [`StudioParamscanWire`], whose stamp rides the CONTAINER once rather than on every row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<CostModelStampWire>,
    /// What the run found wrong with its data and ran anyway (`BacktestResult::data_warnings`). Absent
    /// from a clean run's frame, so that frame is byte-identical to one written before this field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_warnings: Vec<String>,
    /// NET perp funding over the run, received-positive (`BacktestResult::funding_paid`). Absent from
    /// a frame that charged none, so that frame is byte-identical to one written before this field,
    /// and an old frame reads as `0.0`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub funding_paid: f64,
    /// `Some(price)` when the run is a COIN LEDGER (`BacktestResult::settle_price`, decision 0258):
    /// every money field above is then an amount of the contract's settle coin, and this is the
    /// dollars ONE coin is worth. Absent for every other run, so that frame is byte-identical to one
    /// written before this field, and an old frame reads as `None`, the quote-currency run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_price: Option<f64>,
    /// The orders the run REFUSED (`BacktestResult::dropped`, the gates' own words), each distinct
    /// drop once with how many times it was refused, in first-seen order. Grouped because a refusal
    /// repeats on every bar a strategy re-sends it (a reversing cross on a spot pair re-sends its
    /// short each bar), so the list stays short where the raw one grows with the slice; expanded back
    /// by `crate::wire_result::from_wire_result`. Absent from a run that refused nothing, so that
    /// frame is byte-identical to one written before this field, and an old frame reads as none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused_orders: Vec<(OrderDrop, u64)>,
    /// The strategy file the run ran (the profile's `[strategy] file`) and the sha256 of its text
    /// as read: `vike_analytics::report::BacktestReport`'s two fields, attached by the caller to
    /// the top-level answer like the cost stamp. Absent where none was recorded, so such a frame is
    /// byte-identical to one written before the fields, and an old frame reads as none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy_file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy_sha256: String,
}

/// `skip_serializing_if` for a `funding_paid`: serde hands the predicate a `&f64`. A `-0.0` is zero too.
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// **The COST-MODEL STAMP: which cost model chose this answer, and what it could not model.**
/// Ruling: `docs/decisions/0063-the-studio-optimizer-derives-its-cost-model-and-declares-what-it-cannot.md`.
///
/// A report owes it because `StrategyEngineConfig`' `Default` is a ZERO fee rate and the GUI sends no engine
/// params, and a SEARCHING walk is where a cost model starts CHOOSING the winner.
///
/// ⚠ **Computed, never restated:** every field comes from the value the run actually used, on the
/// run's own path (`vike_studio_core::wire_run`), never written beside it (0063's constraint).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CostModelStampWire {
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

/// The sweep grid a `RunSweep` executes — the wire mirror of `run_paramscan_slice`'s
/// `grid: &[(String, Vec<ParamValue>)]` parameter.
///
/// Each axis is `(param_name, values)`; the SERVER expands the cartesian product next to the data
/// (`vike_backtesthub::studio_run::run_paramscan_slice`), so only this compact grid crosses the wire, never the
/// expanded point set. `axes` is empty for a degenerate no-override sweep (one point).
///
/// Each value carries its KIND, decided where the axis is made (the Studio's `start_sweep`): an
/// `Int` reaches an integer knob as an `Int`, a `Float` as a `Float`, and the server writes it into
/// the params table as given. The wire form is the one the `f64` axis had (this module's doc).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ParamscanWire {
    /// The `(name, values)` axes — each key overrides `strategy.params.<name>` across its value grid.
    pub axes: Vec<(String, Vec<ParamValue>)>,
}

/// One sweep grid point's outcome — the wire mirror of `vike_studio_core::ParamscanEntry`. Reuses
/// [`BacktestResultWire`] verbatim, so a sweep entry carries exactly what a single `RunSlice` would.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamscanEntryWire {
    /// The `(name, value)` overrides that produced this point — one row of the cartesian grid, each
    /// value the axis value as sent.
    pub overrides: Vec<(String, ParamValue)>,
    /// The rendered backtest answer for this point.
    pub result: BacktestResultWire,
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
pub struct StudioParamscanWire {
    /// The grid points, ranked best-first by annualized Sharpe (the `StudioParamscan::entries` order).
    pub entries: Vec<ParamscanEntryWire>,
    /// Deflated Sharpe across the trials, or `None` when non-finite (a degenerate/short slice).
    pub dsr: Option<f64>,
    /// Probability of backtest overfitting (CSCV), or `None` when not assessable (`< 2` trials, a
    /// too-short slice, or non-finite returns — the `NaN` case studio-core documents).
    pub pbo: Option<f64>,
    /// Index of the best entry within `entries` — always `0` (entries are pre-ranked), kept explicit
    /// to mirror `StudioParamscan::best_index`.
    pub best_index: usize,
    /// **What this sweep's winner was chosen under** — see [`CostModelStampWire`]. On the CONTAINER, not
    /// on each entry: one cost model priced every point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<CostModelStampWire>,
}

/// The walk-forward configuration a `RunWalkforward` executes — the wire mirror of
/// `run_walkforward_slice`'s `n_splits` parameter, plus the optional window [`Self::search`].
///
/// The `WalkforwardMode` (`Anchored`) is hardcoded in `vike_backtesthub::studio_run::run_walkforward_slice_searching`,
/// and the annualization is DERIVED there from [`DataSliceWire::interval`]
/// (`vike_analytics::report::periods_per_year_for_interval`). ⚠ A `periods_per_year` FIELD would let
/// a caller send a scale contradicting that interval (a hardcoded `252` once put a `1h` slice's
/// `out_of_sample_sharpe` `sqrt(24)` low); the scale is a function of the slice, so only the slice carries it.
///
/// Not `Copy`/`Eq`: [`Self::search`] owns a grid. [`WalkforwardWire::fixed`] spells the fixed walk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalkforwardWire {
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
    /// [`crate::BacktesthubClient::run_walkforward`] (decision 0112, verdict 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<WalkforwardWindowSearchWire>,
}

impl WalkforwardWire {
    /// The FIXED-parameter walk: no window search.
    pub fn fixed(n_splits: usize) -> Self {
        WalkforwardWire { n_splits, search: None }
    }

    /// A walk whose windows SEARCH — see [`WalkforwardWindowSearchWire`] for what the server does with it.
    pub fn searching(n_splits: usize, search: WalkforwardWindowSearchWire) -> Self {
        WalkforwardWire { n_splits, search: Some(search) }
    }
}

/// **WHICH search a walk-forward window runs on its own training half, and under what ranking** —
/// the Studio DTO door's spelling of the profile door's `[walkforward].search` /
/// `[walkforward].rank_by` keys.
///
/// ⚠ **The strings are the operator's own TOKENS, not typed scalars** (as [`crate::proto::SearchSelectionWire`]'s
/// knobs are): their authority runs on the SERVER
/// (`vike_backtest::walkforward::runner::WalkforwardWindowSearch::from_str_ci`,
/// `vike_backtest::harness::RankMetric::from_str_ci`), and typed fields would put a second parser
/// in this crate, which must not grow a `vike-backtest` dependency.
///
/// ⚠ **Not [`crate::proto::SearchSelectionWire`]**, which names a search METHOD for the PROFILE-shaped paramscan
/// verb; this one says whether a WINDOW searches at all, and carries its grid. Its "second place to
/// say 'optimize'" objection is about one verb with TWO doors; this verb has no profile, so this
/// field is the only place the question can be asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalkforwardWindowSearchWire {
    /// `"none"` (the control) or `"paramscan"`. Parsed on the SERVER; anything else is refused there
    /// BY NAME rather than falling through to the fixed walk.
    pub method: String,
    /// The `(name, values)` axes each window scores on its training half — the same grid
    /// [`ParamscanWire`] carries for the sweep verb, reused rather than re-spelled.
    pub grid: ParamscanWire,
    /// Which metric picks each window's winner (`"sharpe"` / `"return"` / `"max_dd"` / `"equity"`,
    /// case-insensitive). `None` = the server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_by: Option<String>,
}

impl WalkforwardWindowSearchWire {
    /// Whether a daemon that does not advertise [`crate::proto::FEATURE_WALKFORWARD_SEARCH`] would
    /// answer this selection DIFFERENTLY from what it asks — the predicate
    /// [`crate::BacktesthubClient::run_walkforward`] refuses on.
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
/// asked?" — yes for this one value ([`WalkforwardWindowSearchWire::needs_capability`] is `false`); every other
/// spelling, known or not, is the server's to accept or refuse.
pub const NO_WINDOW_SEARCH: &str = "none";

/// The rendered answer of a `RunWalkforward` — a FULL mirror of
/// `vike_analytics::result::WalkforwardReport` (the stitched OOS equity curve and its summary
/// stats). Every scalar is finite in normal operation, so none needs [`StudioParamscanWire`]'s
/// `Option` guard, and a window's chosen parameters cross as the `ParamValue`s the window chose (see
/// [`WalkforwardWindow`]'s `chosen_params`). A type of its own because the report also crosses as JSON TEXT,
/// in another key order and under other skip rules (`RunWalkforwardProfile`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalkforwardReportWire {
    /// Per-window outcomes, in split order: the report's own windows, whose serde is this wire's.
    pub windows: Vec<WalkforwardWindow>,
    /// The stitched out-of-sample equity curve across all windows.
    pub out_of_sample_equity_curve: Vec<f64>,
    /// Total OOS return across the stitched curve (a fraction).
    pub out_of_sample_return: f64,
    /// Annualized Sharpe of the stitched OOS returns.
    pub out_of_sample_sharpe: f64,
    /// Fraction of windows profitable out-of-sample (the `overfit_verdict` input).
    pub walkforward_consistency: f64,
    /// **What this walk's windows were costed under, and — when they SEARCHED — what chose each
    /// winner.** See [`CostModelStampWire`]. The profile walk counts no fills per window, so its realised
    /// mix reads 0 and its `not_modelled` says so (`vike_studio_core::cost_model`'s
    /// `WALKFORWARD_FILL_MIX`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<CostModelStampWire>,
    /// What the walk found wrong with its data and ran anyway (`WalkforwardReport::data_warnings`, a
    /// perpetual with no stored funding among them), [`BacktestResultWire::data_warnings`]' twin. Absent
    /// from a clean walk's frame, so that frame is byte-identical to one written before this field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_warnings: Vec<String>,
    /// NET perp funding over the out-of-sample windows (`WalkforwardReport::funding_paid`),
    /// [`BacktestResultWire::funding_paid`]' twin and absent from the frame on the same terms.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub funding_paid: f64,
    /// The currency the walk's money is in WHEN IT IS NOT THE DOLLAR
    /// (`WalkforwardReport::account_currency`): the equity curve is then an amount of it. Absent for the
    /// dollar, so a dollar walk's frame is byte-identical to one written before this field, and an
    /// old frame reads as the dollar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_currency: Option<String>,
    /// The strategy file the run ran (the profile's `[strategy] file`) and the sha256 of its text
    /// as read: `vike_analytics::report::BacktestReport`'s two fields, attached by the caller to
    /// the top-level answer like the cost stamp. Absent where none was recorded, so such a frame is
    /// byte-identical to one written before the fields, and an old frame reads as none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy_file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy_sha256: String,
}

#[path = "wire_studio_tests.rs"]
#[cfg(test)]
mod wire_studio_tests;
