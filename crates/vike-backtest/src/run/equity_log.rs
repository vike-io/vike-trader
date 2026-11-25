//! A bar run's EQUITY LOG (decision 0078's amendment of 2026-11-19, the owner's word): what a saved bar run keeps
//! in place of its whole equity curve, and the rebuild of that curve from it.
//!
//! `crate::run::run_backtest_logged` runs a profile exactly as `run_backtest_cancellable` does and, on the BAR
//! lane, turns the simulator's event log on (`vike_simulator::OrderBrokerSimulator::start_ledger_log`) and answers an
//! [`EquityLog`]: the log's bytes (`vike_simulator::encode_ledger_events`) and the [`EquityLogRecipe`] a rebuild needs
//! beside the stored bars. A run answers NO log, and its saver keeps the whole curve as before, when:
//!
//! - it ran on the TICK lane (`run_ticks`, whose curve is sampled per event, never per bar);
//! - its bars came from a FILE (`data.file`): no store holds them to rebuild over;
//! - its curve is empty;
//! - [`record`]'s own check fails: the curve rebuilt from this very recipe and log, over the bars the engine marked,
//!   differs from the engine's in any bit or any step's time (a cash or position site the log does not cover). It is
//!   said at `warn`.
//!
//! [`rebuild_curve`] is the other half, and the refusals are its doc's. It reads the run's `config.toml` as the run's
//! door read it (its kept rows and store names, `ProfileBacktest::read_kept_rows`), loads each series through the
//! run's own read (`load_series_bars`, split-adjusted as the run was) over the run's window, CUT to the first and last
//! bar the run marked ([`EquityLogSeries::first_bar`], [`EquityLogSeries::last_bar`]: a window open at either end, or
//! reaching past the data, has grown since by the bars a live series collected, and those are no change to the run),
//! and folds the log through the simulator's own fold (`vike_simulator::rebuild_equity_curve`): no second formula,
//! and no curve served from changed data. ⚠ Its profile was written by a SANDBOXED child and is rebuilt in the serving
//! daemon, so a profile that names a file (`data.file`, `[engine.resolution] path`) is refused before anything is read.
//!
//! ⚠ **A run whose bars the store cannot pin keeps its curve**: the saver ([`stamp`]) drops the log when a series'
//! day stamps are unknown (a datahub without `SeriesDays`) or a day of its marked bars is PROVISIONAL (rows a later
//! settled commit replaces), because such bars may read otherwise later, and a refused rebuild of a run that kept no
//! curve file would lose the curve for good.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vike_analytics::TradingRecord;
use vike_data::{BarDay, HistoryStore, HistoryStoreError, TimestampRange};
use vike_marketdata::Bar;
use vike_model::{ContractValue, StockSplit, StoreKindId};
use vike_simulator::{
    LEDGER_EVENTS_FORMAT, LedgerEvent, LedgerStart, OrderBrokerSimulator, decode_ledger_events,
    encode_ledger_events, rebuild_equity_curve,
};
use vike_utility::fnv::Fnv1a64;

use super::profile::{BarSource, ProfileBacktest, decide_mode};
use super::{ledger_label, load_series_bars};

/// What a saved bar run records beside its event log, under its manifest's `detail.equity_log`: everything a
/// rebuild reads besides the stored bars, and what it checks them against. Every number is written so it reads back
/// to the bit (the workspace's `serde_json` is `float_roundtrip`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EquityLogRecipe {
    /// The log's format (`vike_simulator::LEDGER_EVENTS_FORMAT` when it was written).
    pub format: u32,
    /// The build that saved the run, as the saving daemon names its own (`None`: it names none). Filled by the saver
    /// ([`stamp`]).
    #[serde(default)]
    pub build: Option<String>,
    /// How many steps the curve has.
    pub steps: usize,
    /// [`curve_fnv`] of the whole curve the run answered: what a rebuild must come back to.
    pub curve_fnv: String,
    /// The START STATE: the cash, in [`Self::currency`].
    pub cash: f64,
    /// The ledger's currency: a coin ledger's settle coin, else `engine.account_currency`, else `USD`.
    pub currency: String,
    /// `engine.decide`'s word: the order the equity sums the symbols in (`DecideMode::cross_section_order`).
    pub decide: String,
    /// The run's slots, in order.
    pub symbols: Vec<String>,
    /// Each slot's contract value when the run started (a conversion's later rate is in the log).
    pub values: Vec<ContractValue>,
    /// What each slot's bars were read from, and what they were.
    pub series: Vec<EquityLogSeries>,
}

