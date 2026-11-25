//! The cTrader RATE SOURCE for the engine's multi-currency money (decision 0183, phase 1): what one unit
//! of an instrument's QUOTE currency is worth in the ACCOUNT's deposit currency, read from the mid of the
//! conversion pair's own quote on the account's data session.
//!
//! This module is the PURE half (types, the choice of the conversion symbol, the orientation, the quote
//! checks); the session that asks the venue is `crate::transport::connection::fetch_conversion_quote`, and the gateway's
//! facts call it through `crate::CatalogProviderCtrader::quote_rate`. Nothing here touches the engine: the node
//! turns a [`QuoteRate`] (`vike_model`'s one record, with the session's deposit currency beside it) into the
//! engine's per-currency rate.
//!
//! # Where each input comes from (Spotware Open API, <https://help.ctrader.com/open-api/model-messages/>)
//!
//! - **the account's currency**: `ProtoOATrader.depositAssetId` ("Deposit currency of the account"), named
//!   through the asset list (`SymbolMap::deposit_currency`, read at the handshake);
//! - **an instrument's quote currency**: the light symbol's `quoteAssetId` ("Quote asset"), named through
//!   the same asset list (`Instrument::quote`);
//! - **the rate**: the bid and ask of the conversion pair, subscribed at runtime
//!   (`ProtoOASubscribeSpotsReq`, <https://help.ctrader.com/open-api/messages/>: "you'll receive technical
//!   ProtoOASpotEvent with latest price"; the proto of this crate adds "even if market is closed"),
//!   prices in 1/100000 of a unit. The mid of a two-sided quote, never a last trade.
//!
//! ⚠ **Not used for the engine's rate:** `ProtoOAClosePositionDetail.quoteToDepositConversionRate`
//! ("Quote/Deposit currency conversion rate on the time of closing deal execution"). The page gives no
//! direction (which currency is the numerator) and nothing was measured, so it may be READ for verification
//! of a demo close, never relied on (the live drill prints it).
//!
//! # The orientation, which is the whole risk
//!
//! Every rate here is [`AccountPerQuote`]: **account-currency units per ONE unit of the quote currency**,
//! multiplied into a quote-currency amount ([`AccountPerQuote::to_account`]), never divided. On a USD
//! account the conversion pair for a JPY-quoted instrument is USDJPY (the account currency is the BASE of
//! the pair, price = yen per dollar), so the rate is `1/mid` = `0.00667`, NOT `150`; for a GBP-quoted
//! instrument it is GBPUSD (the quote currency is the BASE, price = dollars per pound), so the rate is
//! `mid` = `1.35`. The flipped rate is a 22,500x error on USDJPY. The direction lives in the TYPE (the only
//! constructor refuses a non-rate) and in [`AccountPerQuoteLeg`], and the goldens below name which way they point.
//!
//! # Fail closed
//!
//! No conversion pair, two of them, a one-sided, zero, crossed, non-finite, untimed or old quote, a pair the
//! broker suspended, a session that failed or ran past its deadline: each is a [`CtraderRateRefusal`], which
//! becomes a [`vike_model::QuoteRateError`] whose words open with `ctrader: ` (a session failure is `Unavailable`, no single
//! pair `NoPair`, a quote that is no rate, untimed, older than [`OPEN_MARKET_MAX_SPOT_AGE_MS`] /
//! [`MAX_SPOT_AGE_MS`] or of a suspended pair `BadQuote`; never a token). No path returns
//! `1.0` for "I do not know": the single par is the quote currency being the account's own, which is an
//! identity and not a lookup.
//!
//! # The quote's age
//!
//! The venue's own timestamp on the quote (ASSUMED milliseconds, [`MIN_SPOT_MS_EPOCH`]) is the only clock on
//! the price: the rate is stamped at the ASK, so a price of unknown age would be stamped fresh. A quote with no
//! timestamp is therefore not a rate ([`spot_age_refusal`]; the session waits for a later event that carries
//! one), a market demonstrably open ([`market_state`]) allows [`OPEN_MARKET_MAX_SPOT_AGE_MS`], one
//! demonstrably CLOSED [`MAX_SPOT_AGE_MS`], and one whose state this crate cannot read the strict
//! [`OPEN_MARKET_MAX_SPOT_AGE_MS`] too (fail closed: a frozen feed on an open market must not pass for a
//! weekend). A stamp that lies in the future ([`MAX_SPOT_AFTER_ASK_MS`]) is not a millisecond epoch either.
//!
//! # The mid, and why it is not side-aware
//!
//! shortcut: the rate is the MID of the pair's quote ([`rate_of_quote`]). The vendor's own sample
//! (<https://help.ctrader.com/open-api/symbol-rate-conversion/>) converts a long position's PnL at the BID and a
//! short one's at the ASK, so the mid is off by half the pair's spread (a spread-sized error, one sign per
//! direction) against the venue's own conversion. The engine takes ONE factor per symbol
//! (`SymbolAdmission::quote_rate`), not one per position side, so a side-aware rate has nowhere to go without
//! a change in the engine's admission; upgrade together with that change once the live drill's
//! `ProtoOAGetPositionUnrealizedPnLReq` comparison shows the venue's side.

use vike_model::{
    AccountPerQuote, AccountPerQuoteLeg, CONVERSION_HUB, ConversionLeg, ConversionRefusal,
    ConversionRoute, QuoteRate, SECONDS_PER_DAY, currency_code, same_currency,
};
use vike_utility::calendar::civil_from_days;
use vike_venues::session::new_york_utc_offset_min;

use crate::catalog::{SymbolKind, listed_kind};
use crate::feed::decode::RELATIVE_PRICE_SCALE;
use crate::instrument::symbols::SymbolMap;
use crate::refusals::CtraderRateRefusal;
use crate::transport::proto::{ProtoOaSpotEvent, ProtoOaSymbol, ProtoOaTradingMode};

/// How old the venue's own timestamp on a quote may be when the market is demonstrably CLOSED
/// ([`MarketState::Closed`]): a weekend is about 48 h and the first spot after a subscription is the latest
/// price "even if market is closed", so this has to outlast a closed market and a bank holiday and still catch
/// a frozen price. A judgement call, UNMEASURED.
///
/// shortcut: [`market_state`] reads the symbol's own `schedule` only where the schedule's time zone is spelled
/// UTC or is `America/New_York` (no time-zone database is linked into an order-signing binary for this; New
/// York is the one zone a broker was MEASURED to send, on the CI box at the v0.1.84 boot, and its daylight-saving
/// rule is `vike_venues::session::new_york_utc_offset_min` behind `new_york_offset_seconds`' year guard). For any other zone, or no schedule, the market is
/// [`MarketState::Unknown`] and the STRICT bound [`OPEN_MARKET_MAX_SPOT_AGE_MS`] stands (never this one: a feed
/// frozen for less than 72 h on an open market would pass, with the rate stamped fresh at the ask), so on such a
/// broker the rate is refused over a weekend rather than guessed. Upgrade (resolve one more zone) once the live
/// smoke's `CTSYM` line, or a refusal naming the zone, shows which zone a broker really sends.
pub const MAX_SPOT_AGE_MS: i64 = 72 * 3_600 * 1_000;

/// How old the venue's own timestamp on a quote may be while the market is demonstrably OPEN: the engine's own
/// staleness window (a rate the engine would call stale a minute after it is taken is no rate to hand it). The
/// rate is stamped at the ASK, so the engine's window runs from the ask, on top of this.
pub const OPEN_MARKET_MAX_SPOT_AGE_MS: i64 = vike_execution::DEFAULT_QUOTE_RATE_TTL_MS;

