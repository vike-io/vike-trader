//! A run's INPUTS (decision 0271): the stored series its strategy reads beside its own bars, by name
//! (`vike_model::Strategy::inputs`), planned, read and judged before the run reads a price.
//!
//! # The name
//!
//! An input is named by the series it reads, in the CLI's series spelling: `VENUE:SYMBOL:INTERVAL` for
//! a venue's own candles (what `vike-cli data history get binance:BTCUSDT:4h` reads), and the `source=`
//! lane before it, `LANE:VENUE:SYMBOL:INTERVAL`, for a lane (what `data history get VENUE:SYMBOL:INTERVAL
//! --lane LANE` reads): a user's CSV import is `user-<label>:VENUE:SYMBOL:INTERVAL`. The venue comes
//! first (after the lane) and the interval last, and the symbol between holds at most one `:` (a
//! hyperliquid builder-dex coin, `hyperliquid:XYZ:TSLAUSDC.P:1h`); a first part that is no venue opens a
//! lane. The symbol of a venue's own candles is its USER name in any ASCII case, read as its STORE name
//! (decision 0290, `parse_series_name`): `bybit:btcusdt.p:1h` reads `BTCUSDT.P`, and a name the venue's
//! kept list names no instrument by is refused before the run. The lane is one the store declares or a
//! user's (`vike_data::store::store_kind::refuse_an_unknown_lane`), the interval one the bar vocabulary
//! reads (`vike_marketdata::interval::interval_ms`), because the as-of rule needs a bar's length. One
//! part, `INTERVAL` alone, is the run's OWN symbol at that bar length (`"1d"`: the daily bars of the
//! symbol it trades, in its `data.source` lane when it reads one), so a strategy written for any market
//! reads its own higher or lower timeframe; a run of several symbols, of a cross-venue series list or
//! of a file has no own series, and is refused.
//!
//! # The history read
//!
//! An input whose bars are LONGER than the run's own (a higher timeframe; on the tick lane, every
//! input) is read with its whole stored history before the window, as a script platform computes a
//! higher-timeframe expression over the series' own history: a daily RSI on the first bar is the one
//! the venue's daily bars make, not one that starts at the window. One whose bars are as long or
//! shorter (the minutes inside each bar, another symbol's bars) is read from one of its bars before
//! the window, so its history is as long as the run's own warm-up. The strategy reads it through
//! `vike_model::OrderBroker::input_bars`.
//!
//! The name the strategy's code writes works on its own. A profile's `[inputs]` table only REMAPS:
//! `"<the name the code writes>" = "<the series to read instead>"`, both in this grammar
//! ([`resolve_inputs`]); a row naming no input the strategy declares is refused, since it would remap
//! nothing and look as though it did.
//!
//! # The checks, before any price is read
//!
//! [`super::plan_data`] lists each input as a [`super::DataPlanSeriesRole::Input`] row, from the store's
//! manifest like every planned series (`data.explain`). The run reads each input's bars and refuses,
//! by name and with the way to fill it, one the store does not hold or holds with a hole inside the
//! window ([`input_shortfall`], judged on the bars themselves, so it sees a missing hour the manifest's
//! day index cannot). The tolerance is `data.max_gap` when `data.require_coverage` arms it, else none;
//! `data.on_gap` does not soften it, because a strategy reading an input that is not there reads
//! nothing where it expects a value. That is the rule of a venue's own candles and of a lane the store
//! declares; a user's lane is judged by the next section's.
//!
//! # A user's lane may be sparse
//!
//! An input read from a USER's lane (`user-<label>`, a file the operator imported; decision 0271's
//! amendment of 2026-11-18) may hold a row only at the bars that carry a value, so an event series is
//! stored without the rows of zeros between its events. A bar it does not hold is no hole: each event
//! reads the latest row closed by then, as for every input (`crates/vike-simulator/src/engine/sim_broker/inputs.rs`'s
//! `InputView`), so between two rows the older one is read again. A row keeps its own `Bar::timestamp`
//! (when it opened), and a strategy tells a fresh value from an old one by comparing it to the current
//! bar's: on a lane of the run's own interval, `input.timestamp == bar.timestamp` is the row of this very
//! bar, anything earlier the last row before it. Still refused: a lane with no row inside the window
//! ([`SeriesShortfall::Everything`]: an empty lane, or one imported for another stretch of time), a name
//! the grammar refuses (an unknown lane), and a read that fails. A first row after the window opens or a
//! last row before it ends refuses nothing either (a quiet start or end reads as a quiet middle does) but
//! is SAID, a line of the run's data warnings each (`lane_edge_line`), because past the last row the
//! strategy reads that row to the end. A span its trading calendar calls closed time, or within an armed
//! `data.max_gap`, is not said; `data.explain` still lists every span the store lacks. It is read from
//! where a dense lane is (the module's "The history read"), so a row older than that is not carried into
//! the window: before the first row read closes, it reads `None`.
//!
//! # Closed market time is not a hole
//!
//! A span is judged against the input's TRADING CALENDAR when its instrument has one with closed
//! time: the row the live door's closed-market note reads, for a symbol whose class its KEPT row states
//! (`vike_catalog::session_class_of` over the profile's kept rows, `ProfileBacktest::kept`: an FX pair
//! on an FX-week broker, an alpaca share; a symbol no kept row states is UNKNOWN, never read off its
//! name), through
//! `vike_catalog::session_calendar_for`, in civil time
//! (`vike_venues::SessionCalendar::is_open_civil`). Such a span is not a hole when the market was
//! closed throughout it (a weekend, a night), or when the market was open in it for exactly ONE
//! WHOLE session of at most a day, from its open to its close (a holiday: the calendar is a weekly
//! one and lists none). A part of a session, two sessions, or a longer stretch of an FX week is
//! still a hole. ⚠ On DAILY bars one missing trading day reads exactly as a holiday does and passes
//! too: nothing here can tell the two apart until a holiday table exists
//! (`vike_venues::session`'s module doc, "Deliberately not built").
//!
//! Every other input keeps the plain rule, every span counting by its length: a market open every
//! minute (a crypto venue), and one whose class is unknown (an IBKR stock, an unknown or user venue,
//! and every input of a run handed no kept rows), whose refusal says that its closed hours count as missing. `data.max_gap` keeps its
//! meaning on both: a span that is not closed time counts by its wall-clock length.
//!
//! A door that does not plan inputs (a walk-forward, a run over bars it was handed)
//! refuses a strategy that declares one ([`refuse_unplanned_inputs`]) rather than run it reading
//! nothing.

