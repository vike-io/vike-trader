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
use crate::validation::WalkMode;
use vike_data::TsRange;
use vike_exec::ProfileRisk;
use vike_model::FeeSchedule;

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
    /// Optional parameter-sweep grid: each key is a `strategy.params` field name, each value an
    /// array of TOML values to cross-product over (Task 1 of the sweep harness — expansion lives
    /// in [`super::sweep::expand_sweep`]). Absent or empty means "not a sweep".
    #[serde(default)]
    pub sweep: Option<toml::Table>,
    /// Optional anchored WALK-FORWARD config — the `[sweep]` sibling: present means this profile
    /// can ALSO be run through [`super::walkforward::run_walkforward`] over
    /// `walkforward.n_splits` out-of-sample windows. Absent (the default) is byte-identical to
    /// every profile written before this field existed, and `run_backtest`/`run_sweep` ignore the
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
/// n_splits = 6         # required: the number of out-of-sample windows
/// search  = "sweep"    # absent / "none" = the CONTROL; "sweep" searches the [sweep] table
/// mode    = "rolling"  # absent / "anchored" = expanding train; "rolling" = the preceding chunk
/// rank_by = "return"   # absent / "sharpe"; also "return" | "max_dd" | "equity"
/// ```
///
/// Every knob but `n_splits` defaults to the behaviour that existed before it did, so a profile
/// written against the one-field version of this section parses and runs byte-identically.
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
    /// Number of out-of-sample windows. Must be `>= 1` ([`BacktestProfile::validate`] rejects
    /// `0`); a negative TOML integer fails to deserialize into `usize` as a parse error.
    pub n_splits: usize,
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

impl WalkforwardCfg {
    /// Resolve [`Self::search`] to the driver's [`WindowSearch`]. Absent ⇒ [`WindowSearch::None`],
    /// the control. Case-insensitive and trimmed — the [`EngineCfg::queue_model_kind`] idiom.
    ///
    /// ⚠ ONE spelling, and `"grid"` is deliberately NOT an alias for it. Both parsed for exactly
    /// one release: the design doc said `sweep`, the first implementation said `grid`, and rather
    /// than pick, the two were accepted as synonyms. That is a knob with two names — the shape
    /// where somebody writes one, greps for the other, and concludes the feature is not wired.
    /// `sweep` won because it names the thing it searches: `search = "sweep"` expands the
    /// `[sweep]` table, and this crate's whole vocabulary for that object is already `run_sweep` /
    /// `expand_sweep` / `SweepPoint`. `grid` was the synonym, so `grid` is the one that went.
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
    /// Stop-verb release timing (see [`crate::EngineParams::emulator_release_stops`]). The
    /// HARNESS default is `true` — **mirror-live**: a fired conditional stop is released as a
    /// MARKET that fills the NEXT event, exactly as the live emulator's `ConditionalBook` does.
    /// Set `false` to restore the raw-engine legacy same-event-at-trigger fill (the pinned
    /// backtest divergence, law-map A2). NOTE the low-level [`crate::EngineParams`] default stays
    /// `false`; only this TOML-driven harness path defaults to mirror-live so a profile run
    /// reproduces live fill timing without an explicit knob.
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

/// The HARNESS default for [`EngineCfg::emulator_release_stops`]: `true` (mirror-live). Distinct
/// from the raw [`crate::EngineParams`] default (`false`) on purpose — see the field doc.
fn default_emulator_release_stops() -> bool {
    true
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

    /// Parse + validate a profile from a file on disk. Records the file's directory as
    /// [`Self::base_dir`] so relative sidecar paths (e.g. `[engine.resolution].path`) resolve
    /// next to the profile rather than against the caller's CWD.
    pub fn from_path(path: &Path) -> Result<Self, HarnessError> {
        let s = std::fs::read_to_string(path)
            .map_err(|e| HarnessError::Io(format!("{}: {e}", path.display())))?;
        let mut profile = Self::from_toml_str(&s)?;
        profile.base_dir = path.parent().map(Path::to_path_buf);
        Ok(profile)
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
        // What fails at LOAD in this section, and what deliberately does not. A `[walkforward]`
        // value that is wrong ON ITS OWN TERMS — independent of the store, of the data slice and
        // of which verb is running — fails here: a zero split count (there is no window to test
        // on), an unrecognized `search`/`mode`/`rank_by` string (a typo must never degrade into a
        // silently different protocol), and `search = "sweep"` on a profile carrying no `[sweep]`
        // table (a walk that says it optimizes with nothing to search is a contradiction that no
        // data can resolve, and it is the same shape as the zero-split hole above).
        //
        // The bar-mode and single-symbol rules are NOT checked here, and the difference is what
        // they depend on: those are questions about what the profile RESOLVES against a store,
        // while `[walkforward]` is inert for a plain `run_backtest`/`run_sweep` run — so
        // rejecting a profile that merely CARRIES the section would fail runs that never ask for
        // a walk-forward. `super::walkforward` enforces those where they apply, and re-enforces
        // the rules below too, because a hand-built profile never passed through here at all.
        if let Some(wf) = &self.walkforward {
            if wf.n_splits == 0 {
                return Err(HarnessError::Validation(
                    "walkforward.n_splits must be >= 1, got 0".to_string(),
                ));
            }
            // Every string knob is RESOLVED here so a typo fails at load; only `search`'s value is
            // wanted (the grid rule below reads it), and the other two are called for the parse
            // alone — the drivers read them again from the same resolvers.
            let search = wf.window_search()?;
            wf.walk_mode()?;
            wf.rank_metric()?;
            if search == WindowSearch::Sweep && !self.is_sweep() {
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

    /// True if `sweep` is present and non-empty — the marker that this profile expands into a
    /// parameter grid ([`super::sweep::expand_sweep`]) rather than running as a single backtest.
    pub fn is_sweep(&self) -> bool {
        self.sweep.as_ref().is_some_and(|t| !t.is_empty())
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
        // The harness default for the stop-verb release knob is mirror-live (`true`), NOT the raw
        // `EngineParams` default (`false`).
        assert!(p.engine.emulator_release_stops);
        assert_eq!(p.strategy.name, "sma_cross");

        // 2026-01-01T00:00 UTC and 2026-01-02T00:00 UTC (verified against `date -u -d`).
        let r = p.range().unwrap();
        assert_eq!(r.start, Some(1_767_225_600_000));
        assert_eq!(r.end, Some(1_767_312_000_000));
    }

    /// The harness stop-verb release knob defaults to mirror-live (`true`) and an explicit
    /// `emulator_release_stops = false` parses through to restore the legacy same-event fill.
    #[test]
    fn emulator_release_stops_defaults_true_and_parses_false() {
        // absent -> mirror-live default
        let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
        assert!(p.engine.emulator_release_stops);

        // explicit false threads through
        let toml = BAR_TOML
            .replace("fee_rate = 0.001", "fee_rate = 0.001\nemulator_release_stops = false");
        let p = BacktestProfile::from_toml_str(&toml).unwrap();
        assert!(!p.engine.emulator_release_stops);

        // explicit true also parses
        let toml =
            BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nemulator_release_stops = true");
        let p = BacktestProfile::from_toml_str(&toml).unwrap();
        assert!(p.engine.emulator_release_stops);
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
}