/// How far AFTER the ask the venue's stamp on a quote may lie: the quote is read for up to
/// `transport::connection::SPOT_WAIT` (five seconds) after the subscription, so a live tick can be stamped that late, plus five
/// seconds of clock skew between the venue and this host. A stamp past it is not a clock error but a unit one
/// (a microsecond or nanosecond stamp is 10^3 or 10^6 times a millisecond epoch), and an age that comes out
/// negative would otherwise read as "fresher than fresh" and switch the age check off.
pub const MAX_SPOT_AFTER_ASK_MS: i64 = 10_000;

/// The smallest `ProtoOASpotEvent.timestamp` taken as a MILLISECOND epoch (1973-03-03). The page states only
/// "The Unix time for spot" (<https://help.ctrader.com/open-api/messages/>) and every other stamp of the API is
/// in milliseconds, so milliseconds is the ASSUMPTION; a stamp below this reads as SECONDS (a current one is
/// about 1.7e9) and is refused loudly, because taken as milliseconds it would be an age of 55 years, and taken
/// as fresh it would be wrong the other way.
pub const MIN_SPOT_MS_EPOCH: i64 = 100_000_000_000;

/// **The account's own currency converting into itself** (no pair, no quote), as a rate read answers it: the rate
/// and, beside it, the deposit currency the session priced it in, both the trimmed upper-case code. `None` for an
/// empty code: it is no currency.
#[must_use]
pub fn par(account_ccy: &str, as_of_ms: i64) -> Option<(QuoteRate, String)> {
    let rate = QuoteRate::new(account_ccy, AccountPerQuote::IDENTITY, as_of_ms)?;
    let account = rate.quote_currency.clone();
    Some((rate, account))
}

/// One symbol of the account's list that converts one currency into another (the way round it is listed is its
/// [`ConversionLeg`]'s `orientation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
    /// The symbol id on THIS account's server (ids are per server, so it is never stored).
    pub symbol_id: i64,
    /// The broker's name for it (case kept).
    pub name: String,
}

