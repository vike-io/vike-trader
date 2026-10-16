//! The `[walkforward]` section: `WalkforwardCfg`, its window form and its span resolvers.

use serde::Deserialize;

use crate::harness::HarnessError;
use crate::harness::sweep::RankMetric;
use crate::walkforward::runner::WindowSearch;
use vike_analytics::validation::WalkMode;
use vike_model::time::{Span, parse_span};

#[cfg(doc)]
use super::{BacktestProfile, EngineCfg};

/// The `[walkforward]` section: how many out-of-sample windows to walk this profile's bar slice
/// over ([`crate::walkforward::runner::run_walkforward`]), and — since the optimizing driver
/// landed — whether each window SEARCHES its own training half before it trades its validation
/// half ([`crate::walkforward::runner::run_walkforward_optimized`]).
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
/// [`crate::walkforward::runner::run_walkforward`]'s own doc. A `[walkforward]` key that set the
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
    /// (`crate::walkforward::windows::resolve_windows`).
    #[serde(default)]
    pub train: Option<String>,
    /// Window form (b)/(c) — the VALIDATION window's length. Required with [`Self::train`].
    #[serde(default)]
    pub test: Option<String>,
    /// How far each window advances. Absent ⇒ equal to [`Self::test`], which tiles the
    /// validation windows edge to edge.
    ///
    /// ⚠ Must resolve to at least as many bars as [`Self::test`], and a SHORTER one is refused
    /// by name (`crate::walkforward::windows::resolve_windows`) rather than run: overlapping
    /// validation windows are folded onto ONE running equity by
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
    /// than being refused. `crate::walkforward::windows::resolve_windows`'s `Rounding` carries
    /// the argument.
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
    /// A [`RankMetric`] rather than a [`crate::search::objective::Objective`], and the difference
    /// is what a TOML file can NAME. `RankMetric` is a closed enum with an existing
    /// case-insensitive parser ([`RankMetric::from_str_ci`], shared with the sweep bin so the two
    /// doors cannot accept different spellings) and a direction-folding [`RankMetric::objective`]
    /// constructor, so string → objective is total and an unknown string fails at load with the
    /// valid set printed.
    /// An `Objective` is a `Box<dyn Fn>`: the only ones a profile could name are ones some registry
    /// named for it, and the composite `multi_metric` — the one non-`RankMetric` objective this
    /// crate ships — carries five tunables ([`crate::search::objective::MultiMetricParams`]), so
    /// naming it from TOML asks a second design question (does this section grow a params table,
    /// or silently pick the defaults?) that nothing on this path needs answered yet. The capability
    /// is not lost, only unspelled: [`crate::walkforward::runner::run_walkforward_optimized_with`]
    /// takes any `Objective` a caller can build.
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
