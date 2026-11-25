//! The `[data]` section: `ProfileData` and the resolvers for its time and coverage keys.

use std::collections::BTreeSet;

use serde::Deserialize;

// The coverage/universe vocabulary lives in `vike_model::run_request::gap`, below this file AND the
// CLI that validates the same words: this file RESOLVES the three `[data]` keys into those values
// and the gate (`crate::run::data_plan`) is what applies them. Naming the one enumeration rather than
// declaring copies is what keeps `data.on_gap`'s accepted set and the disposition the gate
// actually switches on the same.
use crate::HarnessError;
use crate::run::data_plan::CoverageGate;
use crate::run::refusals::ProfileRefusal;
use crate::run::tick_replay::{
    ProfileDataMark, ProfileDataReference, ProfileDataSeries, SeriesKind, traded_legs,
};
use vike_data::csv_bars::{CsvBarsFormat, CsvBarsFormatWords};
use vike_model::run_request::gap::{OnGap, UniverseMode};
use vike_model::run_request::replay::ReplayKind;
use vike_model::time::{Span, parse_span};

#[cfg(doc)]
use super::ProfileBacktest;

/// Which data slice to load: bar/tick kind + interval (bars only) + range, over EITHER a
/// single-venue `venue` + `symbols` pair (the frozen form) OR a cross-venue [`Self::series`]
/// array (port backlog G4). Exactly one of the two forms must be present.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProfileData {
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
    pub series: Vec<ProfileDataSeries>,
    /// Tick mode only (`[[data.reference]]`): other venues' series whose touch the strategy hears on
    /// `on_reference_quote`, each event when the recording box received it plus the stream's `delay_ms`
    /// (the hop to this box a live mount waits for; required where nothing was recorded). `trade = true`
    /// also trades one (a hedge leg). Absent: nothing is heard this way, byte-identical.
    #[serde(default)]
    pub reference: Vec<ProfileDataReference>,
    /// Tick mode only (`[[data.mark]]`): series the strategy hears on `on_mark` as an underlying's mark
    /// (a quote's mid, a print's price, a book's mid), each event when the recording box received it plus
    /// the stream's `delay_ms` (required where nothing was recorded), never traded. Absent: byte-identical.
    #[serde(default)]
    pub mark: Vec<ProfileDataMark>,
    /// Tick mode only: the mounted (first) series' recorded stream-health markers reach `on_feed_status`
    /// (`Disconnected`, `Stale`, `Live`), as a live mount hears its feed. Absent (`false`): byte-identical.
    #[serde(default)]
    pub feed_status: bool,
    pub kind: ReplayKind,
    #[cfg_attr(
        feature = "profile-schema",
        schemars(extend("x-default-function" = "default_interval"))
    )]
    #[serde(default = "default_interval")]
    pub interval: String,
    /// Bare epoch-ms integer (as a string), `YYYY-MM-DDTHH` UTC, or a plain `YYYY-MM-DD` UTC date
    /// (its midnight) — see [`ProfileBacktest::range`].
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
    /// start, and [`vike_analytics::TradingRecordSimulator::warmup`] then reports the number the run actually
    /// gated on (`vike_analytics::zero_trade`'s `warmup-shortfall` cause reads it, so a warmup
    /// that swallows the whole slice diagnoses itself instead of reading as "strategy did
    /// nothing"). Freqtrade spells it `startup_candle_count`, LEAN `SetWarmUp`, QuantRocket
    /// `LOOKBACK_WINDOW`.
    ///
    /// # ⚠ It is a FLOOR on `Strategy::warmup()`, never an override — including downwards
    ///
    /// The engine gates every strategy callback on `index >= warmup` and the number it uses is
    /// `max(this, strategy.warmup())` (`vike_simulator::StrategyEngine::new` resolves it once). A profile
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
    /// (`vike_simulator::OrderBrokerSimulator`'s bar reads), not out of state accumulated inside gated
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
    /// counts each such bar in [`vike_analytics::TradingRecordSimulator::intrabar_both_hit`] — so the engine has
    /// always known exactly which bars it was guessing on. With a detail series present that
    /// symbol's step instead runs `vike_simulator::StrategyEngine::fill_pending_granular`, which walks the
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
    /// `StrategyEngine::fill_pending` runs the tagged (`vike_model::OrderBroker`) maker lane before
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
    /// checked in [`ProfileBacktest::refusals`] because neither reads `[data]` alone.
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
    /// route at all, with no wire verb of its own (`vike_backtesthub::backtesthub_server`'s `run_backtest` returns
    /// it as the run's report document and computes nothing).
    ///
    /// # ⚠ It is a REHEARSAL, not a dry run of the engine
    ///
    /// It proves what the store holds. It does not compile the strategy, build the engine params,
    /// or resolve `[engine.resolution]`'s winners sidecar — a profile that plans cleanly can still
    /// fail on any of those. `ProfileBacktest::validate` is what answers for the profile's own
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
    /// key, in [`ProfileBacktest::refusals`] — a disposition or a tolerance for a gate nobody armed
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
    /// Read the bars from a `source=` LANE of the store instead of the venue's own candles: a declared
    /// lane (`"aggtrades"`, `"tardis"`) or a user's, `"user-<label>"` (decision 0268). Absent (the
    /// default) reads the venue's own candles, byte-identical to every profile written before this key.
    /// Bar mode only, and never with [`Self::file`].
    ///
    /// The lane serves the price bars and the intrabar detail tape alike, through the store's sourced
    /// read (`HistoryStore::load_bars_sourced_capped`); the funding series and the instrument grid stay the
    /// venue's, because neither is a candle. A lane holding no bar of a series in the window is refused
    /// at load: naming one says the bars are there, and an empty run would read as a strategy that did
    /// nothing. The report states the lane (the realism stamp's `data.source`, and a `data_warnings`
    /// line).
    #[serde(default)]
    pub source: Option<String>,
    /// Read the bars from a CSV FILE on the box that runs the backtest, without importing it (decision
    /// 0268): ONE series, its columns found by header name, through the decoder the `csv-bars` import
    /// uses, so a file this reads is a file the import takes. A relative path resolves against the
    /// profile's own directory. Bar mode only, and never with [`Self::source`].
    ///
    /// For one-off runs and research; a run a deployment depends on imports its data. Because the file is
    /// not in the store, the keys that plan over the store (`require_coverage`, a `universe` that
    /// consults it, `explain`) and `detail_interval` (a file holds one interval) are refused beside it,
    /// and a backtesthub refuses a profile naming one, since a daemon opens no file a client names. The
    /// report names the file as its source.
    #[serde(default)]
    pub file: Option<String>,
    /// How [`Self::file`]'s cells are written, when not the plain form (`,` between fields, `.` as the
    /// decimal mark, no thousands separator, integer epoch times): the explicit options a regional
    /// export needs, e.g. `{ delimiter = ";", decimal = ",", thousands = "space", time =
    /// "DD.MM.YYYY HH:mm", tz = "Europe/Moscow" }`. Nothing reads the machine's locale, and a file in a
    /// regional form read without these is refused naming the option to set. Only with `file`.
    ///
    /// The table IS the CSV decoder's own words, `vike_data::csv_bars::CsvBarsFormatWords`, the
    /// grammar the `csv-bars` import reads too, so a file reads the same through both doors: each key
    /// absent is its default, and a key that is none of the five is refused.
    #[serde(default)]
    pub file_format: Option<CsvBarsFormatWords>,
    /// OPTION LEGS read from a stored option chain, traded beside the run's series (`[data.options]`,
    /// [`ProfileDataOptions`]). Absent (the default) is byte-identical to every profile written before
    /// this key existed. Tick mode only: a leg prices only in the hours its chain holds a row of it,
    /// and only the event lane lets a series be absent where it has no price.
    #[serde(default)]
    pub options: Option<ProfileDataOptions>,
}