use vike_data::store::series::{SeriesOwn, parse_input_name_for};
use vike_data::store::store_kind::USER_LANE_PREFIX;
use vike_data::{
    HistoryStore, PlannedRead, SeriesId, SeriesMissingSpan, SeriesShortfall, TimestampRange,
};
use vike_marketdata::Bar;
use vike_model::Instrument;
use vike_simulator::{InputSeries, Inputs};
use vike_venues::SessionCalendar;

use super::verdict::fetch_hint;
use super::{human_ms, render_window};
use crate::HarnessError;
use crate::run::profile::ProfileBacktest;
use crate::run::refusals::ProfileRefusal;
use vike_model::run_request::replay::ReplayKind;

/// One input of a run: the name the strategy reads it by and the series that name reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPlanInput {
    /// As the strategy spells it: what `vike_model::OrderBroker::input` is asked.
    pub name: String,
    /// The series read, `kind=bar` at its interval and in its lane when the spelling names one: the
    /// strategy's own name, or the `[inputs]` row's value when the profile remaps it.
    pub id: SeriesId,
    /// One bar's length in ms: a bar of this input closes `interval_ms` after it opens.
    pub interval_ms: i64,
}

impl DataPlanInput {
    /// The series read, in the input grammar.
    pub fn spec(&self) -> String {
        series_spec(&self.id)
    }
}

/// A `kind=bar` series in the input grammar (`LANE:VENUE:SYMBOL:INTERVAL`, or three parts without a
/// lane): an input's and a conversion pair's words.
pub(super) fn series_spec(id: &SeriesId) -> String {
    let interval = id.interval.as_deref().unwrap_or_default();
    match id.source.as_deref() {
        Some(lane) => format!("{lane}:{}:{}:{interval}", id.venue, id.symbol),
        None => format!("{}:{}:{interval}", id.venue, id.symbol),
    }
}

