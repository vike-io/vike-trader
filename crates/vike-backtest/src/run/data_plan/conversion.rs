//! A run's CONVERSION series (decision 0243 phase 2): the history of the pair a profile names for each
//! quote currency that is not its ledger's (`engine.conversion`, read by
//! `crate::run::profile::resolve_conversions`), planned as a series of its own role
//! (`super::DataPlanSeriesRole::Conversion`, which `data.explain` lists), read and judged before any
//! price is read, and handed to the simulator as `vike_simulator::StrategyEngineConfig::conversions`,
//! whose `vike_simulator::ConversionSeries::rate_at` is the rate of a moment.
//!
//! # The read
//!
//! Each pair is read through the store as an input is (`super::inputs`'s `read_series`), from one of
//! its bars before the window, because the close of the bar that closed as the window opened is the
//! rate of its first instant, to the last of its bars that closes by the run's last bar's close
//! (`super::inputs`'s `read_end`: a daily pair of a window that ends yesterday reads no bar of today's,
//! which would keep the run from being answered from saved). It is judged by an input's rule
//! (`super::inputs`'s `holes`): a span its bars do not cover is a hole past `data.max_gap` when
//! `data.require_coverage` arms it, and the closed time of its market's trading calendar, where its
//! name proves one, is not a hole. Its judged window opens one of ITS bars before the run's, for the
//! first rate. A bar whose close is no price is refused too: it would be a rate of nothing. Every pair
//! the store does not cover refuses the run in one sentence, each with the way to fill it.
//!
//! # USDT on a dollar ledger: automatic, and one-to-one wherever there is no rate (decision 0243, amended 2026-11-16)
//!
//! ONE declared exception ([`converts_one_to_one_where_missing`]): a USDT quote on a dollar ledger, by the owner's
//! rulings of 2026-11-16 ("users will not have the nightmare of downloading the Coinbase rate"). It needs no
//! setting. A run on the dollar ledger with a symbol quoted in USDT and no `engine.conversion.USDT` row looks
//! for a USDT-USD daily series in the store itself ([`usdt_usd_found`]); a row that names one is read as before.
//! Either way, the pair is never refused for a missing rate: every moment it has none (before its first bar,
//! in a hole, past its last bar) one USDT is booked as one dollar
//! (`vike_simulator::ConversionSeries::par_where_missing`), and a store that holds no such series books the
//! whole run one-to-one, converting nothing. The run's report says which ([`one_to_one_lines`], lines of
//! `TradingRecord::data_warnings`): `USDT→USD rate not found, booked 1:1`, or the pair and the run's days it
//! booked one-to-one. Nothing else changes: any other quote or ledger is read and refused as before, and a
//! close that is no price is refused here too.
//!
//! A profile that names no pair and trades nothing quoted in USDT on the dollar ledger reads nothing here.

use std::collections::BTreeMap;

use vike_data::{HistoryStore, PlannedRead, SeriesId, TimestampRange};
use vike_model::StoreKindId;
use vike_model::run_request::replay::ReplayKind;
use vike_model::{AccountPerQuote, AccountPerQuoteLeg};
use vike_simulator::ConversionSeries;

use super::inputs::{
    hole_tolerance, holes, read_end, read_series, series_read, series_spec, uncovered_lines,
};
use crate::HarnessError;
use crate::run::profile::ProfileBacktest;
use crate::run::profile::{resolve_conversions, resolve_ledger_currency};

/// One `engine.conversion` row, read against the ledger: the quote currency it converts, the series
/// it reads and which way the pair's name points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPlanConversion {
    /// Upper-case (`vike_model::currency_code`): `JPY`, `USDT`.
    pub quote_currency: String,
    /// The series read, `kind=bar` at its interval and in its lane when the spelling names one.
    pub id: SeriesId,
    /// One bar's length in ms.
    pub interval_ms: i64,
    /// `vike_model::AccountPerQuoteLeg::of_pair_name` of the series' symbol against the ledger.
    pub leg: AccountPerQuoteLeg,
    /// Every moment the pair has no rate is booked one-to-one ([`converts_one_to_one_where_missing`]).
    pub par_where_missing: bool,
}