/// One slot's bars, as a rebuild reads them again and checks them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EquityLogSeries {
    pub venue: String,
    /// Its STORE name.
    pub symbol: String,
    /// The lane its bars were read from (`data.source`), `None` for the venue's own candles.
    pub source: Option<String>,
    pub interval: String,
    /// The window's bounds, as `data.from` / `data.to` resolve.
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// The operator's share splits its bars were adjusted by (decision 0289), each as the run read it.
    pub splits: Vec<StockSplit>,
    /// The time of the first and of the last bar the engine marked: the bars a rebuild reads are the window's cut to
    /// these, whatever the store has collected around them since.
    pub first_bar: i64,
    pub last_bar: i64,
    /// [`closes_fnv`] of the bars the engine marked.
    pub closes_fnv: String,
    /// The store's stamp of every UTC day of the marked bars (`HistoryStore::series_days` of bars over
    /// `first_bar..=last_bar`, the datahub's `SeriesDays`), filled by the saver ([`stamp`]); `None`: the store could not
    /// say.
    #[serde(default)]
    pub days: Option<Vec<BarDay>>,
}

/// A bar run's event log and its recipe: what the harness hands the saver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EquityLog {
    pub recipe: EquityLogRecipe,
    /// `vike_simulator::encode_ledger_events`' bytes: the run's `vike_runs::EQUITY_LOG_FILE`.
    pub events: Vec<u8>,
}

/// FNV-1a 64 (hex) of a whole curve: every equity's bits, then every timestamp, little-endian.
pub fn curve_fnv(equity: &[f64], timestamp: &[i64]) -> String {
    let mut h = Fnv1a64::new();
    equity.iter().for_each(|e| h.write(&e.to_bits().to_le_bytes()));
    timestamp.iter().for_each(|t| h.write(&t.to_le_bytes()));
    format!("{:016x}", h.finish())
}

/// FNV-1a 64 (hex) of the bars a run marked: each bar's time and its close's bits, little-endian.
pub fn closes_fnv(bars: &[Bar]) -> String {
    let mut h = Fnv1a64::new();
    for b in bars {
        h.write(&b.timestamp.to_le_bytes());
        h.write(&b.close.to_bits().to_le_bytes());
    }
    format!("{:016x}", h.finish())
}

/// The saver's half of a recipe: `build` (the saving daemon's own), and each series' day stamps as `store` answers
/// them now, right after the run read them. Answers whether the store PINS every marked bar: every series' days are
/// stamped and none is provisional. When it does not, the saver keeps the curve and drops the log (this module's doc).
pub fn stamp(recipe: &mut EquityLogRecipe, store: &dyn HistoryStore, build: Option<&str>) -> bool {
    recipe.build = build.map(str::to_string);
    for s in &mut recipe.series {
        s.days = s.days_in(store).ok().flatten();
    }
    recipe
        .series
        .iter()
        .all(|s| s.days.as_ref().is_some_and(|days| days.iter().all(|d| !d.provisional)))
}

impl EquityLogSeries {
    /// The span of the bars the engine marked, both ends included.
    fn marked(&self) -> TimestampRange {
        TimestampRange::of(self.first_bar, self.last_bar)
    }

    /// The store's stamps of the marked bars' days, as it answers them now.
    fn days_in(&self, store: &dyn HistoryStore) -> Result<Option<Vec<BarDay>>, HistoryStoreError> {
        let (source, interval) = (self.source.as_deref(), Some(self.interval.as_str()));
        store.series_days(
            StoreKindId::Bar,
            &self.venue,
            source,
            &self.symbol,
            interval,
            self.marked(),
        )
    }

    /// The series' name in a refusal.
    fn named(&self) -> String {
        let lane = self.source.as_deref().map_or_else(String::new, |l| format!(" (lane {l})"));
        format!("{} {} {}{lane}", self.venue, self.symbol, self.interval)
    }
}

