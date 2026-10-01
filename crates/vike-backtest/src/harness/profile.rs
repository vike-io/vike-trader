//! `BacktestProfile` — the TOML config for the backtest harness (`BacktestNode`-lite, Task 1).
//!
//! A profile names a venue/symbol/interval data slice, an engine cost/cash configuration, and
//! a strategy (by registry name + arbitrary TOML params — the registry itself is a later
//! task). `from`/`to` accept either a bare epoch-ms integer (as a string) or a `YYYY-MM-DDTHH`
//! UTC hour label (mirrors `vike-backfill`'s `pmxt_backfill` hour-range convention); both are
//! resolved to epoch-ms by [`BacktestProfile::range`].
//!
//! # Profile widening (`cheap_np` port backlog G4/G6/G7)
//!
//! Three knobs the engine already had but TOML could not reach, added without changing any
//! existing profile's meaning:
//!
//! * **G4 — cross-venue / multi-series slices.** [`DataCfg`] accepts EITHER the frozen
//!   `venue = "…"` + `symbols = [ … ]` single-venue pair OR an `[[data.series]]` array whose
//!   entries each carry their own `venue`/`symbol`/`kind`. Exactly one of the two forms must be
//!   present — mixing them is a validation error, not a silent precedence rule.
//! * **G6 — binary-resolution settlement.** `[engine.resolution]` builds the
//!   [`vike_sim::EngineParams::resolution`] source (and its `resolution_end_ts`) that
//!   `SimBroker::settle_at_payout` has always consumed but no profile could configure.
//! * **G7 — a real fee SCHEDULE, not just a flat rate.** `[engine.fee]` selects a
//!   [`vike_model::FeeSchedule`]; the prediction-market `probability_scaled` curve
//!   (`qty × rate × p(1−p)`) is the shape a flat `fee_rate` cannot express at all.
//!   ⚠ It was the ONLY shape this table reached for a long time, so the equities per-share +
//!   MINIMUM shape and Deribit's premium-capped options shape were costs a profile could not
//!   name. [`FeeCfg`] reaches all five now, plus `kind = "venue"` — the venue's own published
//!   schedule, resolved through the same `fee_lane` + `fee_schedule_for` pair the paper mount
//!   uses, so a backtest and the paper mount of one instrument cannot disagree about cost for no
//!   reason but a missing lookup. That type's doc carries the argument for each.
//!
//! ```toml
//! [data]
//! kind = "tick"
//! from = "1775000000000"
//! to   = "1775002000000"
//!
//! [[data.series]]                       # the reference series: quotes only, its own venue
//! venue = "spot"
//! symbol = "BTCUSDT"
//! kind = "quote"
//!
//! [[data.series]]                       # the tradeable outcome tokens: the taker tape only
//! venue = "polymarket"
//! symbol = "btc-updown-5m-1775001600#0"
//! kind = "trade"
//!
//! [engine]
//! cash = 1000.0
//!
//! [engine.fee]
//! kind = "probability_scaled"
//! taker_rate = 0.072
//!
//! [engine.resolution]
//! kind = "binary_outcome"
//! [engine.resolution.winners]
//! "btc-updown-5m-1775001600" = 0
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use super::HarnessError;
use super::sweep::RankMetric;
use super::walkforward::WindowSearch;
use vike_sim::ResolutionSource;
use vike_strategy::cheap_np::TokenId;
// The coverage/universe vocabulary lives with the pre-flight that enforces it, not here: this file
// RESOLVES the three `[data]` keys into those values and the gate is what applies them. Naming the
// types there rather than declaring copies here is what keeps `data.on_gap`'s accepted set and the
// disposition the gate actually switches on the same enumeration.
use crate::data_plan::{CoverageGate, OnGap, UniverseMode};
use crate::hist_replay::{SeriesKind, SeriesRef};
use vike_analytics::sizing::{
    DrawdownThrottleSizer, FixedDollarSizer, FixedSharesSizer, MaxRiskPctSizer, PassThroughSizer,
    PctEquitySizer, PctVolatilitySizer, PortfolioHeatSizer, PositionSizer,
};
use vike_analytics::validation::WalkMode;
use vike_data::TsRange;
use vike_exec::ProfileRisk;
use vike_model::time::{Span, parse_span};
use vike_model::{Diagnostic, FeeSchedule};
use vike_sim::QueueModelKind;
use vike_sim::{DecideMode, EquitySampling, FillModelKind};

/// The top-level backtest profile: what data to run, how the engine is configured, and which
/// strategy to run. Unknown top-level keys are a hard parse error (typos should not silently
/// no-op).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestProfile {
    /// Free-form label (logs/report titles only — not used for lookup).
    #[serde(default)]
    pub name: Option<String>,
    pub data: DataCfg,
    pub engine: EngineCfg,
    pub strategy: StrategyCfg,
    /// Optional pre-trade `RiskGate` limits (runprofile-wiring-step2). The SAME `[risk]` schema
    /// AND the SAME compile-checked converter ([`vike_exec::ProfileRisk::to_risk_limits`])
    /// `vike-core`'s `RunProfile` uses for paper/live — so a limit proven here (via
    /// [`BacktestProfile::validate`] + a real `SimBroker` denial) carries unchanged into paper and
    /// live, rather than being re-derived by a parallel converter that could drift. Absent (the
    /// default) ⇒ [`vike_sim::EngineParams::risk_limits`] stays `None` ⇒ byte-identical to every
    /// profile written before this field existed: `[engine]` today has no `leverage` knob either,
    /// so `[risk]` is the ONLY way a harness profile arms the gate at all
    /// ([`vike_sim::SimBroker::build_risk_gate`]'s `(None, None) => None` arm).
    ///
    /// ONE FIELD IS REJECTED, not silently ignored: `risk.max_orders_per_window` is a WALL-CLOCK
    /// order-rate throttle, and sim time is not wall time (`build_risk_gate` always disarms it
    /// live-side too, for the identical reason) — see [`BacktestProfile::validate`]. Every other
    /// `risk.*` limit is honored exactly as the live gate honors it.
    #[serde(default)]
    pub risk: Option<ProfileRisk>,
    /// Optional PARAMETER-SEARCH grid: each key is a `strategy.params` field name, each value an
    /// array of TOML values to cross-product over (expansion lives in
    /// [`super::sweep::expand_paramscan`]). Absent or empty means "not a parameter search".
    ///
    /// # `[paramscan]` is the name; `[sweep]` loads FOREVER
    ///
    /// Ruling R2 of `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` renamed the
    /// SECTION: nineteen competitor CLIs were measured and exactly one uses the word "sweep" (an
    /// npm package), while QuantRocket calls it `paramscan`, LEAN `optimize` and Freqtrade
    /// `hyperopt` — the last of which would LIE here, because it names Bayesian search specifically
    /// and this engine has grid, euler, tpe and genetic.
    ///
    /// ⚠ **`#[serde(alias = "sweep")]` is PERMANENT and is not a deprecation with an end date.**
    /// Profiles exist on operators' disks and in this repository's own `profiles/*.toml` fixtures,
    /// and [`BacktestProfile`] is `#[serde(deny_unknown_fields)]` — so without the alias every
    /// `[sweep]` profile in the world would fail to load with "unknown field", which is the
    /// `VIKE_MAX_ORDER_NOTIONAL` shape (a written value an operator already has, refused) applied
    /// to a file rather than to an environment variable. The alias costs one attribute and makes
    /// the rename free. Removing it is not a future tidy-up; it is a breaking change to every
    /// profile ever written.
    ///
    /// ⚠ Writing BOTH spellings in one file is a serde duplicate-field error, which is the correct
    /// answer: two grids in one profile has no meaning, and a silent winner would be the
    /// different-answer defect this whole stage exists to end.
    #[serde(default, alias = "sweep")]
    pub paramscan: Option<toml::Table>,
    /// Optional anchored WALK-FORWARD config — the `[paramscan]` sibling: present means this profile
    /// can ALSO be run through [`super::walkforward::run_walkforward`] over
    /// `walkforward.n_splits` out-of-sample windows. Absent (the default) is byte-identical to
    /// every profile written before this field existed, and `run_backtest`/`run_paramscan` ignore the
    /// section entirely — exactly as they ignore a `[sweep]` table they were not asked to expand.
    ///
    /// It lives IN the profile (rather than riding a wire field) because [`BacktestProfile`] is
    /// `deny_unknown_fields`: a profile carrying `[walkforward]` must parse for the whole TOML to
    /// be shippable verbatim to a remote runner, which is the point of the compute-to-data
    /// `RunWalkforwardProfile` verb.
    #[serde(default)]
    pub walkforward: Option<WalkforwardCfg>,
    /// Directory the profile FILE was read from — set by [`BacktestProfile::from_path`], `None`
    /// for a profile parsed from a string. Relative paths inside the profile (today just
    /// `[engine.resolution].path`) resolve against it, so a profile + its sidecar CSV move
    /// together instead of depending on the caller's CWD. Never a TOML key (`#[serde(skip)]`).
    #[serde(skip)]
    pub base_dir: Option<PathBuf>,
}

/// The `[walkforward]` section: how many out-of-sample windows to walk this profile's bar slice
/// over ([`super::walkforward::run_walkforward`]), and — since the optimizing driver landed —
/// whether each window SEARCHES its own training half before it trades its validation half
/// ([`super::walkforward::run_walkforward_optimized`]).
///
/// ```toml
/// [walkforward]
/// n_splits = 6         # window form (a): the number of out-of-sample windows
/// search  = "sweep"    # absent / "none" = the CONTROL; "sweep" searches the [sweep] table
/// mode    = "rolling"  # absent / "anchored" = expanding train; "rolling" = the preceding chunk
/// rank_by = "return"   # absent / "sharpe"; also "return" | "max_dd" | "equity"
/// ```
///
/// ...or the DURATION window form, which is the same walk with its windows named in time rather
/// than counted out of the range ([`Self::window_form`] is the one place the two are told apart,
/// and declaring both is refused by name):
///
/// ```toml
/// [walkforward]
/// train   = "12mo"     # window form (b)/(c): a `vike_model::time::parse_span` duration
/// test    = "3mo"      # required with `train`
/// step    = "1mo"      # absent ⇒ equal to `test`, which tiles the validation windows
/// purge   = "1w"       # absent ⇒ zero; DURATION FORM ONLY (decision 0046)
/// embargo = "1w"       # absent ⇒ zero, the identity; duration form only
/// ```
///
/// Every knob but the window form itself defaults to the behaviour that existed before it did, so
/// a profile written against the one-field version of this section parses and runs
/// byte-identically.
///
/// # The asymmetry with the Studio is REAL now, and it is deliberate
///
/// This doc used to argue the opposite — that only the split count was configurable, so there was
/// "no way for a profile to disagree with the Studio on what walk-forward means". That argument
/// stopped being true the moment the profile path grew a capability the Studio's DTO path does not
/// have, and a doc still making it would be the last place in the tree claiming a parity that no
/// longer holds.
///
/// The asymmetry is not a gap to close later. `vike_studio_core`'s wire door carries a
/// `WireEngineParams` of three flat scalars — cash, fee rate, slippage — and cannot express a fee
/// SCHEDULE, `[engine.impact]`, `[engine.resolution]`, a `[risk]` table, `snap_to_properties` or
/// `attach_funding`. A window that picks a winner is only as trustworthy as the cost model it
/// picked under, so a GUI arm of this feature would be a strictly lesser version of the same
/// thing: confident per-window winners chosen under costs the operator could not configure. The
/// search knobs therefore live on the door that carries the whole `[engine]` surface, and the
/// Studio keeps the fixed-parameter walk it can serve honestly.
///
/// What is still DERIVED and still not settable here: the annualization
/// ([`super::report::periods_per_year`]). [`Self::mode`] became settable because it changes WHICH
/// BARS a window trains on — a question about the protocol — while annualization changes only the
/// units the same numbers are reported in, and two reports over one slice that disagreed about it
/// would be incomparable for a reason no reader of either could see.
///
/// ⚠ That last point is not theoretical, and the incident is why an annualization knob will not be
/// added here. This section once claimed the mode and the annualization were derived "exactly as
/// the Studio's walk-forward derives them", and the annualization half was FALSE: the Studio
/// passed a bare `252.0` at every interval, so one strategy over one 1h series reported an
/// `oos_sharpe` `sqrt(24) ≈ 4.9x` apart between the two doors — with a doc comment on each side
/// asserting they could not disagree, and no test comparing them. Both planes now reach the ONE
/// derivation in vike-analytics, and two tests — never a comment — are what fail if either drifts
/// off it: one per door, at a non-daily interval, the split named on
/// [`super::walkforward::run_walkforward`]'s own doc. A `[walkforward]` key that set the
/// annualization outright would reopen exactly that divergence, which is why the absent knob is
/// the feature.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalkforwardCfg {
    /// Number of out-of-sample windows — window form (a), the one that existed first.
    ///
    /// ⚠ OPTIONAL since the duration form landed, and the emptiness is not a default: a
    /// `[walkforward]` table declaring NEITHER form is refused by name
    /// ([`Self::window_form`]), because a walk with no window shape is not a walk. Must be
    /// `>= 1` when present; a negative TOML integer fails to deserialize into `usize`.
    ///
    /// A count is defined BY the range and moves when the range moves; a duration does not.
    /// That is why declaring this beside [`Self::train`] is a refusal rather than a
    /// precedence rule.
    #[serde(default)]
    pub n_splits: Option<usize>,
    /// Window form (b)/(c) — the TRAIN window's length, as a
    /// [`vike_model::time::parse_span`] duration (`"12mo"`, `"90d"`, `"5000bars"`).
    /// Mutually exclusive with [`Self::n_splits`]; requires [`Self::test`].
    ///
    /// Calendar and bar suffixes may be MIXED across these three fields — they are one form,
    /// and each span resolves to a bar count independently
    /// (`super::windows::resolve_windows`).
    #[serde(default)]
    pub train: Option<String>,
    /// Window form (b)/(c) — the VALIDATION window's length. Required with [`Self::train`].
    #[serde(default)]
    pub test: Option<String>,
    /// How far each window advances. Absent ⇒ equal to [`Self::test`], which tiles the
    /// validation windows edge to edge.
    ///
    /// ⚠ Must resolve to at least as many bars as [`Self::test`], and a SHORTER one is refused
    /// by name (`super::windows::resolve_windows`) rather than run: overlapping validation
    /// windows are folded onto ONE running equity by
    /// [`crate::walkforward::walk_forward_over_windows`], so the same bars would be compounded
    /// more than once and the reported out-of-sample return and Sharpe would both come back
    /// roughly the overlap factor too high. A LARGER step is legal — that is separation, which
    /// is what [`Self::embargo`] produces deliberately.
    ///
    /// Rounds DOWN to a whole number of bars, like the other two window LENGTHS and unlike the
    /// two gaps below.
    #[serde(default)]
    pub step: Option<String>,
    /// Bars dropped BETWEEN train and test, so `test_start - train_end == purge` exactly.
    ///
    /// ⚠ Legal on the duration form ONLY. On [`Self::n_splits`] it is refused by name:
    /// `docs/decisions/0046-the-bar-mode-walk-forward-has-no-purge.md` is accepted and rules
    /// that the split-count walk stays gap-free.
    /// `docs/decisions/0053-purge-and-embargo-on-the-duration-walk-forward.md` is the
    /// re-argument that admits it here, and it does NOT rest on the leak 0046 refuted.
    ///
    /// ⚠ What this key does NOT buy, stated where an operator meets it: a purge guards a
    /// forward-looking LABEL — a training row whose target is realised after the row's own
    /// timestamp — and a PARAMETER SEARCH has no such row, because a candidate is scored by
    /// being RUN over bars that all lie strictly before `test_start`. For a parameter search
    /// alone this gap costs warm-up and buys nothing (0046 measured exactly that), which is
    /// why it defaults to absent. It exists so a protocol an operator specifies in durations
    /// can be RUN, and because the same splitter carries the label case for the `research`
    /// plane.
    ///
    /// ⚠ ROUNDS UP to a whole number of bars, unlike [`Self::train`]/[`Self::test`]/
    /// [`Self::step`], which round down. A gap is the one quantity where the rounding
    /// direction is a safety property rather than a tolerance: flooring hands back LESS
    /// separation than was asked for (`purge = "90m"` at `interval = "1h"` would be a
    /// 60-minute gap), and a gap that is shorter than one bar therefore becomes ONE bar rather
    /// than being refused. `super::windows::resolve_windows`'s `Rounding` carries the argument.
    #[serde(default)]
    pub purge: Option<String>,
    /// The minimum gap AFTER a validation window before the next one may start. A window
    /// whose `test_start` falls inside the zone is DROPPED, not moved — moving it would make
    /// [`Self::step`] mean something other than what was written. Same duration-form-only
    /// rule as [`Self::purge`], and the same ROUNDS-UP rule.
    ///
    /// ⚠ This key borrows López de Prado's WORD and does not carry his semantics. His embargo
    /// removes bars from a later window's TRAIN half; this one filters whole windows out of the
    /// list, because a `vike_analytics::validation::Split` is a contiguous half-open quadruple with
    /// nowhere to put the hole a train-side embargo would need.
    /// `docs/decisions/0053-purge-and-embargo-on-the-duration-walk-forward.md` argues the
    /// difference and names the train-side form as a reopener; read it before assuming this key
    /// means what a paper says it means.
    #[serde(default)]
    pub embargo: Option<String>,
    /// Which search each window runs on its OWN training half — the knob that chooses between the
    /// two walk-forward protocols. Absent or `"none"` is the CONTROL ([`WindowSearch::None`]):
    /// every window trades the profile's own `[strategy.params]`, which is the fixed-parameter
    /// walk this section has always described. `"sweep"` — named for the table
    /// it expands — scores this profile's `[sweep]` grid on each window's train half and carries
    /// only the winner onto its validation half.
    ///
    /// Case-insensitive and trimmed ([`Self::window_search`]); an unrecognized value is a
    /// fail-fast [`HarnessError::Validation`] at LOAD naming the valid set, never a silent
    /// fallback — the [`EngineCfg::fill_model`] rule, and the stake here is higher than a fill
    /// model's: a `search = "gird"` that degraded to the control would run the no-search walk
    /// while the operator read the report as an optimization's.
    ///
    /// `"sweep"` with no `[sweep]` table is also refused at load — see [`BacktestProfile::validate`]
    /// for why that one is a load-time rule where the bar-mode and single-symbol rules are not.
    #[serde(default)]
    pub search: Option<String>,
    /// The TRAIN-window shape ([`WalkMode`]): absent or `"anchored"` is the expanding window every
    /// split trains from bar 0; `"rolling"` trains on the ONE chunk immediately preceding the
    /// window. Case-insensitive and trimmed ([`Self::walk_mode`]), unknown values refused at load.
    ///
    /// ⚠ It is settable NOW because it started MEANING something now. Until
    /// [`crate::walkforward::walk_forward_strategy`] handed the training half to its window
    /// closure, nothing read `train_start` — the one field the two modes differ in — so `Rolling`
    /// and `Anchored` produced byte-identical reports and the mode was a choice between two
    /// spellings of one answer. The default stays `Anchored` for exactly that reason: every
    /// profile written before this field existed ran anchored, and a report that moved because a
    /// default flipped is indistinguishable from a strategy that changed.
    #[serde(default)]
    pub mode: Option<String>,
    /// How a window SCORES its candidates when [`Self::search`] runs one: any of the four
    /// [`RankMetric`] names the sweep's own `--rank-by` takes (`"sharpe"` | `"return"` |
    /// `"max_dd"` | `"equity"`, case-insensitive), defaulting to [`RankMetric::default`].
    /// Consulted only when a search actually runs — the control scores nothing.
    ///
    /// A [`RankMetric`] rather than a [`crate::objective::Objective`], and the difference is what a
    /// TOML file can NAME. `RankMetric` is a closed enum with an existing case-insensitive parser
    /// ([`RankMetric::from_str_ci`], shared with the sweep bin so the two doors cannot accept
    /// different spellings) and a direction-folding [`RankMetric::objective`] constructor, so
    /// string → objective is total and an unknown string fails at load with the valid set printed.
    /// An `Objective` is a `Box<dyn Fn>`: the only ones a profile could name are ones some registry
    /// named for it, and the composite `multi_metric` — the one non-`RankMetric` objective this
    /// crate ships — carries five tunables ([`crate::objective::MultiMetricParams`]), so naming it
    /// from TOML asks a second design question (does this section grow a params table, or silently
    /// pick the defaults?) that nothing on this path needs answered yet. The capability is not
    /// lost, only unspelled: [`super::walkforward::run_walkforward_optimized_with`] takes any
    /// `Objective` a caller can build.
    #[serde(default)]
    pub rank_by: Option<String>,
}

/// Which WINDOW FORM a `[walkforward]` table declares. Exactly one, always — the resolver
/// that answers this ([`WalkforwardCfg::window_form`]) is also the only place the
/// combination rules live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowForm {
    /// `n_splits = N` — form (a). The count is defined by the range.
    Splits,
    /// `train`/`test`/`step` durations — forms (b) and (c), which are one form wearing two
    /// suffix sets.
    Duration,
}

/// One `[walkforward]` duration field, parsed. Absent ⇒ `Ok(None)`; malformed ⇒ a fail-fast
/// [`HarnessError::Validation`] naming the KEY and carrying the grammar's own message,
/// matching the trim-and-fail-fast shape of [`WalkforwardCfg::walk_mode`].
fn span_field(raw: Option<&str>, key: &str) -> Result<Option<Span>, HarnessError> {
    let Some(raw) = raw else { return Ok(None) };
    parse_span(raw)
        .map(Some)
        .map_err(|e| HarnessError::Validation(format!("walkforward.{key}: {e}")))
}

