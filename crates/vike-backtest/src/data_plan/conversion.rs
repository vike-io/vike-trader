//! A run's CONVERSION series (decision 0243 phase 2): the history of the pair a profile names for each
//! quote currency that is not its ledger's (`engine.conversion`, read by
//! `crate::harness::profile::resolve_conversions`), planned as a series of its own role
//! (`super::DataPlanSeriesRole::Conversion`, which `data.explain` lists), read and judged before any
//! price is read, and handed to the simulator as `vike_simulator::StrategyEngineConfig::conversions`,
//! whose `vike_simulator::ConversionSeries::rate_at` is the rate of a moment.
//!
//! # The read
//!
//! Each pair is read through the store as an input is (`super::inputs`'s `read_series`), from one of
//! its bars before the window, because the close of the bar that closed as the window opened is the
//! rate of its first instant, to one of the run's bars past it. It is judged by an input's rule
//! (`super::inputs`'s `holes`): a span its bars do not cover is a hole past `data.max_gap` when
//! `data.require_coverage` arms it, and the closed time of its market's trading calendar, where its
//! name proves one, is not a hole. Its judged window opens one of ITS bars before the run's, for the
//! first rate. A bar whose close is no price is refused too: it would be a rate of nothing. Every pair
//! the store does not cover refuses the run in one sentence, each with the way to fill it.
//!
//! A profile that names no pair reads nothing here, so every run written before the table is
//! byte-identical.

use std::collections::BTreeMap;

use vike_data::{HistoryStore, SeriesId, TimestampRange};
use vike_model::run_request::replay::ReplayKind;
use vike_model::{AccountPerQuote, AccountPerQuoteLeg};
use vike_simulator::ConversionSeries;

use super::inputs::{hole_tolerance, holes, read_series, series_spec, uncovered_lines};
use crate::harness::profile::resolve_conversions;
use crate::harness::{HarnessError, ProfileBacktest};

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

/// **Read and judge every conversion pair of a run, before its price lane is read**, into what the
/// simulator's booking reads (`vike_simulator::StrategyEngineConfig::conversions`). None for a profile
/// that names none.
///
/// # Errors
/// [`resolve_conversions`]' refusals; [`HarnessError::Data`] for a read that failed, and for every pair
/// the store does not cover over the window or that holds a close that is no price (all of them in one
/// sentence, each with its way out).
pub(crate) fn load_conversions(
    profile: &ProfileBacktest,
    store: &(dyn HistoryStore + Send + Sync),
) -> Result<Vec<ConversionSeries>, HarnessError> {
    let conversions = resolve_conversions(profile)?;
    if conversions.is_empty() {
        return Ok(Vec::new());
    }
    let range = profile.range()?;
    let tolerance = hole_tolerance(profile)?;
    // Read to one of the run's bars past the window, as an input is: a mark at that bar's close.
    let bar_ms = match profile.data.kind {
        ReplayKind::Bar => {
            vike_marketdata::interval::interval_ms(&profile.data.interval).unwrap_or(0)
        }
        ReplayKind::Tick => 0,
    };
    let mut out = Vec::with_capacity(conversions.len());
    let mut refused: Vec<String> = Vec::new();
    for c in conversions {
        let quote = &c.quote_currency;
        let start = c.judged_from(range.start);
        let window = TimestampRange { start, end: range.end.map(|e| e.saturating_add(bar_ms)) };
        let mut bars = read_series(store, &c.id, window).map_err(|e| {
            HarnessError::Data(format!(
                "the conversion pair of {quote} ({}) could not be read: {e}. Nothing ran.",
                c.spec()
            ))
        })?;
        bars.sort_by_key(|b| b.timestamp);
        let head = format!("conversion pair of {quote} {:?} (engine.conversion.{quote})", c.spec());
        let kept = &profile.kept.rows;
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
        if let Some(bar) = bars.iter().find(|b| AccountPerQuote::of_mid(c.leg, b.close).is_err()) {
            refused.push(format!(
                "{head}: its bar at {} closes at {}, which is no price and so no rate",
                vike_model::time::epoch_ms_to_utc_timestamp(bar.timestamp),
                bar.close
            ));
            continue;
        }
        let pair = c.spec();
        let DataPlanConversion { quote_currency: quote_ccy, leg, interval_ms, .. } = c;
        // The symbols it books are named once the run's are known (`book_conversions`).
        out.push(ConversionSeries {
            quote_currency: quote_ccy,
            pair,
            leg,
            interval_ms,
            bars,
            symbols: Vec::new(),
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

/// **Which of `quotes` (the currencies `engine.conversion` names a pair for) `symbol` is quoted in**, or
/// `None`: the ONE rule for which conversion books a symbol (decision 0243 phase 2). The fiat quote its
/// NAME states when it states one (`vike_catalog::fiat_quote_of`, the rule every foreign-quote refusal
/// reads: `USDJPY`, `EUR_JPY`, `CS.D.GBPJPY.CFD.IP`); else a named code its name ENDS in, read on its
/// first `.`-token with `-`, `/` and `_` dropped (`BTCUSDT`, `BTC-USDT`, `BTCUSDT.P`, `HYPE/USDC`), which is
/// how a stablecoin quote is told: only against a code the profile named, so a currency nobody named
/// is never guessed (a name that states no quote keeps the crypto convention, the ledger's own unit).
pub(crate) fn quoted_in<'a>(symbol: &str, quotes: &'a [String]) -> Option<&'a str> {
    if quotes.is_empty() {
        return None;
    }
    let named = |code: &str| quotes.iter().find(|q| vike_model::same_currency(q, code));
    if let Some(fiat) = vike_catalog::fiat_quote_of(symbol) {
        return named(fiat).map(String::as_str);
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
    let cash =
        crate::harness::profile::cash_symbols(profile.engine.margin_mode.as_deref(), &series)?;
    for s in series.iter().filter(|s| cash.contains(&s.symbol)) {
        let Some(quote) = quoted_in(&s.symbol, &quotes) else { continue };
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
    let books = |q: &str| series.iter().any(|s| quoted_in(&s.symbol, &quotes) == Some(q));
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
        let Some(quote) = quoted_in(&s.symbol, &quotes) else { continue };
        if let Some(c) = conversions.iter_mut().find(|c| c.quote_currency == quote) {
            c.symbols.push(s.symbol);
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
