//! `[data.options]` at run time (`crate::run::profile::ProfileDataOptions`): the option LEGS a tick run trades,
//! priced from a stored option CHAIN, and what the simulator needs to settle them.
//!
//! # The contract
//!
//! - **A leg has a price exactly where its chain holds one.** Every priced chain row of the leg
//!   (`vike_data::ChainRow`, read through `HistoryStore::scan_chain` of the table's venue and chain) is ONE
//!   quote event at the row's `ts`. A chain that carries the venue's quotes (a recorded chain, Binance's
//!   `EOHSummary`) prices a row by its bid and ask, so a sale pays the spread, and a row without both sides
//!   is no price; a chain derived from trades (deribit's history: no row carries a quote) prices a row by
//!   its mark less and plus the profile's ASSUMED half-spread (`assumed_half_spread`, a fraction of the
//!   mark, REQUIRED there: without it the run is refused, never filled at the bare mark), and the data
//!   warnings say the spread was assumed. Nothing is filled forward:
//!   an hour the chain has no priced row of the leg is an hour with no event, so a market order on it
//!   waits for its next one and a leg the chain never prices in the window is not a symbol of the run
//!   (the data warnings name it). The chain's `ts` is when the row was KNOWN (`StudyContext::chain`'s
//!   convention), so no event is early.
//! - **One unit of a leg is one unit of its underlying unless the catalog says otherwise.** A chain's mark
//!   is the dollar premium of one unit of the underlying (each producer converts a coin premium at the
//!   index: `vike_data::option_trade_log`'s `hourly_chain_rows`, the datahub's recorder); a leg the catalog
//!   values (`vike_catalog::kept_contract_value`, linear) takes the catalog's units per contract. A leg the
//!   catalog does not value (a coin-settled option: its premium is coin on the venue) is booked here in
//!   dollars at the chain's mark, one unit of the underlying a unit, and the data warnings say so once.
//! - **The spec is the chain row's and the catalog's, never a parsed name.** Strike, expiry and right are the
//!   leg's chain row's (the venue spelling was parsed once, at ingest, by the bridge); style, margining and
//!   settlement are the leg's kept row's where the run holds one. Its `underlying` is the table's, the run's
//!   series whose price settles it (`vike_simulator::StrategyEngineConfig::option_specs`).
//! - **The underlying from its bars** (`underlying_interval`): each bar is one quote event at its LAST
//!   millisecond, priced at its close, the instant the close is known; the chain's hourly rows use the same
//!   stamp, so the two interleave without either seeing the other early.
//!
//! The IV and greeks of a leg are not the engine's: they are `vike_options`' (`implied_vol`,
//! `black_scholes_greeks`) over the same chain rows, asked by whoever reads them (a study).

use std::sync::Arc;

use vike_data::{ChainRow, HistoryStore, TimestampRange};
use vike_marketdata::{Bar, TickQuote};
use vike_model::{ContractOption, ContractOptionExerciseStyle, Margining, NotionalKind};

use crate::HarnessError;
use crate::run::profile::ProfileBacktest;
use crate::run::profile::ProfileDataOptions;

/// How much of the chain one read covers: a year of a busy chain is millions of rows, and a run keeps only
/// its legs' rows, so the window is read a week at a time.
const CHAIN_READ_SPAN_MS: i64 = vike_model::MS_PER_WEEK;

/// What [`load_option_legs`] answers: the events a tick replay adds, and the engine's facts about them.
#[derive(Debug, Default)]
pub(crate) struct OptionLegsLoaded {
    /// Each leg the chain prices in the window, in the table's order, with one quote per priced row.
    /// With the underlying's own events when the table reads it from bars, under its symbol.
    pub(crate) events: Vec<(String, Vec<TickQuote>)>,
    /// Each priced leg's contract, its `underlying` the run's settling series.
    pub(crate) specs: Vec<(String, ContractOption)>,
    /// Each priced leg's units of the underlying per unit of quantity (`StrategyEngineConfig::multipliers`).
    pub(crate) multipliers: Vec<(String, f64)>,
}