impl WalkforwardCfg {
    /// Resolve which window form this section declares, refusing every incoherent
    /// combination BY NAME. The refusal shape is the mutually-exclusive-pair one
    /// [`BacktestProfile::validate_data_slice`] already uses for
    /// `data.venue`/`[[data.series]]` — "two ways to name the SAME thing, set exactly one" —
    /// not the unknown-VALUE shape [`Self::walk_mode`] uses.
    pub fn window_form(&self) -> Result<WindowForm, HarnessError> {
        let durations = self.train.is_some() || self.test.is_some() || self.step.is_some();
        match (self.n_splits.is_some(), durations) {
            (true, true) => Err(HarnessError::Validation(
                "walkforward.n_splits and walkforward.train/test/step are two PROTOCOLS, \
                 not two spellings — a split count is defined BY the range and moves when \
                 the range moves, a duration does not. Set exactly one."
                    .to_string(),
            )),
            (false, false) => Err(HarnessError::Validation(
                "[walkforward] declares no window form — set n_splits = N, or a train/test \
                 pair (e.g. train = \"12mo\", test = \"3mo\")"
                    .to_string(),
            )),
            (true, false) => {
                // 0046 is accepted and scoped to exactly this splitter. Refused rather than
                // ignored: a profile carrying `purge` under `n_splits` believes a gap is
                // armed, and silently walking gap-free is the failure class this repo's
                // settings work exists to kill.
                if self.purge.is_some() || self.embargo.is_some() {
                    return Err(HarnessError::Validation(
                        "walkforward.purge / walkforward.embargo need the DURATION window \
                         form (train/test) — the split-count walk stays gap-free \
                         (docs/decisions/0046-the-bar-mode-walk-forward-has-no-purge.md). \
                         Replace n_splits with a train/test pair, or drop the key."
                            .to_string(),
                    ));
                }
                Ok(WindowForm::Splits)
            }
            (false, true) => {
                if self.train.is_none() || self.test.is_none() {
                    return Err(HarnessError::Validation(
                        "the duration window form needs BOTH walkforward.train and \
                         walkforward.test (step defaults to test) — a half-declared window \
                         has no shape"
                            .to_string(),
                    ));
                }
                Ok(WindowForm::Duration)
            }
        }
    }

    /// The TRAIN span, parsed. See `span_field`.
    pub fn train_span(&self) -> Result<Option<Span>, HarnessError> {
        span_field(self.train.as_deref(), "train")
    }

    /// The VALIDATION span, parsed.
    pub fn test_span(&self) -> Result<Option<Span>, HarnessError> {
        span_field(self.test.as_deref(), "test")
    }

    /// The ADVANCE between windows, parsed. Absent ⇒ the caller substitutes `test`.
    pub fn step_span(&self) -> Result<Option<Span>, HarnessError> {
        span_field(self.step.as_deref(), "step")
    }

    /// The train→test GAP, parsed. Absent ⇒ zero, i.e. `train_end == test_start`.
    pub fn purge_span(&self) -> Result<Option<Span>, HarnessError> {
        span_field(self.purge.as_deref(), "purge")
    }

    /// The gap between consecutive VALIDATION windows, parsed. Absent ⇒ zero, the identity.
    pub fn embargo_span(&self) -> Result<Option<Span>, HarnessError> {
        span_field(self.embargo.as_deref(), "embargo")
    }

    /// Resolve [`Self::search`] to the driver's [`WindowSearch`]. Absent ⇒ [`WindowSearch::None`],
    /// the control. Case-insensitive and trimmed — the [`EngineCfg::queue_model_kind`] idiom.
    ///
    /// ⚠ ONE spelling, and `"grid"` is deliberately NOT an alias for it. Both parsed for exactly
    /// one release: the design doc said `sweep`, the first implementation said `grid`, and rather
    /// than pick, the two were accepted as synonyms. That is a knob with two names — the shape
    /// where somebody writes one, greps for the other, and concludes the feature is not wired.
    /// `sweep` won because, at the time, it named the thing it searches: `search = "sweep"` expands
    /// what was then the `[sweep]` table. `grid` was the synonym, so `grid` is the one that went.
    ///
    /// ⚠ **That premise has since renamed out from under the value, and the value STAYS anyway.**
    /// Ruling 2 renamed the table to `[paramscan]` (`[sweep]` kept as a permanent serde alias) and
    /// this crate's vocabulary for the object with it — `run_paramscan` / `expand_paramscan` /
    /// `ParamscanPoint` / [`Self::search`]'s own [`WindowSearch::Sweep`] being the residue. But
    /// `"sweep"` here is a VALUE INSIDE A PROFILE ON AN OPERATOR'S DISK, not an identifier: the
    /// argument that put it there is spent, and the argument that keeps it is that renaming it
    /// would refuse every walk-forward profile already written. If a second spelling is ever
    /// wanted, it is an ALIAS added beside this arm, never a replacement of it — and the two-names
    /// hazard above is the reason to want a very good argument first.
    pub fn window_search(&self) -> Result<WindowSearch, HarnessError> {
        let Some(raw) = self.search.as_deref() else {
            return Ok(WindowSearch::None);
        };
        // The two accepted spellings are `WindowSearch::from_str_ci`'s, not a second `match` over
        // the same strings — the same rule `rank_metric` below already follows, so this door and
        // the Studio DTO door cannot drift apart on how `sweep` is spelled. The `"grid"` arm stays
        // HERE because it names a TOML key only this door has.
        if let Some(search) = WindowSearch::from_str_ci(raw) {
            return Ok(search);
        }
        match raw.trim().to_ascii_lowercase().as_str() {
            // Named outright rather than swept into the catch-all: a profile written against the
            // one release that accepted it gets told what to write, not just that it is wrong.
            "grid" => Err(HarnessError::Validation(
                "walkforward.search = \"grid\" was renamed to \"sweep\" — one name for one knob, \
                 and it is the table it expands ([sweep]). Write search = \"sweep\"."
                    .to_string(),
            )),
            other => Err(HarnessError::Validation(format!(
                "unknown walkforward.search {other:?} (want none | sweep; absent = none, the \
                 no-search control)"
            ))),
        }
    }

    /// Resolve [`Self::mode`] to a [`WalkMode`]. Absent ⇒ [`WalkMode::Anchored`], the shape every
    /// profile written before this knob existed was walked under. Same trim + lowercase idiom as
    /// [`Self::window_search`], same fail-fast rule.
    pub fn walk_mode(&self) -> Result<WalkMode, HarnessError> {
        let Some(raw) = self.mode.as_deref() else {
            return Ok(WalkMode::Anchored);
        };
        match raw.trim().to_ascii_lowercase().as_str() {
            "anchored" => Ok(WalkMode::Anchored),
            "rolling" => Ok(WalkMode::Rolling),
            other => Err(HarnessError::Validation(format!(
                "unknown walkforward.mode {other:?} (want anchored | rolling; absent = anchored)"
            ))),
        }
    }

    /// Resolve [`Self::rank_by`] to a [`RankMetric`]. Absent ⇒ [`RankMetric::default`]. The parse
    /// is [`RankMetric::from_str_ci`] itself rather than a second `match` over the same four
    /// strings, so this door and the sweep bin's `--rank-by` cannot drift apart on spelling; the
    /// `trim` is this side's own, matching the neighbouring resolvers.
    pub fn rank_metric(&self) -> Result<RankMetric, HarnessError> {
        let Some(raw) = self.rank_by.as_deref() else {
            return Ok(RankMetric::default());
        };
        RankMetric::from_str_ci(raw.trim()).ok_or_else(|| {
            HarnessError::Validation(format!(
                "unknown walkforward.rank_by {raw:?} (want sharpe | return | max_dd | equity; \
                 absent = sharpe)"
            ))
        })
    }
}

/// Which data slice to load: bar/tick kind + interval (bars only) + range, over EITHER a
/// single-venue `venue` + `symbols` pair (the frozen form) OR a cross-venue [`Self::series`]
/// array (port backlog G4). Exactly one of the two forms must be present.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataCfg {
    /// Single-venue form: the one venue every entry of [`Self::symbols`] lives under. Mutually
    /// exclusive with [`Self::series`].
    #[serde(default)]
    pub venue: Option<String>,
    /// Single-venue form: the symbols to load under [`Self::venue`]. Mutually exclusive with
    /// [`Self::series`].
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Cross-venue form (`[[data.series]]`): one entry per series, each naming its own
    /// `venue`, `symbol` and (tick mode only) lane `kind`. Mutually exclusive with
    /// [`Self::venue`]/[`Self::symbols`].
    ///
    /// The ENGINE always supported this — `StrategyEngine::run_ticks` routes each tick by its
    /// own payload symbol — so a strategy that needs a non-tradeable reference feed alongside
    /// its tradeable instruments (the `cheap_np` case: Polymarket outcome tokens driven by a
    /// BTC spot series) was blocked only by this loader. Order is preserved and IS meaningful:
    /// `run_ticks`'s k-way merge breaks an equal-`ts` tie by stream order, so listing the
    /// reference series FIRST delivers a same-millisecond reference sample BEFORE the print it
    /// should inform.
    #[serde(default)]
    pub series: Vec<SeriesRef>,
    pub kind: DataKind,
    #[serde(default = "default_interval")]
    pub interval: String,
    /// Bare epoch-ms integer (as a string) or `YYYY-MM-DDTHH` UTC — see [`BacktestProfile::range`].
    pub from: String,
    /// Same format as `from`.
    pub to: String,
    /// How much history the strategy's indicators need before the run's FIRST tradeable event —
    /// a bar/event count (`"200bars"`) or, in bar mode, a duration (`"7d"`) the base
    /// [`Self::interval`] divides into one. Absent (the default) is byte-identical to every
    /// profile written before this key existed: the gate stays exactly `Strategy::warmup()`.
    ///
    /// # Why an operator needs to declare this at all
    ///
    /// An indicator's value depends on how much history preceded it. A 200-period EMA read 20
    /// bars into a window is not the same number as the same EMA read 400 bars in, and neither
    /// the profile nor the report says which one the run used — so two runs over two windows
    /// produce two answers and nothing in either names the reason. Declaring the requirement
    /// makes it a FACT OF THE PROFILE rather than a property of where the window happened to
    /// start, and [`vike_analytics::BacktestResult::warmup`] then reports the number the run actually
    /// gated on (`vike_analytics::zero_trade`'s `warmup-shortfall` cause reads it, so a warmup
    /// that swallows the whole slice diagnoses itself instead of reading as "strategy did
    /// nothing"). Freqtrade spells it `startup_candle_count`, LEAN `SetWarmUp`, QuantRocket
    /// `LOOKBACK_WINDOW`.
    ///
    /// # ⚠ It is a FLOOR on `Strategy::warmup()`, never an override — including downwards
    ///
    /// The engine gates every strategy callback on `index >= warmup` and the number it uses is
    /// `max(this, strategy.warmup())` (`vike_sim::StrategyEngine::new` resolves it once). A profile
    /// can therefore RAISE the gate and can never lower it, and the asymmetry is deliberate: the
    /// strategy's own `warmup()` is a statement about ITS indicators that a profile's author may
    /// not know — a `[strategy.params]` lookback the fixed `warmup()` does not scale with, a
    /// scripted indicator whose depth the Rust impl cannot see — so honouring a smaller profile
    /// number would silence a real requirement and hand back exactly the unreproducible answer
    /// this key exists to end. Writing `warmup = "0bars"` is refused by the grammar itself
    /// ([`vike_model::time::parse_span`] rejects a zero count), so there is no spelling that
    /// reads as "turn the strategy's own warmup off".
    ///
    /// # ⚠ What it does NOT do: it does not widen the loaded range
    ///
    /// The warmup is paid out of the FRONT of the window `from`/`to` already names — nothing is
    /// prepended, and `from` still means what it said. That is correct for this engine rather
    /// than a shortcut: a strategy here reads its history back off the broker
    /// (`vike_sim::SimBroker`'s bar reads), not out of state accumulated inside gated
    /// `on_bar` calls, so the bars before the gate opens ARE the warmup history and they are
    /// already loaded. The consequence to own is that the TRADEABLE window is shorter than the
    /// requested one by this much, and the equity curve still carries one flat sample per gated
    /// step — so every curve-derived figure (sharpe above all) is computed over a series that
    /// opens with `warmup` motionless samples. Widen the range if that matters; a profile key
    /// that silently moved `from` would make two profiles with the same `from` mean two ranges.
    #[serde(default)]
    pub warmup: Option<String>,
    /// Opt-in INTRABAR DETAIL TAPE: a per-symbol series FINER than [`Self::interval`], loaded
    /// over the same range and bucketed into each coarse step, against which every order is
    /// resolved while the strategy keeps deciding on the coarse timeframe. Absent (the default)
    /// is byte-identical to every profile written before this key existed. BAR MODE ONLY.
    ///
    /// # The ambiguity it removes, and the guess it replaces
    ///
    /// A bar carries open/high/low/close and no path. When a stop and a target both sit inside
    /// one bar, OHLC cannot say which was touched first, and the engine must currently GUESS:
    /// `vike_fills::resolve_intrabar_fills` orders the triggered pair ADVERSE-FIRST and caps the
    /// total reduction to the position, which is the pessimistic bound rather than the truth. It
    /// counts each such bar in [`vike_analytics::BacktestResult::intrabar_both_hit`] — so the engine has
    /// always known exactly which bars it was guessing on. With a detail series present that
    /// symbol's step instead runs `vike_sim::StrategyEngine::fill_pending_granular`, which walks the
    /// sub-bars IN TIME ORDER and fills whichever level the finer series reaches first. The
    /// difference between a plausible backtest and an optimistic one is this one ordering.
    /// Freqtrade ships it as `--timeframe-detail`; NinjaTrader and MultiCharts have the same
    /// thing.
    ///
    /// # ⚠ It changes what `intrabar_both_hit` MEANS, and that is not a bug to report
    ///
    /// The counter only ever increments on the coarse lane, so a symbol with a detail series
    /// contributes ZERO to it however ambiguous its bars were. Read it as "bars whose order the
    /// engine had to guess", never "ambiguous bars": with detail on, a `0` says the guessing
    /// stopped, not that the ambiguity was absent. A run comparing the two lanes should compare
    /// PnL, not this counter.
    ///
    /// # ⚠ The detail lane is not only a finer fill tier — it is a finer PRINT tier
    ///
    /// `fill_pending_granular` also calls `note_print` and `check_stop` per sub-bar, so the
    /// opt-in staleness discipline (`vike_fills::staleness`) measures age against the sub-bar rather
    /// than the coarse step, and a protective stop is evaluated once per sub-bar. That is the
    /// point of having the tier, but it means turning this on can move a run that never had an
    /// ambiguous bracket at all. It also omits the coarse lane's dust guard (a sub-`1e-12` size
    /// is dropped on the coarse path and dispatched here), preserved from the frozen engine.
    ///
    /// # ⚠ It is for a DIRECTIONAL strategy — a TAGGED maker quote never fills on this lane
    ///
    /// `StrategyEngine::fill_pending` runs the tagged (`vike_model::HftBroker`) maker lane before
    /// its pending loop; `StrategyEngine::fill_pending_granular` has no such call, and a symbol
    /// with a detail tape takes the granular path INSTEAD of the coarse one — so a resting tagged
    /// quote on that symbol is not filled late, it is not filled at all. The hole predates this
    /// key (the lane was reachable from no profile) and cannot be refused from a profile, because
    /// nothing in a profile says whether the strategy quotes with tags. Use this key for
    /// stop-versus-target realism on a directional strategy; a maker backtest belongs on the tick
    /// lane's `engine.queue_model`, whose queue-gated tagged twin does exist.
    ///
    /// # ⚠ Two refusals, because both silences are worse than a rejection
    ///
    /// `engine.cash_gate = true` routes the whole step through `fill_step_gated` and
    /// UNCONDITIONALLY disables the granular lane (`StrategyEngine::run`'s `granular` mask is
    /// `!cash_gate && !sub[i].is_empty()`), so the two together would load, pay for and ignore
    /// the tape. And `[walkforward]` slices the coarse bars per window while nothing slices this
    /// series, so every window's LAST coarse step would absorb the whole remaining tape's
    /// sub-bars — look-ahead, not merely waste. Both are refused rather than warned; the pair is
    /// checked in [`BacktestProfile::refusals`] because neither reads `[data]` alone.
    #[serde(default)]
    pub detail_interval: Option<String>,
    /// PLAN-THEN-APPLY: resolve this profile's whole data slice, report what the store holds for
    /// it, and EXIT without computing. Absent (the default) is byte-identical to every profile
    /// written before this key existed.
    ///
    /// # What it prints, and why the answer has to come from the side that RUNS
    ///
    /// Every `(kind, venue, symbol|group, interval)` the run will open — the bar lane, or every
    /// tick lane each `[[data.series]]` entry's `kind` filter admits, plus the GROUPED series the
    /// tick readers union in, which is where the rows of a recorded Polymarket tape actually live —
    /// the resolved window, the store root with the RUNG that chose it, and per series the row
    /// count, the recorded span and what the window asked for and did not get. The client cannot
    /// derive any of it: only the profile TEXT crosses the wire, and the store is on the far side.
    /// So this is a key rather than a flag the client answers, and `vike-cli backtest run
    /// --explain-data` is sugar that sets it — which is what makes the plan available on the REMOTE
    /// route at all, with no wire verb of its own (`crate::compute_server`'s `run_backtest` returns
    /// it as the run's report document and computes nothing).
    ///
    /// # ⚠ It is a REHEARSAL, not a dry run of the engine
    ///
    /// It proves what the store holds. It does not compile the strategy, build the engine params,
    /// or resolve `[engine.resolution]`'s winners sidecar — a profile that plans cleanly can still
    /// fail on any of those. `BacktestProfile::validate` is what answers for the profile's own
    /// shape, and it has already run by the time this is read.
    ///
    /// Prior art: terraform's `plan`, freqtrade's `list-data --show-timerange`.
    #[serde(default)]
    pub explain: bool,
    /// ARM THE COVERAGE GATE: refuse (or warn on) a run whose window the store does not cover.
    /// Absent (the default) is byte-identical to every profile written before this key existed —
    /// the run loads whatever is there and says nothing.
    ///
    /// # The failure this ends, which is the theme of the whole data plane
    ///
    /// A window with a complete trade tape and no book at all runs to completion and REPORTS
    /// FILLS. Per series nothing looks wrong: the trades are contiguous, and the book series simply
    /// has no rows there. `crates/vike-data/src/coverage.rs`'s module doc carries the measurement —
    /// Polymarket's own API serves no book history, so "I filled that gap from the venue" produces
    /// exactly that shape — and the same silence covers the cheaper case of a window that opens
    /// before the tape does, which `vike_data::find_gaps` cannot see BY CONSTRUCTION because its
    /// holes are strictly inside the recorded span.
    ///
    /// # ⚠ What it is NOT: a row-level trust check
    ///
    /// It knows what the file index knows — which days exist and how many rows they hold. A day
    /// that is PRESENT and was recorded through a feed outage passes this gate;
    /// `vike_data::quality` is the question over scanned rows, and a coverage gate that implied
    /// otherwise would be worse than none.
    ///
    /// [`Self::on_gap`] chooses the disposition (default: refuse) and [`Self::max_gap`] the
    /// tolerance (default: none, so any missing span is a finding). Both are REFUSED without this
    /// key, in [`BacktestProfile::refusals`] — a disposition or a tolerance for a gate nobody armed
    /// is a setting that does nothing, which is the class `vike_config::CONSUMPTION` exists to
    /// refuse.
    #[serde(default)]
    pub require_coverage: bool,
    /// How much of the window [`Self::require_coverage`] tolerates missing in ONE span, as a fixed
    /// duration (`"1d"`, `"4h"`, `"900000"` ms). Absent = ZERO tolerance: any missing span at all
    /// is a finding.
    ///
    /// # ⚠ Per SPAN, not per total — and an ABSENT series is never tolerated
    ///
    /// The name says gap, and a gap is one hole; a tape with fifty tolerated holes is still a tape
    /// with fifty holes, and the plan reports the total beside them so the sum is never hidden.
    /// A series the store does not hold AT ALL is exempt from this tolerance under every value: it
    /// is not a gap of some length, it is a lane that was never recorded, and a `max_gap` generous
    /// enough to swallow the window would otherwise turn the gate off for the one case it most
    /// exists to catch.
    ///
    /// A bar COUNT (`"200bars"`) is refused: the store's holes are wall-clock spans, converting a
    /// count needs an interval, and a tick profile has none — the same asymmetry
    /// [`Self::warmup`] states from the other side. A CALENDAR span (`"3mo"`) is refused for
    /// [`Self::warmup`]'s reason exactly: a month is not a fixed length, so the tolerance would
    /// depend on where the window happens to sit.
    #[serde(default)]
    pub max_gap: Option<String>,
    /// What [`Self::require_coverage`] DOES about a finding: `"refuse"` (the default — nothing
    /// runs), `"warn"` (the findings are logged and the run proceeds unchanged), or `"run"` (the
    /// gate is inert).
    ///
    /// # Why `"run"` exists rather than "just remove the key"
    ///
    /// A profile is a committed file and the disposition is an operational choice: a scripted
    /// sweep on a box whose store is legitimately partial needs to keep the armed gate in the file
    /// — so the next person reads what the run is supposed to require — while overriding what it
    /// does today. Deleting the key instead loses that statement. `"run"` is therefore
    /// byte-identical in OUTCOME to an unarmed gate and different in MEANING, and the plan still
    /// reports every finding under it.
    #[serde(default)]
    pub on_gap: Option<String>,
    /// POINT-IN-TIME UNIVERSE MEMBERSHIP — the survivorship defence. `"declared"` (the default,
    /// byte-identical: the symbol list is taken verbatim), `"covered"` (a member whose tape does
    /// not span the window is NAMED and the run proceeds), or `"strict"` (that run is REFUSED).
    ///
    /// # What this can honestly answer, and what it cannot
    ///
    /// The store holds NO LISTING CALENDAR. There is no row anywhere saying when an instrument
    /// began trading or stopped, so membership here is derived from the only evidence there is:
    /// whether the tape this run will read reaches both ends of the window. That is exactly the
    /// EXISTENCE half of `vike_data::window_shortfall` — a leading shortfall is a member that was
    /// not there when the window opened, a trailing one is a member that stopped — and it is
    /// deliberately not the same test as [`Self::require_coverage`], which judges COMPLETENESS
    /// (interior holes) and would fire on a recorder outage in the middle of a symbol's life.
    ///
    /// The bias this catches is the one that actually bites: a symbol list chosen TODAY is a list of
    /// survivors, and backtesting it over last year silently assumes every member existed then. A
    /// multi-symbol bar run does not even fail loudly — `super::run::refuse_ragged_series` catches
    /// the ragged case only when the lengths differ, and two members that are both short by the
    /// same number of bars are not ragged at all.
    ///
    /// # ⚠ It NEVER drops a member, under any value — and that is a decision, not a gap
    ///
    /// Silently narrowing the slice would change what the profile means while the file still names
    /// the wider universe, and the engine's own requirement makes it worse: two sites resolve the
    /// slice (`super::run::bar_engine_params` and `super::run::load_profile_bars`), the bar engine
    /// asserts one row per symbol per step, and a filter applied in one but not the other desyncs
    /// the symbol slots. So the answer is DISCLOSURE or REFUSAL, and editing the universe stays the
    /// author's act. Prior art that does drop: LEAN's `Universe` selection, which owns the data
    /// loader end-to-end and can.
    #[serde(default)]
    pub universe: Option<String>,
}