/// Every FOREX-class symbol of the account's list (the broker's asset class, never a name guess) whose base asset
/// is `base` and quote asset `counter`, by name.
fn forex_pairs(map: &SymbolMap, base: &str, counter: &str) -> Vec<Conversion> {
    let mut found: Vec<Conversion> = map
        .ids()
        .into_iter()
        .filter_map(|id| {
            let name = map.name_of(id).filter(|n| !n.is_empty())?;
            let forex = map.category(id).and_then(|c| c.asset_class).and_then(listed_kind)
                == Some(SymbolKind::Forex);
            let listed = forex
                && same_currency(map.base_asset(id)?, base)
                && same_currency(map.quote_asset(id)?, counter);
            listed.then(|| Conversion { symbol_id: id, name: name.to_string() })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The sentence for two or more symbols between `from` and `to` (either way round): which one the rate is read
/// from would be a guess.
fn many_pairs(from: &str, to: &str, found: &[Conversion]) -> CtraderRateRefusal {
    CtraderRateRefusal::ManyPairs {
        count: found.len(),
        from: from.to_string(),
        to: to.to_string(),
        names: found.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", "),
    }
}

/// **The pair, or the two pairs through USD, that convert `quote` into `account`** in the account's own symbol
/// list, by the pick every converting bridge shares (`vike_model::ConversionRoute::pick`): a FOREX-class symbol
/// whose base and quote assets are the two currencies, in either order; else the quote currency's pair with USD
/// and USD's with the account's currency, whose rates multiply. Each leg is exactly one symbol, or a refusal:
/// none (`NoPair`, named), and two or more between one leg's currencies, either way round (which of them the rate
/// is read from would be a guess), are each named. An empty currency is never the account's.
pub fn conversion_route(
    map: &SymbolMap,
    account: &str,
    quote: &str,
) -> Result<ConversionRoute<Conversion>, CtraderRateRefusal> {
    let listed = |base: &str, counter: &str| {
        let found = forex_pairs(map, base, counter);
        (!found.is_empty()).then_some(found)
    };
    let route = ConversionRoute::pick(quote, account, listed).map_err(|refusal| {
        let code = |raw: &str| currency_code(raw).unwrap_or_default();
        let (quote, account) = (code(quote), code(account));
        match refusal {
            ConversionRefusal::NotACurrency => CtraderRateRefusal::EmptyCurrency,
            ConversionRefusal::SameCurrency => CtraderRateRefusal::SameCurrency { quote },
            ConversionRefusal::BothOrientations { from, to } => {
                let mut found = forex_pairs(map, &from, &to);
                found.extend(forex_pairs(map, &to, &from));
                found.sort_by(|a, b| a.name.cmp(&b.name));
                many_pairs(&from, &to, &found)
            }
            ConversionRefusal::NoPair => {
                let through = if same_currency(&quote, CONVERSION_HUB)
                    || same_currency(&account, CONVERSION_HUB)
                {
                    String::new()
                } else {
                    format!(" (nor a pair of each with {CONVERSION_HUB})")
                };
                CtraderRateRefusal::NoPair { quote, account, through }
            }
        }
    })?;
    let one = |leg: ConversionLeg<Vec<Conversion>>| match <[Conversion; 1]>::try_from(leg.pair) {
        Ok([pair]) => {
            Ok(ConversionLeg { pair, from: leg.from, to: leg.to, orientation: leg.orientation })
        }
        Err(found) => Err(many_pairs(&leg.from, &leg.to, &found)),
    };
    Ok(match route {
        ConversionRoute::Direct(leg) => ConversionRoute::Direct(one(leg)?),
        ConversionRoute::ThroughHub([first, second]) => {
            ConversionRoute::ThroughHub([one(first)?, one(second)?])
        }
    })
}

/// **The rate from a two-sided quote** (`bid` and `ask` as the wire states them, 1/100000 of a price unit):
/// the mid, turned the right way round for `leg`. A zero side or a crossed quote is no quote.
///
/// shortcut: the MID, where the vendor's sample converts at the bid for a long and the ask for a short (see the
/// module doc, "The mid, and why it is not side-aware"); a spread-sized error, upgrade with the engine.
pub fn rate_of_quote(
    leg: AccountPerQuoteLeg,
    bid: u64,
    ask: u64,
) -> Result<AccountPerQuote, CtraderRateRefusal> {
    if bid == 0 || ask == 0 {
        return Err(CtraderRateRefusal::ZeroSide { bid, ask });
    }
    if bid > ask {
        return Err(CtraderRateRefusal::Crossed { bid, ask });
    }
    let mid = (bid as f64 + ask as f64) / 2.0 / RELATIVE_PRICE_SCALE;
    AccountPerQuote::of_mid(leg, mid).map_err(|_| CtraderRateRefusal::MidNoRate { mid })
}

/// Whether a symbol's market is open at an instant, from the symbol's OWN schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketState {
    /// Inside one of the symbol's trading intervals.
    Open,
    /// Outside every trading interval.
    Closed,
    /// Not known: no schedule, or its time zone is not one this crate resolves ([`market_state`]). The
    /// age check then runs at the STRICT bound, [`OPEN_MARKET_MAX_SPOT_AGE_MS`] ([`spot_age_refusal`]).
    Unknown,
}

/// The schedule time zone a symbol states, when it states a schedule too: what [`spot_age_refusal`] names when
/// the market's state is [`MarketState::Unknown`] (`None`: nothing to name, a schedule or its zone is absent).
#[must_use]
pub fn schedule_zone(symbol: &ProtoOaSymbol) -> Option<&str> {
    let zone = symbol.schedule_time_zone.as_deref().map(str::trim).filter(|z| !z.is_empty())?;
    (!symbol.schedule.is_empty()).then_some(zone)
}

/// Names of the time zone [`market_state`] can evaluate with no time-zone database and no daylight saving: a
/// fixed UTC.
const UTC_ZONES: [&str; 4] = ["UTC", "Etc/UTC", "GMT", "Etc/GMT"];

/// The one named zone with daylight saving that [`market_state`] evaluates: the New York wall clock the forex
/// week's 17:00 close and Sunday 17:00 re-open are kept on. Measured: the schedule zone the USDJPY and GBPUSD
/// records stated on the CI box at the v0.1.84 boot (the refusals named it); the intervals themselves were not read.
const NEW_YORK_ZONE: &str = "America/New_York";

/// Seconds from the Unix epoch (a Thursday, 00:00) back to the Sunday before it: the schedule counts "seconds
/// starting from SUNDAY 00:00 in specified time zone" (`ProtoOASymbol.schedule`).
const EPOCH_SECONDS_PAST_SUNDAY: i64 = 4 * SECONDS_PER_DAY;

/// **New York's offset from UTC, in seconds, at `now_s` (epoch seconds)**, under the US rule in force since
/// 2007 (`vike_venues::session::new_york_utc_offset_min`): daylight time (UTC-4) from the second Sunday of March
/// at 02:00 local (07:00 UTC) until the first Sunday of November at 02:00 local (06:00 UTC), else standard time
/// (UTC-5). `None` before 2007, whose rule was another one (never a guess): the shared rule reads every year as
/// 2007's.
fn new_york_offset_seconds(now_s: i64) -> Option<i64> {
    let (year, _, _) = civil_from_days(now_s.div_euclid(SECONDS_PER_DAY));
    (year >= 2007).then(|| -60 * new_york_utc_offset_min(now_s * 1_000))
}

/// Seconds to add to a UTC instant for the wall clock of `zone`, for the zones [`market_state`] evaluates:
/// [`UTC_ZONES`] and [`NEW_YORK_ZONE`], matched without regard to case. `None` for any other zone.
fn zone_offset_seconds(zone: &str, now_s: i64) -> Option<i64> {
    if UTC_ZONES.iter().any(|utc| zone.eq_ignore_ascii_case(utc)) {
        Some(0)
    } else if zone.eq_ignore_ascii_case(NEW_YORK_ZONE) {
        new_york_offset_seconds(now_s)
    } else {
        None
    }
}

/// **Is `symbol`'s market open at `now_ms`?** Read from its `schedule` (intervals of seconds since Sunday 00:00
/// by the wall clock of `schedule_time_zone`), but only where that zone is spelled UTC (`UTC`, `Etc/UTC`, `GMT`,
/// `Etc/GMT`) or is `America/New_York` (daylight saving included, `new_york_offset_seconds`): any other zone, an
/// empty schedule or a clock before 1970 is [`MarketState::Unknown`], never a guess. On the Sunday a daylight
/// changeover falls on, the wall clock repeats or skips the hour from 01:00 to 03:00, where the forex week is
/// closed anyway; a schedule with an interval edge in that hour would be read up to an hour off. A holiday
/// (`ProtoOASymbol.holiday`) is not read: on one the market can be closed while the schedule says open, and the
/// stale-quote check then refuses at [`OPEN_MARKET_MAX_SPOT_AGE_MS`], the safe side.
#[must_use]
pub fn market_state(symbol: &ProtoOaSymbol, now_ms: i64) -> MarketState {
    if symbol.schedule.is_empty() || now_ms < 0 {
        return MarketState::Unknown;
    }
    let now_s = now_ms / 1_000;
    let Some(offset) = symbol
        .schedule_time_zone
        .as_deref()
        .and_then(|zone| zone_offset_seconds(zone.trim(), now_s))
    else {
        return MarketState::Unknown;
    };
    let second = u32::try_from((now_s + offset + EPOCH_SECONDS_PAST_SUNDAY).rem_euclid(604_800))
        .unwrap_or_default();
    // The interval's start is inclusive and its end exclusive (the field docs).
    if symbol.schedule.iter().any(|i| i.start_second <= second && second < i.end_second) {
        MarketState::Open
    } else {
        MarketState::Closed
    }
}

/// Refuse a rate read from a pair the broker has SUSPENDED (`ProtoOASymbol.tradingMode` disabled, with or
/// without pending execution): it stays listed and keeps its last price, which is no rate. `Enabled` and
/// `CLOSE_ONLY_MODE` (still quoted) pass, and so does a mode this crate does not know. `name` is the broker's.
#[must_use]
pub fn trading_refusal(symbol: &ProtoOaSymbol, name: &str) -> Option<CtraderRateRefusal> {
    let mode = symbol.trading_mode.and_then(|m| ProtoOaTradingMode::try_from(m).ok())?;
    matches!(
        mode,
        ProtoOaTradingMode::DisabledWithoutPendingsExecution
            | ProtoOaTradingMode::DisabledWithPendingsExecution
    )
    .then(|| CtraderRateRefusal::Suspended { name: name.to_string(), mode: mode.as_str_name() })
}

/// A span for a refusal sentence: whole hours from an hour up, seconds below.
fn span(ms: i64) -> String {
    if ms >= 3_600_000 { format!("{} h", ms / 3_600_000) } else { format!("{} s", ms / 1_000) }
}

/// Refuse a quote the venue itself stamped too long before `asked_ms`: more than
/// [`OPEN_MARKET_MAX_SPOT_AGE_MS`] while the market is open OR not known (fail closed: `schedule_zone` names the
/// zone this crate could not read, when there is one), more than [`MAX_SPOT_AGE_MS`] only when it is
/// demonstrably closed. A quote with NO timestamp is refused: the request asks for one
/// (`subscribeToSpotTimestamp`), but the page says only that `timestamp` is optional, and a price of unknown age
/// (a suspended pair's weeks-old last price) must not be stamped fresh at the ask. A stamp that is not a
/// millisecond epoch is refused in words too, never read as an age: below [`MIN_SPOT_MS_EPOCH`] (seconds, or
/// zero) and more than [`MAX_SPOT_AFTER_ASK_MS`] after the ask (microseconds, nanoseconds, a clock far ahead).
#[must_use]
pub fn spot_age_refusal(
    timestamp_ms: Option<i64>,
    asked_ms: i64,
    market: MarketState,
    schedule_zone: Option<&str>,
) -> Option<CtraderRateRefusal> {
    let Some(stamp) = timestamp_ms else {
        return Some(CtraderRateRefusal::Untimed);
    };
    if stamp < MIN_SPOT_MS_EPOCH {
        return Some(CtraderRateRefusal::StampInSeconds { stamp });
    }
    if stamp.saturating_sub(asked_ms) > MAX_SPOT_AFTER_ASK_MS {
        return Some(CtraderRateRefusal::StampAfterAsk {
            stamp,
            ahead: span(stamp.saturating_sub(asked_ms)),
            asked_ms,
        });
    }
    let limit = match market {
        MarketState::Closed => MAX_SPOT_AGE_MS,
        MarketState::Open | MarketState::Unknown => OPEN_MARKET_MAX_SPOT_AGE_MS,
    };
    let age = asked_ms.saturating_sub(stamp);
    (age > limit).then(|| {
        let (age, limit) = (span(age), span(limit));
        match (market, schedule_zone) {
            (MarketState::Unknown, Some(zone)) => {
                CtraderRateRefusal::StaleZoneNotUnderstood { age, zone: zone.to_string(), limit }
            }
            (MarketState::Unknown, None) => CtraderRateRefusal::StaleNoSchedule { age, limit },
            _ => CtraderRateRefusal::Stale { age, limit, market },
        }
    })
}

/// Collects the two sides of ONE symbol's quote over the spot events a subscription delivers: a
/// `ProtoOASpotEvent` may carry one side only (the first, "technical" one may), and a later one fills the other
/// in. Not a quote type of its own: the sides stay the WIRE's integers (1/100000 of a unit) because
/// [`rate_of_quote`] averages them BEFORE it divides by `RELATIVE_PRICE_SCALE`, and the mid of two descaled
/// `vike_marketdata::TickQuote` sides is another float (150.09 / 150.11 gives 150.10000000000002, not 150.1).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct QuoteSides {
    bid: Option<u64>,
    ask: Option<u64>,
    timestamp: Option<i64>,
}