/// The run's OWN series, which a one-part input name (`"1d"`) reads at that bar length
/// (`vike_data::store::series::parse_input_name_for`): the single-venue form's one symbol, in its
/// `data.source` lane when it reads one. A run of several symbols, of a cross-venue list or of a file has
/// none, and the error says why.
pub(crate) fn own_series(profile: &ProfileBacktest) -> Result<SeriesOwn<'_>, String> {
    let data = &profile.data;
    let (Some(venue), [symbol]) = (data.venue.as_deref(), data.symbols.as_slice()) else {
        return Err(
            "this run trades no one symbol of one venue (data.venue with one data.symbols  entry)"
                .to_string(),
        );
    };
    if data.file.is_some() {
        return Err("this run reads its bars from a file (data.file), which holds one bar length"
            .to_string());
    }
    Ok(SeriesOwn { venue, symbol, lane: data.source.as_deref() })
}

/// **A name in the input grammar, parsed and read as decision 0290 reads a name**: `parse_input_name_for`
/// against the run's own series ([`own_series`]), then the symbol of a venue's own candles spelled in full
/// becomes its STORE name (`crate::run::KeptRows::store_name` over the profile's kept rows: `bybit:btcusdt.p:1h`
/// reads `BTCUSDT.P`, `hyperliquid:XYZ:TSLAUSDC.P:1h` reads `XYZ-TSLAUSDC.P`). A one-part name is the run's own
/// series, already its store name (`ProfileBacktest::read_kept_rows`), and a lane's series is the operator's own
/// import, read as it was filed. The one reading of an input and of a conversion pair (`resolve_conversions`).
///
/// # Errors
/// The grammar's sentence, or the translator's for a name its venue's kept list names no instrument by.
pub(crate) fn parse_series_name(
    profile: &ProfileBacktest,
    name: &str,
) -> Result<(SeriesId, i64), String> {
    let (mut id, interval_ms) = parse_input_name_for(name, own_series(profile))?;
    if name.contains(':') && id.source.is_none() {
        id.symbol = profile.kept.store_name(&id.venue, &id.symbol)?;
    }
    Ok((id, interval_ms))
}

/// The inputs a run reads: each name the strategy declares, through the profile's `[inputs]` remap,
/// parsed and read as decision 0290 reads a name (`parse_series_name`; a one-part name is the run's own
/// symbol at that bar length: `own_series`). A repeated name is read once.
///
/// # Errors
/// [`HarnessError::Validation`] for an `[inputs]` row naming no declared input, or a name (the
/// strategy's or the row's) the grammar refuses.
pub fn resolve_inputs(
    profile: &ProfileBacktest,
    declared: &[String],
) -> Result<Vec<DataPlanInput>, HarnessError> {
    if let Some(key) = profile.inputs.keys().find(|k| !declared.iter().any(|d| d == *k)) {
        return Err(HarnessError::from(ProfileRefusal::InputsKeyNotDeclared {
            key: key.clone(),
            reads: reads(declared),
        }));
    }
    let mut out: Vec<DataPlanInput> = Vec::with_capacity(declared.len());
    for name in declared {
        if out.iter().any(|i| i.name == *name) {
            continue;
        }
        let read = profile.inputs.get(name).unwrap_or(name);
        let (id, interval_ms) = parse_series_name(profile, read).map_err(|e| {
            let whose = if read == name { "the strategy reads" } else { "[inputs] maps it to" };
            HarnessError::from(ProfileRefusal::InputsNameRefused {
                whose: whose.to_string(),
                read: read.clone(),
                e,
            })
        })?;
        out.push(DataPlanInput { name: name.clone(), id, interval_ms });
    }
    Ok(out)
}