/// Read `[data.options]`' legs (and the underlying's bars, when named) over `range`, or `None` when the
/// profile has no such table. Every finding the run goes ahead with is added to `data_warnings`.
///
/// # Errors
/// A store read that fails; the underlying named by interval with no bar of it in the window (nothing could
/// settle a leg); a leg the catalog values as a coin-margined (inverse) contract.
pub(crate) fn load_option_legs(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    range: TimestampRange,
    data_warnings: &mut Vec<String>,
) -> Result<Option<OptionLegsLoaded>, HarnessError> {
    let Some(options) = &profile.data.options else { return Ok(None) };
    let data = |e: vike_data::HistoryStoreError| HarnessError::Data(e.to_string());
    let mut rows: Vec<Vec<ChainRow>> = vec![Vec::new(); options.legs.len()];
    // Whether the chain carries the venue's quotes at all: a chain derived from trades never does.
    let mut quoted = false;
    for window in read_windows(range) {
        for row in store.scan_chain(&options.venue, &options.chain, window).map_err(data)? {
            quoted |= row.bid.is_some() || row.ask.is_some();
            if let Some(at) = options.legs.iter().position(|leg| *leg == row.instrument) {
                rows[at].push(row);
            }
        }
    }
    // A chain with no quote at all trades at its mark less and plus the profile's ASSUMED half-spread, which
    // such a chain therefore requires: never the bare mark (REVIEW-LIVE-MONEY M7, the coordinator's rule).
    let half_spread = if quoted { None } else { options.assumed_half_spread };
    let marked = rows.iter().flatten().any(|row| row.mark.is_some_and(f64::is_finite));
    if !quoted && half_spread.is_none() && marked {
        return Err(HarnessError::Data(format!(
            "the {} chain {:?} carries no bid or ask in this window (a chain derived from the venue's \
             trades), so its legs have no spread to trade at, and a leg is never filled at the bare mark: \
             set [data.options] assumed_half_spread, the half-spread as a fraction of the mark (0.05 sells \
             at 95% of the mark and buys at 105%), from a measured or cited spread of the venue",
            options.venue, options.chain
        )));
    }
    let mut loaded = OptionLegsLoaded::default();
    let mut dollars_for_coin: Vec<&str> = Vec::new();
    for (leg, leg_rows) in options.legs.iter().zip(rows) {
        let quotes = leg_quotes(leg, &leg_rows, half_spread);
        let Some(first) = leg_rows.first().filter(|_| !quotes.is_empty()) else {
            data_warnings.push(format!(
                "{leg}: the {} chain {:?} holds no priced row of it in this window, so it did not \
                 run and an order on it could never fill",
                options.venue, options.chain
            ));
            continue;
        };
        let row = profile.kept.row(&options.venue, leg);
        let unit = match vike_catalog::kept_contract_value(&options.venue, leg, row) {
            Some(value) if value.kind == NotionalKind::Linear => value.per_unit,
            Some(_) => {
                return Err(HarnessError::Data(format!(
                    "{leg} on {} is valued by the catalog as a coin-margined contract, and \
                     [data.options] books its legs in dollars at the chain's marks",
                    options.venue
                )));
            }
            None => {
                dollars_for_coin.push(leg);
                1.0
            }
        };
        loaded.specs.push((leg.clone(), leg_spec(first, row, &options.underlying)));
        loaded.multipliers.push((leg.clone(), unit));
        loaded.events.push((leg.clone(), quotes));
    }
    match (half_spread, options.assumed_half_spread) {
        (Some(half), _) if !loaded.events.is_empty() => data_warnings.push(format!(
            "the {} chain {:?} carries no bid or ask in this window (a chain derived from the venue's \
             trades), so its legs trade at the mark less and plus an ASSUMED half-spread of {half} of the \
             mark ([data.options] assumed_half_spread), not at a quote",
            options.venue, options.chain
        )),
        (None, Some(half)) if quoted => data_warnings.push(format!(
            "[data.options] assumed_half_spread = {half} is not read: the {} chain {:?} carries the \
             venue's bid and ask, which price its legs",
            options.venue, options.chain
        )),
        _ => {}
    }
    if !dollars_for_coin.is_empty() {
        data_warnings.push(format!(
            "{}: the catalog values no unit of these legs (a coin-settled option's premium is coin \
             on the venue), so this run books each in dollars at the chain's mark, one unit of the \
             underlying a unit, and settles it in dollars at its intrinsic value",
            dollars_for_coin.join(", ")
        ));
    }
    if let Some(interval_ms) = options.underlying_interval_ms()? {
        let events = underlying_events(profile, store, options, range, interval_ms, data_warnings)?;
        loaded.events.push(events);
    }
    Ok(Some(loaded))
}

/// `range` cut into consecutive [`CHAIN_READ_SPAN_MS`] windows; an unbounded range is one read.
fn read_windows(range: TimestampRange) -> Vec<TimestampRange> {
    let (Some(start), Some(end)) = (range.start, range.end) else { return vec![range] };
    let mut out = Vec::new();
    let mut from = start;
    while from <= end {
        let to = from.saturating_add(CHAIN_READ_SPAN_MS - 1).min(end);
        out.push(TimestampRange::of(from, to));
        from = to.saturating_add(1);
        if to == i64::MAX {
            break;
        }
    }
    out
}