/// **Whether a pair converting `quote` into the `ledger` books every moment it has no rate ONE TO ONE**, and is
/// looked for in the store when no row names it (the module doc): a USDT quote on a dollar ledger alone, the
/// owner's rulings of 2026-11-16 (decision 0243's amendments: "USDT is 1 to 1 to USD"). Both are upper-case
/// codes (`vike_model::currency_code`). Every other pair has no rate there, and is refused, as before.
#[must_use]
pub fn converts_one_to_one_where_missing(quote: &str, ledger: &str) -> bool {
    quote == "USDT" && ledger == vike_model::DEFAULT_CURRENCY
}

/// The report's line for a run on the dollar ledger with a symbol quoted in USDT and no rate series at all.
pub const USDT_USD_NOT_FOUND: &str = "USDT→USD rate not found, booked 1:1";

/// **The USDT-USD daily series a run on the dollar ledger converts at with no setting** (the module doc), among the
/// series a store holds (`held`), or `None`: none needed (another ledger, a coin ledger, a row that names the USDT
/// pair, no symbol quoted in USDT) or none held. One is a `kind=bar` `1d` series whose symbol reads as a USDT priced
/// in dollars either way round (`USDT-USD`, `USDTUSD`: `AccountPerQuoteLeg::of_pair_name`); of several, the
/// operator's own import (a `user-<label>` lane) first, then the first spelled, so one store always answers the same.
/// The run reads it (`load_conversions`) and the run's address records it as it records a pair a row names.
///
/// # Errors
/// [`resolve_conversions`]' refusals, and the ledger's and the coin ledger's (`resolve_ledger_currency`).
pub fn usdt_usd_found(
    profile: &ProfileBacktest,
    held: &[SeriesId],
) -> Result<Option<DataPlanConversion>, HarnessError> {
    if !finds_usdt_by_itself(profile, &resolve_conversions(profile)?)? {
        return Ok(None);
    }
    let candidate = |id: &&SeriesId| {
        id.kind == StoreKindId::Bar
            && id.group.is_none()
            && id.interval.as_deref() == Some("1d")
            && AccountPerQuoteLeg::of_pair_name(&id.symbol, "USDT", vike_model::DEFAULT_CURRENCY)
                .is_some()
    };
    let user = |id: &SeriesId| id.source.as_deref().is_some_and(|lane| lane.starts_with("user-"));
    let found =
        held.iter().filter(candidate).min_by_key(|id| (!user(id), series_spec(id))).cloned();
    Ok(found.and_then(|id| {
        let leg =
            AccountPerQuoteLeg::of_pair_name(&id.symbol, "USDT", vike_model::DEFAULT_CURRENCY)?;
        let interval_ms = vike_marketdata::interval::interval_ms("1d")?;
        Some(DataPlanConversion {
            quote_currency: "USDT".to_string(),
            id,
            interval_ms,
            leg,
            par_where_missing: true,
        })
    }))
}

/// Whether the run looks for its USDT rate with no setting (the module doc): the dollar ledger, no coin ledger, no
/// `engine.conversion.USDT` row, and a symbol quoted in USDT.
fn finds_usdt_by_itself(
    profile: &ProfileBacktest,
    named: &[DataPlanConversion],
) -> Result<bool, HarnessError> {
    let ledger = resolve_ledger_currency(profile.engine.account_currency.as_deref())?;
    if !converts_one_to_one_where_missing("USDT", ledger)
        || named.iter().any(|c| c.quote_currency == "USDT")
    {
        return Ok(false);
    }
    let usdt = ["USDT".to_string()];
    let quoted = profile
        .data
        .resolved_series()
        .iter()
        .any(|s| quoted_in(&profile.kept, &s.venue, &s.symbol, &usdt).is_some());
    Ok(quoted && crate::run::profile_run_ledger_coin(profile)?.is_none())
}