impl DataCfg {
    /// The series this profile actually loads: [`Self::series`] verbatim, else the
    /// `venue` × `symbols` whole-lane expansion. Empty only for a profile that failed
    /// [`BacktestProfile::validate`].
    pub fn resolved_series(&self) -> Vec<SeriesRef> {
        if !self.series.is_empty() {
            return self.series.clone();
        }
        let venue = self.venue.clone().unwrap_or_default();
        self.symbols.iter().map(|s| SeriesRef::new(&venue, s)).collect()
    }

    /// The run's single `EngineParams::default_venue` tag: [`Self::venue`] when set, else the
    /// FIRST series' venue. In tick mode with no bar seeding this tag is inert (`run_ticks`
    /// routes by the tick's own symbol); it matters for the bar path's `format_instrument` and
    /// for the `snap_to_properties` grid lookup — which is exactly why `validate` refuses to
    /// combine snapping with a cross-venue slice.
    pub fn default_venue(&self) -> String {
        self.venue
            .clone()
            .or_else(|| self.series.first().map(|s| s.venue.clone()))
            .unwrap_or_default()
    }

    /// True when the resolved series span more than one venue.
    pub fn is_cross_venue(&self) -> bool {
        let mut venues = self.series.iter().map(|s| s.venue.as_str());
        let Some(first) = venues.next() else { return false };
        venues.any(|v| v != first)
    }

    /// Resolve [`Self::warmup`] to the number of STEPS the engine must gate before it dispatches
    /// a strategy callback, or `None` when the key is absent (⇒ `vike_sim::EngineParams::warmup` is
    /// `None` ⇒ the gate is exactly `Strategy::warmup()`, byte-identical).
    ///
    /// # Why the two span shapes resolve differently, and why one is refused outright
    ///
    /// `"200bars"` ([`Span::Bars`]) is already a step count and needs neither an interval nor an
    /// anchor — it is the spelling that means the same thing on both lanes, since the gate the
    /// engine actually applies is an INDEX (`index >= warmup`) and `run_ticks`'s index counts
    /// events. `"7d"` ([`Span::Ms`]) is a step count only once divided by the base
    /// [`Self::interval`], so it is bar-mode only and it refuses a base interval that does not
    /// parse rather than guessing one. The division rounds UP: the key states a MINIMUM history,
    /// and a 7d declaration over 5h bars is 33.6 bars, of which 33 is not seven days.
    /// [`Span::Months`] is refused because a calendar month is not a fixed number of steps — one
    /// month from 31 January is 28 days and from 31 March it is 30 — so converting it would need
    /// the window anchor, and a warmup whose length depends on where the range starts is the
    /// irreproducibility this key exists to remove.
    pub fn warmup_steps(&self) -> Result<Option<usize>, HarnessError> {
        let Some(raw) = self.warmup.as_deref() else { return Ok(None) };
        let span =
            parse_span(raw).map_err(|e| HarnessError::Validation(format!("data.warmup: {e}")))?;
        match span {
            Span::Bars(n) => Ok(Some(n)),
            Span::Months(_) => Err(HarnessError::Validation(format!(
                "data.warmup {raw:?} is a CALENDAR span, and a calendar month is not a fixed \
                 number of steps (one month from 31 January is 28 days, from 31 March 30) — so \
                 its length would depend on where data.from happens to sit, which is the \
                 irreproducibility this key exists to remove. Write it in bars (\"200bars\") or \
                 in fixed time (\"90d\")"
            ))),
            Span::Ms(_) if self.kind == DataKind::Tick => Err(HarnessError::Validation(format!(
                "data.warmup {raw:?} is a DURATION, which is bar-mode only: a tick tape has no \
                 fixed cadence to divide it by, so no number of ticks corresponds to it. The \
                 engine gates run_ticks on an EVENT index, so write the requirement as a count \
                 — e.g. \"200bars\" means 200 ticks here"
            ))),
            Span::Ms(ms) => {
                let Some(base) = vike_model::time::interval_ms(&self.interval) else {
                    return Err(HarnessError::Validation(format!(
                        "data.interval {:?} is not a valid interval, so the duration data.warmup \
                         {raw:?} cannot be converted to a step count",
                        self.interval
                    )));
                };
                if base <= 0 {
                    return Err(HarnessError::Validation(format!(
                        "data.interval {:?} resolves to {base}ms, so the duration data.warmup \
                         {raw:?} names no number of steps",
                        self.interval
                    )));
                }
                // Round UP: the key states a MINIMUM history, and the last partial bar is part
                // of what was asked for. Spelled as a remainder test rather than
                // `(ms + base - 1) / base` because that form overflows for an `ms` near
                // `i64::MAX`, which `parse_span` will hand out (`"9000000000000d"` is a
                // `checked_mul` away from it), and rather than `i64::div_ceil` because signed
                // `div_ceil` is a newer stabilization than this workspace's floor.
                Ok(Some((ms / base + i64::from(ms % base != 0)) as usize))
            }
        }
    }

    /// Resolve [`Self::detail_interval`] to its millisecond step, or `None` when the key is
    /// absent (⇒ `vike_sim::EngineParams::granular_by_symbol` stays empty ⇒ every symbol fills on
    /// the coarse lane, byte-identical).
    ///
    /// # What it refuses, and why each silence would be worse than the refusal
    ///
    /// The grammar is [`vike_model::time::interval_ms`] — the same vocabulary [`Self::interval`]
    /// and `engine.timeframes` use, not the wider [`Span`] one, because this value is handed
    /// STRAIGHT to `HistStore::load_bars` as a series interval and a `"3mo"` names no stored
    /// series. It must be STRICTLY FINER than the base interval: this is the exact inverse of the
    /// `engine.timeframes` rule (those must be strictly COARSER, because a coarse series is
    /// RESAMPLED from the base and a finer one cannot be), and here an equal-or-coarser detail
    /// series would bucket at most one sub-bar per coarse step — which resolves no ambiguity at
    /// all while costing a second whole store scan, and reads as a working realism knob. Tick
    /// mode is refused because the granular lane is `vike_sim::StrategyEngine::run`'s own bar step:
    /// `run_ticks` never reads the per-symbol `sub` buckets, so a tick profile setting this would
    /// parse, load a second series and change nothing — the lane-asymmetric silence
    /// `engine.feed_latency`, `engine.queue_model` and `engine.timeframes` each already refuse.
    pub fn detail_interval_ms(&self) -> Result<Option<i64>, HarnessError> {
        let Some(raw) = self.detail_interval.as_deref() else { return Ok(None) };
        if self.kind == DataKind::Tick {
            return Err(HarnessError::Validation(format!(
                "data.detail_interval {raw:?} is bar-mode only: the intrabar detail tape is \
                 bucketed into COARSE BAR steps and consulted by StrategyEngine::run's bar fill \
                 phase, and run_ticks never reads those buckets — a tick replay already resolves \
                 every order against the tape it is replaying"
            )));
        }
        let Some(ms) = vike_model::time::interval_ms(raw) else {
            return Err(HarnessError::Validation(format!(
                "data.detail_interval {raw:?} is not a valid interval (want a count then one of \
                 s/m/h/d, e.g. \"1m\") — it is handed straight to the hist store as a series \
                 interval, so it must name a series the store can hold"
            )));
        };
        if ms <= 0 {
            return Err(HarnessError::Validation(format!(
                "data.detail_interval {raw:?} resolves to {ms}ms — a zero-length window makes \
                 every sub-bar boundary the same instant"
            )));
        }
        let Some(base) = vike_model::time::interval_ms(&self.interval) else {
            return Err(HarnessError::Validation(format!(
                "data.interval {:?} is not a valid interval, so data.detail_interval {raw:?} \
                 cannot be checked as finer than it",
                self.interval
            )));
        };
        if ms >= base {
            return Err(HarnessError::Validation(format!(
                "data.detail_interval {raw:?} ({ms}ms) must be strictly FINER than data.interval \
                 {:?} ({base}ms) — at this resolution each coarse step buckets at most one \
                 sub-bar, which resolves no stop-versus-target ambiguity while paying for a \
                 second whole store scan. This is the inverse of the engine.timeframes rule, \
                 where a synthesised series must be COARSER than the base",
                self.interval
            )));
        }
        Ok(Some(ms))
    }

    /// Resolve [`Self::on_gap`] to the disposition the coverage gate applies, defaulting to
    /// [`OnGap::Refuse`] when the key is absent.
    ///
    /// # Why the default is the REFUSAL rather than the warning
    ///
    /// Arming a gate and having it warn is indistinguishable from not arming it on any run whose
    /// output nobody reads — which is every scripted run. The whole value of
    /// [`Self::require_coverage`] is that a run over an incomplete window does not produce a
    /// number, so the default disposition has to be the one that produces none.
    ///
    /// The value is matched case-insensitively (a `"Refuse"` typed from a runbook is the same
    /// choice), and an unrecognised one is refused with the valid set named rather than falling
    /// back — a silent fallback to `refuse` would stop a run for a reason the operator never
    /// wrote, and a silent fallback to `run` would disarm the gate they did.
    pub fn on_gap(&self) -> Result<OnGap, HarnessError> {
        let Some(raw) = self.on_gap.as_deref() else { return Ok(OnGap::Refuse) };
        OnGap::parse(raw).ok_or_else(|| {
            HarnessError::Validation(format!(
                "data.on_gap {raw:?} is not one of {roster}. It says what data.require_coverage DOES \
                 about a window the store does not cover: `refuse` stops the run (the default, \
                 because a gate that only warns is invisible on every run nobody reads), `warn` \
                 logs the findings and runs unchanged, `run` leaves the gate inert while keeping \
                 the file's statement of what this run is supposed to require",
                roster = OnGap::roster()
            ))
        })
    }

    /// Resolve [`Self::max_gap`] to the per-span tolerance in milliseconds, or `None` when the key
    /// is absent (⇒ ZERO tolerance: any missing span at all is a finding).
    ///
    /// # What each refused shape would have meant, and why neither can be honoured
    ///
    /// A bar COUNT ([`Span::Bars`]) needs an interval to become a duration, and the store's holes
    /// are wall-clock spans in epoch-ms — so on the tick lane, which has no interval at all, there
    /// is no number of ticks that corresponds to one. [`Span::Months`] is refused for
    /// [`Self::warmup`]'s reason exactly: a calendar month is not a fixed length, so the tolerance
    /// would depend on where `data.from` happens to sit, and two runs with the same `max_gap`
    /// would tolerate different amounts.
    ///
    /// A non-positive duration is refused rather than treated as zero: `"0s"` reads as "tolerate
    /// nothing", which is already what an ABSENT key means, and a key whose only effect is to
    /// restate the default is a key somebody will believe does something else.
    pub fn max_gap_ms(&self) -> Result<Option<i64>, HarnessError> {
        let Some(raw) = self.max_gap.as_deref() else { return Ok(None) };
        let span =
            parse_span(raw).map_err(|e| HarnessError::Validation(format!("data.max_gap: {e}")))?;
        match span {
            Span::Ms(ms) if ms > 0 => Ok(Some(ms)),
            Span::Ms(ms) => Err(HarnessError::Validation(format!(
                "data.max_gap {raw:?} resolves to {ms}ms — a non-positive tolerance is what an \
                 ABSENT data.max_gap already means (tolerate nothing), so this spelling can only \
                 mislead. Remove the key, or name a real duration"
            ))),
            Span::Bars(n) => Err(HarnessError::Validation(format!(
                "data.max_gap {raw:?} is a BAR COUNT, and a gap in this store is a wall-clock span \
                 in epoch-ms: converting {n} bars to a duration needs an interval, and a tick \
                 profile has none. Write the tolerance as fixed time — \"4h\", \"1d\""
            ))),
            Span::Months(_) => Err(HarnessError::Validation(format!(
                "data.max_gap {raw:?} is a CALENDAR span, and a calendar month is not a fixed \
                 number of milliseconds (one month from 31 January is 28 days, from 31 March 30) \
                 — so the tolerance would depend on where data.from happens to sit, and two runs \
                 naming the same tolerance would accept different amounts. Write it in fixed time \
                 — \"30d\""
            ))),
        }
    }

    /// Resolve [`Self::universe`] to the membership rule, defaulting to [`UniverseMode::Declared`]
    /// — the byte-identical no-op.
    ///
    /// Case-insensitive, and an unrecognised value is refused with the set named for
    /// [`Self::on_gap`]'s reason: this key decides whether a run happens, and no fallback is
    /// harmless in both directions.
    pub fn universe_mode(&self) -> Result<UniverseMode, HarnessError> {
        let Some(raw) = self.universe.as_deref() else { return Ok(UniverseMode::Declared) };
        UniverseMode::parse(raw).ok_or_else(|| {
            HarnessError::Validation(format!(
                "data.universe {raw:?} is not one of {roster}. It is the POINT-IN-TIME membership \
                 rule: `declared` takes the symbol list verbatim (the default), `covered` names \
                 every member whose tape does not span the window and runs anyway, `strict` \
                 refuses that run. None of the three ever drops a member — a universe a run \
                 narrowed silently would not be the one the profile names",
                roster = UniverseMode::roster()
            ))
        })
    }

    /// The three coverage keys folded into the one value the gate consults, so a caller cannot
    /// resolve two of them and forget the third.
    ///
    /// A fold rather than three calls at the site for the reason `super::search_select`'s
    /// `evaluator_for` is one: the disposition and the tolerance are meaningless apart from the
    /// arming flag, and the one place that knows how they compose should be the place that says so.
    pub fn coverage_gate(&self) -> Result<CoverageGate, HarnessError> {
        Ok(CoverageGate {
            armed: self.require_coverage,
            max_gap_ms: self.max_gap_ms()?,
            on_gap: self.on_gap()?,
        })
    }
}

fn default_interval() -> String {
    "1d".to_string()
}

/// Bar or tick replay. Serde-mapped to lowercase TOML strings (`kind = "bar"` / `kind = "tick"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataKind {
    Bar,
    Tick,
}

