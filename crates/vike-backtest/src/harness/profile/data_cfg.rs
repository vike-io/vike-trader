//! The `[data]` section: `DataCfg`, `DataKind` and the resolvers for its time and coverage keys.

use serde::Deserialize;

// The coverage/universe vocabulary lives with the pre-flight that enforces it, not here: this file
// RESOLVES the three `[data]` keys into those values and the gate is what applies them. Naming the
// types there rather than declaring copies here is what keeps `data.on_gap`'s accepted set and the
// disposition the gate actually switches on the same enumeration.
use crate::data_plan::{CoverageGate, OnGap, UniverseMode};
use crate::harness::HarnessError;
use crate::hist_replay::SeriesRef;
use vike_model::time::{Span, parse_span};

#[cfg(doc)]
use super::BacktestProfile;

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
    /// has no rows there. `crates/vike-data/src/store/coverage.rs`'s module doc carries the measurement —
    /// Polymarket's own API serves no book history, so "I filled that gap from the venue" produces
    /// exactly that shape — and the same silence covers the cheaper case of a window that opens
    /// before the tape does, which `vike_data::find_gaps` cannot see BY CONSTRUCTION because its
    /// holes are strictly inside the recorded span.
    ///
    /// # ⚠ What it is NOT: a row-level trust check
    ///
    /// It knows what the file index knows — which days exist and how many rows they hold. A day
    /// that is PRESENT and was recorded through a feed outage passes this gate;
    /// `vike_data::store::quality` is the question over scanned rows, and a coverage gate that implied
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
    /// A fold rather than three calls at the site for the reason `crate::search::select`'s
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