/// **The report's lines for USDT on the dollar ledger** (the module doc): [`USDT_USD_NOT_FOUND`] when the run looked
/// for a USDT-USD series and the store holds none, and for each pair it converted at, the run's UTC days it had no
/// rate on and booked one-to-one (none when it had one every day). Nothing for every other run.
pub(crate) fn one_to_one_lines(
    profile: &ProfileBacktest,
    conversions: &[ConversionSeries],
) -> Result<Vec<String>, HarnessError> {
    let usdt = conversions.iter().find(|c| c.quote_currency == "USDT" && c.par_where_missing);
    let Some(pair) = usdt else {
        let looked = finds_usdt_by_itself(profile, &resolve_conversions(profile)?)?;
        return Ok(if looked { vec![USDT_USD_NOT_FOUND.to_string()] } else { Vec::new() });
    };
    let range = profile.range()?;
    let (Some(start), Some(end)) = (range.start, range.end) else {
        return Ok(vec![format!(
            "USDT→USD rate {:?}: every moment of this run it has no rate was booked 1:1 (decision 0243, the \
             owner's rulings of 2026-11-16)",
            pair.pair
        )]);
    };
    use vike_model::MS_PER_DAY;
    let day = |ms: i64| {
        vike_model::time::epoch_ms_to_utc_timestamp(ms).get(..10).unwrap_or_default().to_string()
    };
    // The run's UTC days whose opening rate is one-to-one, as ranges of consecutive days.
    let mut spans: Vec<(i64, i64)> = Vec::new();
    let mut d = start.div_euclid(MS_PER_DAY) * MS_PER_DAY;
    while d <= end {
        if pair.rate_at(d.max(start)).is_some_and(|r| r.par) {
            match spans.last_mut() {
                Some(last) if last.1 + MS_PER_DAY == d => last.1 = d,
                _ => spans.push((d, d)),
            }
        }
        d += MS_PER_DAY;
    }
    if spans.is_empty() {
        return Ok(Vec::new());
    }
    let days: i64 = spans.iter().map(|(a, b)| (b - a) / MS_PER_DAY + 1).sum();
    let listed: Vec<String> = spans
        .iter()
        .map(|&(a, b)| if a == b { day(a) } else { format!("{} to {}", day(a), day(b)) })
        .collect();
    Ok(vec![format!(
        "USDT→USD rate {:?}: no rate on {days} day(s) of this run, booked 1:1: {} (UTC; decision 0243, the \
         owner's rulings of 2026-11-16)",
        pair.pair,
        listed.join(", ")
    )])
}

impl DataPlanConversion {
    /// The series read, in the input grammar.
    pub fn spec(&self) -> String {
        series_spec(&self.id)
    }

    /// The start of the window the pair is judged over: one of its bars before the run's.
    pub(super) fn judged_from(&self, run_start: Option<i64>) -> Option<i64> {
        run_start.map(|s| s.saturating_sub(self.interval_ms))
    }
}

/// The run's bar length, by whose last close a pair is read as an input is (`super::inputs`'s `read_end`):
/// `data.interval`'s on the bar lane, none that does not parse, 0 on the tick lane.
fn pair_bar_ms(profile: &ProfileBacktest) -> i64 {
    match profile.data.kind {
        ReplayKind::Bar => {
            vike_marketdata::interval::interval_ms(&profile.data.interval).unwrap_or(0)
        }
        ReplayKind::Tick => 0,
    }
}

/// The window pair `c` is read over: from one of its bars before `range`, to its last bar that closes by the run's
/// last bar's close (`bar_ms`, [`read_end`]).
fn pair_window(c: &DataPlanConversion, range: TimestampRange, bar_ms: i64) -> TimestampRange {
    TimestampRange {
        start: c.judged_from(range.start),
        end: read_end(range.end, bar_ms, c.interval_ms),
    }
}

/// The reads [`load_conversions`] makes that wait on nothing read before them, named ahead (`super::planned_reads`):
/// `ListSeries` when the run looks for its USDT rate itself, then each pair a row names. The pair `ListSeries` finds
/// is not known before it answers, and is read by its verb alone.
pub(super) fn conversion_reads(profile: &ProfileBacktest) -> Vec<PlannedRead> {
    let (Ok(named), Ok(range)) = (resolve_conversions(profile), profile.range()) else {
        return Vec::new();
    };
    let mut reads = Vec::new();
    if finds_usdt_by_itself(profile, &named).unwrap_or(false) {
        reads.push(PlannedRead::ListSeries);
    }
    let bar_ms = pair_bar_ms(profile);
    reads.extend(named.iter().map(|c| series_read(&c.id, pair_window(c, range, bar_ms))));
    reads
}