/// The inputs `profile`'s strategy declares, resolved: what a door that plans without running
/// (`data.explain`) hands [`super::plan_data`]. It builds the strategy to ask it, and reads no data.
///
/// # Errors
/// The strategy's own build refusal, or [`resolve_inputs`]'.
pub fn declared_inputs(profile: &ProfileBacktest) -> Result<Vec<DataPlanInput>, HarnessError> {
    resolve_inputs(profile, &profile.declared_input_names()?)
}

/// **The venues whose kept rows a run of `profile` reads**, or `None` for every venue's
/// (`ProfileBacktest::read_kept_rows`): its series, each input the strategy declares (through `[inputs]`, read as
/// [`resolve_inputs`] reads it), each `engine.conversion` pair and its `[data.options]` chain. Every kept lookup a
/// run makes is of one of them, so their rows answer what the whole lists answer. `None` when a param table may
/// change what the strategy reads (`[paramscan]`, `[walkforward]`, `[permutation]`: a point builds it again), or when
/// the strategy cannot be built or a name read here (the run then refuses it in its own words).
pub fn kept_venues(profile: &ProfileBacktest) -> Option<Vec<String>> {
    if profile.paramscan.is_some() || profile.walkforward.is_some() || profile.permutation.is_some()
    {
        return None;
    }
    let declared = profile.declared_input_names().ok()?;
    let names = declared
        .iter()
        .map(|name| profile.inputs.get(name).unwrap_or(name))
        .chain(profile.engine.conversion.values());
    let mut venues: Vec<String> =
        profile.data.resolved_series().into_iter().map(|s| s.venue).collect();
    for name in names {
        venues.push(parse_input_name_for(name, own_series(profile)).ok()?.0.venue);
    }
    venues.extend(profile.data.options.as_ref().map(|options| options.venue.clone()));
    venues.sort();
    venues.dedup();
    Some(venues)
}

/// `"the strategy reads A, B"`, or that it reads none.
fn reads(declared: &[String]) -> String {
    if declared.is_empty() {
        return "the strategy reads no input".to_string();
    }
    let names: Vec<String> = declared.iter().map(|n| format!("{n:?}")).collect();
    format!("the strategy reads {}", names.join(", "))
}

/// Refuse a strategy that reads an input at a door that does not load one: `door` names it ("a
/// walk-forward"). `declared` is the strategy's `vike_model::Strategy::inputs`; none passes.
///
/// # Errors
/// [`HarnessError::Data`], naming the inputs and the door that does load them.
pub fn refuse_unplanned_inputs(declared: &[String], door: &str) -> Result<(), HarnessError> {
    if declared.is_empty() {
        return Ok(());
    }
    Err(HarnessError::Data(format!(
        "{door} does not load a strategy's inputs yet, and {}: it would read nothing where a profile \
         backtest reads the series (decision 0271). Nothing ran. Run it as a profile backtest, which \
         plans, reads and checks each input before it starts",
        reads(declared)
    )))
}

/// What of the window `start ..= end` an input's `bars` do not cover, each bar covering its own
/// interval (`ts ..= ts + interval_ms - 1`), in time order: a [`SeriesShortfall::Leading`] span before the
/// first bar that reaches the window, an [`SeriesShortfall::Interior`] span between two bars further apart
/// than one interval, a [`SeriesShortfall::Trailing`] span after the last bar's end, or one
/// [`SeriesShortfall::Everything`] span when no bar reaches the window. An unbounded side is never short.
pub fn input_shortfall(
    bars: &[Bar],
    interval_ms: i64,
    start: Option<i64>,
    end: Option<i64>,
) -> Vec<SeriesMissingSpan> {
    let (lo, hi) = (start.unwrap_or(i64::MIN), end.unwrap_or(i64::MAX));
    let reaching: Vec<(i64, i64)> = bars
        .iter()
        .map(|b| (b.timestamp, b.timestamp.saturating_add(interval_ms).saturating_sub(1)))
        .filter(|(from, to)| *to >= lo && *from <= hi)
        .collect();
    let Some(first) = reaching.first() else {
        return vec![SeriesMissingSpan {
            start_ms: lo,
            end_ms: hi,
            kind: SeriesShortfall::Everything,
        }];
    };
    let mut out = Vec::new();
    // The first instant not yet covered.
    let mut next = start.unwrap_or(first.0);
    let mut kind = SeriesShortfall::Leading;
    for (from, to) in reaching {
        if from > next {
            out.push(SeriesMissingSpan { start_ms: next, end_ms: from - 1, kind });
        }
        kind = SeriesShortfall::Interior;
        next = next.max(to.saturating_add(1));
    }
    if end.is_some() && next <= hi {
        out.push(SeriesMissingSpan { start_ms: next, end_ms: hi, kind: SeriesShortfall::Trailing });
    }
    out
}