/// The bid and ask a leg's chain row trades at. On a chain carrying the venue's quotes (`assumed_half_spread`
/// `None`) a row with a two-sided quote, a bid above zero and an ask not below it, is that quote, and every
/// other row is NO price (no fill without a price): the mark is a model's number, not an offer. On a chain
/// derived from trades (no row carries a quote) the row exists because the leg traded that hour, and its
/// finite mark less and plus the profile's ASSUMED half-spread (a fraction of the mark) are its two sides.
fn row_quote(row: &ChainRow, assumed_half_spread: Option<f64>) -> Option<(f64, f64)> {
    let Some(half) = assumed_half_spread else {
        let (bid, ask) = (row.bid?, row.ask?);
        return (bid.is_finite() && ask.is_finite() && bid > 0.0 && ask >= bid)
            .then_some((bid, ask));
    };
    row.mark.filter(|m| m.is_finite()).map(|mark| (mark * (1.0 - half), mark * (1.0 + half)))
}

/// One quote per chain row of `leg` with a price ([`row_quote`]), at the row's `ts`; of two rows at one `ts`
/// the later read wins (a store answers a snapshot's rows once, so this only guards a repeat).
fn leg_quotes(leg: &str, rows: &[ChainRow], assumed_half_spread: Option<f64>) -> Vec<TickQuote> {
    let mut out: Vec<TickQuote> = Vec::with_capacity(rows.len());
    for row in rows {
        let Some((bid, ask)) = row_quote(row, assumed_half_spread) else { continue };
        let quote = TickQuote {
            timestamp: row.ts,
            local_timestamp: 0,
            bid,
            ask,
            // The chain states no size at its quote.
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: leg.into(),
        };
        // The scan is ts-ascending (`HistoryStore::scan_chain`), and the windows follow one another.
        match out.last_mut() {
            Some(last) if last.timestamp == row.ts => *last = quote,
            _ => out.push(quote),
        }
    }
    out
}

/// The leg's contract: the kept row's where the run holds one, else its chain row's strike, expiry and right
/// with the style unknown, margined linearly and settled in dollars (what this run does); its `underlying`
/// always the run's settling series.
fn leg_spec(
    first: &ChainRow,
    row: Option<&vike_model::Instrument>,
    underlying: &str,
) -> ContractOption {
    let kept = row.and_then(|r| match &r.contract {
        vike_model::Contract::Option(spec) => Some(spec.clone()),
        _ => None,
    });
    let mut spec = kept.unwrap_or_else(|| ContractOption {
        right: first.kind,
        strike: first.strike,
        expiry_ms: first.expiry_ms,
        style: ContractOptionExerciseStyle::Unknown,
        margining: Margining::Linear,
        settle: vike_model::DEFAULT_CURRENCY.to_string(),
        value_currency: None,
        underlying: None,
    });
    spec.underlying = Some(underlying.to_string());
    spec
}

/// The underlying's events from its stored bars of `interval_ms`: one quote per bar at its last millisecond,
/// bid and ask its close, none past `range`'s end. No bar in the window is refused: no leg could settle.
fn underlying_events(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    options: &ProfileDataOptions,
    range: TimestampRange,
    interval_ms: i64,
    data_warnings: &mut Vec<String>,
) -> Result<(String, Vec<TickQuote>), HarnessError> {
    let interval = options.underlying_interval.as_deref().unwrap_or_default();
    let symbol = options.underlying.as_str();
    let venue = profile
        .data
        .resolved_series()
        .into_iter()
        .find(|s| s.symbol == symbol)
        .map(|s| s.venue)
        .unwrap_or_default();
    let bars = super::bar_source::load_series_bars(
        profile,
        store,
        &venue,
        symbol,
        interval,
        range,
        data_warnings,
    )?;
    let quotes = bar_close_quotes(symbol, &bars, interval_ms, range.end);
    if quotes.is_empty() {
        return Err(HarnessError::Data(format!(
            "[data.options] reads {symbol} on {venue} from its {interval} bars, and the store holds \
             none in this window: no leg could settle at its expiry. Fetch them (vike-cli data history \
             fetch {venue}:{symbol}:{interval}), or drop underlying_interval to read its ticks"
        )));
    }
    Ok((symbol.to_string(), quotes))
}

/// Each bar as one quote at its last millisecond (`timestamp + interval_ms - 1`), bid and ask its close, none
/// after `end`.
fn bar_close_quotes(
    symbol: &str,
    bars: &[Bar],
    interval_ms: i64,
    end: Option<i64>,
) -> Vec<TickQuote> {
    bars.iter()
        .map(|b| TickQuote {
            timestamp: b.timestamp.saturating_add(interval_ms - 1),
            local_timestamp: 0,
            bid: b.close,
            ask: b.close,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: symbol.into(),
        })
        .filter(|q| end.is_none_or(|end| q.timestamp <= end))
        .collect()
}