/// **Read and judge every conversion pair of a run, before its price lane is read**, into what the
/// simulator's booking reads (`vike_simulator::StrategyEngineConfig::conversions`): each row the profile names,
/// and, with no setting, the store's USDT-USD series for a USDT quote on the dollar ledger (the module doc). None
/// for a profile that names none and needs none.
///
/// # Errors
/// [`resolve_conversions`]' refusals; [`HarnessError::Data`] for a read that failed, and for every pair
/// the store does not cover over the window (but a USDT pair on the dollar ledger) or that holds a close that is
/// no price (all of them in one sentence, each with its way out).
pub(crate) fn load_conversions(
    profile: &ProfileBacktest,
    store: &(dyn HistoryStore + Send + Sync),
) -> Result<Vec<ConversionSeries>, HarnessError> {
    let mut conversions = resolve_conversions(profile)?;
    if finds_usdt_by_itself(profile, &conversions)? {
        // Every series and every `source=` lane the store holds (its inventory names no lane); a store that
        // cannot list what it holds finds nothing: the run books 1:1 and says so.
        let held: Vec<SeriesId> = store.list_series().unwrap_or_default();
        conversions.extend(usdt_usd_found(profile, &held)?);
    }
    if conversions.is_empty() {
        return Ok(Vec::new());
    }
    let range = profile.range()?;
    let tolerance = hole_tolerance(profile)?;
    let bar_ms = pair_bar_ms(profile);
    let mut out = Vec::with_capacity(conversions.len());
    let mut refused: Vec<String> = Vec::new();
    for c in conversions {
        let quote = &c.quote_currency;
        let window = pair_window(&c, range, bar_ms);
        let start = window.start;
        let mut bars = read_series(store, &c.id, window).map_err(|e| {
            HarnessError::Data(format!(
                "the conversion pair of {quote} ({}) could not be read: {e}. Nothing ran.",
                c.spec()
            ))
        })?;
        bars.sort_by_key(|b| b.timestamp);
        let head = format!("conversion pair of {quote} {:?} (engine.conversion.{quote})", c.spec());
        let kept = &profile.kept.rows;
        // The one exception (the module doc): a USDT pair on the dollar ledger books every moment it has no rate
        // one-to-one, so nothing it lacks is a hole.
        if !c.par_where_missing {
            let (spans, calendar) =
                holes(&bars, &c.id, kept, c.interval_ms, start, range.end, tolerance);
            if !spans.is_empty() {
                let place = format!("engine.conversion.{quote}");
                refused.extend(uncovered_lines(
                    &head,
                    &c.id,
                    &spans,
                    calendar,
                    kept,
                    (place.as_str(), ""),
                ));
                continue;
            }
        }
        if let Some(bar) = bars.iter().find(|b| AccountPerQuote::of_mid(c.leg, b.close).is_err()) {
            refused.push(format!(
                "{head}: its bar at {} closes at {}, which is no price and so no rate",
                vike_model::time::epoch_ms_to_utc_timestamp(bar.timestamp),
                bar.close
            ));
            continue;
        }
        let pair = c.spec();
        let DataPlanConversion {
            quote_currency: quote_ccy,
            leg,
            interval_ms,
            par_where_missing,
            ..
        } = c;
        // The symbols it books are named once the run's are known (`book_conversions`).
        out.push(ConversionSeries {
            quote_currency: quote_ccy,
            pair,
            leg,
            interval_ms,
            bars: std::sync::Arc::new(bars),
            symbols: Vec::new(),
            par_where_missing,
        });
    }
    if !refused.is_empty() {
        return Err(HarnessError::Data(format!(
            "engine.conversion names a conversion pair the store does not cover over this run's \
             window, so the run is refused before any price was read. Nothing ran.\n{}",
            refused.join("\n")
        )));
    }
    Ok(out)
}