/// Each slot of `profile`'s run as a recipe records it, over `bars` (the run's slots, in the engine's order,
/// `ProfileBacktest::slot_series`'), or `None` for a run whose bars no store holds (`data.file`), whose window or
/// splits do not resolve, or with a slot of no bar.
fn series_of(profile: &ProfileBacktest, bars: &[&[Bar]]) -> Option<Vec<EquityLogSeries>> {
    let source = match profile.data.bar_source().ok()? {
        BarSource::Venue => None,
        BarSource::Lane(lane) => Some(lane.to_string()),
        BarSource::File(_) => return None,
    };
    let range = profile.range().ok()?;
    let series = profile.slot_series();
    if series.len() != bars.len() {
        return None;
    }
    series
        .iter()
        .zip(bars)
        .map(|(s, bars)| {
            let (first, last) = (bars.first()?.timestamp, bars.last()?.timestamp);
            let splits = match source {
                Some(_) => Vec::new(),
                None => profile.kept.splits_of(&s.venue, &s.symbol).ok()?,
            };
            Some(EquityLogSeries {
                venue: s.venue.clone(),
                symbol: s.symbol.clone(),
                source: source.clone(),
                interval: profile.data.interval.clone(),
                from: range.start,
                to: range.end,
                first_bar: first,
                last_bar: last,
                // `KeptRows::adjust_for_splits`' own rule: a split the window's first bar precedes moved its prices.
                splits: splits
                    .into_iter()
                    .filter(|(at, _)| first < *at)
                    .map(|(_, split)| split.clone())
                    .collect(),
                closes_fnv: closes_fnv(bars),
                days: None,
            })
        })
        .collect()
}

/// The equity `recipe` and `events` rebuild over `bars` (its slots, in order), one value a step; each step's time is
/// [`step_times`]'. Refused when the two do not fit.
fn rebuilt(
    recipe: &EquityLogRecipe,
    events: &[LedgerEvent],
    bars: &[&[Bar]],
) -> Result<Vec<f64>, String> {
    let decide = decide_mode(Some(recipe.decide.as_str())).map_err(|e| e.to_string())?;
    let n = recipe.symbols.len();
    if recipe.values.len() != n || bars.len() != n || bars.iter().any(|b| b.len() != recipe.steps) {
        return Err(format!(
            "its {n} slot(s) of {} steps do not fit {} series of {:?} bars",
            recipe.steps,
            bars.len(),
            bars.iter().map(|b| b.len()).collect::<Vec<_>>()
        ));
    }
    if let Some(e) = events.iter().find(|e| e.si as usize >= n) {
        return Err(format!("its log names slot {} of a run of {n}", e.si));
    }
    let start = LedgerStart::of(recipe.cash, recipe.values.clone(), &recipe.symbols, decide);
    Ok(rebuild_equity_curve(&start, events, recipe.steps, |si, step| bars[si][step].close))
}

/// Each step's time over `bars`: the first slot's bar time, as `StrategyEngine::run` stamps a step (none without a
/// slot).
fn step_times<'a>(bars: &[&'a [Bar]]) -> impl Iterator<Item = i64> + 'a {
    bars.first().copied().into_iter().flat_map(|b| b.iter().map(|b| b.timestamp))
}

/// **The log of a bar run that just ran**, or `None` (this module's doc lists when): `profile`'s run over `core`
/// (the engine after `StrategyEngine::run`), whose log `start_ledger_log` turned on and answered `start`, and whose
/// answer is `result`. ⚠ Checked before it is answered: the curve its recipe and log rebuild over the bars the
/// engine marked must be `result`'s to the bit, with its times.
pub(crate) fn record(
    profile: &ProfileBacktest,
    core: &OrderBrokerSimulator,
    start: LedgerStart,
    result: &TradingRecord,
) -> Option<EquityLog> {
    let events = core.ledger_events.as_deref()?;
    let steps = result.equity_curve.len();
    let bars: Vec<&[Bar]> = core.bars.iter().map(|b| &b[..]).collect();
    if steps == 0 {
        return None;
    }
    let series = series_of(profile, &bars)?;
    let recipe = EquityLogRecipe {
        format: LEDGER_EVENTS_FORMAT,
        build: None,
        steps,
        curve_fnv: curve_fnv(&result.equity_curve, &result.equity_timestamp),
        cash: start.cash,
        currency: ledger_label(profile).unwrap_or_else(|| vike_model::DEFAULT_CURRENCY.to_string()),
        decide: core.decide.word().to_string(),
        symbols: core.symbols.clone(),
        values: start.values,
        series,
    };
    // Compared in place, step by step: every equity's bits and every step's time, with no copy of either curve.
    let bits = |e: &f64| e.to_bits();
    match rebuilt(&recipe, events, &bars) {
        Ok(equity)
            if equity.iter().map(bits).eq(result.equity_curve.iter().map(bits))
                && step_times(&bars).eq(result.equity_timestamp.iter().copied()) =>
        {
            Some(EquityLog { recipe, events: encode_ledger_events(events) })
        }
        other => {
            let why = other.err().unwrap_or_else(|| "the rebuilt curve differs".to_string());
            tracing::warn!(
                "the event log of this bar run does not rebuild its curve ({why}): the run keeps its whole curve"
            );
            None
        }
    }
}