/// `[data.options]`: option LEGS a run trades, priced from a stored option CHAIN
/// (`vike_data::ChainRow`, the `kind=chain` series of one underlying), beside the run's own series.
///
/// Each leg is one more symbol of the event lane: every priced chain row of it becomes one price event
/// at the row's `ts` (the row's own bid and ask on a chain that carries quotes; on a chain that carries
/// none, the mark less and plus [`Self::assumed_half_spread`]), so the leg has a price exactly where the
/// chain holds one and an order on it waits for the next (no fill without a price). Its strike, expiry and right are its chain row's; the run settles a leg still
/// held at its expiry to cash at its intrinsic value against [`Self::underlying`]'s price
/// (`vike_simulator::StrategyEngineConfig::option_specs`). The run's ledger is the dollar, as the
/// chain's marks are.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProfileDataOptions {
    /// The venue the chain is stored under (`kind=chain/venue=<venue>`), such as `"deribit"`.
    pub venue: String,
    /// The chain series the legs are read from: the underlying's partition, never an option's own
    /// name (`"BTC"` for deribit's coin book, `"BTC_USDC"` for its USDC book, `"BTCUSDT"` on binance).
    pub chain: String,
    /// The run's series whose price settles every leg at its expiry and prices a leg's fee where the
    /// venue charges it on the underlying: one of the run's own symbols, a LINEAR one (a coin-margined
    /// contract would make the run a coin ledger, and the legs are dollars).
    pub underlying: String,
    /// Read [`Self::underlying`] from its stored BARS of this interval (`"1h"`) instead of its
    /// recorded ticks: each bar is one price event at its LAST millisecond, priced at its close, the
    /// instant the close is known. Absent: the underlying's own quotes, trades and book events.
    #[serde(default)]
    pub underlying_interval: Option<String>,
    /// The option instruments to trade, each as the chain names it (`ChainRow::instrument`, such as
    /// `"BTC-5JAN24-48000-C"`). None may repeat or be a series of the run.
    pub legs: Vec<String>,
    /// The HALF-SPREAD a leg trades at on a chain that carries NO bid or ask (a chain derived from the venue's
    /// trades, deribit's history), as a FRACTION OF THE MARK, above 0 and below 1: a leg there sells at
    /// `mark · (1 − assumed_half_spread)` and buys at `mark · (1 + assumed_half_spread)`, and the run's data
    /// warnings say the spread was ASSUMED, not quoted. REQUIRED on such a chain: a run over one without it is
    /// refused in words, never filled at the bare mark (REVIEW-LIVE-MONEY M7). On a chain that carries quotes
    /// each row's own bid and ask are the price and this key is not read (the warnings say so). State where the
    /// number comes from: a measured median of the venue's quoted spreads, or a cited typical one.
    #[serde(default)]
    pub assumed_half_spread: Option<f64>,
}