/// **Which of `quotes` (the currencies `engine.conversion` names a pair for) `symbol` on `venue` is quoted in**,
/// or `None`: the ONE rule for which conversion books a symbol (decision 0243 phase 2). The fiat currency the
/// ledger guard reads for it (`KeptRows::fiat_quote`, `vike_catalog::kept_fiat_quote`: its KEPT row's quote, an
/// ibkr canonical's currency field (`USD.JPY.IDEALPRO` is JPY), else its name's: `USDJPY`, `EUR_JPY`,
/// `CS.D.GBPJPY.CFD.IP`), so the pair the guard's refusal names is the pair that books it; a broker's symbol of no
/// known currency, and a row quoted in a minor unit (`vike_model::FiatQuote::Unknown`, `Minor`), is booked by none.
/// Else a named code its name ENDS in, read on
/// its first `.`-token with `-`, `/` and `_` dropped (`BTCUSDT`, `BTC-USDT`, `BTCUSDT.P`, `HYPE/USDC`), which is
/// how a stablecoin quote is told: only against a code the profile named, so a currency nobody named is never
/// guessed (a name that states no quote keeps the crypto convention, the ledger's own unit).
pub(crate) fn quoted_in<'a>(
    kept: &crate::run::KeptRows,
    venue: &str,
    symbol: &str,
    quotes: &'a [String],
) -> Option<&'a str> {
    if quotes.is_empty() {
        return None;
    }
    let named = |code: &str| quotes.iter().find(|q| vike_model::same_currency(q, code));
    match kept.fiat_quote(venue, symbol) {
        Some(vike_model::FiatQuote::Code(fiat)) => return named(fiat).map(String::as_str),
        Some(vike_model::FiatQuote::Unknown | vike_model::FiatQuote::Minor(_)) => return None,
        None => {}
    }
    let head = symbol.trim().split('.').next().unwrap_or_default();
    let name: String = head
        .chars()
        .filter(|c| !matches!(c, '-' | '/' | '_'))
        .collect::<String>()
        .to_ascii_uppercase();
    quotes
        .iter()
        .find(|q| name.len() > q.len() && name.ends_with(q.to_ascii_uppercase().as_str()))
        .map(String::as_str)
}

/// **The quote currencies a profile names a conversion pair for** (upper-case, `resolve_conversions`),
/// after refusing in words, before anything is read, a symbol one of them would book but the booking
/// does not value: a symbol bought for CASH on a venue that is not crypto-native, a cash purchase of a
/// foreign stock (decision 0243, item 4), which debits its principal in the foreign currency, the
/// per-asset ledger decision 0183 leaves to its own reopener. A margined one
/// (`engine.margin_mode = "cross"`) converts, and so does a crypto pair bought for cash in a stablecoin
/// (the owner's amendment of decision 0243). A row no symbol of the run is quoted in is refused too.
///
/// # Errors
/// [`resolve_conversions`]' refusals; [`HarnessError::Data`] naming the cash symbol or the idle row.
pub(crate) fn conversion_quotes(profile: &ProfileBacktest) -> Result<Vec<String>, HarnessError> {
    let quotes: Vec<String> =
        resolve_conversions(profile)?.into_iter().map(|c| c.quote_currency).collect();
    if quotes.is_empty() {
        return Ok(quotes);
    }
    let series = profile.data.resolved_series();
    let cash = crate::run::profile::cash_symbols(profile.engine.margin_mode.as_deref(), &series)?;
    for s in series.iter().filter(|s| cash.contains(&s.leg_name())) {
        let Some(quote) = quoted_in(&profile.kept, &s.venue, &s.symbol, &quotes) else { continue };
        if !vike_venues::session::is_crypto_native(&s.venue) {
            return Err(HarnessError::Data(format!(
                "{} on {} is bought for cash and quoted in {quote}: a cash purchase of a foreign stock \
                 debits its principal in {quote}, a per-asset ledger the simulator does not keep \
                 (decision 0243, item 4), so the run is refused. Nothing ran. Trade it on margin \
                 (engine.margin_mode = \"cross\"), where its money converts, or state the ledger in \
                 {quote} (engine.account_currency = \"{quote}\") and drop engine.conversion.{quote}",
                s.symbol, s.venue
            )));
        }
    }
    // A row no symbol is quoted in converts nothing, and a misnamed one would leave the symbol it was
    // meant for at par: refused, as an `[inputs]` row naming no input is.
    let books = |q: &str| {
        series.iter().any(|s| quoted_in(&profile.kept, &s.venue, &s.symbol, &quotes) == Some(q))
    };
    let idle: Vec<&str> = quotes.iter().map(String::as_str).filter(|q| !books(q)).collect();
    if !idle.is_empty() {
        return Err(HarnessError::Data(format!(
            "engine.conversion names a pair for {}, and no symbol of this run is quoted in it (a name \
             states its fiat quote, or ends in a code the table names: BTCUSDT in USDT), so the pair \
             would convert nothing. Nothing ran. Drop the row, or key it by the quote currency of the \
             symbol it is for",
            idle.join(", ")
        )));
    }
    Ok(quotes)
}