/// Engine cost/cash configuration handed to the sim broker.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineCfg {
    pub cash: f64,
    #[serde(default)]
    pub fee_rate: f64,
    #[serde(default)]
    pub slippage: f64,
    #[serde(default)]
    pub snap_to_properties: bool,
    /// Opt-in: window-join the stored market `funding` series onto the replayed bars' `Bar.funding`
    /// at each funding-event timestamp, for perp funding accrual + funding-reading strategies. The
    /// market funding-rate backfill (#761) writes a `(venue, symbol, "funding")` bar series whose
    /// each `Bar.funding = Some(rate)` sits at a funding-event ts; with this on,
    /// [`crate::harness::window_join_funding`] attaches each rate to the ONE price bar whose window
    /// contains the event (NEVER forward-filled onto every bar — that would make the `SimBroker`
    /// bar-loop accrual charge every bar instead of once per 8h/1h interval). Both consumers then
    /// work: the accrual (`engine.rs`, charges when `Bar.funding.is_some()`) and a funding-reading
    /// strategy's signal (`FundingCarryController` remembers the last rate in its funding book).
    ///
    /// BAR MODE ONLY — the per-interval accrual runs in the bar loop; the tick lane has no funding
    /// fold, so a profile that sets this with `data.kind = "tick"` is rejected by
    /// [`BacktestProfile::validate`] (the mirror of the tick-only `feed_latency` rule). The funding lookup
    /// keys off each RESOLVED series' own venue, so it works for a cross-venue `[[data.series]]`
    /// slice too (unlike `snap_to_properties`, which is single-venue). Absent (the default `false`)
    /// = the funding series is never read, byte-identical. On but with no stored funding data = a
    /// warning + unchanged bars, never an error.
    ///
    /// EXPECTATION: the price bar interval must be no COARSER than the funding cadence, so at most
    /// one funding event lands in a price bar's window. If more than one does (e.g. 1d price bars
    /// over 8h funding), only the LAST is kept and the collision is logged loudly — use a finer
    /// price interval to avoid collapsing charges.
    #[serde(default)]
    pub attach_funding: bool,
    /// Opt-in feed-latency replay (see [`crate::TickReplayConfig::feed_latency`]): deliver ticks to
    /// the strategy in recorded ARRIVAL (`local_ts`) order instead of venue order, while order
    /// matching stays on venue `ts`. Absent (the default `false`) = today's venue-ordered replay,
    /// byte-identical. TICK MODE ONLY — a bar has no `local_ts`, so a profile that sets this with
    /// `data.kind = "bar"` is rejected by [`BacktestProfile::validate`] rather than silently
    /// ignored (the mirror of the bar-only `attach_funding` rule).
    #[serde(default)]
    pub feed_latency: bool,
    /// Opt-in ORDER latency (see [`vike_sim::LatencyModelKind`]): a fixed entry-leg delay in MILLISECONDS on
    /// every strategy order action (place / modify / cancel). The maker's quotes, re-prices and
    /// pulls only reach the matching engine `order_latency_ms` after it decides them, so it cannot
    /// react within one tick — the missing clock skew that otherwise lets a maker enter and exit in
    /// the same instant. `0` (the default) = zero latency, byte-identical. TICK MODE ONLY (only
    /// `run_ticks` arms the latency gate; the bar lane and vector kernel never consult it).
    #[serde(default)]
    pub order_latency_ms: i64,
    /// Opt-in FILL-notification latency (the response leg, see [`vike_sim::LatencyModelKind`]): a fixed delay in
    /// MILLISECONDS before the strategy LEARNS of a fill. The fill books at the real time (equity/PnL
    /// are exact), but the strategy-visible shadow position ([`vike_model::HftBroker::position`])
    /// does not advance until `fill_latency_ms` later — so a maker that polls its inventory cannot
    /// react (place its exit) inside that gap. Models the "time to realise you're filled" half of a
    /// real round-trip reaction. `0` (the default) = off, byte-identical. TICK MODE ONLY.
    #[serde(default)]
    pub fill_latency_ms: i64,
    /// Opt-in tick-lane fill-model override: `"l2book"` (the ONE accepted value; case-insensitive,
    /// like [`Self::queue_model`]) selects the depth-capped
    /// [`vike_sim::FillModelKind::L2Book`] (a resting order fills only up to the DISPLAYED book
    /// depth; when the within-limit depth cannot cover its size it RESTS rather than filling size the
    /// market never showed), vs the default L1 spread-crossing `Tick` model (fills the full size at
    /// the quote). Absent ⇒ `Tick`, byte-identical. Any OTHER value is a fail-fast
    /// [`HarnessError::Validation`] at load ([`EngineCfg::fill_model_kind`], the mirror of the
    /// [`Self::queue_model`] rule) — a typo like `"l2_book"` must not silently select the
    /// optimistic `Tick` model and undo the depth-cap realism knob. TICK MODE ONLY. Pair with
    /// `slippage = 0.0` (the book walk IS the slippage).
    #[serde(default)]
    pub fill_model: Option<String>,
    #[serde(default)]
    pub seed_bar_interval_ms: Option<i64>,
    /// Opt-in market-impact slippage on top of the flat `slippage` (see [`vike_sim::ImpactModel`]).
    /// Absent (the default) = the frozen flat-slippage cost, byte-identical.
    ///
    /// BOTH MODES since the lane split. This was BAR MODE ONLY and rejected outright in tick
    /// mode, on the reasoning that "the tick lane replays a real book and needs no model" — half
    /// right, and the wrong half is now the point of the knob: an L1 tick tape has no book at
    /// all (it fills any size at the quote), and even an L2 replay walks a RECORDING that never
    /// moves in response to the order, so the permanent footprint is missing there too. Each
    /// lane is charged only what its own price law has not already paid — see
    /// [`vike_sim::ImpactTerms`], which is where that decision lives.
    ///
    /// ⚠ [`ImpactCfg::window`] changes UNITS with the mode (bars vs trade prints).
    #[serde(default)]
    pub impact: Option<ImpactCfg>,
    /// Stop-verb release timing (see [`vike_sim::EngineParams::emulator_release_stops`]).
    ///
    /// ⚠ The HARNESS default is `false`, matching [`vike_sim::EngineParams::default()`]: a fired
    /// conditional stop fills SAME-EVENT at the trigger oracle's price, the raw-engine legacy
    /// behaviour (the pinned backtest divergence, law-map A2). This field previously documented a
    /// harness default of `true` ("mirror-live") while reaching the engine through NEITHER
    /// `EngineParams` construction site in `crate::harness::run` — both end in
    /// `..Default::default()` — so every harness run was `false` in practice regardless of what a
    /// profile's TOML said or what this doc claimed. The default now agrees with what has always
    /// actually happened, and the field is now wired into BOTH construction sites, so an explicit
    /// `true` really does arm mirror-live release: a fired conditional stop converts to a resting
    /// MARKET child that fills the NEXT event, exactly as the live emulator's `ConditionalBook`
    /// does.
    ///
    /// Whether the harness SHOULD default to mirror-live (so a profile run's fill timing matches
    /// what the same order would do against a live venue) rather than the raw-engine default is a
    /// separate, still-open OWNER DECISION — this fix wires the knob and corrects the doc; it
    /// deliberately does not also flip live simulation behaviour as a side effect.
    #[serde(default = "default_emulator_release_stops")]
    pub emulator_release_stops: bool,
    /// Opt-in fee SCHEDULE (port backlog G7) — the shapes a flat [`Self::fee_rate`] cannot
    /// express. Absent (the default) = the flat `fee_rate` path, byte-identical. Setting BOTH
    /// is a validation error: two cost models would be configured and only one could win.
    #[serde(default)]
    pub fee: Option<FeeCfg>,
    /// Opt-in binary-resolution settlement (port backlog G6): builds the
    /// [`vike_sim::EngineParams::resolution`] source + `resolution_end_ts` the engine has always
    /// consumed. Absent (the default) = no settlement source, byte-identical.
    #[serde(default)]
    pub resolution: Option<ResolutionCfg>,
    /// Opt-in QUEUE-POSITION fill model for the tick lane (see [`vike_sim::QueueModelKind`]): a resting
    /// limit — INCLUDING a tagged MAKER quote — no longer fills the instant price touches it, but
    /// only once a taker TRADE has consumed the size AHEAD of it in the FIFO queue (seeded from the
    /// replayed L2 book's size at that price, or the last L1 quote, or [`Self::queue_seed_depth`]).
    /// This is the realistic passive-maker fill: a quote earns the spread only when flow actually
    /// hits it, with partial fills on the trade's excess over the front. Absent (the default) = the
    /// frozen simple-crossing fill (`fill_tagged`), byte-identical. TICK MODE ONLY — the queue lane
    /// is consulted exclusively by `run_ticks`, so a bar-mode profile setting it is rejected by
    /// [`BacktestProfile::validate`] (the mirror of the `feed_latency` rule). Values (case-insensitive):
    /// `"risk_adverse"` (the conservative bound — front shrinks only on hard evidence),
    /// `"prob_power"` / `"prob_power:N"` (probabilistic, `f(x)=x^N`, default `N=1`), `"prob_log"`.
    /// For a real maker backtest feed the TRADE tape (and, for true size-ahead seeding, the BOOK).
    #[serde(default)]
    pub queue_model: Option<String>,
    /// Fallback front-of-queue depth (in size units) seeded when neither the replayed L2 book nor an
    /// L1 quote gives a size at a resting order's price — see [`vike_sim::QueueModelKind`]. Only consulted
    /// when [`Self::queue_model`] is set. Absent ⇒ `0.0` (a quote with no observed size-ahead fills
    /// on the first at-price trade).
    #[serde(default)]
    pub queue_seed_depth: Option<f64>,
    /// Minimum-hold floor in MS for the queued lane ([`vike_sim::EngineParams::queue_min_hold_ms`]): a
    /// position-reducing (closing) fill is deferred until this long after the position opened, so the
    /// backtest can't fabricate a sub-second maker round-trip. Only consulted when [`Self::queue_model`]
    /// is set. Absent ⇒ `0` (off). Calibrate from real MM flip times (Polymarket BTC-5m ≈ 2s p10).
    #[serde(default)]
    pub queue_min_hold_ms: Option<i64>,
    /// Equity-curve density for the tick lane (see [`vike_sim::EquitySampling`]) — the SWEEP knob for
    /// a very long tape. The curve is two `Vec`s grown 16 bytes per priced tick, so a 100M-tick
    /// replay carries 1.6 GB of samples per run whether or not anything reads them.
    ///
    /// - absent or `1` ⇒ [`vike_sim::EquitySampling::EveryTick`], the frozen default, byte-identical;
    /// - `N > 1` ⇒ [`vike_sim::EquitySampling::EveryN`], keep one sample per `N` ticks (plus a closing
    ///   sample at the last tick, so `equity_curve.last()` still agrees with `final_equity`);
    /// - `0` ⇒ [`vike_sim::EquitySampling::Off`], record nothing.
    ///
    /// ⚠ Anything but the default makes every curve-DERIVED report figure (max drawdown, sharpe,
    /// the return series) an approximation — `Off` degenerates them entirely. `final_equity`,
    /// `n_trades`, the trade log and `per_symbol_pnl` are never derived from the curve and do not
    /// move. TICK MODE ONLY: the bar lane records one sample per BAR (bounded by the bar count and
    /// needing no thinning), so a bar-mode profile setting this is REJECTED by
    /// [`BacktestProfile::validate`] rather than silently ignored — the mirror of the
    /// `feed_latency` / `queue_model` rule.
    #[serde(default)]
    pub equity_sample_every: Option<usize>,
    /// Higher timeframes synthesised from the base bar stream, look-ahead safe: a coarse bar
    /// becomes visible only once its window has fully elapsed. Each entry is an
    /// `vike_model::time::interval_ms` spelling (`"4h"`, `"1d"`). Absent (the default) registers
    /// nothing, which is byte-identical to before this key existed.
    ///
    /// ⚠ Validated at LOAD by [`BacktestProfile::validate`] rather than in the engine, because
    /// `StrategyEngine::new` registers each entry with `parse_timeframe(tf).expect(..)` — a
    /// process abort where every neighbouring key gives a named refusal.
    ///
    /// ⚠ **UNMET SPEC CLAUSE, recorded here rather than dropped silently.**
    /// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §9.1 requires BOTH
    /// timeframe failure modes to become load-time refusals before this key ships: (1) the
    /// unparseable-interval panic, validated in [`BacktestProfile::validate`] — which the rule
    /// above delivers — and (2) the ask-for-an-undeclared-timeframe panic, "checked against the
    /// strategy's declared timeframe requirement". **(2) was not built.** What shipped instead is
    /// `vike_sim::SimBroker::bars_for`/`forming_for` returning `None` where they used to
    /// `panic!("timeframe {tf:?} not registered")`, so a strategy asking for a timeframe this
    /// profile did not declare now runs SILENTLY on no higher-TF data where it previously aborted
    /// loudly. It is latent: those two methods have zero callers in this tree and no strategy
    /// declares a timeframe requirement at all. Giving strategies a way to DECLARE one — a
    /// different plan — is what would close it; until then this key ships with §9.1(2) open.
    #[serde(default)]
    pub timeframes: Vec<String>,
    /// Admit a fill only when the shared cash pool can fund it. Default `false`, which is what
    /// every profile written before this key did.
    ///
    /// ⚠ It is a WHOLE-STEP MODE SWITCH, not a per-order check: `true` routes the bar step through
    /// `StrategyEngine::fill_step_gated` (the owning type is the ENGINE, not `SimBroker` — this
    /// doc named the wrong one) instead of the per-symbol `fill_pending` loop, and unconditionally
    /// DISABLES the granular sub-bar lane. Turning it on therefore changes fill granularity as
    /// well as admission.
    ///
    /// ⚠ BAR MODE ONLY, and [`BacktestProfile::validate`] refuses it on a tick profile:
    /// `StrategyEngine::run` is its only reader and `run_ticks` never consults it.
    #[serde(default)]
    pub cash_gate: bool,
    /// WHEN a multi-symbol step is decided — `"sequential"` (absent = the default, byte-identical
    /// to every profile written before this key) or `"simultaneous"`. Resolved by
    /// [`decide_mode`], which owns the unknown-spelling refusal; the ARGUMENT is in
    /// [`vike_sim::DecideMode`], and this doc deliberately does not restate it.
    ///
    /// # What it buys, in one sentence
    ///
    /// `"simultaneous"` makes the run's answer a property of the instrument SET rather than of
    /// the order `data.symbols` was typed in — the cross-sectional folds (`equity_now` and the
    /// three risk folds) and the shared-cash allocator's tie-break both walk the symbols in a
    /// canonical, name-derived order that no permutation of this profile's own symbol list can
    /// change. Today the list order is a strategy parameter nobody chose, no profile records and
    /// no report shows.
    ///
    /// # ⚠ It IMPLIES `cash_gate`, so it inherits both of that key's consequences
    ///
    /// The step routes through `StrategyEngine::fill_step_gated` instead of the per-symbol
    /// `fill_pending` loop, and the granular sub-bar lane is unconditionally DISABLED. That is
    /// not packaging: `fill_step_gated` is the engine's only lane that collects the whole step's
    /// orders before admitting any of them, so it is the only place a cross-section exists to be
    /// ordered. `[data] detail_interval` is refused with it for exactly the reason it is refused
    /// with `cash_gate` — the detail tape would be loaded, paid for and ignored.
    ///
    /// # ⚠ It is REFUSED beside an armed `[risk]` table or `engine.leverage`, and that is a
    /// declared GAP rather than a preference
    ///
    /// With a pre-trade gate armed, `SimBroker::gate_order` folds the `pending` set — which
    /// ACCUMULATES during the `on_bar` fan-out — so the symbol judged first faces the step's
    /// whole budget and every later one faces the remainder. That term is correct (a pre-trade
    /// cap must model the local view; its own doc argues why), which is why it is not removed
    /// here: the fix is a step-boundary budget snapshot, and that is NOT BUILT. Accepting the
    /// pair would let a run claim a cross-sectional decision whose DECISION half was still taken
    /// in list order — the one outcome worse than not having the mode.
    #[serde(default)]
    pub decide: Option<String>,
    /// Maintenance-margin RATE (a fraction of adverse notional), folded as
    /// `|size| · adverse · multiplier · maint_margin` by the liquidation watchdog.
    ///
    /// ⚠ `<= 0.0` short-circuits `check_liquidation` ENTIRELY — `0.0` means margin is OFF, not
    /// "zero margin required". ⚠ It is not part of the pre-trade `RiskGate`; `[risk]` owns that.
    #[serde(default)]
    pub maint_margin: f64,
    /// Equity cushion held above the maintenance requirement before the watchdog liquidates.
    /// ⚠ Defaults to `0.10` to match `EngineParams::default()` — a bare `#[serde(default)]` would
    /// give `0.0` and silently change every profile that omits the key.
    #[serde(default = "default_liq_buffer")]
    pub liq_buffer: f64,
    /// Opt-in stress knob. ⚠ `true` selects the retired TOTAL-WIPE liquidation model — once
    /// `eq_adv <= maint_margin * notional_adv` at the intrabar adverse marks, it force-closes the
    /// WHOLE account (bar mode, `StrategyEngine::check_liquidation`) / the triggering symbol in
    /// full (tick mode, `StrategyEngine::check_liquidation_tick`). `false` (the default) runs the
    /// shared LEAN law (`vike_model::cross_liquidation_plan`) instead: PARTIAL, losers-first
    /// liquidation with the `liq_buffer` grace line — the opposite of a total wipe. Despite its
    /// name, `true` is the CRUDER model, not the gentler one.
    #[serde(default)]
    pub venue_style_liquidation: bool,
    /// Participation cap: the fraction of the EVENT's own volume any one fill may take. `None`
    /// (the default) means uncapped, which is what every profile did before this key — and which
    /// lets a backtest "trade" more than the market traded.
    ///
    /// ⚠ BOTH LANES, and the denominator is the lane's own event rather than a bar. This doc and
    /// its validation message both said "a bar's own volume", which is only half the surface:
    /// `StrategyEngine::fill_pending_tick` hands `event.volume` to the SAME
    /// `StrategyEngine::dispatch_fill` the bar lanes use, and on the tick path that `event` is the
    /// symbol's single just-arrived print. So in bar mode the cap is a fraction of a BAR (or of a
    /// sub-bar on the granular lane) and in tick mode a fraction of ONE PRINT — a far tighter
    /// constraint at the same number. Calibrate per lane; `0.05` does not mean the same thing in
    /// both.
    #[serde(default)]
    pub volume_limit: Option<f64>,
    /// Ceiling on concurrently open positions. ⚠ `0` is the engine's UNLIMITED, not "none" — it is
    /// the default, and it is what every profile written before this key did.
    #[serde(default)]
    pub max_open_positions: usize,
    /// Ceiling on concurrently open LONG positions. ⚠ `0` means unlimited, as above.
    #[serde(default)]
    pub max_open_long: usize,
    /// Ceiling on concurrently open SHORT positions. ⚠ `0` means unlimited, as above.
    #[serde(default)]
    pub max_open_short: usize,
    /// Contract multiplier applied to every symbol without its own row in
    /// [`Self::multipliers`]. ⚠ Defaults to `1.0`, matching `EngineParams::default()` — a bare
    /// `#[serde(default)]` would give `0.0` and zero every position's notional.
    #[serde(default = "default_multiplier")]
    pub multiplier: f64,
    /// Per-symbol contract multipliers, e.g. `ES = 50.0`. A `BTreeMap` rather than a `HashMap`
    /// because the engine takes an ordered `Vec` and a run must not depend on hash order.
    #[serde(default)]
    pub multipliers: BTreeMap<String, f64>,
    /// The run's own leverage. ⚠ Distinct from `[risk] max_leverage`, which reaches the same gate
    /// through `ProfileRisk::im_requirement` but is a CEILING — this is what the run uses, that is
    /// what it may not exceed.
    #[serde(default)]
    pub leverage: Option<f64>,
    /// Clamp an order down to what [`Self::leverage`] allows instead of rejecting it.
    ///
    /// ⚠ UNAVAILABLE to any profile carrying `[risk]`, and [`BacktestProfile::validate`] refuses
    /// the pair outright. `SimBroker::build_risk_gate` returns `None` on this flag BEFORE it
    /// reads `risk_limits`, so the clamp disarms the pre-trade gate ENTIRELY — every `[risk]`
    /// limit is discarded, not just `max_leverage`. That is the opposite precedence from the
    /// clamp-off case, where `[risk]` is what wins and [`Self::leverage`] is what is discarded.
    #[serde(default)]
    pub clamp_to_leverage: bool,
    /// Variation-settlement cadence in milliseconds — how often open-position profit is realised
    /// into cash instead of accruing. `None` (the default) never settles, which is what every
    /// profile did before this key.
    #[serde(default)]
    pub settlement_period_ms: Option<i64>,
    /// Defer an order that falls outside its symbol's trading session instead of filling it.
    /// Deferrals are already counted and already reach the report as
    /// `BacktestResult::session_deferrals`.
    ///
    /// ⚠ RULED (task 11 of the 2026-09-12 backtest-engine-cfg-exposure plan): this key exposes
    /// [`vike_sim::EngineParams::session_gate`] alone. [`vike_sim::EngineParams::session_calendars`] is
    /// `IndexMap<String, SessionCalendar>` and a profile can only name a calendar by STRING — with
    /// no by-name `SessionCalendar` constructor/lookup in this crate, a TOML `[engine.sessions]`
    /// table would parse and reach nothing. A per-symbol calendar table is a later task's
    /// deliverable, and shipping the gate without one is still correct.
    ///
    /// ⚠ **What is NOT correct — and what this doc asserted until the whole-branch review — is
    /// that arming it without a calendar table is a silent no-op.** It is not. With no per-symbol
    /// override each symbol falls back to `vike_model::session_for(default_venue)`, and
    /// `default_venue` is present on BOTH lanes far more often than "no override means
    /// always-open" implies: [`crate::hist_replay::replay_ticks`] assigns
    /// `cfg.params.default_venue` **unconditionally** (tick replay is single-venue), so a tick
    /// profile ALWAYS has one, and `crate::harness::run::bar_engine_params` assigns it whenever
    /// [`Self::snap_to_properties`] is on. `vike_model::session_for` then answers
    /// `vike_model::session::FX_WEEK` — a real calendar excluding Fri 22:00 → Sun 21:00 UTC — for
    /// `dukascopy | oanda | ig | fxcm | ctrader`, `vike_model::session::CRYPTO_24_7` for the
    /// crypto/prediction venues, and always-open for everything else (the mixed-asset venues,
    /// where venue alone cannot say).
    ///
    /// So: on an FX venue `session_gate = true` **defers fills today** and
    /// `BacktestResult::session_deferrals` is not `0`. It is a genuine no-op only where the
    /// resolved calendar is always-open — a crypto/mixed venue, or a bar profile with
    /// `snap_to_properties` off, which leaves `default_venue` `None`. Enabling it is a modelling
    /// choice about the venue's week, not a harmless flag.
    #[serde(default)]
    pub session_gate: bool,
    /// How a strategy's requested size becomes an order size
    /// ([`vike_analytics::sizing::PositionSizer`], the WealthLab PosSizer port). `None` (the
    /// default) passes the request through unchanged
    /// ([`vike_analytics::sizing::PassThroughSizer`]) — what every profile did before this key existed
    /// (`StrategyEngine::new` installs it whenever `EngineParams::sizer` is `None`).
    #[serde(default)]
    pub sizer: Option<SizerCfg>,
}

/// The HARNESS default for [`EngineCfg::multiplier`], equal to `EngineParams::default()`'s `1.0`.
fn default_multiplier() -> f64 {
    1.0
}

/// The HARNESS default for [`EngineCfg::liq_buffer`], equal to `EngineParams::default()`'s `0.10`.
/// ⚠ Do not replace with `#[serde(default)]`: that yields `0.0` and arms liquidation at the
/// maintenance line with no cushion.
fn default_liq_buffer() -> f64 {
    0.10
}

impl EngineCfg {
    /// Resolve the optional [`Self::queue_model`] string to a [`QueueModelKind`] for the tick queue
    /// lane. `None` (absent) ⇒ the frozen simple-crossing fill. Case-insensitive; `"prob_power:N"`
    /// carries the power exponent (`"prob_power"` alone ⇒ `N = 1`). An unrecognized value is a
    /// fail-fast [`HarnessError::Validation`], never a silent fallback.
    pub(crate) fn queue_model_kind(&self) -> Result<Option<QueueModelKind>, HarnessError> {
        let Some(raw) = self.queue_model.as_deref() else {
            return Ok(None);
        };
        let s = raw.trim().to_ascii_lowercase();
        let bad = |m: String| HarnessError::Validation(m);
        let kind = if let Some((head, tail)) = s.split_once(':') {
            match head {
                "prob_power" => QueueModelKind::ProbPower(tail.parse::<f64>().map_err(|_| {
                    bad(format!("queue_model \"prob_power:{tail}\": {tail:?} is not a number"))
                })?),
                other => {
                    return Err(bad(format!(
                        "unknown queue_model {other:?} (want risk_adverse | prob_power[:n] | prob_log)"
                    )));
                }
            }
        } else {
            match s.as_str() {
                "risk_adverse" | "risk-adverse" | "conservative" => QueueModelKind::RiskAdverse,
                "prob_power" => QueueModelKind::ProbPower(1.0),
                "prob_log" => QueueModelKind::ProbLog,
                other => {
                    return Err(bad(format!(
                        "unknown queue_model {other:?} (want risk_adverse | prob_power[:n] | prob_log)"
                    )));
                }
            }
        };
        Ok(Some(kind))
    }

    /// Resolve the optional [`Self::equity_sample_every`] stride to an [`EquitySampling`].
    /// Absent or `1` ⇒ the frozen [`EquitySampling::EveryTick`]; `0` ⇒ [`EquitySampling::Off`];
    /// `N > 1` ⇒ [`EquitySampling::EveryN`]. Total (no error case): every `usize` names a valid
    /// density, unlike the string-keyed `queue_model`/`fill_model` resolvers where a typo could
    /// silently select a different model.
    pub(crate) fn equity_sampling(&self) -> EquitySampling {
        match self.equity_sample_every {
            None | Some(1) => EquitySampling::EveryTick,
            Some(0) => EquitySampling::Off,
            Some(n) => EquitySampling::EveryN(n),
        }
    }

    /// Resolve the optional [`Self::fill_model`] string to the tick-lane [`FillModelKind`].
    /// `None` (absent) ⇒ the default L1 spread-crossing [`FillModelKind::Tick`]. Case-insensitive
    /// (trimmed), mirroring [`Self::queue_model_kind`]. An unrecognized value is a fail-fast
    /// [`HarnessError::Validation`] naming the valid set, never a silent fallback — the old
    /// "absent / any other value ⇒ `Tick`" lenience meant a typo (`"L2_Book"`, `"l2_book"`)
    /// quietly selected the optimistic L1 model and undid the depth-cap realism knob (#819).
    pub(crate) fn fill_model_kind(&self) -> Result<FillModelKind, HarnessError> {
        let Some(raw) = self.fill_model.as_deref() else {
            return Ok(FillModelKind::Tick);
        };
        match raw.trim().to_ascii_lowercase().as_str() {
            "l2book" => Ok(FillModelKind::L2Book),
            other => Err(HarnessError::Validation(format!(
                "unknown fill_model {other:?} (want l2book; absent = the default Tick model)"
            ))),
        }
    }
}

/// Resolve [`EngineCfg::decide`] to the engine's own [`DecideMode`].
///
/// A FREE function taking the raw string rather than a `&self` method beside
/// [`EngineCfg::fill_model_kind`], for one mechanical reason worth stating so nobody "tidies" it
/// back: `crates/vike-ops/tests/engine_cfg_reaches_the_engine.rs` requires the literal
/// `profile.engine.<field>` to appear as a WHOLE identifier at each construction site, and
/// `profile.engine.decide_mode()` is `decide` followed by `_` — not an identifier boundary, so the
/// gate reads the field as unreached and the only cure is a hand-written exemption row in a crate
/// this one cannot see. Passing the value (`decide_mode(profile.engine.decide.as_deref())`) leaves
/// the read spelled the way the gate can check, at no cost to the parse.
///
/// Absent ⇒ [`DecideMode::Sequential`], byte-identical to before the key existed. An unrecognised
/// spelling is REFUSED rather than falling back: falling back would hand a typo the default and
/// report success, and the whole point of the key is that a run says out loud which of the two
/// cross-section answers it computed.
pub(crate) fn decide_mode(raw: Option<&str>) -> Result<DecideMode, HarnessError> {
    let Some(raw) = raw else {
        return Ok(DecideMode::Sequential);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "sequential" => Ok(DecideMode::Sequential),
        "simultaneous" => Ok(DecideMode::Simultaneous),
        other => Err(HarnessError::Validation(format!(
            "unknown engine.decide {other:?} (want sequential | simultaneous; absent = sequential, \
             the per-symbol walk in the order data.symbols lists them)"
        ))),
    }
}