/// **A saved bar run's whole curve, rebuilt** from its `config.toml` (`profile_toml`), its `recipe` and its log's
/// bytes (`events`) over the bars `store` holds now: every step's equity and time, bit for bit the curve the run
/// answered, or the refusal in words. `build` is this daemon's own build. Never a curve from changed data:
///
/// - REFUSED: a log of another format or that does not read; a profile that names a file (`refuse_named_files`,
///   before anything is read); a profile that no longer resolves; slots, a window or a
///   source that differ from the run's; split rows that differ from the ones the run applied (decision 0289); bars
///   whose times or closes differ from the ones the run marked ([`closes_fnv`]), naming the days the store restamped;
///   a rebuilt curve that differs from the one the run answered ([`curve_fnv`]), naming both builds when they differ.
/// - SERVED with a `warn` line: a day the store restamped whose bars still read the closes the run marked (a rewrite of
///   the same rows), and a build other than the saving one whose fold rebuilds the very curve the run answered.
pub fn rebuild_curve(
    profile_toml: &str,
    recipe: &EquityLogRecipe,
    events: &[u8],
    store: &Arc<dyn HistoryStore + Send + Sync>,
    build: Option<&str>,
) -> Result<(Vec<f64>, Vec<i64>), String> {
    if recipe.format != LEDGER_EVENTS_FORMAT {
        return Err(format!(
            "its event log is format {} and this build reads format {LEDGER_EVENTS_FORMAT}",
            recipe.format
        ));
    }
    let events = decode_ledger_events(events)?;
    let mut profile = ProfileBacktest::from_toml_str(profile_toml)
        .map_err(|e| format!("its config.toml no longer reads: {e}"))?;
    refuse_named_files(&profile)?;
    profile
        .read_kept_rows(store.as_ref())
        .map_err(|e| format!("its profile is refused now: {e}"))?;
    // The days the store restamped since the save, read BEFORE the bars, as the bar cache reads them.
    let mut restamped = Vec::new();
    for s in &recipe.series {
        let Some(then) = &s.days else { continue };
        let now = s.days_in(store.as_ref());
        if let Ok(Some(now)) = now {
            let moved: BTreeSet<i64> = now
                .iter()
                .filter(|day| !then.contains(day))
                .chain(then.iter().filter(|day| !now.contains(day)))
                .map(|day| day.day)
                .collect();
            if let Some(first) = moved.first() {
                let since = vike_model::time::utc_rfc3339(first * vike_model::SECONDS_PER_DAY);
                restamped.push(format!("{} on {} day(s) from {since}", s.named(), moved.len()));
            }
        }
    }
    // The slots in the order the run's engine held them, as `record` wrote them (a portfolio's by mount order).
    let series = profile.slot_series();
    let slots: Vec<String> = series.iter().map(|s| s.leg_name()).collect();
    if slots != recipe.symbols || series.len() != recipe.series.len() {
        return Err(format!(
            "its slots are {slots:?} now, and the run's were {:?}",
            recipe.symbols
        ));
    }
    let range = profile.range().map_err(|e| e.to_string())?;
    let mut loaded = Vec::with_capacity(series.len());
    for (s, then) in series.iter().zip(&recipe.series) {
        let interval = &profile.data.interval;
        let mut bars = load_series_bars(
            &profile,
            store,
            &s.venue,
            &s.symbol,
            interval,
            range,
            &mut Vec::new(),
        )
        .map_err(|e| format!("its bars no longer load: {e}"))?;
        // The window as the store holds it now, cut to the bars the run marked: what a live series collected around
        // them since (an open end, a window reaching past the data, a backfill before it) is no change to the run.
        bars.retain(|b| (then.first_bar..=then.last_bar).contains(&b.timestamp));
        if bars.is_empty() {
            return Err(format!(
                "the store holds none of the bars of {} the run marked",
                then.named()
            ));
        }
        loaded.push(bars);
    }
    let bars: Vec<&[Bar]> = loaded.iter().map(Vec::as_slice).collect();
    let now = series_of(&profile, &bars)
        .ok_or("its bars are no store's (a file, or a window that no longer resolves)")?;
    let days = if restamped.is_empty() {
        String::new()
    } else {
        format!("; the store restamped {}", restamped.join(", "))
    };
    for (then, now) in recipe.series.iter().zip(&now) {
        let read = |s: &EquityLogSeries| {
            let marked = (s.first_bar, s.last_bar);
            (
                s.venue.clone(),
                s.symbol.clone(),
                s.source.clone(),
                s.interval.clone(),
                s.from,
                s.to,
                marked,
            )
        };
        if read(then) != read(now) {
            return Err(format!(
                "it read {} and its profile reads {} now",
                then.named(),
                now.named()
            ));
        }
        if then.splits != now.splits {
            return Err(format!(
                "the share splits of {} changed since the run: it applied {:?}, the store holds {:?} now",
                then.named(),
                then.splits,
                now.splits
            ));
        }
        if then.closes_fnv != now.closes_fnv {
            return Err(format!(
                "the stored bars of {} are not the ones the run marked (their times and closes hash {}, the run's {}){days}",
                then.named(),
                now.closes_fnv,
                then.closes_fnv
            ));
        }
    }
    if !restamped.is_empty() {
        tracing::warn!(
            "the curve rebuilt over bars that read the closes the run marked, though the store restamped {}",
            restamped.join(", ")
        );
    }
    let equity = rebuilt(recipe, &events, &bars)?;
    let timestamp: Vec<i64> = step_times(&bars).collect();
    let saved_by = recipe.build.as_deref().unwrap_or("an unnamed build");
    let this = build.unwrap_or("an unnamed build");
    if curve_fnv(&equity, &timestamp) != recipe.curve_fnv {
        return Err(if recipe.build.as_deref() == build {
            "the curve its log rebuilds differs from the one the run answered".to_string()
        } else {
            format!(
                "it was saved by build {saved_by}, and this build ({this}) folds its log to another curve"
            )
        });
    }
    if recipe.build.as_deref() != build {
        tracing::warn!(
            "a run saved by build {saved_by} was rebuilt by build {this}: its curve came back to the bit"
        );
    }
    Ok((equity, timestamp))
}