/// Whether one of `conversions` BOOKS `symbol` ([`ConversionSeries::symbols`]): what the ledger guard of
/// a door handed the pairs leaves out (the tick replay, a run over held bars).
pub(crate) fn books(conversions: &[ConversionSeries], symbol: &str) -> bool {
    conversions.iter().any(|c| c.symbols.iter().any(|s| s == symbol))
}

/// **Say which of the run's symbols each loaded pair BOOKS** ([`ConversionSeries::symbols`], by
/// [`quoted_in`]): the step that hands the simulator its booking (decision 0243 phase 2). A coin ledger
/// (`coin`: the run holds a coin-margined contract, whose money is the settle coin) converts nothing and
/// is refused in words. Every pair books a symbol: an idle row was refused before anything was read
/// (`conversion_quotes`).
///
/// # Errors
/// [`HarnessError::Data`] for a coin ledger that names a pair.
pub(crate) fn book_conversions(
    profile: &ProfileBacktest,
    conversions: &mut [ConversionSeries],
    coin: bool,
) -> Result<(), HarnessError> {
    if conversions.is_empty() {
        return Ok(());
    }
    if coin {
        return Err(HarnessError::Data(
            "engine.conversion names a conversion pair, and this run holds a coin-margined contract: \
             its money is the settle coin, which converts nothing (decision 0258). Nothing ran. Drop \
             engine.conversion, or backtest the linear symbols in a run of their own"
                .to_string(),
        ));
    }
    let quotes: Vec<String> = conversions.iter().map(|c| c.quote_currency.clone()).collect();
    for s in profile.data.resolved_series() {
        let Some(quote) = quoted_in(&profile.kept, &s.venue, &s.symbol, &quotes) else { continue };
        if let Some(c) = conversions.iter_mut().find(|c| c.quote_currency == quote) {
            // the simulator books a run's SLOT, a series naming its account by its leg's name
            c.symbols.push(s.leg_name());
        }
    }
    Ok(())
}

/// Refuse a profile that names a conversion pair (`rows` is its `engine.conversion`) at a door that
/// was handed none to book: `door` names it (a run over held bars whose caller loaded no pair). A
/// profile that names none passes.
///
/// # Errors
/// [`HarnessError::Data`], naming the quote currencies and the door that does load them.
pub fn refuse_unplanned_conversions(
    rows: &BTreeMap<String, String>,
    door: &str,
) -> Result<(), HarnessError> {
    if rows.is_empty() {
        return Ok(());
    }
    let quotes: Vec<&str> = rows.keys().map(String::as_str).collect();
    Err(HarnessError::Data(format!(
        "{door} does not load the conversion pairs engine.conversion names ({}), so it would book \
         each quote as the ledger's own money (decision 0243). Nothing ran. A profile backtest and a \
         walk-forward plan, read and check each pair before they start",
        quotes.join(", ")
    )))
}

#[cfg(test)]
#[path = "conversion_tests.rs"]
mod conversion_tests;