/// TOML shape of the opt-in fee schedule ([`EngineCfg::fee`]) — a [`vike_model::FeeSchedule`]
/// named from a profile, by SHAPE or by VENUE.
///
/// # Why a shape rather than a rate
///
/// [`EngineCfg::fee_rate`] is one flat fraction of notional, and `vike_model::money::fees` expresses
/// five shapes of which only one IS that. This table used to reach exactly one of the other four
/// (`probability_scaled`), so the remaining three were shapes a profile could not cost a run at.
/// The two that take real money when they are missing:
///
/// * **`per_share_with_floor`** — the Interactive Brokers equities fixed schedule
///   ([`vike_model::FeeSchedule::PerShareWithFloor`], whose real rates live in
///   `crates/vike-model/src/money/fees.rs`'s `fee_schedule_for` `"ibkr"` arm): a per-share fee, a
///   per-order MINIMUM, and a percent-of-notional cap. **The minimum is the whole point**, and no
///   flat rate has one: a flat fee scales with size all the way to zero, so a small-size
///   high-frequency configuration backtests as viable and is then eaten by the broker minimum
///   live. That is the failure this key exists to surface before the money is real, and it is
///   why `min` is REQUIRED rather than defaulted — a forgotten floor is exactly the profile that
///   reads as priced and prices at nothing.
/// * **`percent_of_underlying`** — Deribit's options rule, `min(bps × underlying, cap × premium)`
///   ([`vike_model::FeeSchedule::PercentOfUnderlying`]). The cap is the buyer-protecting half and
///   binds for cheap deep-OTM options, so it too is REQUIRED — see the field doc, where a zero
///   cap is refused because `FeeSchedule::commission` would then charge exactly nothing.
///
/// ⚠ **What this crate still cannot do with the Deribit shape, stated here because the type name
/// promises more than the engine delivers.** The accurate figure needs the UNDERLYING price and
/// this engine never has one: `FeeSchedule::commission_with_underlying` has NO caller anywhere in
/// `vike-backtest`, and the one caller in the tree
/// (`crates/vike-paper/src/lib.rs`'s `PaperExecutionClient::commission_for`) is gated on an
/// `underlying_source` that no production code supplies. So a `percent_of_underlying` run is
/// charged the premium-cap-bounded approximation `min(bps × premium, cap × premium)`, which
/// UNDERSTATES the real fee for an option priced well below its underlying. Configuring the shape
/// still buys the cap and the correct type; it does not buy the underlying leg.
///
/// # Why a VENUE and not a number
///
/// `kind = "venue"` costs the run at the venue's own published schedule — the SAME answer the
/// paper/live mount gets, reached through the same two functions rather than a table copied here:
/// `vike_catalog::fee_lane(venue, symbol)` then [`vike_model::fee_schedule_for`], which is
/// verbatim what `crates/vike-mount/src/lib.rs`'s `make_engine` computes for its `static_default`.
/// The lane resolution is the load-bearing half: a `BTCUSDT.P` symbol resolves to
/// `"binance-perp"`, and before the mount learned that, every `.P` paper mount was charged the
/// SPOT row. A backtest that hand-types a rate can disagree with the paper mount of the same
/// instrument for no reason but the missing lookup, and that disagreement is invisible in both
/// reports.
///
/// ```toml
/// [engine.fee]
/// kind = "probability_scaled"
/// taker_rate = 0.072          # the live `cheap_np` cost: 0.072·p·(1−p) per share
///
/// # ...or the IBKR equities shape, whose $ minimum a flat rate cannot express:
/// # kind = "per_share_with_floor"
/// # per_share = 0.005
/// # min = 1.0
/// # max_pct = 0.005
///
/// # ...or "cost this run the way the mount would":
/// # kind = "venue"
/// # venue = "binance"         # symbol defaults to the run's own series on that venue
/// ```
///
/// # What reaches the fill EXACTLY, and what flattens
///
/// [`vike_sim::StrategyEngine::new`] routes a schedule one of two ways (its own comment is the
/// authority): [`vike_model::FeeSchedule::PercentMakerTaker`] and
/// [`vike_model::FeeSchedule::Free`] FLATTEN to the `(maker, taker)` fractions the frozen
/// `size × price × rate × multiplier` fold has always taken — for those two a flat fraction IS
/// the whole shape — and every other shape is carried to the fill site and applied through
/// `FeeSchedule::commission` at the price the fill transacted at. That inversion is what makes
/// this key mean anything: `maker_taker_rates()` reports `(0.0, 0.0)` for `PerShareWithFloor`, so
/// a flattened floor charges ZERO while the profile reads as priced.
///
/// ⚠ **One wrinkle on `per_share_with_floor` that this table cannot police**: the exact path
/// multiplies the schedule's answer by the symbol's CONTRACT MULTIPLIER, which is right for the
/// notional cap and wrong for the per-share term and the floor (a `$1.00` minimum is one dollar,
/// not one dollar per contract). It is exact at the default `engine.multiplier = 1.0`, which is
/// what every equities profile runs at. A refusal would have to read `[engine]` and this struct
/// at once, which only [`BacktestProfile::refusals`] can do.
///
/// NOTE none of this changes [`vike_model::fee_schedule_for`]'s registry default for
/// `"polymarket"` (still `Free`) — the paper fill path and the snapshot cost display read that
/// registry, and flipping it would move existing users' numbers. A profile that wants the
/// verified V2 curve asks for it, either by naming the rates under `probability_scaled` or with
/// the `pm_curve` flag below.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeeCfg {
    /// Which [`vike_model::FeeSchedule`] shape this table describes: `"probability_scaled"` |
    /// `"per_share_with_floor"` | `"percent_of_underlying"` | `"percent_maker_taker"` |
    /// `"venue"` | `"free"`.
    ///
    /// ⚠ The accepted set is [`FeeCfg::KINDS`] and the unknown-`kind` refusal is BUILT from it,
    /// so the roster an operator is shown cannot drift from the roster [`FeeCfg::build_for`]
    /// matches on. An unrecognised value fails the profile at load — never a silent fallback,
    /// because a fallback here prices every fill at something the operator did not write.
    pub kind: String,
    /// `probability_scaled`: the TAKER fraction of the `p(1−p)`-scaled share count.
    #[serde(default)]
    pub taker_rate: f64,
    /// `probability_scaled`: the MAKER fraction, before any rebate.
    #[serde(default)]
    pub maker_rate: f64,
    /// `probability_scaled`: the maker's rebate as a share of the EQUIVALENT TAKER fee, so a
    /// zero `maker_rate` with a positive share is a NEGATIVE maker commission (a rebate — the
    /// sign convention on [`vike_model::FeeSchedule`]).
    ///
    /// ⚠ These three are plain defaulted scalars rather than `Option`s, which is why the
    /// cross-kind rule below can only refuse a NON-ZERO one under another kind: serde cannot
    /// tell `taker_rate = 0.0` from an absent key, and a written zero configures nothing either
    /// way. Every knob added since is an `Option`, which is what lets a wrong-kind knob be
    /// refused rather than ignored.
    #[serde(default)]
    pub maker_rebate_share: f64,
    /// `per_share_with_floor`: the per-SHARE fee. REQUIRED by that kind.
    #[serde(default)]
    pub per_share: Option<f64>,
    /// `per_share_with_floor`: the per-order MINIMUM. REQUIRED by that kind, and required
    /// deliberately — an absent floor defaults to no floor, which is the flat-rate behaviour the
    /// whole shape exists to escape. Write `min = 0.0` to assert there is genuinely none.
    #[serde(default)]
    pub min: Option<f64>,
    /// `per_share_with_floor`: the maximum fraction of trade value the fee may reach. Optional —
    /// absent is `0.0`, which `FeeSchedule::commission` treats as NO ceiling (it caps only on a
    /// positive notional bound), so omitting it charges the uncapped per-share-or-floor figure.
    /// That default is safe in the direction that matters: it can only over-charge.
    #[serde(default)]
    pub max_pct: Option<f64>,
    /// `percent_of_underlying`: basis points of the UNDERLYING notional. REQUIRED by that kind.
    #[serde(default)]
    pub bps: Option<f64>,
    /// `percent_of_underlying`: the cap, as a fraction of the PREMIUM notional (Deribit's real
    /// rule caps the underlying term at 12.5 % of the premium). REQUIRED, and a non-positive
    /// value is REFUSED rather than read as "no cap": `FeeSchedule::commission` on this shape is
    /// `min(bps × premium, cap × premium)`, so a zero cap makes the whole commission zero — the
    /// silent free backtest, wearing a configured fee model.
    #[serde(default)]
    pub premium_cap_pct: Option<f64>,
    /// `percent_maker_taker`: maker basis points of quote-notional. At least one of this and
    /// [`Self::taker_bps`] is required; the other defaults to `0.0`.
    ///
    /// This kind is the one shape [`EngineCfg::fee_rate`] can already express, and it is here so
    /// a profile can say the two SIDES apart (`fee_rate` charges one number to both). A
    /// genuinely zero-fee venue writes the zero explicitly, which makes it an assertion instead
    /// of an omission.
    #[serde(default)]
    pub maker_bps: Option<f64>,
    /// `percent_maker_taker`: taker basis points of quote-notional. See [`Self::maker_bps`].
    #[serde(default)]
    pub taker_bps: Option<f64>,
    /// `venue`: which venue's published schedule to cost the run at. REQUIRED by that kind, and
    /// it is not inferred from `[data]` — `EngineParams::fee_schedule` is ONE schedule for the
    /// whole run, so on a cross-venue `[[data.series]]` slice there is no single right answer to
    /// infer and naming it is the operator asserting which venue's book this run is priced at.
    #[serde(default)]
    pub venue: Option<String>,
    /// `venue`: which SYMBOL selects the venue's fee LANE. Absent (the normal case) resolves from
    /// the run's own series on [`Self::venue`], which is what keeps the `.P` suffix from being
    /// typed twice — see [`FeeCfg::build_for`] for the two ways that resolution refuses.
    ///
    /// Set it outright when the run's series carry a symbol spelling the lane resolver cannot
    /// read, or to state the lane a mixed run is costed at.
    #[serde(default)]
    pub symbol: Option<String>,
    /// `venue`, polymarket only: take the verified 2026 V2 fee regime
    /// ([`vike_model::POLYMARKET_V2_FEE_CURVE`], via
    /// [`vike_model::fee_schedule_for_with_pm_curve`]) instead of the registry's `Free`.
    ///
    /// ⚠ It exists because `kind = "venue"` on polymarket otherwise costs the run at EXACTLY
    /// ZERO — `fee_schedule_for("polymarket")` is a deliberate `Free` that cannot be flipped
    /// without moving every existing consumer's numbers — and a free prediction-market backtest
    /// is the flattering direction. Absent or `false` is byte-identical to what the mount
    /// resolves. `true` on any other venue is REFUSED: that function delegates to
    /// [`vike_model::fee_schedule_for`] for every venue but polymarket, so the flag would
    /// configure nothing while the operator read the run as curve-priced.
    #[serde(default)]
    pub pm_curve: Option<bool>,
}

impl FeeCfg {
    /// The accepted `kind` values — the ONE roster, both matched on by [`Self::build_for`] and
    /// printed by the unknown-`kind` refusal, so an operator can never be shown a set the code
    /// does not accept.
    pub const KINDS: &[&str] = &[
        "probability_scaled",
        "per_share_with_floor",
        "percent_of_underlying",
        "percent_maker_taker",
        "venue",
        "free",
    ];

    /// Every knob this table declares that is actually SET, paired with the ONE `kind` that
    /// reads it.
    ///
    /// Each knob belongs to exactly one kind, which is what makes the cross-kind rule in
    /// [`Self::validate_shape`] a single loop rather than a pairwise matrix — and what makes a
    /// NEW knob join that rule by adding one row here instead of by being remembered.
    fn set_keys(&self) -> Vec<(&'static str, &'static str)> {
        let mut out = Vec::new();
        for (key, owner, is_set) in [
            ("taker_rate", "probability_scaled", self.taker_rate != 0.0),
            ("maker_rate", "probability_scaled", self.maker_rate != 0.0),
            ("maker_rebate_share", "probability_scaled", self.maker_rebate_share != 0.0),
            ("per_share", "per_share_with_floor", self.per_share.is_some()),
            ("min", "per_share_with_floor", self.min.is_some()),
            ("max_pct", "per_share_with_floor", self.max_pct.is_some()),
            ("bps", "percent_of_underlying", self.bps.is_some()),
            ("premium_cap_pct", "percent_of_underlying", self.premium_cap_pct.is_some()),
            ("maker_bps", "percent_maker_taker", self.maker_bps.is_some()),
            ("taker_bps", "percent_maker_taker", self.taker_bps.is_some()),
            ("venue", "venue", self.venue.is_some()),
            ("symbol", "venue", self.symbol.is_some()),
            ("pm_curve", "venue", self.pm_curve.is_some()),
        ] {
            if is_set {
                out.push((key, owner));
            }
        }
        out
    }

    /// Every check that needs neither the data slice nor any I/O: the `kind` itself, that each
    /// SET knob belongs to that kind, that each numeric knob is finite and non-negative, and that
    /// the kind's REQUIRED knobs are present.
    ///
    /// Split out of [`Self::build_for`] for the reason `ResolutionCfg::validate_shape` was: this
    /// half is answerable at LOAD, and [`BacktestProfile::refusals`] runs it there through
    /// [`Self::build`]. The slice-dependent half cannot be, and says so at its own site.
    fn validate_shape(&self) -> Result<(), HarnessError> {
        let kind = self.kind.as_str();
        if !Self::KINDS.contains(&kind) {
            return Err(HarnessError::Validation(format!(
                "unknown engine.fee.kind {kind:?} (known: {}; a single flat fraction of notional \
                 may also stay on engine.fee_rate, which is the same shape as \
                 percent_maker_taker charged to both sides)",
                Self::KINDS.join(" | ")
            )));
        }
        for (key, owner) in self.set_keys() {
            if owner != kind {
                return Err(HarnessError::Validation(format!(
                    "engine.fee.{key} is a knob of kind {owner:?}, and this table declares kind \
                     {kind:?} — nothing would read it. A COST knob that configures nothing is \
                     worse than an absent one: the operator reads the profile as priced at what \
                     they wrote and the run prices at something else. Set the kind this knob \
                     belongs to, or drop the knob."
                )));
            }
        }
        for (name, v) in [
            ("taker_rate", Some(self.taker_rate)),
            ("maker_rate", Some(self.maker_rate)),
            ("maker_rebate_share", Some(self.maker_rebate_share)),
            ("per_share", self.per_share),
            ("min", self.min),
            ("max_pct", self.max_pct),
            ("bps", self.bps),
            ("premium_cap_pct", self.premium_cap_pct),
            ("maker_bps", self.maker_bps),
            ("taker_bps", self.taker_bps),
        ] {
            let Some(v) = v else { continue };
            if !v.is_finite() || v < 0.0 {
                return Err(HarnessError::Validation(format!(
                    "engine.fee.{name} must be finite and >= 0, got {v}"
                )));
            }
        }
        match kind {
            "per_share_with_floor" => {
                if self.per_share.is_none() || self.min.is_none() {
                    return Err(HarnessError::Validation(
                        "engine.fee.kind = \"per_share_with_floor\" needs BOTH per_share and \
                         min. The floor is the whole reason this shape exists — a per-share fee \
                         with no minimum is a flat rate wearing a different name, and it is the \
                         configuration that makes a small-size high-frequency run backtest as \
                         viable and then lose to the broker minimum live. Write min = 0.0 to \
                         assert there is genuinely no floor."
                            .to_string(),
                    ));
                }
            }
            "percent_of_underlying" => {
                if self.bps.is_none() || self.premium_cap_pct.is_none() {
                    return Err(HarnessError::Validation(
                        "engine.fee.kind = \"percent_of_underlying\" needs BOTH bps and \
                         premium_cap_pct — the cap is half the rule it models \
                         (min(bps x underlying, cap x premium)), and it is the half that binds \
                         for a cheap deep-OTM option."
                            .to_string(),
                    ));
                }
                if self.premium_cap_pct.is_some_and(|c| c <= 0.0) {
                    return Err(HarnessError::Validation(format!(
                        "engine.fee.premium_cap_pct must be > 0, got {} — a zero cap is not \
                         \"no cap\". FeeSchedule::commission on this shape is \
                         min(bps x premium, cap x premium), so a zero cap zeroes the whole \
                         commission and the run pays no fee at all while the profile reads as \
                         fee-modelled. crates/vike-model/src/money/fees.rs's fee_schedule_for carries \
                         the real Deribit cap in its \"deribit\" arm.",
                        self.premium_cap_pct.unwrap_or(0.0)
                    )));
                }
            }
            "percent_maker_taker" => {
                if self.maker_bps.is_none() && self.taker_bps.is_none() {
                    return Err(HarnessError::Validation(
                        "engine.fee.kind = \"percent_maker_taker\" needs at least one of \
                         maker_bps / taker_bps — a table naming neither describes no cost, which \
                         is what kind = \"free\" says on purpose. A genuinely zero-fee side is \
                         written as the explicit 0.0, so the zero is an assertion rather than an \
                         omission."
                            .to_string(),
                    ));
                }
            }
            "venue" => {
                if self.venue.is_none() {
                    return Err(HarnessError::Validation(
                        "engine.fee.kind = \"venue\" needs engine.fee.venue — it is not inferred \
                         from [data]. EngineParams::fee_schedule is ONE schedule for the whole \
                         run, so a cross-venue slice has no single answer to infer, and naming \
                         the venue is the operator asserting which venue's book this run is \
                         costed at."
                            .to_string(),
                    ));
                }
                if self.pm_curve == Some(true) && self.venue.as_deref() != Some("polymarket") {
                    return Err(HarnessError::Validation(format!(
                        "engine.fee.pm_curve is polymarket-only, and this table names venue {:?} \
                         — vike_model::fee_schedule_for_with_pm_curve delegates to \
                         fee_schedule_for for every other venue, so the flag would configure \
                         nothing while the run was read as curve-priced. Drop it.",
                        self.venue.as_deref().unwrap_or("")
                    )));
                }
            }
            // Every accepted kind either has an arm above or requires no knob at all
            // (`probability_scaled`, whose three rates are all optional, and `free`, which takes
            // none). Membership was checked at the top of this function, so an unrecognised kind
            // never reaches here — an empty arm rather than a panic, because aborting the process
            // is not a thing a config path may do.
            _ => {}
        }
        Ok(())
    }

    /// Resolve to a [`vike_model::FeeSchedule`] WITHOUT the run's data slice — the door
    /// [`BacktestProfile::refusals`] drives at load, where the slice-dependent half of
    /// `kind = "venue"` cannot be answered.
    ///
    /// Identical to [`Self::build_for`] for every shape kind. For `kind = "venue"` with no
    /// explicit [`Self::symbol`] it resolves the venue's BARE lane, which on both dual-lane
    /// venues is the SPOT row — the more expensive one of the two
    /// (`crates/vike-model/src/money/fees.rs`'s `fee_schedule_for` `"binance"`/`"binance-perp"` arms
    /// carry the measurement), so this door over-charges rather than flatters. The harness never
    /// takes it for a real run: `crate::harness::run`'s two `EngineParams` construction sites
    /// both call [`Self::build_for`] with the profile's resolved series.
    pub fn build(&self) -> Result<FeeSchedule, HarnessError> {
        self.build_for(&[])
    }

    /// Resolve to a [`vike_model::FeeSchedule`] for a run over `series` — the door the harness
    /// uses, and the only one that can get a `kind = "venue"` LANE right.
    ///
    /// ⚠ **Two refusals here are RUN-TIME where every neighbour's is load-time, and the reason is
    /// structural rather than a preference**: they compare this table against the resolved data
    /// slice, and [`BacktestProfile::refusals`] is the only function that sees both. That is the
    /// same split `ResolutionCfg::build` already carries for its winners-coverage check. They
    /// always fire — both `EngineParams` construction sites reach this before a bar is loaded —
    /// but they fire when the run starts rather than when the profile loads, so a `--validate`
    /// pass does not show them.
    pub fn build_for(&self, series: &[SeriesRef]) -> Result<FeeSchedule, HarnessError> {
        self.validate_shape()?;
        // Every `unwrap_or` below is unreachable: `validate_shape` has already refused an absent
        // REQUIRED knob, and the fallback is the identity of the optional ones.
        match self.kind.as_str() {
            "probability_scaled" => Ok(FeeSchedule::ProbabilityScaled {
                taker_rate: self.taker_rate,
                maker_rate: self.maker_rate,
                maker_rebate_share: self.maker_rebate_share,
            }),
            "per_share_with_floor" => Ok(FeeSchedule::PerShareWithFloor {
                per_share: self.per_share.unwrap_or(0.0),
                min: self.min.unwrap_or(0.0),
                max_pct: self.max_pct.unwrap_or(0.0),
            }),
            "percent_of_underlying" => Ok(FeeSchedule::PercentOfUnderlying {
                bps: self.bps.unwrap_or(0.0),
                premium_cap_pct: self.premium_cap_pct.unwrap_or(0.0),
            }),
            "percent_maker_taker" => Ok(FeeSchedule::PercentMakerTaker {
                maker_bps: self.maker_bps.unwrap_or(0.0),
                taker_bps: self.taker_bps.unwrap_or(0.0),
            }),
            "venue" => {
                let venue = self.venue.as_deref().unwrap_or("");
                let lane = self.fee_lane_for(venue, series)?;
                Ok(if self.pm_curve == Some(true) {
                    vike_model::fee_schedule_for_with_pm_curve(lane)
                } else {
                    vike_model::fee_schedule_for(lane)
                })
            }
            "free" => Ok(FeeSchedule::Free),
            // Unreachable: `validate_shape` refused anything outside `Self::KINDS` before this
            // match ran. It is spelled as the unknown-kind refusal rather than as a panic so the
            // two doors can never disagree about what is accepted.
            other => Err(HarnessError::Validation(format!(
                "unknown engine.fee.kind {other:?} (known: {})",
                Self::KINDS.join(" | ")
            ))),
        }
    }