/// **A profile key that names a file, refused before a rebuild reads anything.** A rebuild runs in the serving
/// daemon over a profile a SANDBOXED child wrote (a saved run's `config.toml`, a held run's profile text), so a key
/// naming a file would make the daemon open any path on its box and say in its answer whether it exists. These are the
/// two keys a run opens as a file: `data.file` (the field `ProfileData::bar_source` reads as [`BarSource::File`]) and
/// `[engine.resolution] path`, the two the backtesthub's wire door refuses (`vike-backtesthub`'s `wire_profile`). No
/// saved run needs one: a run over a file's bars leaves no log ([`series_of`]), and a profile that crossed a wire was
/// refused both. `strategy.file` and a model param are not read here (a rebuild builds no strategy); every other key
/// is a store name, a word or a time.
fn refuse_named_files(profile: &ProfileBacktest) -> Result<(), String> {
    let refused = |key: &str, path: &str| {
        format!(
            "its profile names the file {path:?} ({key}), and a curve is never rebuilt from a file: \
             this daemon opens no file a run's profile names"
        )
    };
    if let Some(path) = profile.data.file.as_deref() {
        return Err(refused("`data.file`", path));
    }
    if let Some(path) = profile.engine.resolution.as_ref().and_then(|r| r.path.as_deref()) {
        return Err(refused("`[engine.resolution] path`", path));
    }
    Ok(())
}

#[cfg(test)]
#[path = "equity_log_tests.rs"]
mod equity_log_tests;