impl ProfileDataOptions {
    /// [`Self::underlying_interval`] in milliseconds, or `None` when absent; a value that is no
    /// interval, or a zero-length one, is refused.
    pub fn underlying_interval_ms(&self) -> Result<Option<i64>, HarnessError> {
        let Some(raw) = self.underlying_interval.as_deref() else { return Ok(None) };
        match vike_marketdata::interval::interval_ms(raw) {
            Some(ms) if ms > 0 => Ok(Some(ms)),
            _ => Err(HarnessError::from(ProfileRefusal::DataOptionsUnderlyingIntervalInvalid {
                raw: raw.to_string(),
            })),
        }
    }
}

/// Where a bar run reads its bars: [`ProfileData::bar_source`]'s answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarSource<'a> {
    /// The venue's own candles, the sourceless series: a profile naming neither key.
    Venue,
    /// A `source=` lane of the store, [`ProfileData::source`].
    Lane(&'a str),
    /// A CSV file read without importing it, [`ProfileData::file`], as the profile spells it.
    File(&'a str),
}

impl ProfileData {
    /// Resolve [`Self::source`] and [`Self::file`] into where the bars come from, refusing every
    /// combination that cannot be honoured. Both absent is [`BarSource::Venue`], byte-identical.
    ///
    /// # What it refuses, and why
    ///
    /// Both keys at once names two places for one series. A lane must be one the store can name
    /// (`vike_data::store::store_kind::refuse_an_unknown_lane`, the check the store's own sourced verbs
    /// make). Tick mode is refused for either: a lane and a file are read as BARS, and the tick replay
    /// reads the venue's own tape. A file holds one series of one interval and is not in the store, so a
    /// file run names one series, takes no detail tape, and arms none of the keys that plan over the
    /// store; each of those would otherwise load, judge the store's series instead of the file's, and
    /// report it as the run's.
    pub fn bar_source(&self) -> Result<BarSource<'_>, HarnessError> {
        if self.file_format.is_some() && self.file.is_none() {
            return Err(HarnessError::from(ProfileRefusal::DataFileFormatWithoutFile));
        }
        let source = match (self.source.as_deref(), self.file.as_deref()) {
            (None, None) => return Ok(BarSource::Venue),
            (Some(_), Some(_)) => {
                return Err(HarnessError::from(ProfileRefusal::DataSourceAndFileBothSet));
            }
            (Some(lane), None) => {
                vike_data::store::store_kind::refuse_an_unknown_lane(lane)
                    .map_err(|e| HarnessError::from(ProfileRefusal::DataSourceLaneUnknown { e }))?;
                BarSource::Lane(lane)
            }
            (None, Some(path)) if path.trim().is_empty() => {
                return Err(HarnessError::from(ProfileRefusal::DataFileEmpty));
            }
            (None, Some(path)) => BarSource::File(path),
        };
        if self.kind == ReplayKind::Tick {
            return Err(HarnessError::from(ProfileRefusal::DataSourceAndFileBarModeOnly));
        }
        if let BarSource::File(_) = source {
            self.csv_format()?;
            let series = self.resolved_series().len();
            if series != 1 {
                return Err(HarnessError::from(ProfileRefusal::DataFileSeriesNotOne { series }));
            }
            if self.detail_interval.is_some() {
                return Err(HarnessError::from(ProfileRefusal::DataFileDetailIntervalUnavailable));
            }
            let plans = self.require_coverage
                || self.explain
                || self.universe_mode().is_ok_and(|m| m.consults_the_store());
            if plans {
                return Err(HarnessError::from(ProfileRefusal::DataFileNotInStore));
            }
        }
        Ok(source)
    }

    /// The form [`Self::file`] is read in: [`Self::file_format`]'s, or the plain form when absent.
    pub fn csv_format(&self) -> Result<CsvBarsFormat, HarnessError> {
        let Some(words) = &self.file_format else {
            return Ok(CsvBarsFormat::default());
        };
        CsvBarsFormat::parse(words)
            .map_err(|e| HarnessError::from(ProfileRefusal::DataFileFormatInvalid { e }))
    }

    /// The `source` of the series a bar run reads: the lane, [`vike_data::store::store_kind::FILE_SOURCE`]
    /// for a file, `None` for the venue's own candles (and for a profile [`Self::bar_source`] refuses).
    /// What the run's planned series ids carry, so a lane's or a file's run never addresses as the
    /// venue's.
    pub fn bar_series_source(&self) -> Option<&str> {
        match self.bar_source() {
            Ok(BarSource::Lane(lane)) => Some(lane),
            Ok(BarSource::File(_)) => Some(vike_data::store::store_kind::FILE_SOURCE),
            Ok(BarSource::Venue) | Err(_) => None,
        }
    }

    /// The series this profile actually loads as its symbol slots: [`Self::series`] verbatim, else
    /// the `venue` × `symbols` whole-lane expansion, then each reference it also trades
    /// (`crate::run::tick_replay::traded_legs`). Empty only for a profile that failed
    /// [`ProfileBacktest::validate`].
    pub fn resolved_series(&self) -> Vec<ProfileDataSeries> {
        let mut out = if !self.series.is_empty() {
            self.series.clone()
        } else {
            let venue = self.venue.clone().unwrap_or_default();
            self.symbols.iter().map(|s| ProfileDataSeries::new(&venue, s)).collect()
        };
        out.extend(traded_legs(&self.reference));
        out
    }

    /// [`Self::options`]' own rules, at load: tick mode, a chain and a venue named, at least one leg,
    /// no leg repeated or a series of the run, the underlying one of the run's series, an
    /// underlying interval that resolves, and an assumed half-spread inside (0, 1). `Ok` when the table is
    /// absent. (Whether the chain NEEDS the half-spread is known only once its rows are read: the run's.)
    pub fn options_check(&self) -> Result<(), HarnessError> {
        let Some(options) = &self.options else { return Ok(()) };
        if self.kind != ReplayKind::Tick {
            return Err(HarnessError::from(ProfileRefusal::DataOptionsTickModeOnly));
        }
        if options.venue.trim().is_empty() || options.chain.trim().is_empty() {
            return Err(HarnessError::from(ProfileRefusal::DataOptionsVenueAndChainMissing));
        }
        if options.legs.is_empty() {
            return Err(HarnessError::from(ProfileRefusal::DataOptionsLegsEmpty));
        }
        let series = self.resolved_series();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for leg in &options.legs {
            if leg.trim().is_empty() || !seen.insert(leg.as_str()) {
                return Err(HarnessError::from(ProfileRefusal::DataOptionsLegEmptyOrRepeated {
                    leg: leg.clone(),
                }));
            }
            if series.iter().any(|s| s.symbol == *leg) {
                return Err(HarnessError::from(ProfileRefusal::DataOptionsLegIsRunSeries {
                    leg: leg.clone(),
                }));
            }
        }
        if !series.iter().any(|s| s.symbol == options.underlying) {
            return Err(HarnessError::from(ProfileRefusal::DataOptionsUnderlyingNotRunSeries {
                underlying: options.underlying.clone(),
            }));
        }
        options.underlying_interval_ms()?;
        if let Some(half) = options.assumed_half_spread
            && !(half.is_finite() && half > 0.0 && half < 1.0)
        {
            return Err(HarnessError::from(
                ProfileRefusal::DataOptionsAssumedHalfSpreadOutOfRange { half },
            ));
        }
        Ok(())
    }

    /// The run's single `StrategyEngineConfig::default_venue` tag: [`Self::venue`] when set, else the
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

    /// What the strategy hears beside its market ([`Self::reference`], [`Self::mark`],
    /// [`Self::feed_status`]) is heard between ticks, so on the tick lane alone; a reference is ANOTHER
    /// venue's touch (live hears a series of the mount's own venue on its market lanes), listed once,
    /// and a touch has no prints to hear.
    pub(super) fn validate_heard(&self) -> Result<(), HarnessError> {
        let heard = !self.reference.is_empty() || !self.mark.is_empty();
        if self.kind == ReplayKind::Bar && (heard || self.feed_status) {
            return Err(HarnessError::from(ProfileRefusal::DataHeardStreamsTickModeOnly));
        }
        let own_venue = self.default_venue();
        let mut references: std::collections::BTreeSet<(&str, &str)> = Default::default();
        for r in &self.reference {
            if r.venue == own_venue {
                return Err(HarnessError::from(ProfileRefusal::DataReferenceOnOwnVenue {
                    symbol: r.symbol.clone(),
                    venue: r.venue.clone(),
                }));
            }
            if r.kind == SeriesKind::Trade {
                return Err(HarnessError::from(ProfileRefusal::DataReferenceKindTrade {
                    symbol: r.symbol.clone(),
                }));
            }
            if !references.insert((r.venue.as_str(), r.symbol.as_str())) {
                return Err(HarnessError::from(ProfileRefusal::DataReferenceListedTwice {
                    symbol: r.symbol.clone(),
                    venue: r.venue.clone(),
                }));
            }
        }
        Ok(())
    }

    /// True when the resolved series span more than one venue (a traded reference is always on
    /// another venue than the run's own).
    pub fn is_cross_venue(&self) -> bool {
        let traded_leg = self.reference.iter().any(|r| r.trade);
        let mut venues = self.series.iter().map(|s| s.venue.as_str());
        let Some(first) = venues.next() else { return traded_leg };
        traded_leg || venues.any(|v| v != first)
    }

    /// Resolve [`Self::warmup`] to the number of STEPS the engine must gate before it dispatches
    /// a strategy callback, or `None` when the key is absent (⇒ `vike_simulator::StrategyEngineConfig::warmup` is
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
        let span = parse_span(raw)
            .map_err(|e| HarnessError::from(ProfileRefusal::DataWarmupSpanInvalid { e }))?;
        match span {
            Span::Bars(n) => Ok(Some(n)),
            Span::Months(_) => Err(HarnessError::from(ProfileRefusal::DataWarmupCalendarSpan {
                raw: raw.to_string(),
            })),
            Span::Ms(_) if self.kind == ReplayKind::Tick => {
                Err(HarnessError::from(ProfileRefusal::DataWarmupDurationOnTicks {
                    raw: raw.to_string(),
                }))
            }
            Span::Ms(ms) => {
                let Some(base) = vike_marketdata::interval::interval_ms(&self.interval) else {
                    return Err(HarnessError::from(ProfileRefusal::DataWarmupIntervalInvalid {
                        interval: self.interval.clone(),
                        raw: raw.to_string(),
                    }));
                };
                if base <= 0 {
                    return Err(HarnessError::from(
                        ProfileRefusal::DataWarmupIntervalNotPositive {
                            interval: self.interval.clone(),
                            base,
                            raw: raw.to_string(),
                        },
                    ));
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
    /// absent (⇒ `vike_simulator::StrategyEngineConfig::granular_by_symbol` stays empty ⇒ every symbol fills on
    /// the coarse lane, byte-identical).
    ///
    /// # What it refuses, and why each silence would be worse than the refusal
    ///
    /// The grammar is [`vike_marketdata::interval::interval_ms`] — the same vocabulary [`Self::interval`]
    /// and `engine.timeframes` use, not the wider [`Span`] one, because this value is handed
    /// STRAIGHT to `HistoryStore::load_bars` as a series interval and a `"3mo"` names no stored
    /// series. It must be STRICTLY FINER than the base interval: this is the exact inverse of the
    /// `engine.timeframes` rule (those must be strictly COARSER, because a coarse series is
    /// RESAMPLED from the base and a finer one cannot be), and here an equal-or-coarser detail
    /// series would bucket at most one sub-bar per coarse step — which resolves no ambiguity at
    /// all while costing a second whole store scan, and reads as a working realism knob. Tick
    /// mode is refused because the granular lane is `vike_simulator::StrategyEngine::run`'s own bar step:
    /// `run_ticks` never reads the per-symbol `sub` buckets, so a tick profile setting this would
    /// parse, load a second series and change nothing — the lane-asymmetric silence
    /// `engine.feed_latency`, `engine.queue_model` and `engine.timeframes` each already refuse.
    pub fn detail_interval_ms(&self) -> Result<Option<i64>, HarnessError> {
        let Some(raw) = self.detail_interval.as_deref() else { return Ok(None) };
        if self.kind == ReplayKind::Tick {
            return Err(HarnessError::from(ProfileRefusal::DataDetailIntervalBarModeOnly {
                raw: raw.to_string(),
            }));
        }
        let Some(ms) = vike_marketdata::interval::interval_ms(raw) else {
            return Err(HarnessError::from(ProfileRefusal::DataDetailIntervalInvalid {
                raw: raw.to_string(),
            }));
        };
        if ms <= 0 {
            return Err(HarnessError::from(ProfileRefusal::DataDetailIntervalNotPositive {
                raw: raw.to_string(),
                ms,
            }));
        }
        let Some(base) = vike_marketdata::interval::interval_ms(&self.interval) else {
            return Err(HarnessError::from(
                ProfileRefusal::DataDetailIntervalBaseIntervalInvalid {
                    interval: self.interval.clone(),
                    raw: raw.to_string(),
                },
            ));
        };
        if ms >= base {
            return Err(HarnessError::from(ProfileRefusal::DataDetailIntervalNotFiner {
                raw: raw.to_string(),
                ms,
                interval: self.interval.clone(),
                base,
            }));
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
            HarnessError::from(ProfileRefusal::DataOnGapUnknown {
                raw: raw.to_string(),
                roster: OnGap::roster(),
            })
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
        let span = parse_span(raw)
            .map_err(|e| HarnessError::from(ProfileRefusal::DataMaxGapSpanInvalid { e }))?;
        match span {
            Span::Ms(ms) if ms > 0 => Ok(Some(ms)),
            Span::Ms(ms) => Err(HarnessError::from(ProfileRefusal::DataMaxGapNotPositive {
                raw: raw.to_string(),
                ms,
            })),
            Span::Bars(n) => Err(HarnessError::from(ProfileRefusal::DataMaxGapBarCount {
                raw: raw.to_string(),
                n,
            })),
            Span::Months(_) => Err(HarnessError::from(ProfileRefusal::DataMaxGapCalendarSpan {
                raw: raw.to_string(),
            })),
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
            HarnessError::from(ProfileRefusal::DataUniverseUnknown {
                raw: raw.to_string(),
                roster: UniverseMode::roster(),
            })
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

/// A `delay_ms` a `[[data.reference]]` or `[[data.mark]]` table states is a whole number of milliseconds,
/// 0 or more, refused in words before the typed read (which would only name the type): a negative one
/// would hear another venue before its event reached this box. An absent one is the replay's to judge
/// (`crate::run::tick_replay`'s module doc, "Heard streams"): the recorded receive time needs none.
pub(super) fn refuse_invalid_delays(doc: &toml::Table) -> Result<(), HarnessError> {
    let data = doc.get("data").and_then(toml::Value::as_table);
    for table in ["reference", "mark"] {
        let streams = data.and_then(|d| d.get(table)).and_then(toml::Value::as_array);
        for stream in streams.into_iter().flatten() {
            let Some(delay) = stream.get("delay_ms") else { continue };
            if delay.as_integer().is_some_and(|ms| ms >= 0) {
                continue;
            }
            let symbol = stream.get("symbol").and_then(toml::Value::as_str).unwrap_or_default();
            return Err(HarnessError::from(ProfileRefusal::DataStreamDelayInvalid {
                table: table.to_string(),
                symbol: symbol.to_string(),
                delay: delay.clone(),
            }));
        }
    }
    Ok(())
}