    /// The `vike_model::fee_schedule_for` LANE KEY this `kind = "venue"` table resolves to.
    ///
    /// ⚠ **Reached through `vike_catalog::fee_lane`, never re-derived.** That function owns the
    /// `.P` split and the per-contract-class sub-keys a venue may price apart, and it is the
    /// function `crates/vike-mount/src/lib.rs`'s `make_engine` calls for its own
    /// `static_default` — so the backtest and the paper mount of one instrument resolve the same
    /// row by construction rather than by two tables agreeing.
    ///
    /// With an explicit [`Self::symbol`] that is the whole job. Otherwise the lane comes from the
    /// run's own series ON THAT VENUE, and the two ways that can fail are refused rather than
    /// guessed:
    ///
    /// * **no series on the venue** — there is nothing to read a lane from, and answering with
    ///   the bare venue row would silently price a perp run at spot fees, which is the exact
    ///   defect the lane key was introduced to end.
    /// * **series straddling two lanes** — one `EngineParams::fee_schedule` cannot serve both,
    ///   and a venue's two lanes are priced completely differently, so either choice misprices
    ///   half the run.
    ///
    /// ⚠ Picking the FIRST series would be wrong even where it looks harmless: series order is
    /// meaningful (a non-tradeable reference feed is listed first on purpose), so the first entry
    /// of a cross-venue slice is routinely not on the venue being costed at all.
    fn fee_lane_for<'a>(
        &'a self,
        venue: &'a str,
        series: &[SeriesRef],
    ) -> Result<&'a str, HarnessError> {
        if let Some(sym) = self.symbol.as_deref() {
            return Ok(vike_catalog::fee_lane(venue, sym));
        }
        let mut lanes: BTreeSet<&'a str> = BTreeSet::new();
        for s in series.iter().filter(|s| s.venue == venue) {
            lanes.insert(vike_catalog::fee_lane(venue, &s.symbol));
        }
        match lanes.len() {
            // The slice-free door (`build`): no series were offered at all, so the bare-venue
            // row is the only honest answer and its own doc states the direction of the error.
            0 if series.is_empty() => Ok(vike_catalog::fee_lane(venue, "")),
            0 => Err(HarnessError::Validation(format!(
                "engine.fee.venue = {venue:?} names a venue this run does not load, so there is \
                 no symbol to resolve its fee LANE from — and answering with the bare-venue row \
                 would charge a perp run at the venue's SPOT schedule, the mispricing the lane \
                 key exists to end. This run's series are on: {}. Name a venue the slice trades, \
                 or set engine.fee.symbol outright.",
                series
                    .iter()
                    .map(|s| s.venue.as_str())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
            1 => Ok(lanes.into_iter().next().unwrap_or(venue)),
            _ => Err(HarnessError::Validation(format!(
                "engine.fee.kind = \"venue\" resolves ONE schedule for the whole run, and this \
                 run's {venue:?} series straddle {} fee lanes ({}) — a venue prices its lanes \
                 completely differently (crates/vike-model/src/money/fees.rs's fee_schedule_for \
                 carries the measurement per lane), so one row would misprice the other lane. \
                 Split the run per lane, or set engine.fee.symbol to name the lane this run is \
                 costed at.",
                lanes.len(),
                lanes.into_iter().collect::<Vec<_>>().join(", ")
            ))),
        }
    }
}

/// TOML shape of the opt-in binary-resolution settlement source ([`EngineCfg::resolution`]).
///
/// One `kind` today — `"binary_outcome"`, the two-outcome prediction-market convention the
/// `cheap_np` tape uses: a series symbol is `<slug>#<outcome_index>` where the slug's trailing
/// `-<sts>` is the window open in epoch SECONDS ([`vike_strategy::TokenId`] is the parser, shared with
/// the strategy so the two cannot drift). A token pays `1.0` when its `outcome_index` equals the
/// window's `winning_index` and `0.0` otherwise, from `(sts + window_secs) × 1000` onward;
/// every other symbol (a spot reference series, another window's token) resolves to `None` and
/// is never settled or latched.
///
/// The winners map comes from an inline `[engine.resolution.winners]` table, a `slug,winning_index`
/// CSV named by `path` (the `FORMAT CSVWithNames` export `cheap_np_run` reads — a header row and
/// quoted slugs are both tolerated), or both (inline wins on a clash).
///
/// **Non-binary `winning_index` values are REAL and are never coerced.** The on-chain
/// resolutions table carries `2`/`3`/`4` rows (markets whose outcome a two-outcome payout
/// cannot represent) and `-1` for unresolved. A CSV row like that is DROPPED with a warning —
/// it is a data fact, and a month-wide export legitimately contains a handful — and an INLINE
/// entry like that is a hard error, since a hand-written value is an authoring mistake. Either
/// way the window then has no payout, so [`ResolutionCfg::build`] REJECTS the profile if any
/// series symbol in the run parses as a window token with no winner: an unsettled position
/// would otherwise be silently marked at its last traded price and reported as PnL.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionCfg {
    /// currently only `"binary_outcome"`
    pub kind: String,
    /// Seconds from a window's open (`sts`) to its resolution. Defaults to
    /// [`vike_model::fair::UPDOWN_WINDOW_SECS`] (300). NOTE the symbol GRAMMAR is independent of this:
    /// `TokenId::parse` pins `sts` to a multiple of `UPDOWN_WINDOW_SECS`, so a different `window_secs`
    /// moves the payout TIME without redefining the grid.
    #[serde(default = "default_window_secs")]
    pub window_secs: i64,
    /// Optional `slug,winning_index` CSV. Relative paths resolve against the profile file's own
    /// directory ([`BacktestProfile::base_dir`]), not the CWD.
    #[serde(default)]
    pub path: Option<String>,
    /// Optional inline `slug = winning_index` table. Merged over `path`'s rows.
    #[serde(default)]
    pub winners: BTreeMap<String, i64>,
    /// Explicit override for [`vike_sim::EngineParams::resolution_end_ts`] (epoch-ms or
    /// `YYYY-MM-DDTHH`). Absent = the LATEST resolution among the run's own series
    /// (`max(sts) + window_secs`), which is what pins the end-of-run sweep to the window rather
    /// than to the `RESOLUTION_PROBE_SENTINEL`.
    #[serde(default)]
    pub end_ts: Option<String>,
}

fn default_window_secs() -> i64 {
    vike_model::fair::UPDOWN_WINDOW_SECS
}

impl ResolutionCfg {
    /// Structural checks that need no I/O — run at profile-parse time by
    /// [`BacktestProfile::validate`]. The winners-file read and the coverage check live in
    /// [`Self::build`], which is the first point that knows the profile's `base_dir`.
    fn validate_shape(&self) -> Result<(), HarnessError> {
        if self.kind != "binary_outcome" {
            return Err(HarnessError::Validation(format!(
                "unknown engine.resolution.kind {:?} (known: \"binary_outcome\")",
                self.kind
            )));
        }
        if self.window_secs <= 0 {
            return Err(HarnessError::Validation(format!(
                "engine.resolution.window_secs must be > 0, got {}",
                self.window_secs
            )));
        }
        for (slug, wi) in &self.winners {
            if !(0..=1).contains(wi) {
                return Err(HarnessError::Validation(format!(
                    "engine.resolution.winners[{slug:?}] = {wi} is not a binary outcome index \
                     (0 or 1). Non-binary and unresolved (-1) values are real on-chain facts and \
                     are never coerced to \"both sides lose\" — drop the window from the run \
                     instead."
                )));
            }
        }
        if let Some(s) = &self.end_ts {
            parse_ts(s)?;
        }
        Ok(())
    }

    /// Build the settlement source + its `resolution_end_ts` for a run over `symbols`.
    ///
    /// `base_dir` resolves a relative [`Self::path`]. Fails when a series symbol parses as a
    /// window token whose slug has no binary winner (see the type doc for why that is an error
    /// and not a shrug).
    pub fn build(
        &self,
        base_dir: Option<&Path>,
        symbols: &[String],
    ) -> Result<(ResolutionSource, Option<i64>), HarnessError> {
        self.validate_shape()?;
        let mut winners: BTreeMap<String, u8> = BTreeMap::new();
        if let Some(path) = &self.path {
            let path = match (Path::new(path).is_relative(), base_dir) {
                (true, Some(dir)) => dir.join(path),
                _ => PathBuf::from(path),
            };
            let text = std::fs::read_to_string(&path)
                .map_err(|e| HarnessError::Io(format!("{}: {e}", path.display())))?;
            let mut dropped = 0usize;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Some((slug, wi)) = line.split_once(',') else { continue };
                // ClickHouse `FORMAT CSVWithNames` quotes the slug; a quoted key matches NO
                // token symbol, so unquoting is load-bearing, not cosmetic.
                let slug = slug.trim().trim_matches('"');
                let Ok(wi) = wi.trim().parse::<i64>() else {
                    continue; // the CSVWithNames header row lands here
                };
                match wi {
                    0 | 1 => {
                        winners.insert(slug.to_string(), wi as u8);
                    }
                    // NOT coerced — a market that resolved to something a two-outcome payout
                    // cannot represent, or an unresolved (-1) row. Dropped here; the coverage
                    // check below turns it into a hard error if the run actually needs it.
                    _ => dropped += 1,
                }
            }
            if dropped > 0 {
                tracing::warn!(
                    path = %path.display(),
                    dropped,
                    "engine.resolution: dropped rows with a non-binary winning_index"
                );
            }
        }
        for (slug, wi) in &self.winners {
            winners.insert(slug.clone(), *wi as u8);
        }

        // Coverage: every tradeable window token in the run must have a payout, else its
        // position would silently end the run marked at the last traded price.
        let window_secs = self.window_secs;
        let mut latest_sts: Option<i64> = None;
        let mut missing: BTreeSet<String> = BTreeSet::new();
        for sym in symbols {
            let Some(tok) = TokenId::parse(sym) else { continue };
            let slug = sym.rsplit_once('#').map(|(s, _)| s).unwrap_or(sym);
            if !winners.contains_key(slug) {
                missing.insert(slug.to_string());
            }
            latest_sts = Some(latest_sts.map_or(tok.sts, |m: i64| m.max(tok.sts)));
        }
        if !missing.is_empty() {
            let names: Vec<&str> = missing.iter().take(5).map(String::as_str).collect();
            return Err(HarnessError::Validation(format!(
                "engine.resolution: {} window slug(s) in data.series have no binary \
                 winning_index (first: {names:?}). A window with no payout would end the run \
                 marked at its last traded price — remove it from the slice or supply its \
                 resolution.",
                missing.len()
            )));
        }

        let end_ts = match &self.end_ts {
            Some(s) => Some(parse_ts(s)?),
            None => latest_sts.map(|sts| (sts + window_secs) * 1000),
        };

        let source: ResolutionSource = Box::new(move |sym: &str, ts: i64| {
            let tok = TokenId::parse(sym)?;
            let slug = sym.rsplit_once('#').map(|(s, _)| s)?;
            let wi = *winners.get(slug)?;
            if ts < (tok.sts + window_secs) * 1000 {
                return None; // still trading
            }
            Some(if tok.oidx == wi { 1.0 } else { 0.0 })
        });
        Ok((source, end_ts))
    }
}

/// The HARNESS default for [`EngineCfg::emulator_release_stops`]: `false`, matching
/// [`vike_sim::EngineParams::default()`]. ⚠ This previously returned `true` ("mirror-live") while the
/// field reached the engine through NEITHER `EngineParams` construction site in
/// `crate::harness::run` — so every harness run was `false` in practice no matter what this
/// function answered. `false` makes the code agree with what has always actually happened; now
/// that the field is wired into both sites, an explicit `true` in a profile's TOML really does arm
/// mirror-live release. Flipping this default to mirror-live is a separate, still-open owner
/// decision — see the field doc.
fn default_emulator_release_stops() -> bool {
    false
}

/// TOML shape of the opt-in market-impact model, e.g.
///
/// ```toml
/// [engine.impact]
/// model = "almgren_chriss"
/// exec_time = 1.0
/// window = 21
/// # gamma = 0.314   # published; the LEVEL knob, and the only one an L2 run can move
/// # eta   = 0.142   # published; the temporary half, which a book walk already pays
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactCfg {
    /// which model — currently only `"almgren_chriss"`
    pub model: String,
    /// Execution horizon, in whatever period this run's context is measured over — bar-periods
    /// in bar mode, TRADE PRINTS in tick mode (see [`vike_sim::AlmgrenChriss::exec_time`]
    /// and [`vike_sim::TickWindow`]). The default `1.0` means "worked inside one period".
    ///
    /// ⚠ **It scales the TEMPORARY term only.** The permanent term is horizon-independent in the
    /// paper and `exec_time` cancels out of it exactly, so this knob is PROVABLY INERT on the L2
    /// lane, which is charged the permanent term and nothing else. Reach for [`Self::gamma`]
    /// there. Do not read this field as the answer to the tick lane's unit mismatch either — an
    /// earlier revision of the module docs said it was, and it moves only the smaller addend.
    #[serde(default = "default_exec_time")]
    pub exec_time: f64,
    /// Rolling sigma/volume lookback — in BARS in bar mode, in TRADE PRINTS in tick mode. The
    /// default is one trading month of daily bars; on a liquid tape it is a fraction of a
    /// second, so a tick profile should set it rather than inherit it. Lengthening it averages
    /// away sampling noise; it does NOT change the UNIT the context is measured in.
    #[serde(default = "default_impact_window")]
    pub window: usize,
    /// Permanent-impact coefficient, defaulting to the published [`vike_sim::AC_GAMMA`].
    ///
    /// **The one lever that reaches an L2-lane charge**, and a linear one: that lane pays
    /// `gamma * sigma * (qty/avg_volume)^alpha` and nothing else. It exists because the published
    /// calibration is fitted on DAILY US-equity context, and a run that measures its context per
    /// TRADE PRINT is spending the coefficient in a unit it was not fitted in — an operator who
    /// has measured that mismatch restates the LEVEL here rather than being told to turn a knob
    /// wired to nothing. The exponents are deliberately not exposed: they are the SHAPE (monotone,
    /// concave), and refitting them is a different model.
    #[serde(default = "default_ac_gamma")]
    pub gamma: f64,
    /// Temporary-impact coefficient, defaulting to the published [`vike_sim::AC_ETA`]. The
    /// twin of [`Self::gamma`] for the half a book walk already pays — so it moves the bar and
    /// L1-tick lanes and is inert on an L2 fill that walked a book. Same rationale, same fence
    /// around the exponents.
    #[serde(default = "default_ac_eta")]
    pub eta: f64,
}

fn default_exec_time() -> f64 {
    1.0
}

fn default_impact_window() -> usize {
    vike_sim::DEFAULT_IMPACT_WINDOW
}

fn default_ac_gamma() -> f64 {
    vike_sim::AC_GAMMA
}

fn default_ac_eta() -> f64 {
    vike_sim::AC_ETA
}

impl ImpactCfg {
    /// Resolve to a live model, or `Err` on an unknown `model` name / nonsensical numbers — a
    /// typo in a profile must fail loudly, not silently price fills at zero impact.
    pub fn build(&self) -> Result<Arc<dyn vike_sim::ImpactModel>, HarnessError> {
        if self.exec_time <= 0.0 || !self.exec_time.is_finite() {
            return Err(HarnessError::Validation(format!(
                "engine.impact.exec_time must be > 0, got {}",
                self.exec_time
            )));
        }
        if self.window < 3 {
            return Err(HarnessError::Validation(format!(
                "engine.impact.window must be >= 3 (two returns), got {}",
                self.window
            )));
        }
        // Both coefficients must be > 0 and finite. A NEGATIVE one would make the cost fall with
        // size and eventually go negative — a model that PAYS a large order — and the trait's
        // contract (finite, non-negative, non-decreasing in `qty`) is the thing the fill site
        // relies on when it declines to bound the estimate from above. Zero is refused too: it
        // spells "this half is switched off", which the profile can already say by not naming the
        // knob, and which would otherwise silently disarm the L2 lane's only charge.
        for (name, v) in [("gamma", self.gamma), ("eta", self.eta)] {
            if v <= 0.0 || !v.is_finite() {
                return Err(HarnessError::Validation(format!(
                    "engine.impact.{name} must be > 0 and finite, got {v}"
                )));
            }
        }
        match self.model.as_str() {
            "almgren_chriss" => Ok(Arc::new(vike_sim::AlmgrenChriss::with_coefficients(
                self.gamma,
                self.eta,
                self.exec_time,
            ))),
            other => Err(HarnessError::Validation(format!(
                "unknown engine.impact.model {other:?} (known: \"almgren_chriss\")"
            ))),
        }
    }
}

/// TOML shape of the opt-in position sizer ([`EngineCfg::sizer`]) — how a strategy's requested
/// size becomes an order size, via [`vike_analytics::sizing`]'s swappable `PositionSizer`
/// framework (the WealthLab PosSizer port). Every concrete sizer that crate ships gets a `kind`
/// row here and NOTHING else — a new sizer added there with no row here is unreachable from a
/// profile, not silently mapped to the nearest existing one.
///
/// Two kinds — `"portfolio_heat"` and `"drawdown_throttle"` — WRAP a base sizer rather than
/// standing alone (see [`vike_analytics::sizing::PortfolioHeatSizer`]/[`vike_analytics::sizing::DrawdownThrottleSizer`]);
/// the wrapped sizer nests under `[engine.sizer.base]`, which is why [`Self::base`] is boxed —
/// this type nests itself.
///
/// ```toml
/// [engine.sizer]
/// kind = "portfolio_heat"
/// max_heat = 0.10
/// [engine.sizer.base]
/// kind = "fixed_dollar"
/// amount = 1000.0
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SizerCfg {
    /// One of `"pass_through"` | `"fixed_dollar"` | `"fixed_shares"` | `"pct_equity"` |
    /// `"pct_volatility"` | `"max_risk_pct"` | `"portfolio_heat"` | `"drawdown_throttle"` — every
    /// concrete [`vike_analytics::sizing::PositionSizer`] this crate ships and nothing else
    /// ([`Self::build`]).
    pub kind: String,
    /// [`vike_analytics::sizing::FixedDollarSizer::amount`] — fixed cash notional per entry. Required for
    /// `"fixed_dollar"`.
    #[serde(default)]
    pub amount: Option<f64>,
    /// [`vike_analytics::sizing::FixedSharesSizer::shares`] — fixed share/contract count per entry.
    /// Required for `"fixed_shares"`.
    #[serde(default)]
    pub shares: Option<f64>,
    /// The knob `"pct_equity"` / `"pct_volatility"` / `"max_risk_pct"` each read as their own
    /// fraction of equity (target notional, ATR-risk budget, and stop-risk budget respectively —
    /// see [`vike_analytics::sizing`]'s doc on each). Required for those three kinds.
    #[serde(default)]
    pub pct: Option<f64>,
    /// [`vike_analytics::sizing::PortfolioHeatSizer::max_heat`] — total open-risk cap as a fraction of
    /// equity. Required for `"portfolio_heat"`.
    #[serde(default)]
    pub max_heat: Option<f64>,
    /// [`vike_analytics::sizing::DrawdownThrottleSizer::sensitivity`]. Required for `"drawdown_throttle"`.
    #[serde(default)]
    pub sensitivity: Option<f64>,
    /// [`vike_analytics::sizing::DrawdownThrottleSizer::floor`]. Required for `"drawdown_throttle"`.
    #[serde(default)]
    pub floor: Option<f64>,
    /// The wrapped sizer for `"portfolio_heat"` / `"drawdown_throttle"` — required for those two
    /// kinds ([`Self::build`] refuses their absence), and REFUSED under every other kind, which
    /// reads it nowhere: a base under a scalar kind would run one sizer while the profile read as
    /// a chain. Boxed because `SizerCfg` nests itself — see [`MAX_SIZER_DEPTH`] for the bound on
    /// how far.
    #[serde(default)]
    pub base: Option<Box<SizerCfg>>,
}

/// Bound on how many `SizerCfg` nodes one `[engine.sizer]` chain may nest (the top-level table
/// counts as depth 1; each `[engine.sizer.base…]` adds one).
///
/// `SizerCfg` is the first self-referential config struct in this file — `FeeCfg`/`ImpactCfg`/
/// `ResolutionCfg` are all flat — and [`SizerCfg::build`]'s recursion through [`SizerCfg::base`] is
/// one stack frame per level. Only the two WRAPPING kinds (`"portfolio_heat"`,
/// `"drawdown_throttle"`) ever consume a `base` at all, so the deepest MEANINGFUL profile composes
/// both of them once around one terminal scalar sizer — three `SizerCfg` nodes, e.g.
/// `drawdown_throttle` -> `portfolio_heat` -> `fixed_dollar`. `4` leaves exactly one level of
/// headroom past that (e.g. layering the same wrapping kind twice, such as two `portfolio_heat`
/// tiers at different `max_heat` caps) without leaving the bound so loose that an arbitrarily deep
/// `[engine.sizer.base.base.base…]` chain — no config file has a legitimate reason to nest further
/// than a human would hand-write — reads as accepted rather than refused.
///
/// ⚠ This bounds [`SizerCfg::build`]'s OWN recursion, not `toml`'s — and MEASURED (module test
/// `sizer_depth_probe`, ignored; the Task 12 fix-round report carries the full numbers) is a
/// genuine gap between the two. Deserializing the TOML into nested `SizerCfg`/`Box<SizerCfg>`
/// values happens BEFORE `build` (or `validate`, which calls it) ever runs, so for a chain a
/// little past this bound (measured: past ~50-79 levels) the `toml` crate's OWN internal
/// recursion limit fires FIRST, during `toml::from_str` itself, as a generic
/// `HarnessError::Parse("recursion limit")` that never mentions `engine.sizer` — this bound is
/// unreachable for those profiles, not merely redundant. It does NOT crash: no stack overflow was
/// observed at any depth tried. But the cost of DISCOVERING that limit is not free — measured
/// growth is roughly quadratic in nesting depth (depth 100 ⇒ ~5ms, depth 5,000 ⇒ ~8.3s), so a
/// sufficiently large adversarial chain (depth 50,000 was tried) turns "returns a clean error"
/// into "does not return inside several minutes" well before any crash would occur. This is a
/// property of the `toml` crate's handling of deeply-dotted table paths in general, not something
/// specific to `SizerCfg` or fixable from inside `build`/`validate` — it is a documented residual,
/// not a guard this bound provides.
pub const MAX_SIZER_DEPTH: usize = 4;

impl SizerCfg {
    /// Resolve the declared kind into the engine's trait object.
    ///
    /// ⚠ An unknown spelling — a kind missing one of its own required knobs — or a `base` chain
    /// past [`MAX_SIZER_DEPTH`] — is a `HarnessError::Validation` naming the valid set (or the
    /// limit), never a silent fallback to "no sizing", which would run the whole backtest unsized
    /// while the operator read the report as a sized one. This is the
    /// [`EngineCfg::fill_model_kind`] rule.
    pub fn build(&self) -> Result<Box<dyn PositionSizer>, HarnessError> {
        self.build_at_depth(1)
    }