/// **The trading calendar an input's coverage is judged against**, when its KEPT row (among `kept`, the
/// profile's `kept` rows) states a class with closed time (the module doc's "Closed market time is not a
/// hole"): `vike_catalog::trading_calendar`, the rule a live mount's input freshness reads too. `None` for a
/// market open every minute and for one whose class is unknown: both keep the plain rule.
fn trading_calendar(id: &SeriesId, kept: &[Instrument]) -> Option<SessionCalendar> {
    vike_catalog::trading_calendar(
        &id.venue,
        vike_catalog::kept_row_of_store_name(kept, &id.venue, &id.symbol).ok(),
    )
}

/// **Whether a span missing from an input is a HOLE.** A span no bar reaches the window over
/// ([`SeriesShortfall::Everything`]) always is; any other is not when it is no longer than `tolerance`, nor
/// when the input's `calendar` ([`trading_calendar`]) says it is closed time or one holiday
/// (`vike_venues::SessionCalendar::closed_or_one_session`, the rule the datahub's stored gaps use too).
fn is_hole(m: &SeriesMissingSpan, calendar: Option<&SessionCalendar>, tolerance: i64) -> bool {
    if m.kind == SeriesShortfall::Everything {
        return true;
    }
    if m.len_ms() <= tolerance {
        return false;
    }
    calendar.is_none_or(|cal| !cal.closed_or_one_session(m.start_ms, m.end_ms))
}

/// The data warning of a user's lane that starts after the window opens ([`SeriesShortfall::Leading`]) or
/// ends before it does ([`SeriesShortfall::Trailing`]), `head` naming the input; `None` for any other span.
/// The run goes on: a sparse lane may be quiet at either end, and the line says what the strategy reads there.
fn lane_edge_line(head: &str, m: &SeriesMissingSpan) -> Option<String> {
    let (edge, reads) = match m.kind {
        SeriesShortfall::Leading => {
            ("first row opens after the window does", "no value until it closes")
        }
        SeriesShortfall::Trailing => {
            ("last row closes before the window ends", "that row to the end")
        }
        SeriesShortfall::Interior | SeriesShortfall::Everything => return None,
    };
    Some(format!(
        "{head}: the user's lane's {edge}, so the strategy reads {reads}: no row over {} ({})",
        render_window(Some(m.start_ms), Some(m.end_ms)),
        human_ms(m.len_ms())
    ))
}