impl QuoteSides {
    /// Fold one spot event in. An event for another symbol changes nothing.
    pub fn on_event(&mut self, event: &ProtoOaSpotEvent, symbol_id: i64) {
        if event.symbol_id != symbol_id {
            return;
        }
        self.bid = event.bid.or(self.bid);
        self.ask = event.ask.or(self.ask);
        self.timestamp = event.timestamp.or(self.timestamp);
    }

    /// `(bid, ask)` once both sides have been seen.
    #[must_use]
    pub fn two_sided(&self) -> Option<(u64, u64)> {
        Some((self.bid?, self.ask?))
    }

    /// `(bid, ask)` once both sides have been seen AND some event carried the venue's timestamp: an untimed
    /// quote is not complete (the first "technical" event may omit the stamp, and the next one may carry it).
    #[must_use]
    pub fn timed_two_sided(&self) -> Option<(u64, u64)> {
        self.timestamp.and(self.two_sided())
    }

    /// The venue's own timestamp on the latest event that carried one.
    #[must_use]
    pub fn timestamp_ms(&self) -> Option<i64> {
        self.timestamp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::symbols::SymbolLists;
    use crate::refusals::CtraderRateRefusalId;
    use crate::transport::proto::{
        ProtoOaAsset, ProtoOaAssetClass, ProtoOaLightSymbol, ProtoOaSymbolCategory,
    };
    use vike_utility::calendar::days_from_civil;

    /// A USD account's list as the demo broker publishes it (category 1 -> asset class `Forex`, category 3
    /// -> `Indices`): USDJPY (USD is the BASE), GBPUSD (the quote currency is the base), EURGBP (a cross),
    /// USDCHF, and a `JPYUSD` index CFD named like a pair but in the wrong class.
    fn usd_account(extra: Vec<ProtoOaLightSymbol>) -> SymbolMap {
        account_listing(16, extra)
    }

    /// [`usd_account`]'s list, on an account whose deposit is the asset `deposit_asset_id` (5 EUR, 16 USD).
    fn account_listing(deposit_asset_id: i64, extra: Vec<ProtoOaLightSymbol>) -> SymbolMap {
        let symbol = |id, name: &str, category, base, quote| ProtoOaLightSymbol {
            symbol_id: id,
            symbol_name: Some(name.to_string()),
            symbol_category_id: Some(category),
            base_asset_id: Some(base),
            quote_asset_id: Some(quote),
            ..Default::default()
        };
        let asset = |asset_id, name: &str| ProtoOaAsset {
            asset_id,
            name: name.to_string(),
            ..Default::default()
        };
        let class = |id, name: &str| ProtoOaAssetClass {
            id: Some(id),
            name: Some(name.to_string()),
            ..Default::default()
        };
        let category = |id, asset_class_id| ProtoOaSymbolCategory {
            id,
            asset_class_id,
            name: "Default Category".to_string(),
            ..Default::default()
        };
        let mut light = vec![
            symbol(1, "EURUSD", 1, 5, 16),
            symbol(2, "USDJPY", 1, 16, 7),
            symbol(3, "GBPUSD", 1, 8, 16),
            symbol(4, "EURGBP", 1, 5, 8),
            symbol(5, "USDCHF", 1, 16, 9),
            symbol(6, "JPYUSD", 3, 7, 16),
        ];
        light.extend(extra);
        SymbolMap::from_lists(&SymbolLists {
            light,
            categories: vec![category(1, 1), category(3, 3)],
            asset_classes: vec![class(1, "Forex"), class(3, "Indices")],
            assets: vec![
                asset(5, "EUR"),
                asset(7, "JPY"),
                asset(8, "GBP"),
                asset(9, "CHF"),
                asset(16, "USD"),
            ],
            deposit_asset_id: Some(deposit_asset_id),
            ..Default::default()
        })
    }

    /// Each leg of the route as `(symbol name, from, to, orientation)`.
    fn legs_of(route: &ConversionRoute<Conversion>) -> Vec<(&str, &str, &str, AccountPerQuoteLeg)> {
        route
            .legs()
            .iter()
            .map(|l| (l.pair.name.as_str(), l.from.as_str(), l.to.as_str(), l.orientation))
            .collect()
    }

    /// The ONE direct pair `quote` converts through on the map's own deposit currency.
    fn pick(map: &SymbolMap, quote: &str) -> Result<ConversionLeg<Conversion>, CtraderRateRefusal> {
        match conversion_route(map, map.deposit_currency().expect("a deposit"), quote)? {
            ConversionRoute::Direct(leg) => Ok(leg),
            cross => panic!("a direct pair, not {:?}", legs_of(&cross)),
        }
    }

    /// The conversion pair is chosen from the broker's own list by its base and quote assets, with the
    /// orientation of the ACCOUNT's side: USD is the base of USDJPY and the quote of GBPUSD.
    #[test]
    fn the_pair_is_found_by_its_assets_and_knows_which_side_the_account_is() {
        let map = usd_account(vec![]);
        let jpy = pick(&map, "JPY").expect("USDJPY");
        assert_eq!(
            (jpy.pair.name.as_str(), jpy.orientation),
            ("USDJPY", AccountPerQuoteLeg::AccountIsBase)
        );
        let gbp = pick(&map, "gbp").expect("GBPUSD");
        assert_eq!(
            (gbp.pair.name.as_str(), gbp.orientation),
            ("GBPUSD", AccountPerQuoteLeg::QuoteIsBase)
        );
        assert_eq!(
            pick(&map, "CHF").expect("USDCHF").orientation,
            AccountPerQuoteLeg::AccountIsBase
        );
        assert_eq!(map.id_of(&jpy.pair.name), Some(jpy.pair.symbol_id));
    }

    /// **A EUR deposit with no EURJPY converts a yen through USD** (decision 0213 as amended, owner 2026-11-22): the
    /// yen into dollars over USDJPY (the dollar is its BASE, so `1 / mid`), then the dollar into euros over EURUSD (the
    /// euro is its BASE, so `1 / mid` again); a franc likewise over USDCHF and EURUSD. A pound has its DIRECT pair,
    /// EURGBP (the euro is its base), and takes no detour. A currency with no pair of USD (NZD) is refused, naming the
    /// detour that was looked for too.
    #[test]
    fn a_euro_account_converts_a_yen_through_usd_by_two_pairs_and_a_pound_directly() {
        let map = account_listing(5, vec![]);
        let deposit = map.deposit_currency().expect("EUR").to_string();
        assert_eq!(deposit, "EUR");
        let jpy = conversion_route(&map, &deposit, "JPY").expect("a cross");
        assert!(matches!(jpy, ConversionRoute::ThroughHub(_)), "{:?}", legs_of(&jpy));
        assert_eq!(
            legs_of(&jpy),
            [
                ("USDJPY", "JPY", "USD", AccountPerQuoteLeg::AccountIsBase),
                ("EURUSD", "USD", "EUR", AccountPerQuoteLeg::AccountIsBase),
            ]
        );
        let chf = conversion_route(&map, &deposit, "CHF").expect("a cross");
        assert_eq!(
            legs_of(&chf),
            [
                ("USDCHF", "CHF", "USD", AccountPerQuoteLeg::AccountIsBase),
                ("EURUSD", "USD", "EUR", AccountPerQuoteLeg::AccountIsBase),
            ]
        );
        let gbp = pick(&map, "GBP").expect("EURGBP");
        assert_eq!(
            (gbp.pair.name.as_str(), gbp.orientation),
            ("EURGBP", AccountPerQuoteLeg::AccountIsBase)
        );
        let why = conversion_route(&map, &deposit, "NZD").expect_err("no NZD pair at all");
        assert_eq!(
            why,
            CtraderRateRefusal::NoPair {
                quote: "NZD".into(),
                account: "EUR".into(),
                through: " (nor a pair of each with USD)".into()
            }
        );
        assert_eq!(
            vike_model::QuoteRateError::from(why),
            vike_model::QuoteRateError::NoPair(
                "ctrader: the account's symbol list has no forex pair between NZD and the account's currency EUR \
                 (nor a pair of each with USD), so NZD has no rate"
                    .into()
            ),
            "the seam's kind and words: the venue's head, once"
        );
    }

    /// No pair on a USD account (EURGBP is not a USD pair, and USD is the hub: there is no detour), a pair in the wrong class
    /// (`JPYUSD` is an index CFD here: the class is the broker's word, never the name) and two pairs
    /// between the same currencies are each refused in words.
    #[test]
    fn no_pair_a_wrong_class_pair_and_two_pairs_are_refused_by_name() {
        let map = usd_account(vec![]);
        for currency in ["NZD", "ZAR"] {
            let why = pick(&map, currency).expect_err(currency);
            assert_eq!(
                why,
                CtraderRateRefusal::NoPair {
                    quote: currency.into(),
                    account: "USD".into(),
                    through: String::new()
                }
            );
        }
        // `JPYUSD` (category 3, Indices) is not a pair: USDJPY alone answers JPY, the CFD never counts.
        assert_eq!(pick(&map, "JPY").expect("one").pair.name, "USDJPY");

        let twin = ProtoOaLightSymbol {
            symbol_id: 7,
            symbol_name: Some("USDJPY.x".to_string()),
            symbol_category_id: Some(1),
            base_asset_id: Some(16),
            quote_asset_id: Some(7),
            ..Default::default()
        };
        let two = usd_account(vec![twin.clone()]);
        let many = |names: &str| CtraderRateRefusal::ManyPairs {
            count: 2,
            from: "JPY".into(),
            to: "USD".into(),
            names: names.into(),
        };
        let why = pick(&two, "JPY").expect_err("two pairs");
        assert_eq!(why, many("USDJPY, USDJPY.x"));

        // The same twin on ONE leg of a cross (a EUR deposit, JPY through USD) is refused alike, naming that leg.
        let why = conversion_route(&account_listing(5, vec![twin]), "EUR", "JPY").expect_err("two");
        assert_eq!(why, many("USDJPY, USDJPY.x"));

        // A pair listed BOTH ways round (a forex JPYUSD beside USDJPY) is the same guess, never a choice.
        let reverse = ProtoOaLightSymbol {
            symbol_id: 8,
            symbol_name: Some("JPYUSD.fx".to_string()),
            symbol_category_id: Some(1),
            base_asset_id: Some(7),
            quote_asset_id: Some(16),
            ..Default::default()
        };
        let why = pick(&usd_account(vec![reverse]), "JPY").expect_err("both ways");
        assert_eq!(why, many("JPYUSD.fx, USDJPY"));
    }

    /// An empty currency is never the account's: no pair is chosen for it.
    #[test]
    fn an_empty_currency_chooses_nothing() {
        let map = usd_account(vec![]);
        assert!(matches!(
            conversion_route(&map, "", "JPY"),
            Err(CtraderRateRefusal::EmptyCurrency)
        ));
        assert!(matches!(
            conversion_route(&map, "USD", "  "),
            Err(CtraderRateRefusal::EmptyCurrency)
        ));
    }

    /// **The orientation, by value.** USDJPY at 150.10 (the account is the BASE): a yen is a FRACTION of a
    /// dollar. 100,000 units are 15,010,000 yen = 100,000 dollars, and a 0.10 yen move on them is 10,000 yen
    /// = 66.62 dollars, NOT 10,000.
    #[test]
    fn a_yen_quote_on_a_usd_account_is_a_fraction_of_a_dollar_per_yen() {
        // 150.09 / 150.11 in the wire's 1/100000: mid 150.10
        let r = rate_of_quote(AccountPerQuoteLeg::AccountIsBase, 15_009_000, 15_011_000)
            .expect("a quote");
        assert!(
            r.get() < 0.01 && r.get() > 0.006,
            "{} is yen per dollar, the flipped rate",
            r.get()
        );
        assert!((r.to_account(100_000.0 * 150.10) - 100_000.0).abs() < 1e-6);
        assert!((r.to_account(100_000.0 * 0.10) - 66.62).abs() < 0.01);
    }

    /// GBPUSD at 1.35 (the quote currency is the BASE): a pound is MORE than a dollar. EURGBP at 0.85,
    /// 100,000 units: 85,000 pounds = 114,750 dollars (understated 1.35x if left raw).
    #[test]
    fn a_pound_quote_on_a_usd_account_is_more_than_one_dollar_per_pound() {
        let r = rate_of_quote(AccountPerQuoteLeg::QuoteIsBase, 134_999, 135_001).expect("a quote");
        assert!(r.get() > 1.0, "{} is pounds per dollar, the flipped rate", r.get());
        assert!((r.to_account(85_000.0) - 114_750.0).abs() < 1e-6);
    }

    /// The two legs are each other's inverse on the same quote: a flipped leg is the 22,500x bug.
    #[test]
    fn the_two_legs_of_one_quote_are_reciprocal() {
        let (bid, ask) = (15_009_000, 15_011_000);
        let a = rate_of_quote(AccountPerQuoteLeg::AccountIsBase, bid, ask).expect("a").get();
        let b = rate_of_quote(AccountPerQuoteLeg::QuoteIsBase, bid, ask).expect("b").get();
        assert!((a * b - 1.0).abs() < 1e-12, "{a} * {b}");
    }

    /// A zero side (either), a crossed quote and a locked-but-sane quote: only the sane one is a rate.
    #[test]
    fn a_zero_or_crossed_quote_is_no_rate_and_a_locked_one_is() {
        for (bid, ask, refusal) in [
            (0, 135_001, CtraderRateRefusal::ZeroSide { bid: 0, ask: 135_001 }),
            (134_999, 0, CtraderRateRefusal::ZeroSide { bid: 134_999, ask: 0 }),
            (0, 0, CtraderRateRefusal::ZeroSide { bid: 0, ask: 0 }),
            (135_002, 135_001, CtraderRateRefusal::Crossed { bid: 135_002, ask: 135_001 }),
        ] {
            for leg in [AccountPerQuoteLeg::AccountIsBase, AccountPerQuoteLeg::QuoteIsBase] {
                assert_eq!(
                    rate_of_quote(leg, bid, ask),
                    Err(refusal.clone()),
                    "{bid}/{ask} {leg:?}"
                );
            }
        }
        assert_eq!(
            rate_of_quote(AccountPerQuoteLeg::QuoteIsBase, 135_000, 135_000).expect("locked").get(),
            1.35
        );
    }

    #[test]
    fn a_rate_cannot_be_built_from_a_non_rate() {
        for bad in [0.0, -1.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(AccountPerQuote::new(bad), None, "{bad}");
        }
        assert_eq!(AccountPerQuote::new(0.0066).map(AccountPerQuote::get), Some(0.0066));
        assert_eq!(par("  ", 5), None, "an empty code is no currency");
        let (rate, account_ccy) = par(" usd ", 5).expect("a code");
        assert_eq!(
            (
                rate.quote_currency.as_str(),
                account_ccy.as_str(),
                rate.account_per_quote.get(),
                rate.as_of_ms
            ),
            ("USD", "USD", 1.0, 5)
        );
    }

    /// The first ("technical") event may carry one side; the quote is usable only when both were seen, for
    /// THE symbol asked, and the venue's timestamp is the latest one stated.
    #[test]
    fn a_quote_is_two_sided_only_after_both_sides_arrived_for_that_symbol() {
        let event = |symbol_id, bid, ask, timestamp| ProtoOaSpotEvent {
            ctid_trader_account_id: 99,
            symbol_id,
            bid,
            ask,
            timestamp,
            ..Default::default()
        };
        let mut sides = QuoteSides::default();
        sides.on_event(&event(2, Some(15_009_000), None, Some(10)), 2);
        assert_eq!(sides.two_sided(), None, "one side is no quote");
        sides.on_event(&event(3, Some(1), Some(2), Some(99)), 2);
        assert_eq!(sides.two_sided(), None, "another symbol's event changes nothing");
        sides.on_event(&event(2, None, Some(15_011_000), Some(11)), 2);
        assert_eq!(sides.two_sided(), Some((15_009_000, 15_011_000)));
        assert_eq!(sides.timestamp_ms(), Some(11));
        sides.on_event(&event(2, Some(15_010_000), None, None), 2);
        assert_eq!(
            sides.two_sided(),
            Some((15_010_000, 15_011_000)),
            "a later bid replaces the old"
        );
        assert_eq!(sides.timestamp_ms(), Some(11), "an event without a stamp keeps the last");
    }

    const ASKED: i64 = 1_700_000_000_000;

    /// A quote the venue stamped days ago is no rate; a weekend-old one is when the market is demonstrably
    /// CLOSED; a stamp after the ask (clock skew, a tick of the wait) is not old.
    #[test]
    fn an_old_quote_is_refused_and_a_weekend_old_one_is_not() {
        let hours = |h: i64| ASKED - h * 3_600_000;
        let market = MarketState::Closed;
        assert_eq!(spot_age_refusal(Some(hours(48)), ASKED, market, None), None, "a weekend");
        assert_eq!(spot_age_refusal(Some(hours(72)), ASKED, market, None), None);
        let why = spot_age_refusal(Some(hours(73)), ASKED, market, None).expect("too old");
        assert_eq!(
            why,
            CtraderRateRefusal::Stale { age: "73 h".into(), limit: "72 h".into(), market }
        );
        for market in [MarketState::Closed, MarketState::Open, MarketState::Unknown] {
            assert_eq!(spot_age_refusal(Some(ASKED + 5_000), ASKED, market, None), None);
        }
    }

    /// **A market this crate cannot read gets the STRICT bound, never the weekend one**: a frozen conversion
    /// pair 20 h old on a broker whose schedule zone is `Europe/London` (not evaluated without a time-zone
    /// database) is refused in words naming the zone, where the old 72 h bound let its rate through, stamped
    /// fresh at the ask. A quote inside the strict bound still passes, and a UTC schedule is unchanged.
    #[test]
    fn an_unknown_market_is_held_to_the_strict_bound_and_the_zone_is_named() {
        let ago = |ms: i64| ASKED - ms;
        let zone = Some("Europe/London");
        let why = spot_age_refusal(Some(ago(20 * 3_600_000)), ASKED, MarketState::Unknown, zone)
            .expect("a frozen price on a market that cannot be read");
        assert_eq!(
            why,
            CtraderRateRefusal::StaleZoneNotUnderstood {
                age: "20 h".into(),
                zone: "Europe/London".into(),
                limit: "120 s".into()
            }
        );
        assert_eq!(spot_age_refusal(Some(ago(120_000)), ASKED, MarketState::Unknown, zone), None);
        assert!(spot_age_refusal(Some(ago(121_000)), ASKED, MarketState::Unknown, zone).is_some());
        // No schedule at all: the same strict bound, worded without a zone.
        let why = spot_age_refusal(Some(ago(48 * 3_600_000)), ASKED, MarketState::Unknown, None)
            .expect("no schedule, no weekend allowance");
        assert_eq!(
            why,
            CtraderRateRefusal::StaleNoSchedule { age: "48 h".into(), limit: "120 s".into() }
        );
        // A market that IS read keeps its bounds: closed allows the weekend, open the window.
        assert_eq!(
            spot_age_refusal(Some(ago(20 * 3_600_000)), ASKED, MarketState::Closed, zone),
            None
        );
        assert!(
            spot_age_refusal(Some(ago(20 * 3_600_000)), ASKED, MarketState::Open, zone).is_some()
        );
    }

    /// The zone a refusal names is the symbol's own, and only when it states a schedule for it.
    #[test]
    fn the_schedule_zone_is_named_only_with_a_schedule() {
        let week = [(86_400, 5 * 86_400)];
        assert_eq!(
            schedule_zone(&schedule_symbol(Some(" Europe/London "), &week)),
            Some("Europe/London")
        );
        assert_eq!(schedule_zone(&schedule_symbol(Some("UTC"), &[])), None);
        assert_eq!(schedule_zone(&schedule_symbol(None, &week)), None);
        assert_eq!(schedule_zone(&schedule_symbol(Some("  "), &week)), None);
    }

    /// **The unit guard is two-sided**: a seconds-valued stamp reads as 1970, a MICROsecond or NANOsecond one as
    /// the far future (a negative age that would pass every age check as fresher than fresh), and both are
    /// refused in words; ten seconds past the ask is the most a live tick can be stamped.
    #[test]
    fn a_microsecond_or_nanosecond_stamp_is_refused_not_read_as_fresh() {
        for stamp in [ASKED * 1_000, ASKED * 1_000_000, ASKED + MAX_SPOT_AFTER_ASK_MS + 1, i64::MAX]
        {
            for market in [MarketState::Open, MarketState::Closed, MarketState::Unknown] {
                let why = spot_age_refusal(Some(stamp), ASKED, market, None).expect("not millis");
                assert_eq!(why.id(), CtraderRateRefusalId::StampAfterAsk, "{stamp}: {why}");
            }
        }
        for stamp in [ASKED + MAX_SPOT_AFTER_ASK_MS, ASKED, ASKED - 1_000] {
            assert_eq!(spot_age_refusal(Some(stamp), ASKED, MarketState::Open, None), None);
        }
    }

    /// **A market that is open allows the engine's window and no more**: a price two minutes old is a rate,
    /// one 121 s old is not, and a weekend-old one is refused outright while the schedule says open.
    #[test]
    fn an_open_market_allows_the_engines_window_and_no_more() {
        let ago = |ms: i64| ASKED - ms;
        assert_eq!(OPEN_MARKET_MAX_SPOT_AGE_MS, 120_000);
        assert_eq!(spot_age_refusal(Some(ago(120_000)), ASKED, MarketState::Open, None), None);
        let why =
            spot_age_refusal(Some(ago(121_000)), ASKED, MarketState::Open, None).expect("stale");
        assert_eq!(
            why,
            CtraderRateRefusal::Stale {
                age: "121 s".into(),
                limit: "120 s".into(),
                market: MarketState::Open
            }
        );
        assert!(
            spot_age_refusal(Some(ago(48 * 3_600_000)), ASKED, MarketState::Open, None).is_some()
        );
    }

    /// **A quote with no timestamp is no rate, in words**, whatever the market says: its age is unknown and
    /// the rate is stamped fresh at the ask.
    #[test]
    fn an_untimed_quote_is_refused_in_words() {
        for market in [MarketState::Open, MarketState::Closed, MarketState::Unknown] {
            let why = spot_age_refusal(None, ASKED, market, None).expect("untimed");
            assert_eq!(why, CtraderRateRefusal::Untimed);
        }
    }

    /// **The unit is ASSUMED milliseconds, and a seconds-valued stamp is refused loudly**: a current stamp in
    /// seconds (about 1.7e9) is not read as a 55-year-old price, nor as a fresh one; zero and negative stamps
    /// go the same way.
    #[test]
    fn a_seconds_valued_stamp_is_refused_loudly_not_read_as_an_age() {
        for stamp in [ASKED / 1_000, 1, 0, -5, MIN_SPOT_MS_EPOCH - 1] {
            for market in [MarketState::Open, MarketState::Unknown] {
                let why =
                    spot_age_refusal(Some(stamp), ASKED, market, None).expect("not milliseconds");
                assert_eq!(why, CtraderRateRefusal::StampInSeconds { stamp });
            }
        }
        assert_eq!(
            spot_age_refusal(Some(MIN_SPOT_MS_EPOCH), MIN_SPOT_MS_EPOCH, MarketState::Open, None),
            None
        );
    }

    fn schedule_symbol(zone: Option<&str>, intervals: &[(u32, u32)]) -> ProtoOaSymbol {
        ProtoOaSymbol {
            symbol_id: 2,
            schedule_time_zone: zone.map(str::to_string),
            schedule: intervals
                .iter()
                .map(|(start_second, end_second)| crate::transport::proto::ProtoOaInterval {
                    start_second: *start_second,
                    end_second: *end_second,
                })
                .collect(),
            ..Default::default()
        }
    }

    /// 2023-11-14 22:13:20 UTC (`ASKED`) is a Tuesday: 2 days 22 h 13 m 20 s past Sunday 00:00 UTC.
    const TUESDAY_SECOND: u32 = 2 * 86_400 + 22 * 3_600 + 13 * 60 + 20;

    /// The market is read from the symbol's OWN schedule, here in UTC (New York's tests follow): inside an
    /// interval it is open (start inclusive, end exclusive), outside every one closed, and a zone this crate
    /// does not read, no zone or no schedule is unknown, never a guess. Sunday 00:00 is second 0 and the epoch
    /// (a Thursday) is 4 days past it.
    #[test]
    fn the_market_is_read_from_the_symbols_utc_schedule_and_nothing_else() {
        // Monday 00:00 .. Friday 22:00 UTC, the usual forex week.
        const MONDAY: u32 = 86_400;
        const FRIDAY_CLOSE: u32 = 5 * 86_400 + 22 * 3_600;
        let week = [(MONDAY, FRIDAY_CLOSE)];
        let open = schedule_symbol(Some("UTC"), &week);
        assert_eq!(market_state(&open, ASKED), MarketState::Open, "a Tuesday evening");
        // The Sunday 00:00 UTC before `ASKED`, so a second of the week maps to an instant.
        let sunday = ASKED - i64::from(TUESDAY_SECOND) * 1_000;
        let at = |second: u32| sunday + i64::from(second) * 1_000;
        assert_eq!(market_state(&open, at(MONDAY - 1)), MarketState::Closed, "Sunday night");
        assert_eq!(market_state(&open, at(MONDAY)), MarketState::Open, "start inclusive");
        assert_eq!(market_state(&open, at(FRIDAY_CLOSE - 1)), MarketState::Open);
        assert_eq!(market_state(&open, at(FRIDAY_CLOSE)), MarketState::Closed, "end exclusive");
        assert_eq!(market_state(&open, at(6 * 86_400)), MarketState::Closed, "Saturday");
        assert_eq!(market_state(&open, at(7 * 86_400 + MONDAY)), MarketState::Open, "next week");
        for zone in ["utc", " Etc/UTC ", "GMT"] {
            assert_eq!(market_state(&schedule_symbol(Some(zone), &week), ASKED), MarketState::Open);
        }
        for unknown in [
            schedule_symbol(Some("Europe/Berlin"), &week),
            schedule_symbol(None, &week),
            schedule_symbol(Some("UTC"), &[]),
        ] {
            assert_eq!(market_state(&unknown, ASKED), MarketState::Unknown, "{unknown:?}");
        }
        assert_eq!(market_state(&open, -1), MarketState::Unknown);
    }

    /// Epoch seconds of a UTC civil instant.
    fn utc(year: i64, month: u32, day: u32, hour: i64, minute: i64, second: i64) -> i64 {
        days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second
    }

    /// **New York is UTC-4 from the second Sunday of March 02:00 local (07:00 UTC) to the first Sunday of
    /// November 02:00 local (06:00 UTC), UTC-5 outside it**, to the second, in three years that put the Sundays
    /// on different dates (2023, 2026, 2027); there is no rule before 2007 and none is guessed.
    #[test]
    fn new_york_follows_the_us_daylight_saving_rule_to_the_second() {
        assert_eq!(
            utc(2023, 11, 14, 22, 13, 20),
            ASKED / 1_000,
            "the helper reads the same instant"
        );
        let (edt, est) = (-4 * 3_600, -5 * 3_600);
        for (instant, want) in [
            (utc(2026, 1, 15, 12, 0, 0), est),
            (utc(2026, 3, 8, 6, 59, 59), est),
            (utc(2026, 3, 8, 7, 0, 0), edt),
            (utc(2026, 10, 23, 23, 59, 5), edt),
            (utc(2026, 11, 1, 5, 59, 59), edt),
            (utc(2026, 11, 1, 6, 0, 0), est),
            (utc(2023, 3, 12, 6, 59, 59), est),
            (utc(2023, 3, 12, 7, 0, 0), edt),
            (utc(2023, 11, 5, 5, 59, 59), edt),
            (utc(2023, 11, 5, 6, 0, 0), est),
            (utc(2027, 3, 14, 6, 59, 59), est),
            (utc(2027, 3, 14, 7, 0, 0), edt),
            (utc(2027, 11, 7, 5, 59, 59), edt),
            (utc(2027, 11, 7, 6, 0, 0), est),
            // The rule's first year: the first second of 2007 and both 2007 changeovers.
            (utc(2007, 1, 1, 0, 0, 0), est),
            (utc(2007, 3, 11, 6, 59, 59), est),
            (utc(2007, 3, 11, 7, 0, 0), edt),
            (utc(2007, 11, 4, 5, 59, 59), edt),
            (utc(2007, 11, 4, 6, 0, 0), est),
        ] {
            assert_eq!(new_york_offset_seconds(instant), Some(want), "{instant}");
        }
        assert_eq!(new_york_offset_seconds(utc(2006, 7, 5, 12, 0, 0)), None);
        // 2006 is refused whole: its last second, and its own changeovers (the first Sunday of April and the
        // last Sunday of October) as well as the dates the 2007 rule would have switched on.
        for instant in [
            utc(2006, 12, 31, 23, 59, 59),
            utc(2006, 4, 2, 7, 0, 0),
            utc(2006, 10, 29, 6, 0, 0),
            utc(2006, 3, 12, 7, 0, 0),
            utc(2006, 11, 5, 6, 0, 0),
        ] {
            assert_eq!(new_york_offset_seconds(instant), None, "{instant}");
        }
        assert_eq!(new_york_offset_seconds(-1), None);
    }

    /// **A New York schedule is read by the New York wall clock, across both changeovers.** The forex week, Sunday
    /// 17:00 to Friday 17:00 New York time: it closes at 21:00 UTC under daylight time and at 22:00 UTC under
    /// standard time, and a fixed offset of either kind would be an hour out on one side of each changeover (the
    /// Sunday re-open of 2026-11-01 at 22:00 UTC, not 21:00; of 2026-03-08 at 21:00 UTC, not 22:00).
    #[test]
    fn a_new_york_schedule_is_read_by_the_new_york_wall_clock_across_both_changeovers() {
        const REOPEN: u32 = 17 * 3_600;
        const CLOSE: u32 = 5 * 86_400 + 17 * 3_600;
        let week = schedule_symbol(Some("America/New_York"), &[(REOPEN, CLOSE)]);
        for (instant, want, what) in [
            (utc(2026, 10, 23, 20, 59, 59), MarketState::Open, "Friday 16:59:59 EDT"),
            (utc(2026, 10, 23, 21, 0, 0), MarketState::Closed, "Friday 17:00 EDT, end exclusive"),
            (utc(2026, 10, 23, 23, 59, 5), MarketState::Closed, "the v0.1.84 boot"),
            (utc(2026, 10, 25, 20, 59, 59), MarketState::Closed, "Sunday 16:59:59 EDT"),
            (utc(2026, 10, 25, 21, 0, 0), MarketState::Open, "Sunday 17:00 EDT, start inclusive"),
            (utc(2026, 11, 1, 21, 59, 59), MarketState::Closed, "changeover Sunday 16:59:59 EST"),
            (utc(2026, 11, 1, 22, 0, 0), MarketState::Open, "changeover Sunday 17:00 EST"),
            (utc(2026, 11, 6, 21, 59, 59), MarketState::Open, "Friday 16:59:59 EST"),
            (utc(2026, 11, 6, 22, 0, 0), MarketState::Closed, "Friday 17:00 EST"),
            (utc(2026, 3, 6, 21, 59, 59), MarketState::Open, "Friday 16:59:59 EST"),
            (utc(2026, 3, 6, 22, 0, 0), MarketState::Closed, "Friday 17:00 EST"),
            (utc(2026, 3, 8, 20, 59, 59), MarketState::Closed, "changeover Sunday 16:59:59 EDT"),
            (utc(2026, 3, 8, 21, 0, 0), MarketState::Open, "changeover Sunday 17:00 EDT"),
        ] {
            assert_eq!(market_state(&week, instant * 1_000), want, "{what}");
        }
    }

    /// **Only UTC and New York are read; any other zone stays UNKNOWN** (and so on the strict bound, refused in
    /// words naming it): a US neighbour of New York, an alias of it, a near-miss spelling and an empty name; a
    /// New York schedule with no intervals, and a clock before the rule's first year, are unknown too. The
    /// spelling of New York is trimmed and not case-sensitive, like the UTC names.
    #[test]
    fn a_zone_other_than_utc_and_new_york_is_still_not_read() {
        let week = [(86_400, 5 * 86_400)];
        for zone in [
            "Europe/London",
            "Europe/Berlin",
            "America/Chicago",
            "America/Toronto",
            "US/Eastern",
            "EST5EDT",
            "America/New_York2",
            "New_York",
            "",
        ] {
            assert_eq!(
                market_state(&schedule_symbol(Some(zone), &week), ASKED),
                MarketState::Unknown,
                "{zone:?}"
            );
        }
        for zone in ["America/New_York", " america/new_york ", "AMERICA/NEW_YORK"] {
            // `ASKED` is a Tuesday 17:13 New York time (standard time), inside Monday 00:00 to Friday 00:00.
            assert_eq!(
                market_state(&schedule_symbol(Some(zone), &week), ASKED),
                MarketState::Open,
                "{zone:?}"
            );
        }
        let york = |intervals: &[(u32, u32)]| schedule_symbol(Some("America/New_York"), intervals);
        assert_eq!(market_state(&york(&[]), ASKED), MarketState::Unknown, "no schedule");
        assert_eq!(
            market_state(&york(&week), utc(2006, 7, 5, 12, 0, 0) * 1_000),
            MarketState::Unknown,
            "before the 2007 rule"
        );
    }

    /// **The case the CI box refused at the v0.1.84 boot**: Friday 2026-10-23 23:59:05 UTC, the pair's last price 3 h
    /// old and its schedule zone New York. The market reads CLOSED, so the price is a rate; the same age on the
    /// open week (Monday noon UTC), and a closed-market price past 72 h, are still refused, and so is the same
    /// price with the zone another one (the strict bound, naming the zone).
    #[test]
    fn a_three_hour_old_price_on_friday_night_new_york_time_is_a_rate_and_only_then() {
        const REOPEN: u32 = 17 * 3_600;
        const CLOSE: u32 = 5 * 86_400 + 17 * 3_600;
        let ask = |symbol: &ProtoOaSymbol, asked_s: i64, age_h: i64| {
            let asked_ms = asked_s * 1_000;
            spot_age_refusal(
                Some(asked_ms - age_h * 3_600_000),
                asked_ms,
                market_state(symbol, asked_ms),
                schedule_zone(symbol),
            )
        };
        let york = schedule_symbol(Some("America/New_York"), &[(REOPEN, CLOSE)]);
        let friday_night = utc(2026, 10, 23, 23, 59, 5);
        assert_eq!(ask(&york, friday_night, 3), None, "closed: the last price stands");
        assert_eq!(ask(&york, friday_night, 72), None, "closed: up to 72 h");
        assert!(ask(&york, friday_night, 73).is_some(), "closed: past 72 h");
        let monday_noon = utc(2026, 10, 26, 12, 0, 0);
        let why = ask(&york, monday_noon, 3).expect("open: a 3 h old price is stale");
        assert!(
            matches!(&why, CtraderRateRefusal::Stale { market: MarketState::Open, .. }),
            "{why}"
        );
        let london = schedule_symbol(Some("Europe/London"), &[(REOPEN, CLOSE)]);
        let why = ask(&london, friday_night, 3).expect("a zone that is not read");
        assert!(
            matches!(&why, CtraderRateRefusal::StaleZoneNotUnderstood { zone, .. } if zone == "Europe/London"),
            "{why}"
        );
    }

    /// A suspended pair keeps its last price and stays listed: refused by name. Enabled, close-only and an
    /// absent mode are rates.
    #[test]
    fn a_suspended_pair_is_refused_and_an_enabled_one_is_not() {
        let mode = |m: Option<ProtoOaTradingMode>| ProtoOaSymbol {
            symbol_id: 2,
            trading_mode: m.map(|m| m as i32),
            ..Default::default()
        };
        for ok in [None, Some(ProtoOaTradingMode::Enabled), Some(ProtoOaTradingMode::CloseOnlyMode)]
        {
            assert_eq!(trading_refusal(&mode(ok), "USDJPY"), None, "{ok:?}");
        }
        for off in [
            ProtoOaTradingMode::DisabledWithoutPendingsExecution,
            ProtoOaTradingMode::DisabledWithPendingsExecution,
        ] {
            let why = trading_refusal(&mode(Some(off)), "USDJPY").expect("suspended");
            assert_eq!(
                why,
                CtraderRateRefusal::Suspended { name: "USDJPY".into(), mode: off.as_str_name() }
            );
        }
        let unknown = ProtoOaSymbol { symbol_id: 2, trading_mode: Some(99), ..Default::default() };
        assert_eq!(trading_refusal(&unknown, "USDJPY"), None, "a mode this crate does not know");
    }

    /// The quote is complete only with a timestamp: one whose events carried none is not, and the next
    /// event that does completes it.
    #[test]
    fn a_quote_is_timed_only_once_an_event_carried_a_stamp() {
        let event = |bid, ask, timestamp| ProtoOaSpotEvent {
            ctid_trader_account_id: 99,
            symbol_id: 2,
            bid,
            ask,
            timestamp,
            ..Default::default()
        };
        let mut sides = QuoteSides::default();
        sides.on_event(&event(Some(15_009_000), Some(15_011_000), None), 2);
        assert_eq!(sides.two_sided(), Some((15_009_000, 15_011_000)));
        assert_eq!(sides.timed_two_sided(), None, "two sides and no stamp");
        sides.on_event(&event(None, None, Some(ASKED)), 2);
        assert_eq!(sides.timed_two_sided(), Some((15_009_000, 15_011_000)));
    }
}