    /// [`Self::build`]'s actual recursion, carrying the depth of `self` in the chain (the
    /// outermost `[engine.sizer]` table is depth 1) so a `base` past [`MAX_SIZER_DEPTH`] is
    /// refused before it is even matched against a `kind`, bounding this function's own stack
    /// usage to `MAX_SIZER_DEPTH + 1` frames regardless of how deep the DESERIALIZED value it was
    /// handed already is.
    fn build_at_depth(&self, depth: usize) -> Result<Box<dyn PositionSizer>, HarnessError> {
        if depth > MAX_SIZER_DEPTH {
            return Err(HarnessError::Validation(format!(
                "engine.sizer nesting is {depth} levels deep ([engine.sizer{}]) — \
                 MAX_SIZER_DEPTH is {MAX_SIZER_DEPTH}; flatten the base chain (no composition of \
                 the two wrapping kinds needs to nest this deep)",
                ".base".repeat(depth - 1)
            )));
        }
        fn need(field: &str, kind: &str, v: Option<f64>) -> Result<f64, HarnessError> {
            v.ok_or_else(|| {
                HarnessError::Validation(format!(
                    "engine.sizer.{field} is required when engine.sizer.kind = {kind:?}"
                ))
            })
        }
        let kind = self.kind.trim().to_ascii_lowercase();
        let sizer: Box<dyn PositionSizer> = match kind.as_str() {
            "pass_through" => Box::new(PassThroughSizer),
            "fixed_dollar" => {
                Box::new(FixedDollarSizer { amount: need("amount", &kind, self.amount)? })
            }
            "fixed_shares" => {
                Box::new(FixedSharesSizer { shares: need("shares", &kind, self.shares)? })
            }
            "pct_equity" => Box::new(PctEquitySizer { pct: need("pct", &kind, self.pct)? }),
            "pct_volatility" => Box::new(PctVolatilitySizer { pct: need("pct", &kind, self.pct)? }),
            "max_risk_pct" => Box::new(MaxRiskPctSizer { pct: need("pct", &kind, self.pct)? }),
            "portfolio_heat" => {
                let base = self.base.as_ref().ok_or_else(|| {
                    HarnessError::Validation(
                        "engine.sizer.kind = \"portfolio_heat\" wraps a base sizer — add an \
                         [engine.sizer.base] table naming it"
                            .to_string(),
                    )
                })?;
                Box::new(PortfolioHeatSizer {
                    base: base.build_at_depth(depth + 1)?,
                    max_heat: need("max_heat", &kind, self.max_heat)?,
                })
            }
            "drawdown_throttle" => {
                let base = self.base.as_ref().ok_or_else(|| {
                    HarnessError::Validation(
                        "engine.sizer.kind = \"drawdown_throttle\" wraps a base sizer — add an \
                         [engine.sizer.base] table naming it"
                            .to_string(),
                    )
                })?;
                Box::new(DrawdownThrottleSizer {
                    base: base.build_at_depth(depth + 1)?,
                    sensitivity: need("sensitivity", &kind, self.sensitivity)?,
                    floor: need("floor", &kind, self.floor)?,
                })
            }
            other => {
                return Err(HarnessError::Validation(format!(
                    "unknown engine.sizer.kind {other:?} (want pass_through | fixed_dollar | \
                     fixed_shares | pct_equity | pct_volatility | max_risk_pct | portfolio_heat \
                     | drawdown_throttle)"
                )));
            }
        };
        // ⚠ Only the two WRAPPING arms above ever read [`Self::base`]; every other arm builds one
        // sizer and never looks at it. So a `[engine.sizer.base]` table under a scalar kind
        // parses, validates, and runs ONE sizer while the operator reads the profile as a chain —
        // the silent-no-op class this whole surface exists to refuse. Checked here, AFTER the
        // match, so exactly one list of wrapping kinds exists (an unknown kind still gets its own
        // message above, which is the more useful one). ⚠ A new wrapping kind must be added to
        // this `matches!` as well as to the match above, or its base reads as forbidden.
        if self.base.is_some() && !matches!(kind.as_str(), "portfolio_heat" | "drawdown_throttle") {
            return Err(HarnessError::Validation(format!(
                "engine.sizer.kind = {kind:?} does not wrap a base sizer, but \
                 [engine.sizer{}.base] is set — only \"portfolio_heat\" and \"drawdown_throttle\" \
                 read it, so the chain would run {kind:?} alone. Remove the base table, or name a \
                 wrapping kind",
                ".base".repeat(depth - 1)
            )));
        }
        Ok(sizer)
    }
}

/// Strategy selection: a registry name (resolved by a later task) plus arbitrary TOML params
/// the strategy constructor interprets itself.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyCfg {
    pub name: String,
    #[serde(default = "default_params")]
    pub params: toml::Value,
}

fn default_params() -> toml::Value {
    toml::Value::Table(Default::default())
}