/// **Read and judge every input of a run, before its price lane is read**, into what the simulator's
/// as-of view holds (`vike_simulator::StrategyEngineConfig::inputs`): `declared` is the strategy's
/// `vike_model::Strategy::inputs`, resolved through `[inputs]` ([`resolve_inputs`]).
///
/// Each input's bars are read up to the last that closes by the run's last decision ([`read_end`]), from its
/// first stored bar when its bars are longer than the run's own and from one of
/// ITS bars before the window otherwise (the bar that closes as the window opens is the first value a
/// decision can see): the module doc's "The history read". Through the store like the run's own bars:
/// the sourceless series, or its lane by `load_bars_sourced_capped`. Bars read past what a decision may
/// see are never shown (the as-of view is the guard, not the range).
///
/// No input and no `[inputs]` row reads nothing and answers [`Inputs`] with no series and the bar run's
/// own bar length (where its forecasts are stamped), so a strategy that declares none runs byte-identically.
///
/// An input read from the venue's own candles (no lane) is split-adjusted as the run's own bars are
/// (`crate::run::KeptRows::adjust_for_splits`), each split's report line added to `data_warnings` (the
/// run's, `TradingRecord::data_warnings`) once. A user's lane whose first row opens after the window
/// does, or whose last row closes before it ends, adds a line saying so (`lane_edge_line`).
///
/// # Errors
/// [`resolve_inputs`]' refusals; [`HarnessError::Data`] for a read that failed, and for every input
/// the store does not cover over the window, a user's lane only when it holds no row inside it (all of
/// them in one sentence, each with its way out).
pub(crate) fn load_inputs(
    profile: &ProfileBacktest,
    declared: &[String],
    store: &(dyn HistoryStore + Send + Sync),
    data_warnings: &mut Vec<String>,
) -> Result<Inputs, HarnessError> {
    if declared.is_empty() && profile.inputs.is_empty() {
        // No series, but the run's own bar length still rides: a bar step's forecasts are stamped at its
        // close (decision 0271), with or without inputs. Nothing else reads it without a series.
        let bar_ms = match profile.data.kind {
            ReplayKind::Bar => vike_marketdata::interval::interval_ms(&profile.data.interval),
            ReplayKind::Tick => None,
        };
        return Ok(Inputs { bar_ms: bar_ms.unwrap_or(0), series: Vec::new() });
    }
    let inputs = resolve_inputs(profile, declared)?;
    let range = profile.range()?;
    let tolerance = hole_tolerance(profile)?;
    let bar_ms = run_bar_ms(profile)?;
    let mut series = Vec::with_capacity(inputs.len());
    let mut refused: Vec<String> = Vec::new();
    for input in &inputs {
        let id = &input.id;
        let mut bars = read_series(store, id, input_window(input, range, bar_ms)).map_err(|e| {
            HarnessError::Data(format!(
                "the input {:?} ({}) could not be read: {e}. Nothing ran.",
                input.name,
                input.spec()
            ))
        })?;
        if id.source.is_none() {
            for line in profile.kept.adjust_for_splits(&id.venue, &id.symbol, &mut bars)? {
                if !data_warnings.contains(&line) {
                    data_warnings.push(line);
                }
            }
        }
        let (mut spans, calendar) = holes(
            &bars,
            id,
            &profile.kept.rows,
            input.interval_ms,
            range.start,
            range.end,
            tolerance,
        );
        let spec = input.spec();
        let read = if spec == input.name { String::new() } else { format!(" (read as {spec:?})") };
        let head = format!("input {:?}{read}", input.name);
        // A user's lane may be sparse (the module doc's "A user's lane may be sparse"): only a lane
        // with no row inside the window is refused, and one that starts late or ends early is said.
        if id.source.as_deref().is_some_and(|lane| lane.starts_with(USER_LANE_PREFIX)) {
            if !spans.iter().any(|m| m.kind == SeriesShortfall::Everything) {
                data_warnings.extend(spans.iter().filter_map(|m| lane_edge_line(&head, m)));
            }
            spans.retain(|m| m.kind == SeriesShortfall::Everything);
        }
        if spans.is_empty() {
            series.push(InputSeries {
                name: input.name.clone(),
                interval_ms: input.interval_ms,
                bars,
            });
        } else {
            refused.extend(uncovered_lines(
                &head,
                id,
                &spans,
                calendar,
                &profile.kept.rows,
                ("the input", " in [inputs]"),
            ));
        }
    }
    if !refused.is_empty() {
        return Err(HarnessError::Data(format!(
            "the strategy reads an input the store does not cover over this run's window, so the \
             run is refused before any price was read. Nothing ran.\n{}",
            refused.join("\n")
        )));
    }
    Ok(Inputs { bar_ms, series })
}

/// The tolerance a series read beside the price lane is judged with: `data.max_gap` when
/// `data.require_coverage` arms it, else none (`data.on_gap` does not soften it).
pub(super) fn hole_tolerance(profile: &ProfileBacktest) -> Result<i64, HarnessError> {
    let gate = profile.data.coverage_gate()?;
    Ok(if gate.armed { gate.max_gap_ms.unwrap_or(0) } else { 0 })
}

