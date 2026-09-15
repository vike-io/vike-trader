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
//!   [`crate::EngineParams::resolution`] source (and its `resolution_end_ts`) that
//!   `SimBroker::settle_at_payout` has always consumed but no profile could configure.
//! * **G7 — a real fee SCHEDULE, not just a flat rate.** `[engine.fee]` selects a
//!   [`vike_model::FeeSchedule`]; the prediction-market `probability_scaled` curve
//!   (`qty × rate × p(1−p)`) is the shape a flat `fee_rate` cannot express at all.
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
use crate::ResolutionSource;
use crate::cheap_np::TokenId;
use crate::engine::{EquitySampling, FillModelKind};
use crate::hist_replay::{SeriesKind, SeriesRef};
use crate::queue_model::QueueModelKind;
use crate::sizing::{
    DrawdownThrottleSizer, FixedDollarSizer, FixedSharesSizer, MaxRiskPctSizer, PassThroughSizer,
    PctEquitySizer, PctVolatilitySizer, PortfolioHeatSizer, PositionSizer,
};
use crate::validation::WalkMode;
use vike_data::TsRange;
use vike_exec::ProfileRisk;
use vike_model::FeeSchedule;
use vike_model::time::{Span, parse_span};

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
    /// default) ⇒ [`crate::EngineParams::risk_limits`] stays `None` ⇒ byte-identical to every
    /// profile written before this field existed: `[engine]` today has no `leverage` knob either,
    /// so `[risk]` is the ONLY way a harness profile arms the gate at all
    /// ([`crate::engine::SimBroker::build_risk_gate`]'s `(None, None) => None` arm).
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
    /// list, because a `crate::validation::Split` is a contiguous half-open quadruple with
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
        match raw.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(WindowSearch::None),
            "sweep" => Ok(WindowSearch::Sweep),
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
    /// Opt-in ORDER latency (see [`crate::latency`]): a fixed entry-leg delay in MILLISECONDS on
    /// every strategy order action (place / modify / cancel). The maker's quotes, re-prices and
    /// pulls only reach the matching engine `order_latency_ms` after it decides them, so it cannot
    /// react within one tick — the missing clock skew that otherwise lets a maker enter and exit in
    /// the same instant. `0` (the default) = zero latency, byte-identical. TICK MODE ONLY (only
    /// `run_ticks` arms the latency gate; the bar lane and vector kernel never consult it).
    #[serde(default)]
    pub order_latency_ms: i64,
    /// Opt-in FILL-notification latency (the response leg, see [`crate::latency`]): a fixed delay in
    /// MILLISECONDS before the strategy LEARNS of a fill. The fill books at the real time (equity/PnL
    /// are exact), but the strategy-visible shadow position ([`vike_model::HftBroker::position`])
    /// does not advance until `fill_latency_ms` later — so a maker that polls its inventory cannot
    /// react (place its exit) inside that gap. Models the "time to realise you're filled" half of a
    /// real round-trip reaction. `0` (the default) = off, byte-identical. TICK MODE ONLY.
    #[serde(default)]
    pub fill_latency_ms: i64,
    /// Opt-in tick-lane fill-model override: `"l2book"` (the ONE accepted value; case-insensitive,
    /// like [`Self::queue_model`]) selects the depth-capped
    /// [`crate::engine::FillModelKind::L2Book`] (a resting order fills only up to the DISPLAYED book
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
    /// Opt-in market-impact slippage on top of the flat `slippage` (see [`crate::impact`]).
    /// Absent (the default) = the frozen flat-slippage cost, byte-identical.
    ///
    /// BOTH MODES since the lane split. This was BAR MODE ONLY and rejected outright in tick
    /// mode, on the reasoning that "the tick lane replays a real book and needs no model" — half
    /// right, and the wrong half is now the point of the knob: an L1 tick tape has no book at
    /// all (it fills any size at the quote), and even an L2 replay walks a RECORDING that never
    /// moves in response to the order, so the permanent footprint is missing there too. Each
    /// lane is charged only what its own price law has not already paid — see
    /// [`crate::impact::ImpactTerms`], which is where that decision lives.
    ///
    /// ⚠ [`ImpactCfg::window`] changes UNITS with the mode (bars vs trade prints).
    #[serde(default)]
    pub impact: Option<ImpactCfg>,
    /// Stop-verb release timing (see [`crate::EngineParams::emulator_release_stops`]).
    ///
    /// ⚠ The HARNESS default is `false`, matching [`crate::EngineParams::default()`]: a fired
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
    /// [`crate::EngineParams::resolution`] source + `resolution_end_ts` the engine has always
    /// consumed. Absent (the default) = no settlement source, byte-identical.
    #[serde(default)]
    pub resolution: Option<ResolutionCfg>,
    /// Opt-in QUEUE-POSITION fill model for the tick lane (see [`crate::queue_model`]): a resting
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
    /// L1 quote gives a size at a resting order's price — see [`crate::queue_model`]. Only consulted
    /// when [`Self::queue_model`] is set. Absent ⇒ `0.0` (a quote with no observed size-ahead fills
    /// on the first at-price trade).
    #[serde(default)]
    pub queue_seed_depth: Option<f64>,
    /// Minimum-hold floor in MS for the queued lane ([`crate::EngineParams::queue_min_hold_ms`]): a
    /// position-reducing (closing) fill is deferred until this long after the position opened, so the
    /// backtest can't fabricate a sub-second maker round-trip. Only consulted when [`Self::queue_model`]
    /// is set. Absent ⇒ `0` (off). Calibrate from real MM flip times (Polymarket BTC-5m ≈ 2s p10).
    #[serde(default)]
    pub queue_min_hold_ms: Option<i64>,
    /// Equity-curve density for the tick lane (see [`crate::EquitySampling`]) — the SWEEP knob for
    /// a very long tape. The curve is two `Vec`s grown 16 bytes per priced tick, so a 100M-tick
    /// replay carries 1.6 GB of samples per run whether or not anything reads them.
    ///
    /// - absent or `1` ⇒ [`crate::EquitySampling::EveryTick`], the frozen default, byte-identical;
    /// - `N > 1` ⇒ [`crate::EquitySampling::EveryN`], keep one sample per `N` ticks (plus a closing
    ///   sample at the last tick, so `equity_curve.last()` still agrees with `final_equity`);
    /// - `0` ⇒ [`crate::EquitySampling::Off`], record nothing.
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
    /// `crate::engine::SimBroker::bars_for`/`forming_for` returning `None` where they used to
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
    /// [`EngineParams::session_gate`] alone. [`EngineParams::session_calendars`] is
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
    /// ([`crate::sizing::PassThroughSizer`]) — what every profile did before this key existed
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

/// TOML shape of the opt-in fee schedule ([`EngineCfg::fee`]).
///
/// Today one `kind`, deliberately: `"probability_scaled"` — the prediction-market curve
/// `fee = qty × rate × p(1−p)` ([`vike_model::FeeSchedule::ProbabilityScaled`]). It is the ONE
/// shape with no flat equivalent, so it is the one a profile genuinely could not reach:
/// `FeeSchedule::maker_taker_rates()` reports `(0.0, 0.0)` for it, and the flat
/// [`EngineCfg::fee_rate`] has no `p(1−p)` term at all. Flat maker/taker costs stay on
/// `fee_rate`.
///
/// ```toml
/// [engine.fee]
/// kind = "probability_scaled"
/// taker_rate = 0.072          # the live `cheap_np` cost: 0.072·p·(1−p) per share
/// maker_rate = 0.0
/// maker_rebate_share = 0.0
/// ```
///
/// NOTE this does NOT change [`vike_model::fee_schedule_for`]'s registry default for
/// `"polymarket"` (still `Free`) — the paper fill path and the snapshot cost display read that
/// registry, and flipping it would move existing users' numbers. A profile that wants the curve
/// asks for it here.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeeCfg {
    /// currently only `"probability_scaled"`
    pub kind: String,
    #[serde(default)]
    pub taker_rate: f64,
    #[serde(default)]
    pub maker_rate: f64,
    #[serde(default)]
    pub maker_rebate_share: f64,
}

impl FeeCfg {
    /// Resolve to a [`vike_model::FeeSchedule`], or `Err` on an unknown `kind` / a negative rate
    /// — a typo must fail the profile, not silently price every fill at zero.
    pub fn build(&self) -> Result<FeeSchedule, HarnessError> {
        match self.kind.as_str() {
            "probability_scaled" => {
                for (name, v) in [
                    ("taker_rate", self.taker_rate),
                    ("maker_rate", self.maker_rate),
                    ("maker_rebate_share", self.maker_rebate_share),
                ] {
                    if !v.is_finite() || v < 0.0 {
                        return Err(HarnessError::Validation(format!(
                            "engine.fee.{name} must be finite and >= 0, got {v}"
                        )));
                    }
                }
                Ok(FeeSchedule::ProbabilityScaled {
                    taker_rate: self.taker_rate,
                    maker_rate: self.maker_rate,
                    maker_rebate_share: self.maker_rebate_share,
                })
            }
            other => Err(HarnessError::Validation(format!(
                "unknown engine.fee.kind {other:?} (known: \"probability_scaled\"; flat \
                 maker/taker costs stay on engine.fee_rate)"
            ))),
        }
    }
}

/// TOML shape of the opt-in binary-resolution settlement source ([`EngineCfg::resolution`]).
///
/// One `kind` today — `"binary_outcome"`, the two-outcome prediction-market convention the
/// `cheap_np` tape uses: a series symbol is `<slug>#<outcome_index>` where the slug's trailing
/// `-<sts>` is the window open in epoch SECONDS ([`crate::TokenId`] is the parser, shared with
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
    /// [`crate::cheap_np::WINDOW_SECS`] (300). NOTE the symbol GRAMMAR is independent of this:
    /// `TokenId::parse` pins `sts` to a multiple of `WINDOW_SECS`, so a different `window_secs`
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
    /// Explicit override for [`crate::EngineParams::resolution_end_ts`] (epoch-ms or
    /// `YYYY-MM-DDTHH`). Absent = the LATEST resolution among the run's own series
    /// (`max(sts) + window_secs`), which is what pins the end-of-run sweep to the window rather
    /// than to the `RESOLUTION_PROBE_SENTINEL`.
    #[serde(default)]
    pub end_ts: Option<String>,
}

fn default_window_secs() -> i64 {
    crate::cheap_np::WINDOW_SECS
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
/// [`crate::EngineParams::default()`]. ⚠ This previously returned `true` ("mirror-live") while the
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
    /// in bar mode, TRADE PRINTS in tick mode (see [`crate::impact::AlmgrenChriss::exec_time`]
    /// and [`crate::impact::TickWindow`]). The default `1.0` means "worked inside one period".
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
    /// Permanent-impact coefficient, defaulting to the published [`crate::impact::AC_GAMMA`].
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
    /// Temporary-impact coefficient, defaulting to the published [`crate::impact::AC_ETA`]. The
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
    crate::DEFAULT_IMPACT_WINDOW
}