impl BacktestProfile {
    /// Parse + validate a profile from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, HarnessError> {
        let profile: BacktestProfile =
            toml::from_str(s).map_err(|e| HarnessError::Parse(e.to_string()))?;
        profile.validate()?;
        Ok(profile)
    }

    /// Parse + validate a profile from a file on disk, handing back the TEXT it was parsed from.
    ///
    /// Records the file's directory as [`Self::base_dir`] so relative sidecar paths (e.g.
    /// `[engine.resolution].path`) resolve next to the profile rather than against the caller's CWD.
    ///
    /// ⚠ **The text is returned rather than discarded so a run record can store the bytes that
    /// actually drove the run.** Re-reading the file at persist time would be a different read: an
    /// operator editing a profile while a long backtest runs is ordinary, and a record holding
    /// bytes the run did NOT use is worse than no record, because it looks authoritative.
    pub fn from_path_with_text(path: &Path) -> Result<(Self, String), HarnessError> {
        let s = std::fs::read_to_string(path)
            .map_err(|e| HarnessError::Io(format!("{}: {e}", path.display())))?;
        let mut profile = Self::from_toml_str(&s)?;
        profile.base_dir = path.parent().map(Path::to_path_buf);
        Ok((profile, s))
    }

    /// Parse + validate a profile from a file on disk. [`Self::from_path_with_text`] when the
    /// caller also wants the text; this is the door for callers that do not, and it DELEGATES so
    /// there is one parse and one `base_dir` rule rather than two.
    pub fn from_path(path: &Path) -> Result<Self, HarnessError> {
        Self::from_path_with_text(path).map(|(profile, _)| profile)
    }

    /// Resolve `data.from`/`data.to` to an inclusive epoch-ms [`TsRange`].
    pub fn range(&self) -> Result<TsRange, HarnessError> {
        let start = parse_ts(&self.data.from)?;
        let end = parse_ts(&self.data.to)?;
        Ok(TsRange::of(start, end))
    }

    /// Semantic validation beyond what serde's field/type checking already enforces: exactly
    /// one data-slice form with non-empty, unique series, `from <= to`, `cash > 0`, and the
    /// mode-compatibility rules for the opt-in engine knobs.
    ///
    /// ⚠ **This is the FIRST-ERROR ADAPTER over [`Self::refusals`] now, and nothing else.** It
    /// keeps the signature, the error TYPE and the error VALUE every existing caller and test
    /// reads: the rules accumulate the real [`HarnessError`]s they have always raised and this
    /// hands back the first of them verbatim — so an unparseable `data.from` still comes back as a
    /// [`HarnessError::Parse`] rather than being re-wrapped as a `Validation`, which a rebuilt
    /// error would have silently done. Rule ORDER is what makes that equivalence hold, and
    /// [`Self::refusals`] carries why it may not be reshuffled.
    ///
    /// [`Self::validate_all`] is the door for a caller that wants every mistake in one pass.
    pub fn validate(&self) -> Result<(), HarnessError> {
        match self.refusals().into_iter().next() {
            Some((_, err)) => Err(err),
            None => Ok(()),
        }
    }

    /// Every rule [`Self::validate`] runs, reported as a LIST — so somebody learns about five
    /// mistakes in one pass instead of paying a round trip per mistake.
    ///
    /// # Why an accumulating door exists
    ///
    /// A first-error validator charges one edit-and-rerun per mistake, and a profile is one file
    /// with eighty-odd keys in it: a freshly written one genuinely carries several at once. On the
    /// remote route each of those reruns is also a dial to a compute daemon that then re-reads the
    /// profile it was handed, so the round trips are neither free nor parallel. Nothing here is new
    /// validation — the rules, their order and their sentences are [`Self::validate`]'s — and the
    /// FIRST element is always the diagnostic `validate` itself would have returned. The two doors
    /// cannot disagree about which mistake governs, because they are one walk.
    ///
    /// Every diagnostic this produces today is [`vike_model::Severity::Error`]: a load-time
    /// refusal is all this validator has to say, and a profile that collects none of them is a
    /// profile that loads.
    ///
    /// # What it does NOT accumulate, said here rather than left to be discovered
    ///
    /// - **An unknown KEY — and that is a property of serde's error model, not a gap here.**
    ///   [`BacktestProfile`] is `#[serde(deny_unknown_fields)]`, and serde stops at the FIRST
    ///   undeclared key: by the time any rule below could run, deserialization has already failed
    ///   naming one key, and there is no profile to validate at all. So a file with five typo'd
    ///   key NAMES still reports one of them, and every diagnostic in this list is about a key
    ///   that parsed. Enumerating all five needs a `toml::Value` walk against the schema — a
    ///   different mechanism, checking a different thing, and deliberately not attempted from
    ///   inside a validator whose input is the already-deserialized struct.
    /// - **A second mistake inside ONE delegated rule.** A rule that calls out to
    ///   [`Self::validate_data_slice`], [`ImpactCfg::build`], [`SizerCfg::build`] or
    ///   [`WalkforwardCfg::window_form`] gets that function's own first error, so two mistakes in
    ///   one `[data]` table still report as one. The gain here is across TABLES, which is where a
    ///   new profile's mistakes actually sit; pushing accumulation down into those functions is a
    ///   wider change, because each is also called on paths that want a `Result` and each owns
    ///   refusal sentences whose evidence symbol is PUBLISHED (see [`Self::refusals`]).
    pub fn validate_all(&self) -> Vec<Diagnostic> {
        self.refusals()
            .into_iter()
            .map(|(key, err)| Diagnostic::error(key, err.message()))
            .collect()
    }

    /// The accumulating core both doors read: one row per rule that refused, each carrying the key
    /// a reader should look at and the [`HarnessError`] that rule actually raised.
    ///
    /// ⚠ **ORDER IS THE CONTRACT.** [`Self::validate`] returns element zero, so the sequence below
    /// is the short-circuit sequence this function replaced, unchanged — every `return Err(…)`
    /// became a push and the walk carries on. Reordering two rules changes which refusal every
    /// existing caller and test sees from an invalid profile, and the published surface would NOT
    /// catch it: `crates/vike-backtest/src/profile_surface.rs` sorts its refusal rows by message
    /// text, so a source reorder moves nothing in the asset. This file's own tests are the only
    /// thing that would.
    ///
    /// ⚠ **The refusal SENTENCES live here, and their home is a published fact rather than a
    /// private one.** `profile_surface`'s `parse_refusals` reads this file as TEXT, harvests every
    /// `HarnessError::Validation(` literal, and publishes each one with the ENCLOSING `fn` as its
    /// evidence symbol into `crates/vike-backtest/tests/fixtures/profile.json`, which is compared
    /// in-process on every PR. So moving a sentence between functions rewrites that asset and
    /// rewording one rewrites the operator-facing reference: both are real edits with a re-render
    /// attached, never incidental tidying.
    ///
    /// ⚠ **A rule whose input an earlier rule already refused is SKIPPED, never run on wreckage**,
    /// and each of those sites says so where it sits rather than being counted here: the
    /// `engine.multipliers` KEY check needs a resolved data slice, the coarser-than-base comparison
    /// needs a `data.interval` that parsed, the split-count and grid rules need a `[walkforward]`
    /// window form and a `search` value that resolved, and `from <= to` needs a range. What makes
    /// accumulating safe at all is that every OTHER rule reads only its own key, so nothing it says
    /// depends on a question already answered wrong.
    fn refusals(&self) -> Vec<(&'static str, HarnessError)> {
        let mut out: Vec<(&'static str, HarnessError)> = Vec::new();
        // The `[data]` half first, exactly as before — and its verdict is REMEMBERED, because one
        // later rule reads the slice this one may have just refused.
        let slice_ok = match self.validate_data_slice() {
            Ok(()) => true,
            Err(e) => {
                out.push(("data", e));
                false
            }
        };
        if self.engine.cash <= 0.0 {
            let e = HarnessError::Validation(format!(
                "engine.cash must be > 0, got {}",
                self.engine.cash
            ));
            out.push(("engine.cash", e));
        }
        // `risk.max_orders_per_window` is a WALL-CLOCK order-rate throttle; sim time is not wall
        // time (a month of ticks can replay in under a second), so it is REJECTED at load rather
        // than silently ignored — `SimBroker::build_risk_gate` always disarms it regardless
        // (`limits.max_orders_per_window = None`), and a limit an operator believes is armed but
        // never actually checked is exactly the failure class this `[risk]` wiring exists to kill.
        // Every other `risk.*` limit is honored.
        if let Some(r) = &self.risk
            && r.max_orders_per_window.is_some()
        {
            let e = HarnessError::Validation(
                "risk.max_orders_per_window is a wall-clock order-rate throttle — meaningless \
                     in backtest sim time (a replayed month can execute in under a second, which \
                     would spuriously rate-limit every order; SimBroker::build_risk_gate always \
                     disarms this field for the identical reason on the live-parity gate it \
                     mounts). Remove it from `[risk]` — every other risk.* limit is honored"
                    .to_string(),
            );
            out.push(("risk.max_orders_per_window", e));
        }
        // Fail on a bad model name / horizon at PARSE time, not silently at fill time.
        //
        // ⚠ There is deliberately NO mode rejection here any more. This block used to refuse
        // `data.kind = "tick"` outright ("the tick lane replays a real book, which already
        // prices size better than any model"). The premise was only half true: an L1 tick
        // tape fills ANY size at the quote, and an L2 replay walks a RECORDING that never
        // reacts to the order, so the permanent term is missing on both. The lane now charges
        // exactly what its own price law has not already paid
        // (`crates/vike-sim/src/engine/sim_broker.rs`'s `impact_terms`), which is a
        // decision the engine can make per FILL and this validator could only ever have made
        // per RUN — the L2 tier degrades to the L1 tier on any event with no book, so even
        // one profile has both answers in it.
        if let Some(imp) = &self.engine.impact
            && let Err(e) = imp.build()
        {
            out.push(("engine.impact", e));
        }
        if self.engine.feed_latency && self.data.kind == DataKind::Bar {
            let e = HarnessError::Validation(
                "engine.feed_latency is tick-mode only: it re-orders recorded ticks by their \
                 machine receive stamp (local_ts), which a bar series does not carry"
                    .to_string(),
            );
            out.push(("engine.feed_latency", e));
        }
        // The queue-position fill model is consulted exclusively by the `run_ticks` tick/book replay
        // lanes, so it is tick-mode only (the mirror of the `feed_latency` rule). A bar-mode profile
        // setting it would silently do nothing — rejected here rather than mislead. Also validates
        // the string so a typo fails at load, not silently.
        if self.engine.queue_model.is_some() && self.data.kind == DataKind::Bar {
            let e = HarnessError::Validation(
                "engine.queue_model is tick-mode only: the FIFO queue lane runs in the tick/book \
                 replay path, which a bar series does not drive"
                    .to_string(),
            );
            out.push(("engine.queue_model", e));
        }
        // Fail-fast on an unrecognized queue_model string.
        if let Err(e) = self.engine.queue_model_kind() {
            out.push(("engine.queue_model", e));
        }
        // The equity-curve density knob is read only by `run_ticks` (the bar lane's curve is one
        // sample per BAR — bounded by the bar count, nothing to thin), so it is tick-mode only.
        // A bar-mode profile setting it would silently do nothing and quietly leave the operator
        // believing the run's drawdown/sharpe were computed over a subsample they chose.
        if self.engine.equity_sample_every.is_some() && self.data.kind == DataKind::Bar {
            let e = HarnessError::Validation(
                "engine.equity_sample_every is tick-mode only: the bar lane records one equity \
                 sample per BAR (bounded by the bar count), so there is nothing to thin"
                    .to_string(),
            );
            out.push(("engine.equity_sample_every", e));
        }
        // Same rule for the tick-lane fill-model override: resolve it here so a typo ("L2Book",
        // "l2_book") fails the profile at LOAD with the valid set named, never a silent
        // optimistic-`Tick` fallback that would quietly undo the depth-cap realism knob (#819).
        if let Err(e) = self.engine.fill_model_kind() {
            out.push(("engine.fill_model", e));
        }
        // `attach_funding` window-joins the market funding series onto the price bars, and the
        // accrual it feeds runs ONLY in the bar loop — the tick lane has no per-interval funding
        // fold — so it is bar-mode only (the mirror of the tick-only `feed_latency` rule).
        if self.engine.attach_funding && self.data.kind == DataKind::Tick {
            let e = HarnessError::Validation(
                "engine.attach_funding is bar-mode only: the per-interval funding accrual runs in \
                 the bar loop; the tick lane carries no funding fold to feed"
                    .to_string(),
            );
            out.push(("engine.attach_funding", e));
        }
        // `cash_gate` is bar-mode only. `StrategyEngine::run` is its ONLY reader — the `granular`
        // mask and the `fill_step_gated` branch — and `StrategyEngine::fill_step_gated`'s single
        // caller sits inside that bar loop; `StrategyEngine::run_ticks` never consults it, and
        // neither does any `SimBroker` fill lane the tick path drives. So `kind = "tick"` +
        // `cash_gate = true` would parse, validate, reach `EngineParams` and change nothing — the
        // exact defect every other lane-asymmetric key here already refuses (`feed_latency`,
        // `queue_model`, `equity_sample_every`, `attach_funding`, `timeframes`).
        if self.engine.cash_gate && self.data.kind == DataKind::Tick {
            let e = HarnessError::Validation(
                "engine.cash_gate is bar-mode only: the shared-cash admission phase is \
                 StrategyEngine::run's own bar step (fill_step_gated), and run_ticks never \
                 consults it — a tick profile setting it would change nothing"
                    .to_string(),
            );
            out.push(("engine.cash_gate", e));
        }
        // `engine.decide`'s three refusals. The KEY itself parses in either lane (an unknown
        // spelling is `decide_mode`'s own refusal); what is checked here is the combinations, all
        // three of which would otherwise let a run BELIEVE it computed a cross-section it did not.
        //
        // ⚠ The resolver is called rather than the raw string matched, so a future spelling
        // cannot be silently exempted from any of the three by being spelled differently. An
        // unparseable value falls through to `Sequential` here and is refused by the resolver at
        // the construction site, so it is reported once rather than twice.
        if decide_mode(self.engine.decide.as_deref()).unwrap_or_default().is_simultaneous() {
            // (1) BAR MODE ONLY, for the same reason `cash_gate` above is: the allocator the mode
            // orders lives in `StrategyEngine::fill_step_gated`, reached only from
            // `StrategyEngine::run`. `run_ticks` consults neither field, so a tick profile asking
            // for a simultaneous cross-section would parse, validate, reach `EngineParams` and
            // change nothing.
            if self.data.kind == DataKind::Tick {
                let e = HarnessError::Validation(
                    "engine.decide = \"simultaneous\" is bar-mode only: the cross-sectional \
                     allocator it orders is StrategyEngine::fill_step_gated, reached only from \
                     StrategyEngine::run — run_ticks consults neither it nor cash_gate, so a tick \
                     profile setting it would change nothing"
                        .to_string(),
                );
                out.push(("engine.decide", e));
            }
            // (2) The DECLARED GAP. With a pre-trade gate armed (`[risk]`, or `engine.leverage`
            // through `SimBroker::build_risk_gate`'s leverage→initial-margin mapping),
            // `SimBroker::gate_order` folds the `pending` set — which ACCUMULATES across the
            // `on_bar` fan-out — so the symbol judged FIRST faces the step's whole budget and each
            // later one faces the remainder. Ordering the allocator cannot repair that: by the
            // time the fill phase ranks anything, the loser's order was already denied or shrunk
            // at submit. The fold is not the bug (a pre-trade cap must model the local view — see
            // `SimBroker::margin_in_use_pending_aware`); the missing piece is a step-boundary
            // budget snapshot, and it is NOT BUILT. Refused rather than accepted, because a mode
            // whose DECISION half is still taken in list order while its name says otherwise is
            // worse than no mode at all.
            //
            // ⚠ `clamp_to_leverage` does not escape this: it routes to
            // `SimBroker::cap_to_leverage`, whose own pending fold has the same shape.
            if self.risk.is_some() || self.engine.leverage.is_some() {
                let e = HarnessError::Validation(
                    "engine.decide = \"simultaneous\" is not built for a run with a pre-trade \
                     risk gate: remove [risk] and engine.leverage, or keep engine.decide \
                     unset. With a gate armed, SimBroker::gate_order folds the pending set as it \
                     ACCUMULATES during the on_bar fan-out, so the symbol judged first faces the \
                     whole step budget and later ones face the remainder — the DECISION is still \
                     taken in symbol-list order and no fill-phase ranking can repair it. Closing \
                     it needs a step-boundary budget snapshot, which does not exist"
                        .to_string(),
                );
                out.push(("engine.decide", e));
            }
            // (3) `detail_interval`, refused for exactly the reason `cash_gate` refuses it below:
            // this mode IMPLIES the gated lane, and `StrategyEngine::run`'s granular mask is
            // `!gated && !sub[i].is_empty()`, so the detail tape would be loaded, paid for and
            // then ignored.
            if self.data.detail_interval.is_some() {
                let e = HarnessError::Validation(
                    "data.detail_interval and engine.decide = \"simultaneous\" are mutually \
                     exclusive: the simultaneous mode implies the shared-cash admission phase, \
                     which routes the whole bar step through fill_step_gated and disables the \
                     granular sub-bar lane — the detail tape would be loaded, paid for and ignored"
                        .to_string(),
                );
                out.push(("engine.decide", e));
            }
        }
        // `timeframes` is bar-mode only (the coarse series are resampled from the base BAR
        // stream, and a tick profile has none), and every entry must be a valid,
        // strictly-coarser-than-base interval — left to the engine it is
        // `parse_timeframe(tf).expect(..)`, a process abort where every neighbouring key gives a
        // named refusal.
        if !self.engine.timeframes.is_empty() {
            if self.data.kind == DataKind::Tick {
                let e = HarnessError::Validation(
                    "engine.timeframes is bar-mode only: the coarse series are resampled from the \
                     base BAR stream, and a tick profile has none"
                        .to_string(),
                );
                out.push(("engine.timeframes", e));
            }
            // The base interval is the DENOMINATOR of the coarser-than-base rule below, so a
            // profile whose own `data.interval` does not parse gets that refusal and then the
            // COMPARISON is skipped — while each entry is still checked on its own terms, which
            // is the half that needs no denominator.
            let base = vike_model::time::interval_ms(&self.data.interval);
            if base.is_none() {
                let e = HarnessError::Validation(format!(
                    "data.interval {:?} is not a valid interval, so engine.timeframes cannot be \
                     checked against it",
                    self.data.interval
                ));
                out.push(("data.interval", e));
            }
            for tf in &self.engine.timeframes {
                let Some(ms) = vike_model::time::interval_ms(tf) else {
                    let e = HarnessError::Validation(format!(
                        "engine.timeframes {tf:?} is not a valid interval (want a count then one \
                         of s/m/h/d, e.g. \"4h\")"
                    ));
                    out.push(("engine.timeframes", e));
                    continue;
                };
                if ms <= 0 {
                    let e = HarnessError::Validation(format!(
                        "engine.timeframes {tf:?} resolves to {ms}ms — a zero-length window makes \
                         every boundary the same instant"
                    ));
                    out.push(("engine.timeframes", e));
                    // A zero-length window is also "not coarser than the base", so this entry has
                    // said its one useful thing — the second sentence would only repeat it.
                    continue;
                }
                if let Some(base) = base
                    && ms <= base
                {
                    let e = HarnessError::Validation(format!(
                        "engine.timeframes {tf:?} ({ms}ms) must be coarser than data.interval {:?} \
                         ({base}ms) — a finer window cannot be synthesised from the base stream \
                         and would return the base bars re-labelled",
                        self.data.interval
                    ));
                    out.push(("engine.timeframes", e));
                }
            }
        }
        if self.engine.maint_margin < 0.0 {
            let e = HarnessError::Validation(format!(
                "engine.maint_margin must be >= 0, got {} — 0 means the liquidation watchdog is \
                 off, and a negative rate has no meaning",
                self.engine.maint_margin
            ));
            out.push(("engine.maint_margin", e));
        }
        if self.engine.liq_buffer < 0.0 {
            let e = HarnessError::Validation(format!(
                "engine.liq_buffer must be >= 0, got {}",
                self.engine.liq_buffer
            ));
            out.push(("engine.liq_buffer", e));
        }
        if let Some(v) = self.engine.volume_limit
            && !(v > 0.0 && v <= 1.0)
        {
            let e = HarnessError::Validation(format!(
                "engine.volume_limit must be in (0, 1], got {v} — it is the fraction of the \
                 EVENT's own volume one fill may take (a bar's volume in bar mode, ONE PRINT's \
                 in tick mode), so 1.0 is all of it"
            ));
            out.push(("engine.volume_limit", e));
        }
        if self.engine.multiplier <= 0.0 {
            let e = HarnessError::Validation(format!(
                "engine.multiplier must be > 0, got {} — it scales every position's notional, so \
                 zero makes every trade worth nothing",
                self.engine.multiplier
            ));
            out.push(("engine.multiplier", e));
        }
        for (sym, m) in &self.engine.multipliers {
            if *m <= 0.0 {
                let e = HarnessError::Validation(format!(
                    "engine.multipliers.{sym} must be > 0, got {m}"
                ));
                out.push(("engine.multipliers", e));
            }
        }
        // ...and the KEY half of the same rule. `StrategyEngine::new` resolves each loaded
        // symbol's multiplier by EXACT name (`p.multipliers.iter().find(|(name, _)| name == s)`)
        // and falls back to the global `p.multiplier`, so a row naming a symbol this run does not
        // load is read by nobody: the run sizes at 1.0 (or whatever the global says) while the
        // operator reads the profile as contract-sized. A typo is the whole failure mode — which
        // makes this the `[engine]` surface's own instance of the defect class this file exists
        // to refuse, and the reason the valid set is named back in the message.
        if slice_ok && !self.engine.multipliers.is_empty() {
            // Gated on `slice_ok` rather than on `validate_data_slice` having merely RUN: this
            // rule names the loaded symbols back, so it is only answerable once the slice is
            // well-formed and its symbols are known unique. On a refused slice it is skipped —
            // a "the data slice is: " listing built from wreckage would be a second, wrong
            // refusal chasing the first one.
            let slice: BTreeSet<String> =
                self.data.resolved_series().into_iter().map(|s| s.symbol).collect();
            for sym in self.engine.multipliers.keys() {
                if !slice.contains(sym) {
                    let known: Vec<&str> = slice.iter().map(String::as_str).collect();
                    let e = HarnessError::Validation(format!(
                        "engine.multipliers.{sym} names a symbol this run does not load — the \
                         engine matches a multiplier row by EXACT symbol name and otherwise falls \
                         back to the global engine.multiplier, so this row would change nothing. \
                         The data slice is: {}",
                        known.join(", ")
                    ));
                    out.push(("engine.multipliers", e));
                }
            }
        }
        if let Some(l) = self.engine.leverage
            && l <= 0.0
        {
            let e = HarnessError::Validation(format!(
                "engine.leverage must be > 0, got {l} — omit the key for unlevered"
            ));
            out.push(("engine.leverage", e));
        }
        // ⚠ ORDER IS LOAD-BEARING, and this block used to have it backwards. The real end state
        // — `clamp_to_leverage` is unavailable to ANY profile carrying `[risk]` — was stated
        // NOWHERE, so a profile with `[risk]` and `clamp_to_leverage = true` was told by the
        // "needs engine.leverage" rule to add a key that the leverage/`[risk]` rule then refused.
        // Two refusals, one dead end, and the constraint that actually governs never named. It is
        // named FIRST now.
        //
        // ⚠ An accumulating door shows BOTH of that pair at once where the first-error door
        // showed only the governing one, and that is an improvement rather than the old dead end:
        // the reader sees "drop clamp_to_leverage" and "clamp needs engine.leverage" together, and
        // the first answer dissolves both. What must not change is WHICH comes first.
        //
        // The mechanism: `SimBroker::build_risk_gate` returns `None` on `p.clamp_to_leverage`
        // BEFORE it so much as reads `p.risk_limits`, so the clamp disarms the WHOLE pre-trade
        // gate — every `[risk]` limit, not merely `max_leverage`. `[risk]` is the discarded side
        // here, which is the OPPOSITE of what the leverage/`[risk]` rule below describes for the
        // clamp-off case.
        if self.engine.clamp_to_leverage && self.risk.is_some() {
            let e = HarnessError::Validation(
                "engine.clamp_to_leverage is unavailable to a profile carrying [risk]: \
                 SimBroker::build_risk_gate returns None on clamp_to_leverage BEFORE it reads \
                 risk_limits, so the clamp disarms the pre-trade gate entirely and EVERY [risk] \
                 limit — not just max_leverage — is discarded. Drop clamp_to_leverage to keep \
                 [risk]'s gate, or remove [risk] and clamp against engine.leverage alone"
                    .to_string(),
            );
            out.push(("engine.clamp_to_leverage", e));
        }
        if self.engine.clamp_to_leverage && self.engine.leverage.is_none() {
            let e = HarnessError::Validation(
                "engine.clamp_to_leverage needs engine.leverage: there is nothing to clamp to"
                    .to_string(),
            );
            out.push(("engine.clamp_to_leverage", e));
        }
        // Two margin sources, one gate. `SimBroker::build_risk_gate` matches
        // `(&p.risk_limits, p.leverage)` and its `(Some(l), _)` arm takes `risk_limits`
        // UNCONDITIONALLY, so with the clamp OFF (the default) `engine.leverage` is the silently
        // discarded side — whether or not `[risk]` sets `max_leverage`. Refuse rather than let
        // that precedence pick for the operator, the same shape as the `engine.fee` /
        // `engine.fee_rate` refusal below. ⚠ The clamp-ON case is NOT this one and is refused
        // above: there the early `None` return discards `[risk]` instead.
        if self.engine.leverage.is_some() && self.risk.is_some() {
            let e = HarnessError::Validation(
                "engine.leverage and [risk] are two different margin sources for the same gate — \
                 set exactly one. With clamp_to_leverage off (the default) there are three \
                 configurations and [risk] wins all of them: SimBroker::build_risk_gate's \
                 (Some(l), _) arm takes risk_limits unconditionally, so a [risk] that sets \
                 max_leverage answers with ITS number, a [risk] that does not sets no margin \
                 requirement at all, and either way engine.leverage's 1/L im_requirement is \
                 silently discarded. (With clamp_to_leverage on, neither reaches a gate — that \
                 combination is refused separately.) Set risk.max_leverage instead, or remove \
                 [risk] to use engine.leverage's 1/L margin mapping"
                    .to_string(),
            );
            out.push(("engine.leverage", e));
        }
        if let Some(ms) = self.engine.settlement_period_ms
            && ms <= 0
        {
            let e = HarnessError::Validation(format!(
                "engine.settlement_period_ms must be > 0, got {ms} — omit the key to never \
                 settle"
            ));
            out.push(("engine.settlement_period_ms", e));
        }
        // Two cost models, one fee: refuse rather than let a silent precedence rule pick.
        if let Some(fee) = &self.engine.fee {
            if let Err(e) = fee.build() {
                out.push(("engine.fee", e));
            }
            if self.engine.fee_rate != 0.0 {
                let e = HarnessError::Validation(format!(
                    "engine.fee and a non-zero engine.fee_rate ({}) are two different cost \
                     models — set exactly one",
                    self.engine.fee_rate
                ));
                out.push(("engine.fee", e));
            }
        }
        // Fail on a bad `kind` / missing knob / missing wrapped base at PARSE time, the same
        // load-fast rule as `fee`/`impact` above — never a silent fallback to "no sizing", which
        // would run the whole backtest unsized while the operator read the report as a sized one.
        if let Some(sizer) = &self.engine.sizer
            && let Err(e) = sizer.build()
        {
            out.push(("engine.sizer", e));
        }
        // What fails at LOAD in this section, and what deliberately does not. A `[walkforward]`
        // value that is wrong ON ITS OWN TERMS — independent of the store, of the data slice and
        // of which verb is running — fails here: a zero split count (there is no window to test
        // on), an unrecognized `search`/`mode`/`rank_by` string (a typo must never degrade into a
        // silently different protocol), `search = "sweep"` on a profile carrying no `[sweep]`
        // table (a walk that says it optimizes with nothing to search is a contradiction that no
        // data can resolve, and it is the same shape as the zero-split hole above), and — since
        // the duration window form landed — an incoherent WINDOW FORM or a duration that is not
        // one (the block below argues each).
        //
        // The bar-mode and single-symbol rules are NOT checked here, and the difference is what
        // they depend on: those are questions about what the profile RESOLVES against a store,
        // while `[walkforward]` is inert for a plain `run_backtest`/`run_paramscan` run — so
        // rejecting a profile that merely CARRIES the section would fail runs that never ask for
        // a walk-forward. `super::walkforward` enforces those where they apply, and re-enforces
        // the rules below too, because a hand-built profile never passed through here at all.
        if let Some(wf) = &self.walkforward {
            // The WINDOW FORM is the first question, and every incoherent combination of it is
            // wrong on its own terms too: a form that is absent, doubled or half-declared (a table
            // with no window shape is not a walk), and `purge`/`embargo` under the split-count
            // form (`docs/decisions/0046-the-bar-mode-walk-forward-has-no-purge.md`). What is NOT
            // checked here is how many BARS a duration resolves to: that needs `data.interval` AND
            // the series, so it belongs to `super::windows::resolve_windows`, which the drivers
            // call.
            //
            // An unresolvable form leaves the split-count rule below UNASKED rather than guessed
            // at: `n_splits = 0` beside a `train`/`test` pair is already refused BY the form rule,
            // and answering it twice would name two mistakes where the profile has one.
            let form = match wf.window_form() {
                Ok(form) => Some(form),
                Err(e) => {
                    out.push(("walkforward", e));
                    None
                }
            };
            if form == Some(WindowForm::Splits) && wf.n_splits == Some(0) {
                let e = HarnessError::Validation(
                    "walkforward.n_splits must be >= 1, got 0".to_string(),
                );
                out.push(("walkforward.n_splits", e));
            }
            // Every duration is RESOLVED here so a typo or a zero fails at load, exactly as the
            // string knobs below are; the values are discarded because the driver reads them
            // again from the same resolvers.
            for (key, parsed) in [
                ("walkforward.train", wf.train_span()),
                ("walkforward.test", wf.test_span()),
                ("walkforward.step", wf.step_span()),
                ("walkforward.purge", wf.purge_span()),
                ("walkforward.embargo", wf.embargo_span()),
            ] {
                if let Err(e) = parsed {
                    out.push((key, e));
                }
            }
            // Every string knob is RESOLVED here so a typo fails at load; only `search`'s value is
            // wanted (the grid rule below reads it), and the other two are called for the parse
            // alone — the drivers read them again from the same resolvers.
            let search = match wf.window_search() {
                Ok(search) => Some(search),
                Err(e) => {
                    out.push(("walkforward.search", e));
                    None
                }
            };
            if let Err(e) = wf.walk_mode() {
                out.push(("walkforward.mode", e));
            }
            if let Err(e) = wf.rank_metric() {
                out.push(("walkforward.rank_by", e));
            }
            if search == Some(WindowSearch::Sweep) && !self.is_paramscan() {
                let e = HarnessError::Validation(
                    "walkforward.search = \"sweep\" needs a [sweep] table to search — this \
                     profile has none, so every window would 'select' the one parameter set it \
                     already has. Add the grid, or drop the key for the no-search control."
                        .to_string(),
                );
                out.push(("walkforward.search", e));
            }
        }
        // Fail on a bad resolution kind/window at PARSE time; the winners file + coverage check
        // need the profile's base_dir and run late, in `ResolutionCfg::build`.
        if let Some(res) = &self.engine.resolution
            && let Err(e) = res.validate_shape()
        {
            out.push(("engine.resolution", e));
        }
        // ── the two `[data]` time resolvers, promoted from RUN time to LOAD time ────────────────
        // Both are already called on the run path — `warmup_steps` at both `bar_engine_params`
        // lanes, `detail_interval_ms` in the tape loader — so every refusal here already fired.
        // What moves is WHEN: calling them from this function makes them answerable before a store
        // is opened, which is the whole difference between a typo costing a round trip and a typo
        // costing nothing.
        if let Err(e) = self.data.warmup_steps() {
            out.push(("data.warmup", e));
        }
        if let Err(e) = self.data.detail_interval_ms() {
            out.push(("data.detail_interval", e));
        }
        // The three coverage/universe key SPELLINGS, answered before a store is opened for the same
        // reason the two above are: a typo in a disposition should cost nothing, not a round trip
        // to a compute daemon that then reads the profile it was handed. Each one decides whether a
        // run happens at all, so none of them falls back.
        if let Err(e) = self.data.on_gap() {
            out.push(("data.on_gap", e));
        }
        if let Err(e) = self.data.max_gap_ms() {
            out.push(("data.max_gap", e));
        }
        if let Err(e) = self.data.universe_mode() {
            out.push(("data.universe", e));
        }
        // ...and the ARMING rule for the two that are meaningless alone. A disposition or a
        // tolerance without `require_coverage` is a key an operator wrote, a reader will believe,
        // and nothing consults — `vike_config::CONSUMPTION`'s whole subject, and worse here than
        // there because what it appears to configure is a REFUSAL: somebody reads
        // `on_gap = "refuse"` and believes a run over an uncovered window cannot happen.
        if self.data.on_gap.is_some() && !self.data.require_coverage {
            let e = HarnessError::Validation(
                "data.on_gap is set without data.require_coverage = true, so the gate it \
                 configures is not armed and this key is read by nothing — while it reads as \
                 though a run over an uncovered window would be stopped. Arm the gate, or remove \
                 the disposition"
                    .to_string(),
            );
            out.push(("data.on_gap", e));
        }
        if self.data.max_gap.is_some() && !self.data.require_coverage {
            let e = HarnessError::Validation(
                "data.max_gap is set without data.require_coverage = true, so the gate whose \
                 tolerance it names is not armed and this key is read by nothing. Arm the gate, or \
                 remove the tolerance"
                    .to_string(),
            );
            out.push(("data.max_gap", e));
        }
        // ── the three rules that read TWO tables, which is why they live here and not on a cfg ──
        // A cfg struct sees its own table. Each rule below compares two, so this function is the
        // only place that can ask the question at all.
        if self.data.detail_interval.is_some() {
            // ⚠ The dangerous one: the run SUCCEEDS and the number looks plausible.
            // `StrategyEngine::run`'s granular mask is `!cash_gate && !sub[i].is_empty()`, so the
            // detail tape would be loaded, scanned and then ignored while the operator believed
            // every order was resolved against it.
            if self.engine.cash_gate {
                let e = HarnessError::Validation(
                    "data.detail_interval and engine.cash_gate are mutually exclusive: \
                     cash_gate routes the whole bar step through the gated fill lane and \
                     UNCONDITIONALLY disables the granular sub-bar lane, so the detail tape would \
                     be loaded, scanned and then ignored while every order was reported as \
                     resolved against it. Set one."
                        .to_string(),
                );
                out.push(("data.detail_interval", e));
            }
            // ⚠ This one is LOOK-AHEAD, not waste. A walk hands each window a SLICED coarse series
            // while nothing slices the detail tape, and a sub-bar is bucketed by
            // `partition_point(|e| e <= sub.ts) - 1` — so every window's LAST coarse step would
            // absorb the whole remaining tape and fill against bars from AFTER the window ended.
            if self.walkforward.is_some() {
                let e = HarnessError::Validation(
                    "data.detail_interval is unavailable to a [walkforward] profile: the walk \
                     hands each window a sliced coarse series while nothing slices the detail \
                     tape, so every window's last step would fill against bars from AFTER the \
                     window ended. That is look-ahead, not merely waste. Run the profile without \
                     [walkforward], or without the detail tape."
                        .to_string(),
                );
                out.push(("data.detail_interval", e));
            }
        }
        // The per-share shapes model an EQUITIES book, where a contract multiplier is 1.0. The fee
        // path multiplies `FeeSchedule::commission`'s answer by the symbol's multiplier, which is
        // right for a notional cap and WRONG for the per-share term and the floor: a $1.00
        // per-order minimum is one dollar, not one dollar per contract, so a multiplier of 50
        // charges a $50 minimum. `FeeCfg` cannot see `[engine]`, so the rule is here.
        if let Some(fee) = &self.engine.fee
            && fee.kind == "per_share_with_floor"
        {
            let multiplied = self.engine.multiplier != 1.0
                || self.engine.multipliers.values().any(|m| *m != 1.0);
            if multiplied {
                let e = HarnessError::Validation(
                    "engine.fee.kind = \"per_share_with_floor\" needs engine.multiplier = 1.0 \
                     (the default) and no engine.multipliers row other than 1.0: the fee path \
                     scales the schedule's answer by the contract multiplier, so a per-ORDER \
                     minimum would be charged per CONTRACT. This shape models an equities book. \
                     Drop the multiplier, or price a multiplied contract with percent_maker_taker."
                        .to_string(),
                );
                out.push(("engine.fee.kind", e));
            }
        }
        // The range is the LAST rule, as it was. `from <= to` needs a range that PARSED, so an
        // unparseable stamp is reported and the comparison skipped rather than asked of the
        // `i64::MIN`/`MAX` fallbacks, which would answer a question about values nobody wrote.
        match self.range() {
            Err(e) => out.push(("data", e)),
            Ok(range) => {
                let start = range.start.unwrap_or(i64::MIN);
                let end = range.end.unwrap_or(i64::MAX);
                if start > end {
                    let e = HarnessError::Validation(format!(
                        "data.from ({start}) must be <= data.to ({end})"
                    ));
                    out.push(("data.from", e));
                }
            }
        }
        out
    }

    /// The `[data]` half of [`Self::validate`]: exactly one slice form, non-empty, symbols
    /// unique, and the lane filter only where a lane exists.
    fn validate_data_slice(&self) -> Result<(), HarnessError> {
        let legacy = self.data.venue.is_some() || !self.data.symbols.is_empty();
        let series = !self.data.series.is_empty();
        match (legacy, series) {
            (true, true) => {
                return Err(HarnessError::Validation(
                    "data.venue/data.symbols and [[data.series]] are two ways to name the SAME \
                     slice — set exactly one (the [[data.series]] form is the cross-venue one)"
                        .to_string(),
                ));
            }
            (false, false) => {
                return Err(HarnessError::Validation(
                    "data.symbols must not be empty (or use the cross-venue [[data.series]] form)"
                        .to_string(),
                ));
            }
            (true, false) => {
                if self.data.venue.is_none() {
                    return Err(HarnessError::Validation(
                        "data.venue is required alongside data.symbols".to_string(),
                    ));
                }
                if self.data.symbols.is_empty() {
                    return Err(HarnessError::Validation(
                        "data.symbols must not be empty".to_string(),
                    ));
                }
            }
            (false, true) => {}
        }

        // `run_ticks`/`StrategyEngine` resolve a payload to a symbol SLOT by name, so a repeated
        // symbol (even across venues) would silently route both series into one slot.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for s in self.data.resolved_series() {
            if !seen.insert(s.symbol.clone()) {
                return Err(HarnessError::Validation(format!(
                    "duplicate symbol {:?} in the data slice — the engine routes ticks to a \
                     symbol slot BY NAME, so two series cannot share one symbol",
                    s.symbol
                )));
            }
        }

        if self.data.kind == DataKind::Bar
            && let Some(s) = self.data.series.iter().find(|s| s.kind != SeriesKind::Tick)
        {
            return Err(HarnessError::Validation(format!(
                "data.series[{:?}].kind = {:?} is tick-mode only: a bar series has no \
                     quote/trade/book lanes to filter",
                s.symbol, s.kind
            )));
        }

        // The PIT grid is looked up under ONE `default_venue`, so snapping a cross-venue slice
        // would read the wrong venue's grid for every series but the first.
        if self.engine.snap_to_properties && self.data.is_cross_venue() {
            return Err(HarnessError::Validation(
                "engine.snap_to_properties is single-venue only: the point-in-time instrument \
                 grid is looked up under one default_venue, so a cross-venue [[data.series]] \
                 slice would snap every series to the first series' venue"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// True if `paramscan` is present and non-empty — the marker that this profile expands into a
    /// parameter grid ([`super::sweep::expand_paramscan`]) rather than running as a single backtest.
    ///
    /// ⚠ **NOT to be confused with `vike_data::removal::SeriesSelector::is_sweep`**, which is a
    /// different concept in a different crate and, until this rename, wore the identical bare name
    /// and call syntax: how broadly a data-store DELETION selector reaches. That one is a
    /// bulk-delete safety gate and KEEPS its name — `crates/vike-datahub/src/server.rs`'s
    /// `delete_series_verb` still calls `selector.is_sweep()`, and a mechanical rename that took
    /// both would have silently re-scoped a bulk delete. This one is the parameter search, and
    /// renaming it is what disambiguates the two. Both spellings COMPILE at either site, so the
    /// only thing separating them is which meaning was intended.
    pub fn is_paramscan(&self) -> bool {
        self.paramscan.as_ref().is_some_and(|t| !t.is_empty())
    }
}

/// Parse a `from`/`to` field: a bare epoch-ms integer first, else a `YYYY-MM-DDTHH` UTC hour
/// label (Howard-Hinnant civil-calendar math, mirrors `vike-backfill`'s `pmxt_backfill` hour
/// range helpers). An hour label with no minutes/seconds means `:00:00`.
pub(crate) fn parse_ts(s: &str) -> Result<i64, HarnessError> {
    if let Ok(ms) = s.parse::<i64>() {
        return Ok(ms);
    }
    vike_model::time::parse_hour_label(s)
        .map(|(y, m, d, h)| {
            vike_model::time::days_from_civil(y, m, d) * 86_400_000 + h as i64 * 3_600_000
        })
        .ok_or_else(|| {
            let mut msg = String::new();
            let _ = write!(msg, "invalid timestamp {s:?}: expected epoch-ms or YYYY-MM-DDTHH");
            HarnessError::Parse(msg)
        })
}

#[path = "profile_tests.rs"]
#[cfg(test)]
mod profile_tests;