/// The run's own bar length, as of whose close an input is read: `data.interval`'s on the bar lane, 0 on the tick lane.
fn run_bar_ms(profile: &ProfileBacktest) -> Result<i64, HarnessError> {
    match profile.data.kind {
        ReplayKind::Bar => vike_marketdata::interval::interval_ms(&profile.data.interval)
            .ok_or_else(|| {
                HarnessError::Data(format!(
                    "data.interval {:?} is not a bar length, so an input cannot be read as of a \
                     bar's close",
                    profile.data.interval
                ))
            }),
        ReplayKind::Tick => Ok(0),
    }
}

/// The window `input` is read over (the module doc's "The history read"), to [`read_end`].
fn input_window(input: &DataPlanInput, range: TimestampRange, bar_ms: i64) -> TimestampRange {
    // A higher timeframe reads its whole history; the tick lane's bar_ms is 0, so every input does.
    let history = input.interval_ms > bar_ms;
    TimestampRange {
        start: range.start.filter(|_| !history).map(|s| s.saturating_sub(input.interval_ms)),
        end: read_end(range.end, bar_ms, input.interval_ms),
    }
}

/// **The last `ts` a series of `interval_ms` bars beside the price lane (an input, a conversion pair) is read to**:
/// every bar that CLOSES by the run's last moment (`vike_marketdata::Bar::closed_at`; the run's last bar opens by
/// `end` and closes `bar_ms` later), and never short of `end`, the window its holes are judged over. A later bar
/// closes after the run ended and no decision or rate ever sees it, so a window that ends yesterday reads no bar of
/// today's and its run stays keyable (`vike-backtesthub`'s witness refuses a read that reaches today).
/// shortcut: a series of bars SHORTER than the run's still reads up to `bar_ms - interval_ms` past `end`, as the run's
/// last bar is not known before its prices are read; bound it by that bar if such a run must be answered from saved.
pub(super) fn read_end(end: Option<i64>, bar_ms: i64, interval_ms: i64) -> Option<i64> {
    end.map(|e| e.max(e.saturating_add(bar_ms).saturating_sub(interval_ms)))
}

/// The reads [`load_inputs`] makes, named ahead (`super::planned_reads`): none when it would refuse before reading.
pub(super) fn input_reads(profile: &ProfileBacktest, declared: &[String]) -> Vec<PlannedRead> {
    if declared.is_empty() && profile.inputs.is_empty() {
        return Vec::new();
    }
    let (Ok(inputs), Ok(range), Ok(bar_ms)) =
        (resolve_inputs(profile, declared), profile.range(), run_bar_ms(profile))
    else {
        return Vec::new();
    };
    inputs.iter().map(|input| series_read(&input.id, input_window(input, range, bar_ms))).collect()
}

/// [`read_series`]' read, named ahead (`vike_data::HistoryStore::prefetch`).
pub(super) fn series_read(id: &SeriesId, window: TimestampRange) -> PlannedRead {
    let (venue, symbol) = (id.venue.clone(), id.symbol.clone());
    let interval = id.interval.clone().unwrap_or_default();
    match id.source.clone() {
        None => PlannedRead::Bars { venue, symbol, interval, range: window },
        Some(source) => PlannedRead::BarsSourcedCapped {
            venue,
            source,
            symbol,
            interval,
            range: window,
            budget: None,
        },
    }
}

/// A `kind=bar` series over `window`, through the store like the run's own bars: the sourceless
/// series, or its lane by `load_bars_sourced_capped`.
pub(super) fn read_series(
    store: &(dyn HistoryStore + Send + Sync),
    id: &SeriesId,
    window: TimestampRange,
) -> Result<Vec<Bar>, vike_data::HistoryStoreError> {
    let interval = id.interval.as_deref().unwrap_or_default();
    match id.source.as_deref() {
        None => store.load_bars(&id.venue, &id.symbol, interval, window),
        Some(lane) => {
            store.load_bars_sourced_capped(&id.venue, lane, &id.symbol, interval, window, None)
        }
    }
}