fn default_ac_gamma() -> f64 {
    crate::impact::AC_GAMMA
}

fn default_ac_eta() -> f64 {
    crate::impact::AC_ETA
}

impl ImpactCfg {
    /// Resolve to a live model, or `Err` on an unknown `model` name / nonsensical numbers — a
    /// typo in a profile must fail loudly, not silently price fills at zero impact.
    pub fn build(&self) -> Result<Arc<dyn crate::impact::ImpactModel>, HarnessError> {
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
            "almgren_chriss" => Ok(Arc::new(crate::impact::AlmgrenChriss::with_coefficients(
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
/// standing alone (see [`crate::sizing::PortfolioHeatSizer`]/[`crate::sizing::DrawdownThrottleSizer`]);
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
    /// [`crate::sizing::FixedDollarSizer::amount`] — fixed cash notional per entry. Required for
    /// `"fixed_dollar"`.
    #[serde(default)]
    pub amount: Option<f64>,
    /// [`crate::sizing::FixedSharesSizer::shares`] — fixed share/contract count per entry.
    /// Required for `"fixed_shares"`.
    #[serde(default)]
    pub shares: Option<f64>,
    /// The knob `"pct_equity"` / `"pct_volatility"` / `"max_risk_pct"` each read as their own
    /// fraction of equity (target notional, ATR-risk budget, and stop-risk budget respectively —
    /// see [`vike_analytics::sizing`]'s doc on each). Required for those three kinds.
    #[serde(default)]
    pub pct: Option<f64>,
    /// [`crate::sizing::PortfolioHeatSizer::max_heat`] — total open-risk cap as a fraction of
    /// equity. Required for `"portfolio_heat"`.
    #[serde(default)]
    pub max_heat: Option<f64>,
    /// [`crate::sizing::DrawdownThrottleSizer::sensitivity`]. Required for `"drawdown_throttle"`.
    #[serde(default)]
    pub sensitivity: Option<f64>,
    /// [`crate::sizing::DrawdownThrottleSizer::floor`]. Required for `"drawdown_throttle"`.
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
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.validate_data_slice()?;
        if self.engine.cash <= 0.0 {
            return Err(HarnessError::Validation(format!(
                "engine.cash must be > 0, got {}",
                self.engine.cash
            )));
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
            return Err(HarnessError::Validation(
                "risk.max_orders_per_window is a wall-clock order-rate throttle — meaningless \
                     in backtest sim time (a replayed month can execute in under a second, which \
                     would spuriously rate-limit every order; SimBroker::build_risk_gate always \
                     disarms this field for the identical reason on the live-parity gate it \
                     mounts). Remove it from `[risk]` — every other risk.* limit is honored"
                    .to_string(),
            ));
        }
        if let Some(imp) = &self.engine.impact {
            // Fail on a bad model name / horizon at PARSE time, not silently at fill time.
            //
            // ⚠ There is deliberately NO mode rejection here any more. This block used to refuse
            // `data.kind = "tick"` outright ("the tick lane replays a real book, which already
            // prices size better than any model"). The premise was only half true: an L1 tick
            // tape fills ANY size at the quote, and an L2 replay walks a RECORDING that never
            // reacts to the order, so the permanent term is missing on both. The lane now charges
            // exactly what its own price law has not already paid
            // (`crates/vike-backtest/src/engine/sim_broker.rs`'s `impact_terms`), which is a
            // decision the engine can make per FILL and this validator could only ever have made
            // per RUN — the L2 tier degrades to the L1 tier on any event with no book, so even
            // one profile has both answers in it.
            imp.build()?;
        }
        if self.engine.feed_latency && self.data.kind == DataKind::Bar {
            return Err(HarnessError::Validation(
                "engine.feed_latency is tick-mode only: it re-orders recorded ticks by their \
                 machine receive stamp (local_ts), which a bar series does not carry"
                    .to_string(),
            ));
        }
        // The queue-position fill model is consulted exclusively by the `run_ticks` tick/book replay
        // lanes, so it is tick-mode only (the mirror of the `feed_latency` rule). A bar-mode profile
        // setting it would silently do nothing — rejected here rather than mislead. Also validates
        // the string so a typo fails at load, not silently.
        if self.engine.queue_model.is_some() && self.data.kind == DataKind::Bar {
            return Err(HarnessError::Validation(
                "engine.queue_model is tick-mode only: the FIFO queue lane runs in the tick/book \
                 replay path, which a bar series does not drive"
                    .to_string(),
            ));
        }
        // Fail-fast on an unrecognized queue_model string.
        self.engine.queue_model_kind()?;
        // The equity-curve density knob is read only by `run_ticks` (the bar lane's curve is one
        // sample per BAR — bounded by the bar count, nothing to thin), so it is tick-mode only.
        // A bar-mode profile setting it would silently do nothing and quietly leave the operator
        // believing the run's drawdown/sharpe were computed over a subsample they chose.
        if self.engine.equity_sample_every.is_some() && self.data.kind == DataKind::Bar {
            return Err(HarnessError::Validation(
                "engine.equity_sample_every is tick-mode only: the bar lane records one equity \
                 sample per BAR (bounded by the bar count), so there is nothing to thin"
                    .to_string(),
            ));
        }
        // Same rule for the tick-lane fill-model override: resolve it here so a typo ("L2Book",
        // "l2_book") fails the profile at LOAD with the valid set named, never a silent
        // optimistic-`Tick` fallback that would quietly undo the depth-cap realism knob (#819).
        self.engine.fill_model_kind()?;
        // `attach_funding` window-joins the market funding series onto the price bars, and the
        // accrual it feeds runs ONLY in the bar loop — the tick lane has no per-interval funding
        // fold — so it is bar-mode only (the mirror of the tick-only `feed_latency` rule).
        if self.engine.attach_funding && self.data.kind == DataKind::Tick {
            return Err(HarnessError::Validation(
                "engine.attach_funding is bar-mode only: the per-interval funding accrual runs in \
                 the bar loop; the tick lane carries no funding fold to feed"
                    .to_string(),
            ));
        }
        // `cash_gate` is bar-mode only. `StrategyEngine::run` is its ONLY reader — the `granular`
        // mask and the `fill_step_gated` branch — and `StrategyEngine::fill_step_gated`'s single
        // caller sits inside that bar loop; `StrategyEngine::run_ticks` never consults it, and
        // neither does any `SimBroker` fill lane the tick path drives. So `kind = "tick"` +
        // `cash_gate = true` would parse, validate, reach `EngineParams` and change nothing — the
        // exact defect every other lane-asymmetric key here already refuses (`feed_latency`,
        // `queue_model`, `equity_sample_every`, `attach_funding`, `timeframes`).
        if self.engine.cash_gate && self.data.kind == DataKind::Tick {
            return Err(HarnessError::Validation(
                "engine.cash_gate is bar-mode only: the shared-cash admission phase is \
                 StrategyEngine::run's own bar step (fill_step_gated), and run_ticks never \
                 consults it — a tick profile setting it would change nothing"
                    .to_string(),
            ));
        }
        // `timeframes` is bar-mode only (the coarse series are resampled from the base BAR
        // stream, and a tick profile has none), and every entry must be a valid,
        // strictly-coarser-than-base interval — left to the engine it is
        // `parse_timeframe(tf).expect(..)`, a process abort where every neighbouring key gives a
        // named refusal.
        if !self.engine.timeframes.is_empty() {
            if self.data.kind == DataKind::Tick {
                return Err(HarnessError::Validation(
                    "engine.timeframes is bar-mode only: the coarse series are resampled from the \
                     base BAR stream, and a tick profile has none"
                        .to_string(),
                ));
            }
            let base = vike_model::time::interval_ms(&self.data.interval).ok_or_else(|| {
                HarnessError::Validation(format!(
                    "data.interval {:?} is not a valid interval, so engine.timeframes cannot be \
                     checked against it",
                    self.data.interval
                ))
            })?;
            for tf in &self.engine.timeframes {
                let ms = vike_model::time::interval_ms(tf).ok_or_else(|| {
                    HarnessError::Validation(format!(
                        "engine.timeframes {tf:?} is not a valid interval (want a count then one \
                         of s/m/h/d, e.g. \"4h\")"
                    ))
                })?;
                if ms <= 0 {
                    return Err(HarnessError::Validation(format!(
                        "engine.timeframes {tf:?} resolves to {ms}ms — a zero-length window makes \
                         every boundary the same instant"
                    )));
                }
                if ms <= base {
                    return Err(HarnessError::Validation(format!(
                        "engine.timeframes {tf:?} ({ms}ms) must be coarser than data.interval {:?} \
                         ({base}ms) — a finer window cannot be synthesised from the base stream \
                         and would return the base bars re-labelled",
                        self.data.interval
                    )));
                }
            }
        }
        if self.engine.maint_margin < 0.0 {
            return Err(HarnessError::Validation(format!(
                "engine.maint_margin must be >= 0, got {} — 0 means the liquidation watchdog is \
                 off, and a negative rate has no meaning",
                self.engine.maint_margin
            )));
        }
        if self.engine.liq_buffer < 0.0 {
            return Err(HarnessError::Validation(format!(
                "engine.liq_buffer must be >= 0, got {}",
                self.engine.liq_buffer
            )));
        }
        if let Some(v) = self.engine.volume_limit
            && !(v > 0.0 && v <= 1.0)
        {
            return Err(HarnessError::Validation(format!(
                "engine.volume_limit must be in (0, 1], got {v} — it is the fraction of the \
                 EVENT's own volume one fill may take (a bar's volume in bar mode, ONE PRINT's \
                 in tick mode), so 1.0 is all of it"
            )));
        }
        if self.engine.multiplier <= 0.0 {
            return Err(HarnessError::Validation(format!(
                "engine.multiplier must be > 0, got {} — it scales every position's notional, so \
                 zero makes every trade worth nothing",
                self.engine.multiplier
            )));
        }
        for (sym, m) in &self.engine.multipliers {
            if *m <= 0.0 {
                return Err(HarnessError::Validation(format!(
                    "engine.multipliers.{sym} must be > 0, got {m}"
                )));
            }
        }
        // ...and the KEY half of the same rule. `StrategyEngine::new` resolves each loaded
        // symbol's multiplier by EXACT name (`p.multipliers.iter().find(|(name, _)| name == s)`)
        // and falls back to the global `p.multiplier`, so a row naming a symbol this run does not
        // load is read by nobody: the run sizes at 1.0 (or whatever the global says) while the
        // operator reads the profile as contract-sized. A typo is the whole failure mode — which
        // makes this the `[engine]` surface's own instance of the defect class this file exists
        // to refuse, and the reason the valid set is named back in the message.
        if !self.engine.multipliers.is_empty() {
            // `validate_data_slice` already ran (it is this function's first statement), so the
            // slice is well-formed and its symbols are unique by the time this resolves.
            let slice: BTreeSet<String> =
                self.data.resolved_series().into_iter().map(|s| s.symbol).collect();
            for sym in self.engine.multipliers.keys() {
                if !slice.contains(sym) {
                    let known: Vec<&str> = slice.iter().map(String::as_str).collect();
                    return Err(HarnessError::Validation(format!(
                        "engine.multipliers.{sym} names a symbol this run does not load — the \
                         engine matches a multiplier row by EXACT symbol name and otherwise falls \
                         back to the global engine.multiplier, so this row would change nothing. \
                         The data slice is: {}",
                        known.join(", ")
                    )));
                }
            }
        }
        if let Some(l) = self.engine.leverage
            && l <= 0.0
        {
            return Err(HarnessError::Validation(format!(
                "engine.leverage must be > 0, got {l} — omit the key for unlevered"
            )));
        }
        // ⚠ ORDER IS LOAD-BEARING, and this block used to have it backwards. The real end state
        // — `clamp_to_leverage` is unavailable to ANY profile carrying `[risk]` — was stated
        // NOWHERE, so a profile with `[risk]` and `clamp_to_leverage = true` was told by the
        // "needs engine.leverage" rule to add a key that the leverage/`[risk]` rule then refused.
        // Two refusals, one dead end, and the constraint that actually governs never named. It is
        // named FIRST now.
        //
        // The mechanism: `SimBroker::build_risk_gate` returns `None` on `p.clamp_to_leverage`
        // BEFORE it so much as reads `p.risk_limits`, so the clamp disarms the WHOLE pre-trade
        // gate — every `[risk]` limit, not merely `max_leverage`. `[risk]` is the discarded side
        // here, which is the OPPOSITE of what the leverage/`[risk]` rule below describes for the
        // clamp-off case.
        if self.engine.clamp_to_leverage && self.risk.is_some() {
            return Err(HarnessError::Validation(
                "engine.clamp_to_leverage is unavailable to a profile carrying [risk]: \
                 SimBroker::build_risk_gate returns None on clamp_to_leverage BEFORE it reads \
                 risk_limits, so the clamp disarms the pre-trade gate entirely and EVERY [risk] \
                 limit — not just max_leverage — is discarded. Drop clamp_to_leverage to keep \
                 [risk]'s gate, or remove [risk] and clamp against engine.leverage alone"
                    .to_string(),
            ));
        }
        if self.engine.clamp_to_leverage && self.engine.leverage.is_none() {
            return Err(HarnessError::Validation(
                "engine.clamp_to_leverage needs engine.leverage: there is nothing to clamp to"
                    .to_string(),
            ));
        }
        // Two margin sources, one gate. `SimBroker::build_risk_gate` matches
        // `(&p.risk_limits, p.leverage)` and its `(Some(l), _)` arm takes `risk_limits`
        // UNCONDITIONALLY, so with the clamp OFF (the default) `engine.leverage` is the silently
        // discarded side — whether or not `[risk]` sets `max_leverage`. Refuse rather than let
        // that precedence pick for the operator, the same shape as the `engine.fee` /
        // `engine.fee_rate` refusal below. ⚠ The clamp-ON case is NOT this one and is refused
        // above: there the early `None` return discards `[risk]` instead.
        if self.engine.leverage.is_some() && self.risk.is_some() {
            return Err(HarnessError::Validation(
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
            ));
        }
        if let Some(ms) = self.engine.settlement_period_ms
            && ms <= 0
        {
            return Err(HarnessError::Validation(format!(
                "engine.settlement_period_ms must be > 0, got {ms} — omit the key to never \
                 settle"
            )));
        }
        // Two cost models, one fee: refuse rather than let a silent precedence rule pick.
        if let Some(fee) = &self.engine.fee {
            fee.build()?;
            if self.engine.fee_rate != 0.0 {
                return Err(HarnessError::Validation(format!(
                    "engine.fee and a non-zero engine.fee_rate ({}) are two different cost \
                     models — set exactly one",
                    self.engine.fee_rate
                )));
            }
        }
        // Fail on a bad `kind` / missing knob / missing wrapped base at PARSE time, the same
        // load-fast rule as `fee`/`impact` above — never a silent fallback to "no sizing", which
        // would run the whole backtest unsized while the operator read the report as a sized one.
        if let Some(sizer) = &self.engine.sizer {
            sizer.build()?;
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
            let form = wf.window_form()?;
            if form == WindowForm::Splits && wf.n_splits == Some(0) {
                return Err(HarnessError::Validation(
                    "walkforward.n_splits must be >= 1, got 0".to_string(),
                ));
            }
            // Every duration is RESOLVED here so a typo or a zero fails at load, exactly as the
            // string knobs below are; the values are discarded because the driver reads them
            // again from the same resolvers.
            wf.train_span()?;
            wf.test_span()?;
            wf.step_span()?;
            wf.purge_span()?;
            wf.embargo_span()?;
            // Every string knob is RESOLVED here so a typo fails at load; only `search`'s value is
            // wanted (the grid rule below reads it), and the other two are called for the parse
            // alone — the drivers read them again from the same resolvers.
            let search = wf.window_search()?;
            wf.walk_mode()?;
            wf.rank_metric()?;
            if search == WindowSearch::Sweep && !self.is_paramscan() {
                return Err(HarnessError::Validation(
                    "walkforward.search = \"sweep\" needs a [sweep] table to search — this \
                     profile has none, so every window would 'select' the one parameter set it \
                     already has. Add the grid, or drop the key for the no-search control."
                        .to_string(),
                ));
            }
        }
        // Fail on a bad resolution kind/window at PARSE time; the winners file + coverage check
        // need the profile's base_dir and run late, in `ResolutionCfg::build`.
        if let Some(res) = &self.engine.resolution {
            res.validate_shape()?;
        }
        let range = self.range()?;
        let (start, end) = (range.start.unwrap_or(i64::MIN), range.end.unwrap_or(i64::MAX));
        if start > end {
            return Err(HarnessError::Validation(format!(
                "data.from ({start}) must be <= data.to ({end})"
            )));
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠ **THE ALIAS WARRANTY, and it is the whole of what makes the R2 rename safe.** Owner ruling
    /// R2 renamed the section `[sweep]` → `[paramscan]`; [`BacktestProfile`] is
    /// `deny_unknown_fields`, so without `#[serde(alias = "sweep")]` every profile already written
    /// — on operators' disks, in this repository's own measurement records, in the `sweep.toml`
    /// `vike-cli init` has scaffolded since it shipped — would fail to LOAD with "unknown field".
    ///
    /// Both spellings must produce the IDENTICAL parsed grid, which is what this asserts. The alias
    /// is PERMANENT: this is not a deprecation with an end date, and deleting it is a breaking
    /// change to every profile ever written, not a tidy-up.
    #[test]
    fn both_the_new_section_and_the_legacy_one_load_and_parse_identically() {
        let grid = "\nfast = [5, 10]\nslow = [20, 30]\n";
        let new = BacktestProfile::from_toml_str(&format!("{BAR_TOML}\n[paramscan]{grid}"))
            .expect("[paramscan] is the name");
        let old = BacktestProfile::from_toml_str(&format!("{BAR_TOML}\n[sweep]{grid}"))
            .expect("[sweep] must keep loading FOREVER — every profile on disk spells it this way");
        assert_eq!(new.paramscan, old.paramscan, "one grid, two spellings");
        assert!(new.is_paramscan() && old.is_paramscan(), "both declare a parameter search");
        assert_eq!(
            new.paramscan.as_ref().map(toml::Table::len),
            Some(2),
            "…and the grid is the one that was written, not an empty table"
        );
    }

    /// ⚠ …and writing BOTH in one file is REFUSED, which is the right answer rather than a gap:
    /// two grids in one profile has no meaning, and a silent winner would be the different-answer
    /// defect the whole stage exists to end. serde produces this for a field with an alias.
    #[test]
    fn writing_both_spellings_in_one_profile_is_refused() {
        let err = BacktestProfile::from_toml_str(&format!(
            "{BAR_TOML}\n[paramscan]\nfast = [5]\n\n[sweep]\nslow = [20]\n"
        ))
        .expect_err("one profile, one grid");
        let msg = err.to_string();
        assert!(
            msg.contains("paramscan") || msg.contains("sweep") || msg.contains("duplicate"),
            "the refusal names the collision: {msg}"
        );
    }

    const BAR_TOML: &str = r#"
name = "bar-demo"

[data]
venue = "binance"
symbols = ["BTCUSDT", "ETHUSDT"]
kind = "bar"
interval = "1h"
from = "2026-01-01T00"
to = "2026-01-02T00"

[engine]
cash = 100000.0
fee_rate = 0.001

[strategy]
name = "sma_cross"
[strategy.params]
fast = 10
slow = 20
"#;

    const TICK_TOML: &str = r#"
name = "demo"

[data]
venue = "polymarket"
symbols = ["0xTOK"]
kind = "tick"
from = "2026-04-13T19"
to = "2026-04-13T20"

[engine]
cash = 10000.0
snap_to_properties = true

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
"#;

    const BARE_MS_TOML: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "1700000000000"
to = "1700003600000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
"#;

    #[test]
    fn queue_model_parses_resolves_and_is_tick_only() {
        // Each accepted string resolves to the right kind on a tick profile.
        let kind = |qm: &str| {
            let toml =
                TICK_TOML.replace("snap_to_properties = true", &format!("queue_model = \"{qm}\""));
            BacktestProfile::from_toml_str(&toml).unwrap().engine.queue_model_kind().unwrap()
        };
        assert!(matches!(kind("risk_adverse"), Some(QueueModelKind::RiskAdverse)));
        assert!(matches!(kind("prob_log"), Some(QueueModelKind::ProbLog)));
        assert!(matches!(kind("prob_power"), Some(QueueModelKind::ProbPower(n)) if n == 1.0));
        assert!(matches!(kind("prob_power:2.5"), Some(QueueModelKind::ProbPower(n)) if n == 2.5));
        // Absent ⇒ None (the frozen simple-crossing fill).
        assert!(
            BacktestProfile::from_toml_str(TICK_TOML)
                .unwrap()
                .engine
                .queue_model_kind()
                .unwrap()
                .is_none()
        );
        // An unrecognized string fails validation at LOAD (fail-fast, not a silent fallback).
        let bad = TICK_TOML.replace("snap_to_properties = true", "queue_model = \"fifo\"");
        assert!(BacktestProfile::from_toml_str(&bad).is_err(), "unknown queue_model rejected");
        // Tick-mode only: a bar profile setting it is rejected with a clear message.
        let barq = BAR_TOML.replace("fee_rate = 0.001", "queue_model = \"risk_adverse\"");
        let err = BacktestProfile::from_toml_str(&barq).unwrap_err();
        assert!(
            format!("{err}").contains("tick-mode only"),
            "bar-mode queue_model rejected: {err}"
        );
    }

    /// The tick-lane `fill_model` knob parses STRICTLY, like `queue_model`: absent ⇒ the default
    /// `Tick`, the one accepted spelling resolves to `L2Book`, and a typo fails at LOAD with the
    /// valid set named — never the old silent optimistic-`Tick` fallback that undid the #819
    /// depth-cap realism knob.
    #[test]
    fn fill_model_parses_strictly_and_defaults_to_tick() {
        // Absent ⇒ the default L1 spread-crossing Tick model.
        let p = BacktestProfile::from_toml_str(TICK_TOML).unwrap();
        assert_eq!(p.engine.fill_model_kind().unwrap(), FillModelKind::Tick);

        // The accepted spelling resolves to the depth-capped model; case/whitespace normalize
        // (the same trim + lowercase idiom `queue_model_kind` uses).
        let kind = |fm: &str| {
            let toml =
                TICK_TOML.replace("snap_to_properties = true", &format!("fill_model = \"{fm}\""));
            BacktestProfile::from_toml_str(&toml).unwrap().engine.fill_model_kind().unwrap()
        };
        assert_eq!(kind("l2book"), FillModelKind::L2Book);
        assert_eq!(kind("L2Book"), FillModelKind::L2Book);
        assert_eq!(kind(" l2book "), FillModelKind::L2Book);

        // A typo fails validation at LOAD, naming the valid set (fail-fast, not a silent
        // fallback). "l2_book" is exactly the audit-finding footgun.
        for junk in ["l2_book", "tick", "book"] {
            let bad =
                TICK_TOML.replace("snap_to_properties = true", &format!("fill_model = \"{junk}\""));
            let err = BacktestProfile::from_toml_str(&bad).unwrap_err();
            let msg = format!("{err}");
            assert!(
                msg.contains("unknown fill_model") && msg.contains("l2book"),
                "junk fill_model {junk:?} rejected with the valid set named: {msg}"
            );
        }
    }

    #[test]
    fn timeframes_reach_the_profile() {
        let toml =
            BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ntimeframes = [\"4h\", \"1d\"]");
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        assert_eq!(p.engine.timeframes, vec!["4h".to_string(), "1d".to_string()]);
    }

    #[test]
    fn timeframes_default_to_empty() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
        assert!(p.engine.timeframes.is_empty());
    }

    /// An unparseable timeframe is refused at LOAD. Left to the engine it is
    /// `parse_timeframe(tf).expect("valid timeframe")` — a process abort, where every neighbouring
    /// `[engine]` key gives a named error.
    #[test]
    fn an_unparseable_timeframe_is_rejected_at_load() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ntimeframes = [\"4hh\"]");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("timeframes"), "{m}");
                assert!(m.contains("4hh"), "names the offending value: {m}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// `interval_ms` accepts a zero count, so `"0h"` parses to 0 ms and would make every window
    /// boundary the same instant. Refused here because nothing downstream checks it.
    #[test]
    fn a_zero_length_timeframe_is_rejected_at_load() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ntimeframes = [\"0h\"]");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("timeframes"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// A coarser-than-base timeframe is the whole point; a FINER one cannot be synthesised from the
    /// base stream and would silently return the base bars re-labelled.
    #[test]
    fn a_timeframe_finer_than_the_base_interval_is_rejected() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ntimeframes = [\"1m\"]");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("timeframes"), "{m}");
                assert!(m.contains("coarser"), "{m}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// Tick mode synthesises no bars, so a declared timeframe there configures nothing.
    #[test]
    fn timeframes_are_rejected_on_the_tick_lane() {
        let toml = TICK_TOML.replace(
            "snap_to_properties = true",
            "snap_to_properties = true\ntimeframes = [\"4h\"]",
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("timeframes"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn parses_bar_profile() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
        assert_eq!(p.name.as_deref(), Some("bar-demo"));
        assert_eq!(p.data.venue.as_deref(), Some("binance"));
        assert_eq!(p.data.symbols, vec!["BTCUSDT", "ETHUSDT"]);
        assert_eq!(p.data.kind, DataKind::Bar);
        assert_eq!(p.data.interval, "1h");
        assert_eq!(p.engine.cash, 100000.0);
        assert_eq!(p.engine.fee_rate, 0.001);
        // defaults
        assert_eq!(p.engine.slippage, 0.0);
        assert!(!p.engine.snap_to_properties);
        assert_eq!(p.engine.seed_bar_interval_ms, None);
        // The harness default for the stop-verb release knob is `false`, matching the raw
        // `EngineParams` default — see `default_emulator_release_stops`'s doc for why this is not
        // the mirror-live `true` its doc used to claim.
        assert!(!p.engine.emulator_release_stops);
        assert_eq!(p.strategy.name, "sma_cross");

        // 2026-01-01T00:00 UTC and 2026-01-02T00:00 UTC (verified against `date -u -d`).
        let r = p.range().unwrap();
        assert_eq!(r.start, Some(1_767_225_600_000));
        assert_eq!(r.end, Some(1_767_312_000_000));
    }

    /// The harness stop-verb release knob defaults to `false` (matching
    /// `EngineParams::default()`) and an explicit `emulator_release_stops = true` parses through
    /// to arm the opt-in mirror-live release. ⚠ This test previously asserted the OPPOSITE default
    /// (`true`) — that default never reached the engine through either `EngineParams`
    /// construction site in `harness::run`, so every harness run was `false` regardless; see
    /// `default_emulator_release_stops`'s doc.
    #[test]
    fn emulator_release_stops_defaults_false_and_parses_true() {
        // absent -> the harness default, false (matches EngineParams::default())
        let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
        assert!(!p.engine.emulator_release_stops);

        // explicit true threads through
        let toml =
            BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nemulator_release_stops = true");
        let p = BacktestProfile::from_toml_str(&toml).unwrap();
        assert!(p.engine.emulator_release_stops);

        // explicit false also parses
        let toml = BAR_TOML
            .replace("fee_rate = 0.001", "fee_rate = 0.001\nemulator_release_stops = false");
        let p = BacktestProfile::from_toml_str(&toml).unwrap();
        assert!(!p.engine.emulator_release_stops);
    }

    #[test]
    fn parses_tick_profile() {
        let p = BacktestProfile::from_toml_str(TICK_TOML).unwrap();
        assert_eq!(p.data.kind, DataKind::Tick);
        assert_eq!(p.data.symbols, vec!["0xTOK"]);
        assert!(p.engine.snap_to_properties);
        // interval defaults even though tick data doesn't consume it.
        assert_eq!(p.data.interval, "1d");

        let r = p.range().unwrap();
        // 2026-04-13T19:00 / 20:00 UTC (verified against `date -u -d`).
        assert_eq!(r.start, Some(1_776_106_800_000));
        assert_eq!(r.end, Some(1_776_110_400_000));
    }

    #[test]
    fn range_accepts_bare_epoch_ms() {
        let p = BacktestProfile::from_toml_str(BARE_MS_TOML).unwrap();
        let r = p.range().unwrap();
        assert_eq!(r.start, Some(1_700_000_000_000));
        assert_eq!(r.end, Some(1_700_003_600_000));
    }

    #[test]
    fn rejects_empty_symbols() {
        let toml = r#"
[data]
venue = "binance"
symbols = []
kind = "bar"
from = "2026-01-01T00"
to = "2026-01-02T00"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
        let err = BacktestProfile::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(_)), "got {err:?}");
    }

    #[test]
    fn rejects_from_after_to() {
        let toml = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "2026-01-02T00"
to = "2026-01-01T00"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
        let err = BacktestProfile::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(_)), "got {err:?}");
    }

    #[test]
    fn rejects_non_positive_cash() {
        let toml = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "2026-01-01T00"
to = "2026-01-02T00"
[engine]
cash = 0.0
[strategy]
name = "buy_hold"
"#;
        let err = BacktestProfile::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(_)), "got {err:?}");
    }

    #[test]
    fn rejects_unknown_data_kind() {
        let toml = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "quote"
from = "2026-01-01T00"
to = "2026-01-02T00"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
        let err = BacktestProfile::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, HarnessError::Parse(_)), "got {err:?}");
    }

    #[test]
    fn rejects_unknown_top_level_key() {
        let toml = r#"
bogus = "field"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "2026-01-01T00"
to = "2026-01-02T00"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
        let err = BacktestProfile::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, HarnessError::Parse(_)), "got {err:?}");
    }

    #[test]
    fn rejects_unparsable_timestamp() {
        let toml = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "not-a-timestamp"
to = "2026-01-02T00"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
        let err = BacktestProfile::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, HarnessError::Parse(_)), "got {err:?}");
    }

    // --- the opt-in market-impact knob (`[engine.impact]`) ---

    /// Absent `[engine.impact]` must stay absent — the whole byte-identical-default claim starts
    /// here, at the config layer.
    #[test]
    fn impact_is_absent_unless_configured() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
        assert!(p.engine.impact.is_none());
    }

    #[test]
    fn impact_parses_with_published_defaults() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.impact]\nmodel = \"almgren_chriss\"",
        );
        let p = BacktestProfile::from_toml_str(&toml).unwrap();
        let imp = p.engine.impact.expect("impact configured");
        assert_eq!(imp.model, "almgren_chriss");
        assert_eq!(imp.exec_time, 1.0);
        assert_eq!(imp.window, crate::DEFAULT_IMPACT_WINDOW);
        assert!(imp.build().is_ok());
    }

    #[test]
    fn impact_honors_explicit_exec_time_and_window() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.impact]\nmodel = \"almgren_chriss\"\nexec_time = 0.5\nwindow = 60",
        );
        let p = BacktestProfile::from_toml_str(&toml).unwrap();
        let imp = p.engine.impact.unwrap();
        assert_eq!(imp.exec_time, 0.5);
        assert_eq!(imp.window, 60);
    }

    /// A typo'd model name must FAIL the profile, not silently price fills at zero impact.
    #[test]
    fn an_unknown_impact_model_is_rejected() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.impact]\nmodel = \"almgren-chris\"",
        );
        let err = BacktestProfile::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(_)), "got {err:?}");
    }

    #[test]
    fn nonsensical_impact_numbers_are_rejected() {
        for bad in ["exec_time = 0.0", "exec_time = -1.0", "window = 2"] {
            let toml = BAR_TOML.replace(
                "fee_rate = 0.001",
                &format!("fee_rate = 0.001\n\n[engine.impact]\nmodel = \"almgren_chriss\"\n{bad}"),
            );
            let err = BacktestProfile::from_toml_str(&toml).unwrap_err();
            assert!(matches!(err, HarnessError::Validation(_)), "{bad}: got {err:?}");
        }
    }

    /// The tick lane ACCEPTS `[engine.impact]` — and the acceptance is a load-bearing claim, not
    /// a relaxation for its own sake. It was rejected on the premise that "the tick lane replays
    /// a real book", which is false for an L1 tape (any size fills at the quote) and incomplete
    /// for an L2 one (a recorded book never moves in response to the order). A profile that names
    /// the knob in tick mode must therefore VALIDATE and must carry the model through, exactly as
    /// the bar profile does.
    #[test]
    fn impact_on_the_tick_lane_is_accepted_and_carries_its_model() {
        let toml = TICK_TOML.replace(
            "snap_to_properties = true",
            "snap_to_properties = true\n\n[engine.impact]\nmodel = \"almgren_chriss\"\nwindow = 500",
        );
        assert!(toml.contains("[engine.impact]"), "the TICK_TOML anchor must still match");
        let p = BacktestProfile::from_toml_str(&toml).expect("tick + impact must validate");
        assert_eq!(p.data.kind, DataKind::Tick, "the fixture must still be a TICK profile");
        let imp = p.engine.impact.expect("impact configured");
        assert_eq!(imp.window, 500, "the tick lane's window must reach the profile");
        assert!(imp.build().is_ok(), "the model must still be buildable in tick mode");
    }

    /// The coefficient knobs default to the PUBLISHED constants and are settable — the lever the
    /// module docs point an operator at when a per-print context spends a daily-fitted
    /// coefficient. Asserted end to end (parse -> `build` -> a MOVED number) rather than by
    /// reading the field back, because a field that parses and is dropped on the floor between
    /// here and `AlmgrenChriss` would pass a field-equality test.
    #[test]
    fn the_impact_coefficients_default_to_the_published_pair_and_are_settable() {
        // `ImpactModel` is deliberately NOT imported: `build()` hands back an `Arc<dyn ImpactModel>`,
        // and a trait OBJECT resolves its own trait's methods without the trait being in scope.
        use crate::impact::{AC_ETA, AC_GAMMA, ImpactInputs, ImpactTerms};
        let ctx = ImpactInputs { qty: 3_000.0, avg_volume: 80_000.0, sigma: 0.02 };
        let cfg = |extra: &str| {
            let toml = TICK_TOML.replace(
                "snap_to_properties = true",
                &format!("snap_to_properties = true\n\n[engine.impact]\nmodel = \"almgren_chriss\"{extra}"),
            );
            BacktestProfile::from_toml_str(&toml).expect("must validate").engine.impact.unwrap()
        };

        let published = cfg("");
        assert_eq!(
            published.gamma.to_bits(),
            AC_GAMMA.to_bits(),
            "the gamma default must be the paper's"
        );
        assert_eq!(
            published.eta.to_bits(),
            AC_ETA.to_bits(),
            "the eta default must be the paper's"
        );

        // The L2 lane charges PermanentOnly, so this is the selection an L2 operator's knob has
        // to reach — halving gamma must halve the charge, not leave it where it was.
        let base = published.build().unwrap().impact_frac_for(&ctx, ImpactTerms::PermanentOnly);
        assert!(base > 0.0, "the fixture must produce a real charge");
        let halved = cfg("\ngamma = 0.157")
            .build()
            .unwrap()
            .impact_frac_for(&ctx, ImpactTerms::PermanentOnly);
        assert!(
            (halved - base / 2.0).abs() < 1e-15,
            "gamma did not reach the model: {halved} vs {}",
            base / 2.0
        );
    }

    /// A coefficient that would break the trait's contract is refused at PARSE time, by name.
    /// Zero is refused with the rest: it disarms a half of the model silently, and on the L2 lane
    /// `gamma = 0` disarms the ONLY term that lane charges — an operator would see an armed
    /// profile priced exactly like an unarmed one.
    #[test]
    fn a_nonsensical_impact_coefficient_is_refused_by_name() {
        for (key, value) in [("gamma", "0.0"), ("gamma", "-0.5"), ("eta", "0.0"), ("eta", "-1e9")] {
            let toml = TICK_TOML.replace(
                "snap_to_properties = true",
                &format!(
                    "snap_to_properties = true\n\n[engine.impact]\nmodel = \"almgren_chriss\"\n{key} = {value}"
                ),
            );
            match BacktestProfile::from_toml_str(&toml).unwrap_err() {
                HarnessError::Validation(m) => assert!(
                    m.contains(&format!("engine.impact.{key}")),
                    "{key}={value} was refused without naming the key: {m}"
                ),
                other => panic!("{key}={value} expected a validation error, got {other:?}"),
            }
        }
    }

    /// ...and a BAD model is still refused in tick mode. Removing the mode rejection must not
    /// have removed the `build()` call that runs beside it — a typo'd model name silently pricing
    /// fills at zero impact is the failure that check exists for, and it would now be reachable
    /// on a whole extra lane.
    #[test]
    fn a_typod_impact_model_is_still_rejected_on_the_tick_lane() {
        let toml = TICK_TOML.replace(
            "snap_to_properties = true",
            "snap_to_properties = true\n\n[engine.impact]\nmodel = \"almgren-chriss\"",
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("unknown engine.impact.model"), "{m}")
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// A bar series carries no `local_ts`, so feed-latency delivery has nothing to order by — the
    /// combination is rejected rather than silently ignored (the mirror of the bar-only `attach_funding`
    /// rule above).
    #[test]
    fn feed_latency_on_the_bar_lane_is_rejected() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nfeed_latency = true");
        assert!(toml.contains("feed_latency = true"), "the BAR_TOML anchor must still match");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("tick-mode only"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// Absent = OFF (today's venue-ordered replay); accepted on the tick lane.
    #[test]
    fn feed_latency_defaults_off_and_is_accepted_on_the_tick_lane() {
        let off = BacktestProfile::from_toml_str(TICK_TOML).unwrap();
        assert!(!off.engine.feed_latency, "absent must mean the frozen venue-ordered replay");

        let toml = TICK_TOML
            .replace("snap_to_properties = true", "snap_to_properties = true\nfeed_latency = true");
        let on = BacktestProfile::from_toml_str(&toml).unwrap();
        assert!(on.engine.feed_latency);
    }

    // --- the opt-in market-funding join (`engine.attach_funding`) -----------------------------

    /// Absent `[engine].attach_funding` must default to OFF — the byte-identical-default claim.
    #[test]
    fn attach_funding_defaults_off_and_parses_true() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
        assert!(!p.engine.attach_funding, "absent must mean OFF (funding series never read)");

        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nattach_funding = true");
        let p = BacktestProfile::from_toml_str(&toml).unwrap();
        assert!(p.engine.attach_funding);
    }

    /// The tick lane has no per-interval funding fold, so the join has no consumer there — the
    /// combination is rejected rather than silently ignored (the mirror of the tick-only `feed_latency` rule).
    #[test]
    fn attach_funding_on_the_tick_lane_is_rejected() {
        let toml = TICK_TOML.replace(
            "snap_to_properties = true",
            "snap_to_properties = true\nattach_funding = true",
        );
        assert!(toml.contains("attach_funding = true"), "the TICK_TOML anchor must still match");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("bar-mode only"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    // --- G4: the cross-venue `[[data.series]]` slice ------------------------------------------

    const CROSS_VENUE_TOML: &str = r#"
name = "cheap-np-window"

[data]
kind = "tick"
from = "1774999800000"
to = "1775002500000"

[[data.series]]
venue = "spot"
symbol = "BTCUSDT"
kind = "quote"

[[data.series]]
venue = "polymarket"
symbol = "btc-updown-5m-1775001600#0"
kind = "trade"

[[data.series]]
venue = "polymarket"
symbol = "btc-updown-5m-1775001600#1"
kind = "trade"

[engine]
cash = 1000.0

[engine.fee]
kind = "probability_scaled"
taker_rate = 0.072

[engine.resolution]
kind = "binary_outcome"
[engine.resolution.winners]
"btc-updown-5m-1775001600" = 0

[strategy]
name = "cheap_catch_updown_fair_value"
[strategy.params]
spot_symbol = "BTCUSDT"
"#;

    /// The OLD single-venue form still loads, unchanged — the whole back-compat claim.
    #[test]
    fn the_single_venue_form_still_loads_and_expands_to_series() {
        for toml in [BAR_TOML, TICK_TOML, BARE_MS_TOML] {
            let p = BacktestProfile::from_toml_str(toml).unwrap();
            assert!(p.data.series.is_empty(), "no [[data.series]] table was written");
            let series = p.data.resolved_series();
            assert_eq!(series.len(), p.data.symbols.len());
            for (s, sym) in series.iter().zip(&p.data.symbols) {
                assert_eq!(&s.venue, p.data.venue.as_ref().unwrap());
                assert_eq!(&s.symbol, sym);
                // The expansion is the WHOLE-LANE kind — quotes + trades + books, exactly what
                // `replay_ticks` loaded before the lane filter existed.
                assert_eq!(s.kind, SeriesKind::Tick);
            }
            assert_eq!(&p.data.default_venue(), p.data.venue.as_ref().unwrap());
            assert!(!p.data.is_cross_venue());
        }
    }

    #[test]
    fn cross_venue_series_parse_in_order_with_lane_filters() {
        let p = BacktestProfile::from_toml_str(CROSS_VENUE_TOML).unwrap();
        assert!(p.data.venue.is_none());
        assert!(p.data.symbols.is_empty());
        let series = p.data.resolved_series();
        assert_eq!(series.len(), 3);
        // ORDER is meaningful: the reference series is first, so an equal-ts spot sample reaches
        // the strategy before the print it should inform (`run_ticks` ties break by stream order).
        assert_eq!((series[0].venue.as_str(), series[0].kind), ("spot", SeriesKind::Quote));
        assert_eq!((series[1].venue.as_str(), series[1].kind), ("polymarket", SeriesKind::Trade));
        assert_eq!(series[2].symbol, "btc-updown-5m-1775001600#1");
        assert!(p.data.is_cross_venue());
        assert_eq!(p.data.default_venue(), "spot");
    }

    #[test]
    fn mixing_the_two_slice_forms_is_rejected() {
        let toml = CROSS_VENUE_TOML.replace("[data]\nkind", "[data]\nvenue = \"spot\"\nkind");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("exactly one"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn a_duplicate_symbol_across_series_is_rejected() {
        let toml = CROSS_VENUE_TOML.replace("btc-updown-5m-1775001600#1", "BTCUSDT");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("duplicate symbol"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// A bar series has no quote/trade/book lanes, so a lane filter there is a typo, not a knob.
    #[test]
    fn a_lane_filter_on_the_bar_lane_is_rejected() {
        let toml = r#"
[data]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"
[[data.series]]
venue = "binance"
symbol = "BTCUSDT"
kind = "trade"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
        match BacktestProfile::from_toml_str(toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("tick-mode only"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// One `default_venue` + many venues = the wrong grid for every series but the first.
    #[test]
    fn snapping_a_cross_venue_slice_is_rejected() {
        let toml =
            CROSS_VENUE_TOML.replace("cash = 1000.0", "cash = 1000.0\nsnap_to_properties = true");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("single-venue only"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    // --- G7: the fee schedule -----------------------------------------------------------------

    #[test]
    fn fee_is_absent_unless_configured() {
        assert!(BacktestProfile::from_toml_str(BAR_TOML).unwrap().engine.fee.is_none());
    }

    #[test]
    fn probability_scaled_fee_builds_the_curve() {
        let p = BacktestProfile::from_toml_str(CROSS_VENUE_TOML).unwrap();
        let schedule = p.engine.fee.as_ref().unwrap().build().unwrap();
        assert_eq!(
            schedule,
            FeeSchedule::ProbabilityScaled {
                taker_rate: 0.072,
                maker_rate: 0.0,
                maker_rebate_share: 0.0,
            }
        );
        // The number the live bot pays: 0.072·p·(1−p) per share, at the fill's own price.
        assert_eq!(schedule.commission(false, 1.0, 0.25), 0.072 * 0.25 * 0.75);
    }

    #[test]
    fn an_unknown_fee_kind_is_rejected() {
        let toml = CROSS_VENUE_TOML.replace("\"probability_scaled\"", "\"prob_scaled\"");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("unknown engine.fee.kind"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn a_fee_schedule_plus_a_flat_fee_rate_is_rejected() {
        let toml = CROSS_VENUE_TOML.replace("cash = 1000.0", "cash = 1000.0\nfee_rate = 0.001");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("two different cost models"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    // --- Task 12: [engine.sizer] ---------------------------------------------------------------

    #[test]
    fn no_sizer_is_the_default() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
        assert!(p.engine.sizer.is_none());
    }

    #[test]
    fn a_sizer_reaches_the_profile_and_builds() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"pass_through\"",
        );
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        let cfg = p.engine.sizer.as_ref().expect("sizer present");
        assert!(cfg.build().is_ok());
    }

    #[test]
    fn an_unknown_sizer_kind_is_rejected() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"telepathy\"",
        );
        match BacktestProfile::from_toml_str(&toml) {
            Err(HarnessError::Validation(m)) => assert!(m.contains("telepathy"), "{m}"),
            Ok(p) => match p.engine.sizer.as_ref().expect("present").build() {
                Err(HarnessError::Validation(m)) => assert!(m.contains("telepathy"), "{m}"),
                Err(other) => panic!("expected a validation error, got {other:?}"),
                // `Box<dyn PositionSizer>` doesn't implement `Debug`, so this arm can't be
                // printed — the assertion itself is the diagnostic.
                Ok(_) => panic!("expected build() to reject an unknown kind"),
            },
            Err(other) => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// Every scalar (non-wrapping) sizer kind builds given its own required knob.
    #[test]
    fn every_scalar_sizer_kind_builds() {
        for (kind, extra) in [
            ("fixed_dollar", "amount = 1000.0"),
            ("fixed_shares", "shares = 10.0"),
            ("pct_equity", "pct = 0.1"),
            ("pct_volatility", "pct = 0.1"),
            ("max_risk_pct", "pct = 0.02"),
        ] {
            let toml = BAR_TOML.replace(
                "fee_rate = 0.001",
                &format!("fee_rate = 0.001\n\n[engine.sizer]\nkind = \"{kind}\"\n{extra}"),
            );
            let p = BacktestProfile::from_toml_str(&toml)
                .unwrap_or_else(|e| panic!("{kind} should parse: {e:?}"));
            p.engine
                .sizer
                .as_ref()
                .unwrap()
                .build()
                .unwrap_or_else(|e| panic!("{kind} should build: {e:?}"));
        }
    }

    #[test]
    fn a_scalar_sizer_kind_missing_its_knob_is_rejected() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"fixed_dollar\"",
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("engine.sizer.amount"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn a_sizer_portfolio_heat_missing_its_base_is_rejected() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"portfolio_heat\"\nmax_heat = 0.1",
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("[engine.sizer.base]"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn a_sizer_drawdown_throttle_missing_its_base_is_rejected() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"drawdown_throttle\"\n\
             sensitivity = 0.5\nfloor = 0.2",
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("[engine.sizer.base]"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// Only the two WRAPPING kinds read `SizerCfg::base`. Under a scalar kind the table is read by
    /// nobody, so the run installs ONE sizer while the operator reads the profile as a chain —
    /// the same silent-no-op class every other rule in this validator refuses.
    #[test]
    fn a_base_under_a_non_wrapping_sizer_kind_is_rejected() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"fixed_dollar\"\namount = 1000.0\n\n\
             [engine.sizer.base]\nkind = \"pass_through\"",
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("fixed_dollar"), "{m}");
                assert!(m.contains("[engine.sizer.base]"), "{m}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// The same refusal one level down, so the rule is not merely a top-level check: a base under
    /// a scalar kind nested inside a legitimate wrapper is refused, and the message names the
    /// nested path rather than the outer one.
    #[test]
    fn a_base_under_a_nested_non_wrapping_sizer_kind_is_rejected() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"portfolio_heat\"\nmax_heat = 0.1\n\n\
             [engine.sizer.base]\nkind = \"fixed_dollar\"\namount = 1000.0\n\n\
             [engine.sizer.base.base]\nkind = \"pass_through\"",
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("[engine.sizer.base.base]"), "{m}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn a_sizer_portfolio_heat_builds_with_a_nested_base() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"portfolio_heat\"\nmax_heat = 0.1\n\
             [engine.sizer.base]\nkind = \"fixed_dollar\"\namount = 1000.0",
        );
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        assert!(p.engine.sizer.as_ref().unwrap().build().is_ok());
    }

    #[test]
    fn a_sizer_drawdown_throttle_builds_with_a_nested_base() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"drawdown_throttle\"\n\
             sensitivity = 0.5\nfloor = 0.2\n\
             [engine.sizer.base]\nkind = \"fixed_dollar\"\namount = 1000.0",
        );
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        assert!(p.engine.sizer.as_ref().unwrap().build().is_ok());
    }

    /// A chain of exactly `depth` `SizerCfg` nodes: `depth - 1` `"portfolio_heat"` wrappers ending
    /// in one terminal `"pass_through"` leaf — so the whole chain always BUILDS (not just parses)
    /// whenever `depth <= MAX_SIZER_DEPTH`. `depth` counts the same way `SizerCfg::build_at_depth`
    /// does: the top-level `[engine.sizer]` table is depth 1.
    fn nested_sizer_toml(depth: usize) -> String {
        assert!(depth >= 1);
        let mut path = "engine.sizer".to_string();
        let mut out = String::new();
        for level in 1..=depth {
            out.push_str(&format!("[{path}]\n"));
            if level < depth {
                out.push_str("kind = \"portfolio_heat\"\nmax_heat = 0.5\n");
                path.push_str(".base");
            } else {
                out.push_str("kind = \"pass_through\"\n");
            }
        }
        out
    }

    #[test]
    fn a_sizer_chain_at_the_depth_limit_builds() {
        let toml = format!("{BAR_TOML}\n{}", nested_sizer_toml(MAX_SIZER_DEPTH));
        let p = BacktestProfile::from_toml_str(&toml).expect("parses and validates");
        assert!(p.engine.sizer.as_ref().unwrap().build().is_ok());
    }

    #[test]
    fn a_sizer_chain_past_the_depth_limit_is_rejected() {
        let toml = format!("{BAR_TOML}\n{}", nested_sizer_toml(MAX_SIZER_DEPTH + 1));
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("MAX_SIZER_DEPTH"), "{m}");
                assert!(m.contains(&(MAX_SIZER_DEPTH + 1).to_string()), "{m}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// Manual measurement, not a CI regression test: does deserializing a `[engine.sizer]` chain
    /// far past `MAX_SIZER_DEPTH` blow the stack DURING PARSING — before `SizerCfg::build`'s own
    /// bound ever gets a chance to run — or does `toml`'s recursive descent handle a deep chain
    /// fine, in which case `build` (called from `validate`) catches it cleanly?
    ///
    /// Escalates depth and times each `from_toml_str` call, bailing out (and always failing, via
    /// `panic!`, so nextest prints the captured measurements regardless of outcome) once a level
    /// is already slow — no point re-proving a hang at a bigger number once one is observed.
    /// `#[ignore]`d because the answer at the top of this escalation is a MULTI-MINUTE HANG (see
    /// the Task 12 fix-round report — a first flat run at depth 50,000 hit nextest's own slow-test
    /// timeout at ~244s with no result either way), which would make the default suite
    /// unusable if this ran by default. Run explicitly:
    /// `cargo nextest run -p vike-backtest --features hist-replay --run-ignored ignored-only sizer_depth_probe`.
    #[test]
    #[ignore]
    fn sizer_depth_probe() {
        use std::fmt::Write as _;
        use std::time::Instant;
        let mut report = String::new();
        for depth in
            [10usize, 20, 30, 50, 80, 100, 500, 1_000, 2_000, 5_000, 10_000, 20_000, 50_000]
        {
            let toml = format!("{BAR_TOML}\n{}", nested_sizer_toml(depth));
            let start = Instant::now();
            let result = BacktestProfile::from_toml_str(&toml);
            let elapsed = start.elapsed();
            let outcome = match &result {
                Ok(_) => "Ok (build's own MAX_SIZER_DEPTH check should have refused this — bug if \
                          so)"
                .to_string(),
                Err(e) => format!("Err({e:?})"),
            };
            let _ = writeln!(report, "depth={depth:>6} elapsed={elapsed:>10.2?} outcome={outcome}");
            // Growth here is not linear (see the report) — once one level is already slow, a
            // bigger one will not finish inside this test's own runtime budget.
            if elapsed.as_secs() >= 3 {
                let _ = writeln!(report, "(stopping escalation: depth={depth} was already slow)");
                break;
            }
        }
        panic!("sizer_depth_probe measurements (this failure is expected — see doc):\n{report}");
    }

    // --- G6: the settlement source ------------------------------------------------------------

    #[test]
    fn resolution_is_absent_unless_configured() {
        assert!(BacktestProfile::from_toml_str(BAR_TOML).unwrap().engine.resolution.is_none());
    }

    /// The source pays 1.0 to the winning outcome and 0.0 to the loser, and ONLY from the
    /// window close onward — a spot symbol or another window's token is never touched.
    #[test]
    fn binary_outcome_source_pays_the_winner_from_the_close() {
        let p = BacktestProfile::from_toml_str(CROSS_VENUE_TOML).unwrap();
        let symbols: Vec<String> = p.data.resolved_series().into_iter().map(|s| s.symbol).collect();
        let (src, end_ts) = p.engine.resolution.as_ref().unwrap().build(None, &symbols).unwrap();

        let res_ms = (1_775_001_600 + 300) * 1000;
        assert_eq!(end_ts, Some(res_ms), "the sweep probes at THIS window's close, not a sentinel");

        // still trading
        assert_eq!(src("btc-updown-5m-1775001600#0", res_ms - 1), None);
        // resolved: outcome 0 won
        assert_eq!(src("btc-updown-5m-1775001600#0", res_ms), Some(1.0));
        assert_eq!(src("btc-updown-5m-1775001600#1", res_ms), Some(0.0));
        // the reference series is never settled or latched (port backlog G5)
        assert_eq!(src("BTCUSDT", res_ms), None);
        // a window with no winner row is never invented
        assert_eq!(src("btc-updown-5m-1775001900#0", i64::MAX / 4), None);
    }

    /// A non-binary INLINE `winning_index` is an authoring mistake, not a data fact — reject it
    /// rather than coerce it to "both sides lose".
    #[test]
    fn a_non_binary_inline_winning_index_is_rejected() {
        for bad in ["2", "-1", "4"] {
            let toml = CROSS_VENUE_TOML.replace(
                "\"btc-updown-5m-1775001600\" = 0",
                &format!("\"btc-updown-5m-1775001600\" = {bad}"),
            );
            match BacktestProfile::from_toml_str(&toml).unwrap_err() {
                HarnessError::Validation(m) => {
                    assert!(m.contains("not a binary outcome index"), "{bad}: {m}")
                }
                other => panic!("{bad}: expected a validation error, got {other:?}"),
            }
        }
    }

    /// A window token in the slice with no payout would end the run marked at its last traded
    /// price. That must be loud.
    #[test]
    fn a_window_with_no_resolution_row_is_rejected_at_build() {
        let p = BacktestProfile::from_toml_str(CROSS_VENUE_TOML).unwrap();
        let symbols = vec![
            "BTCUSDT".to_string(),
            "btc-updown-5m-1775001600#0".to_string(),
            "btc-updown-5m-1775001900#0".to_string(), // no winner row
        ];
        // `ResolutionSource` is a boxed closure and therefore not `Debug`, so match on the
        // Result rather than `unwrap_err`.
        match p.engine.resolution.as_ref().unwrap().build(None, &symbols) {
            Err(HarnessError::Validation(m)) => {
                assert!(m.contains("no binary winning_index"), "{m}");
                assert!(m.contains("btc-updown-5m-1775001900"), "{m}");
            }
            Err(other) => panic!("expected a validation error, got {other:?}"),
            Ok(_) => panic!("expected a validation error, got Ok"),
        }
    }

    /// The `slug,winning_index` CSV the ClickHouse export produces: header row, quoted slugs,
    /// and non-binary rows that are DROPPED (a real on-chain fact) rather than coerced.
    #[test]
    fn resolution_reads_the_clickhouse_csv_and_drops_non_binary_rows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("res.csv"),
            "\"slug\",\"winning_index\"\n\
             \"btc-updown-5m-1775001600\",1\n\
             \"btc-updown-5m-1775001900\",4\n",
        )
        .unwrap();

        let toml = CROSS_VENUE_TOML.replace(
            "[engine.resolution.winners]\n\"btc-updown-5m-1775001600\" = 0",
            "path = \"res.csv\"",
        );
        let p = BacktestProfile::from_toml_str(&toml).unwrap();
        let symbols = vec!["btc-updown-5m-1775001600#0".to_string()];
        let (src, _) =
            p.engine.resolution.as_ref().unwrap().build(Some(dir.path()), &symbols).unwrap();
        // The slug is UNQUOTED before use — a quoted key matches no token symbol at all.
        assert_eq!(src("btc-updown-5m-1775001600#0", i64::MAX / 4), Some(0.0));
        assert_eq!(src("btc-updown-5m-1775001600#1", i64::MAX / 4), Some(1.0));
        // The `winning_index = 4` row was dropped, so that window has no payout...
        assert_eq!(src("btc-updown-5m-1775001900#0", i64::MAX / 4), None);
        // ...and asking to RUN it is an error, not a silent unsettled position.
        assert!(
            p.engine
                .resolution
                .as_ref()
                .unwrap()
                .build(Some(dir.path()), &["btc-updown-5m-1775001900#0".to_string()])
                .is_err()
        );
    }

    /// A relative sidecar path resolves next to the PROFILE, not against the CWD.
    #[test]
    fn a_relative_resolution_path_resolves_against_the_profile_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("res.csv"), "btc-updown-5m-1775001600,0\n").unwrap();
        let toml = CROSS_VENUE_TOML.replace(
            "[engine.resolution.winners]\n\"btc-updown-5m-1775001600\" = 0",
            "path = \"res.csv\"",
        );
        let profile_path = dir.path().join("run.toml");
        std::fs::write(&profile_path, &toml).unwrap();

        let p = BacktestProfile::from_path(&profile_path).unwrap();
        assert_eq!(p.base_dir.as_deref(), Some(dir.path()));
        let symbols = vec!["btc-updown-5m-1775001600#0".to_string()];
        assert!(
            p.engine.resolution.as_ref().unwrap().build(p.base_dir.as_deref(), &symbols).is_ok()
        );
    }

    #[test]
    fn an_unknown_resolution_kind_is_rejected() {
        let toml = CROSS_VENUE_TOML.replace("\"binary_outcome\"", "\"binary\"");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("unknown engine.resolution.kind"), "{m}")
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// An explicit `end_ts` overrides the derived "latest window close in the slice".
    #[test]
    fn an_explicit_resolution_end_ts_overrides_the_derived_one() {
        let toml = CROSS_VENUE_TOML
            .replace("kind = \"binary_outcome\"", "kind = \"binary_outcome\"\nend_ts = \"12345\"");
        let p = BacktestProfile::from_toml_str(&toml).unwrap();
        let symbols = vec!["btc-updown-5m-1775001600#0".to_string()];
        let (_, end) = p.engine.resolution.as_ref().unwrap().build(None, &symbols).unwrap();
        assert_eq!(end, Some(12_345));
    }

    // --- the opt-in `[risk]` section (runprofile-wiring-step2) --------------------------------

    /// Absent `[risk]` must parse to `None` — the byte-identical-default claim: nothing in
    /// `BAR_TOML`/`TICK_TOML` sets it, so every profile written before this field existed keeps
    /// parsing exactly as before.
    #[test]
    fn risk_is_absent_unless_configured() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
        assert!(p.risk.is_none());
        let p = BacktestProfile::from_toml_str(TICK_TOML).unwrap();
        assert!(p.risk.is_none());
    }

    /// A `[risk]` section parses into the SAME `vike_exec::ProfileRisk` fields paper/live use, and
    /// `ProfileRisk::to_risk_limits` maps them onto the real `RiskLimits` the engine reads.
    #[test]
    fn risk_section_parses_into_profile_risk() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[risk]\nmax_notional_per_order = 250000.0\nmax_leverage = 2.0\n\
             min_qty = 0.001",
        );
        let p = BacktestProfile::from_toml_str(&toml).unwrap();
        let risk = p.risk.as_ref().expect("`[risk]` configured");
        assert_eq!(risk.max_notional_per_order, Some(250_000.0));
        assert_eq!(risk.max_leverage, Some(2.0));
        assert_eq!(risk.min_qty, Some(0.001));

        let limits = risk.to_risk_limits();
        assert_eq!(limits.max_notional_per_order, Some(250_000.0));
        assert_eq!(limits.max_leverage, Some(2.0));
        // A backtest `[risk] max_leverage` arms the SAME buying-power check paper/live arm
        // (issue #822): 2x ⇒ 50% initial margin. `SimBroker::build_risk_gate` already maps its own
        // `EngineParams::leverage` this way, so the two edges now agree on what "2x" means.
        assert_eq!(limits.im_requirement, Some(0.5));
        assert_eq!(limits.min_qty, Some(0.001));
    }

    /// A typo'd `[risk]` key must fail the profile — `ProfileRisk` carries its own
    /// `deny_unknown_fields`, so the nested-denial policy applies inside `[risk]` too, exactly as
    /// it does for `vike-core`'s `RunProfile`.
    #[test]
    fn unknown_risk_key_is_rejected() {
        let toml =
            BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[risk]\nmax_levarage = 10.0");
        let err = BacktestProfile::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, HarnessError::Parse(_)), "got {err:?}");
    }

    /// `risk.max_orders_per_window` is a wall-clock throttle the sim gate always disarms
    /// (`SimBroker::build_risk_gate`) — REJECTED at load rather than silently ignored, the
    /// documented divergence this wiring step must surface loudly instead of papering over.
    #[test]
    fn risk_max_orders_per_window_is_rejected_at_load() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[risk]\nmax_orders_per_window = 5\nwindow_ms = 1000",
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("max_orders_per_window"), "{m}");
                assert!(m.contains("wall-clock"), "{m}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// The rejection fires from the TICK lane too — the throttle is meaningless in sim time
    /// regardless of bar/tick mode.
    #[test]
    fn risk_max_orders_per_window_is_rejected_on_the_tick_lane_too() {
        let toml = TICK_TOML.replace(
            "snap_to_properties = true",
            "snap_to_properties = true\n\n[risk]\nmax_orders_per_window = 1\nwindow_ms = 1000",
        );
        let err = BacktestProfile::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(_)), "got {err:?}");
    }

    /// Every OTHER `risk.*` limit is unaffected by the `max_orders_per_window` gate — a profile
    /// setting only operator-budget fields (no throttle) parses and validates cleanly.
    #[test]
    fn risk_without_max_orders_per_window_is_accepted() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[risk]\nmax_notional_per_order = 1000.0\nmax_total_exposure = 5000.0",
        );
        let p = BacktestProfile::from_toml_str(&toml).expect("no throttle set -> accepted");
        assert_eq!(p.risk.unwrap().max_notional_per_order, Some(1000.0));
    }

    #[test]
    fn cash_gate_reaches_the_profile() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ncash_gate = true");
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        assert!(p.engine.cash_gate);
    }

    /// `cash_gate`'s ONLY readers are `StrategyEngine::run`'s `granular` mask and its
    /// `fill_step_gated` branch — both inside the BAR loop — and `fill_step_gated`'s single caller
    /// is that branch. `run_ticks` never consults it, so a tick profile setting it would parse,
    /// validate, reach `EngineParams` and change nothing: the defect this whole surface exists to
    /// refuse, which is why it now joins the lane-asymmetric refusals.
    #[test]
    fn cash_gate_is_bar_mode_only() {
        let toml = TICK_TOML.replace("snap_to_properties = true", "cash_gate = true");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("cash_gate"), "{m}");
                assert!(m.contains("bar-mode only"), "{m}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// The companion: an explicit `false` on a tick profile configures nothing and must keep
    /// validating — the refusal is on the value that would mislead, not on the key's presence.
    #[test]
    fn cash_gate_false_on_a_tick_profile_is_accepted() {
        let toml = TICK_TOML.replace("snap_to_properties = true", "cash_gate = false");
        let p =
            BacktestProfile::from_toml_str(&toml).expect("an explicit false configures nothing");
        assert!(!p.engine.cash_gate);
    }

    #[test]
    fn cash_gate_defaults_off() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
        assert!(!p.engine.cash_gate, "the default must stay byte-identical to before this key");
    }

    #[test]
    fn margin_keys_reach_the_profile() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\nmaint_margin = 0.005\nliq_buffer = 0.2\n\
             venue_style_liquidation = true",
        );
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        assert_eq!(p.engine.maint_margin, 0.005);
        assert_eq!(p.engine.liq_buffer, 0.2);
        assert!(p.engine.venue_style_liquidation);
    }

    /// `liq_buffer`'s engine default is 0.10, NOT 0.0 — a serde `Default::default()` here would
    /// silently change every profile that omits the key.
    #[test]
    fn liq_buffer_defaults_to_the_engine_value() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
        assert_eq!(p.engine.liq_buffer, 0.10);
        assert_eq!(p.engine.maint_margin, 0.0);
        assert!(!p.engine.venue_style_liquidation);
    }

    #[test]
    fn a_negative_maint_margin_is_rejected() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nmaint_margin = -0.01");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("maint_margin"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn volume_limit_reaches_the_profile() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nvolume_limit = 0.05");
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        assert_eq!(p.engine.volume_limit, Some(0.05));
    }

    #[test]
    fn volume_limit_defaults_to_none() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
        assert_eq!(p.engine.volume_limit, None);
    }

    /// A participation cap above 1.0 claims more than the whole bar traded.
    #[test]
    fn a_volume_limit_over_one_is_rejected() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nvolume_limit = 1.5");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("volume_limit"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn position_caps_reach_the_profile() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\nmax_open_positions = 10\nmax_open_long = 6\nmax_open_short = 4",
        );
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        assert_eq!(p.engine.max_open_positions, 10);
        assert_eq!(p.engine.max_open_long, 6);
        assert_eq!(p.engine.max_open_short, 4);
    }

    /// 0 is the engine's "unlimited", which is what every profile did before these keys.
    #[test]
    fn position_caps_default_to_unlimited() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
        assert_eq!(p.engine.max_open_positions, 0);
        assert_eq!(p.engine.max_open_long, 0);
        assert_eq!(p.engine.max_open_short, 0);
    }

    #[test]
    fn multipliers_reach_the_profile() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\nmultiplier = 1.0\n\n[engine.multipliers]\n\
             BTCUSDT = 50.0\nETHUSDT = 20.0",
        );
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        assert_eq!(p.engine.multiplier, 1.0);
        assert_eq!(p.engine.multipliers.get("BTCUSDT"), Some(&50.0));
        assert_eq!(p.engine.multipliers.get("ETHUSDT"), Some(&20.0));
    }

    #[test]
    fn multiplier_defaults_to_one_and_the_table_is_empty() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
        assert_eq!(p.engine.multiplier, 1.0);
        assert!(p.engine.multipliers.is_empty());
    }

    #[test]
    fn a_zero_multiplier_is_rejected() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nmultiplier = 0.0");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("multiplier"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// The per-symbol table gets its own negative-value test, distinct from the global
    /// `multiplier` scalar above — `validate` is first-failure-wins, so this sets exactly one bad
    /// per-symbol value and leaves the global scalar at its valid default. The key is a symbol
    /// the slice ACTUALLY loads, so the value rule is what fails rather than the key rule below.
    #[test]
    fn a_zero_per_symbol_multiplier_is_rejected() {
        let toml = BAR_TOML
            .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.multipliers]\nBTCUSDT = 0.0");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("multipliers"), "{m}");
                assert!(m.contains("BTCUSDT"), "{m}");
                assert!(m.contains("> 0"), "the VALUE rule must be the one that fires: {m}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// A `multipliers` key naming a symbol the run does not load is silently inert today:
    /// `StrategyEngine::new` matches by EXACT name and otherwise falls back to the global
    /// `multiplier`, so a typo'd row leaves every position sized at the global while the profile
    /// reads as contract-sized. The slice is named back so the typo is visible in the message.
    #[test]
    fn a_multiplier_for_a_symbol_outside_the_slice_is_rejected() {
        let toml = BAR_TOML
            .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.multipliers]\nES = 50.0");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("engine.multipliers.ES"), "{m}");
                assert!(m.contains("BTCUSDT"), "the slice must be named back: {m}");
                assert!(m.contains("ETHUSDT"), "the slice must be named back: {m}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// ...and the companion, so the rule above cannot be satisfied by refusing everything: a row
    /// naming a symbol the slice DOES load still validates.
    #[test]
    fn a_multiplier_for_a_symbol_in_the_slice_is_accepted() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.multipliers]\nETHUSDT = 50.0",
        );
        let p = BacktestProfile::from_toml_str(&toml).expect("the symbol is in the slice");
        assert_eq!(p.engine.multipliers.get("ETHUSDT"), Some(&50.0));
    }

    #[test]
    fn leverage_keys_reach_the_profile() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\nleverage = 5.0\nclamp_to_leverage = true",
        );
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        assert_eq!(p.engine.leverage, Some(5.0));
        assert!(p.engine.clamp_to_leverage);
    }

    #[test]
    fn leverage_defaults_to_none() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
        assert_eq!(p.engine.leverage, None);
        assert!(!p.engine.clamp_to_leverage);
    }

    #[test]
    fn a_non_positive_leverage_is_rejected() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nleverage = 0.0");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("leverage"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// `clamp_to_leverage` with no `leverage` set has nothing to clamp to — the brief's step 4
    /// adds this rule but its own step 1 test list omits a case for it, so this closes that gap.
    #[test]
    fn clamp_to_leverage_without_leverage_is_rejected() {
        let toml =
            BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nclamp_to_leverage = true");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("clamp_to_leverage"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// The end state the two older rules left UNSTATED: `clamp_to_leverage` is unavailable to any
    /// profile carrying `[risk]`. Before the reorder, this exact profile was told to add
    /// `engine.leverage` — and adding it was then refused by the next rule, a dead end with the
    /// governing constraint named nowhere.
    #[test]
    fn clamp_to_leverage_with_a_risk_table_is_rejected() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\nclamp_to_leverage = true\n\n[risk]\nmax_notional_per_order = 1000.0",
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("clamp_to_leverage"), "{m}");
                assert!(m.contains("[risk]"), "{m}");
                // The misdirection this replaces: never send the operator to add a key the next
                // rule refuses.
                assert!(
                    !m.contains("needs engine.leverage"),
                    "the old rule-18 misdirection must not fire first: {m}"
                );
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// ...and with `engine.leverage` supplied as well, the SAME refusal must answer — the clamp
    /// rule is what governs, whichever way the operator arrived at the combination.
    #[test]
    fn clamp_to_leverage_with_a_risk_table_and_leverage_is_rejected_the_same_way() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\nleverage = 5.0\nclamp_to_leverage = true\n\n[risk]\n\
             max_notional_per_order = 1000.0",
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("clamp_to_leverage"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// `SimBroker::build_risk_gate` matches `(&p.risk_limits, p.leverage)`; with the clamp OFF its
    /// `(Some(l), _)` arm takes `risk_limits` UNCONDITIONALLY whenever `[risk]` is present — even
    /// one that never sets `max_leverage` — and silently drops `engine.leverage`. Declaring both
    /// must be a named refusal, not a silent precedence pick. (The clamp-ON case has the opposite
    /// precedence and its own refusal, two tests above.)
    #[test]
    fn engine_leverage_with_a_risk_table_is_rejected() {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\nleverage = 5.0\n\n[risk]\nmax_notional_per_order = 1000.0",
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("engine.leverage"), "{m}");
                assert!(m.contains("[risk]") || m.contains("risk"), "{m}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// The companion to the rejection above: `engine.leverage` with NO `[risk]` table is exactly
    /// the shape `SimBroker::build_risk_gate`'s `(None, Some(lev))` arm consumes, and must keep
    /// working.
    #[test]
    fn engine_leverage_alone_with_no_risk_table_still_works() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nleverage = 5.0");
        let p = BacktestProfile::from_toml_str(&toml).expect("leverage alone, no [risk] table");
        assert_eq!(p.engine.leverage, Some(5.0));
        assert!(p.risk.is_none());
    }

    #[test]
    fn settlement_period_reaches_the_profile() {
        let toml = BAR_TOML
            .replace("fee_rate = 0.001", "fee_rate = 0.001\nsettlement_period_ms = 86400000");
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        assert_eq!(p.engine.settlement_period_ms, Some(86_400_000));
    }

    #[test]
    fn settlement_period_defaults_to_none() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
        assert_eq!(p.engine.settlement_period_ms, None);
    }

    #[test]
    fn a_non_positive_settlement_period_is_rejected() {
        let toml =
            BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nsettlement_period_ms = 0");
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(m.contains("settlement_period_ms"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn session_gate_reaches_the_profile() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nsession_gate = true");
        let p = BacktestProfile::from_toml_str(&toml).expect("parses");
        assert!(p.engine.session_gate);
    }

    #[test]
    fn session_gate_defaults_off() {
        let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
        assert!(!p.engine.session_gate);
    }

    /// RULED (see `EngineCfg::session_gate`'s doc): step 1 found no by-name `SessionCalendar`
    /// lookup, so this task's deliverable is `session_gate` alone — no `[engine.sessions]` table.
    /// The brief's own third test was written as a decision point that passed either way; this
    /// replaces it with the real assertion for the scope that was actually chosen: the gate parses
    /// and validates cleanly with no calendar configured.
    ///
    /// ⚠ This comment used to add "fail-permissive always-open at runtime", and that was FALSE as
    /// a general claim — `crate::hist_replay::replay_ticks` sets `default_venue` unconditionally
    /// and `bar_engine_params` sets it under `snap_to_properties`, so each symbol resolves through
    /// `vike_model::session_for`, which hands back a real `FX_WEEK` calendar for the FX/CFD
    /// venues. Arming the gate with no calendar table is a MODELLING choice, not a no-op. What
    /// this test asserts is only what its name says: no calendar table is needed to LOAD it.
    #[test]
    fn session_gate_without_a_calendar_parses_and_validates_cleanly() {
        let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nsession_gate = true");
        let p = BacktestProfile::from_toml_str(&toml)
            .expect("no calendar table needed to arm the gate");
        assert!(p.engine.session_gate);
    }

    /// The text a profile was parsed FROM comes back with it, so the run record can store the bytes
    /// that actually drove the run rather than re-reading a file that may have changed since.
    #[test]
    fn loading_a_profile_hands_back_the_exact_text_it_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sma.toml");
        std::fs::write(&path, BAR_TOML).unwrap();

        let (profile, text) = BacktestProfile::from_path_with_text(&path).unwrap();

        assert_eq!(text, BAR_TOML, "byte for byte, comments and blank lines included");
        assert_eq!(profile.data.interval, "1h");
        assert!(profile.base_dir.is_some(), "and it still records the sidecar base directory");
    }

    /// The old door keeps working and keeps meaning the same thing — it has callers this plan does
    /// not touch, and it must not grow a second parse.
    #[test]
    fn the_path_loader_still_answers_and_delegates_to_the_same_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sma.toml");
        std::fs::write(&path, BAR_TOML).unwrap();

        let only = BacktestProfile::from_path(&path).unwrap();
        let (both, _) = BacktestProfile::from_path_with_text(&path).unwrap();

        assert_eq!(only.data.interval, both.data.interval);
        assert_eq!(only.base_dir, both.base_dir);
    }
}