/// The HOLES of `bars` (each covering its own `interval_ms`) inside `start ..= end`: the spans
/// [`input_shortfall`] finds that [`is_hole`] keeps, judged by the series' trading calendar where its
/// KEPT row (among `kept`, the profile's `kept` rows) states one. The second answer says whether it had one.
pub(super) fn holes(
    bars: &[Bar],
    id: &SeriesId,
    kept: &[Instrument],
    interval_ms: i64,
    start: Option<i64>,
    end: Option<i64>,
    tolerance: i64,
) -> (Vec<SeriesMissingSpan>, bool) {
    let calendar = trading_calendar(id, kept);
    let spans = input_shortfall(bars, interval_ms, start, end)
        .into_iter()
        .filter(|m| is_hole(m, calendar.as_ref(), tolerance))
        .collect();
    (spans, calendar.is_some())
}

/// The refusal lines of one series read beside the price lane (`head` names it: `input "…"`, or a
/// conversion pair): what is missing of it, the rule its holes were judged by when it is not the plain
/// one (`calendar`: it has a trading calendar; none and not a 24/7 market by its `kept` row:
/// `vike_catalog::trades_every_day`), and how to fill it. `remap` is the reader's spelling of "point
/// it at another series" around the series: `("the input", " in [inputs]")`.
pub(super) fn uncovered_lines(
    head: &str,
    id: &SeriesId,
    spans: &[SeriesMissingSpan],
    calendar: bool,
    kept: &[Instrument],
    remap: (&str, &str),
) -> Vec<String> {
    let interval = id.interval.as_deref().unwrap_or_default();
    let (point, place) = remap;
    let lane = id.source.as_deref().map(|l| format!(" in the lane `{l}`")).unwrap_or_default();
    let what = if spans.iter().any(|m| m.kind == SeriesShortfall::Everything) {
        "holds no bar of"
    } else {
        "does not cover the window for"
    };
    let mut out =
        vec![format!("{head}: the store {what} {} {} {interval}{lane}", id.venue, id.symbol)];
    for m in spans.iter().filter(|m| m.kind != SeriesShortfall::Everything) {
        out.push(format!(
            "    {} {} ({})",
            m.kind.as_str(),
            render_window(Some(m.start_ms), Some(m.end_ms)),
            human_ms(m.len_ms())
        ));
    }
    if spans.iter().any(|m| m.kind != SeriesShortfall::Everything) {
        if calendar {
            out.push(
                "    judged by its market's trading calendar: the hours it is closed are not \
                 missing, nor one whole session missing alone (a holiday)"
                    .to_string(),
            );
        } else if !vike_catalog::trades_every_day(
            &id.venue,
            vike_catalog::kept_row_of_store_name(kept, &id.venue, &id.symbol).ok(),
        ) {
            out.push(format!(
                "    no trading calendar is known for {} {}, so the hours its market is closed (a \
                 weekend, a holiday) count as missing too: data.require_coverage with data.max_gap \
                 accepts a gap up to that length",
                id.venue, id.symbol
            ));
        }
    }
    let import = |label: &str| {
        format!(
            "Studio → Data → Import CSV, or `vike-cli data history import csv-bars <FILE> --venue {} \
             --symbol {} --interval {interval} --lane {USER_LANE_PREFIX}{label}`",
            id.venue, id.symbol
        )
    };
    let user = id.source.as_deref().and_then(|l| l.strip_prefix(USER_LANE_PREFIX));
    out.push(match (id.source.as_deref(), user) {
        (Some(_), Some(label)) => format!("    import it: {}", import(label)),
        (Some(lane), None) => format!(
            "    the lane `{lane}` is written by its own backfill; or import a file of the series ({}) \
             and point {point} at `{USER_LANE_PREFIX}<label>:{}:{}:{interval}`{place}",
            import("<label>"),
            id.venue,
            id.symbol
        ),
        (None, _) => {
            let fetch = fetch_hint(id, spans.first())
                .map(|cmd| format!("fetch it: `{cmd}`, or "))
                .unwrap_or_default();
            format!(
                "    {fetch}import a file of it ({}) and point {point} at \
                 `{USER_LANE_PREFIX}<label>:{}:{}:{interval}`{place}",
                import("<label>"),
                id.venue,
                id.symbol
            )
        }
    });
    out
}

#[cfg(test)]
#[path = "inputs_tests.rs"]
mod inputs_tests;
