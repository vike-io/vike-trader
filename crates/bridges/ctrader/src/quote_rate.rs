//! The cTrader RATE SOURCE for the engine's multi-currency money (decision 0183, phase 1): what one unit
//! of an instrument's QUOTE currency is worth in the ACCOUNT's deposit currency, read from the mid of the
//! conversion pair's own quote on the account's data session.
//!
//! This module is the PURE half (types, the choice of the conversion symbol, the orientation, the quote
//! checks); the session that asks the venue is `crate::conn::fetch_conversion_quote`, and the gateway's
//! facts call it through `crate::CtraderCatalog::quote_rate`. Nothing here touches the engine: the node
//! turns a [`QuoteRate`] into the engine's per-currency rate.
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
//! constructor refuses a non-rate) and in [`Leg`], and the goldens below name which way they point.
//!
//! # Fail closed
//!
//! No conversion pair, two of them, a one-sided, zero, crossed, non-finite, untimed or old quote, a pair the
//! broker suspended, a session that failed or ran past its deadline: each is a [`RateError`]. No path returns
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

use std::fmt;

use crate::catalog::{SymbolKind, listed_kind};
use crate::event_mapper::RELATIVE_PRICE_SCALE;
use crate::proto::{ProtoOaSpotEvent, ProtoOaSymbol, ProtoOaTradingMode};
use crate::symbols::SymbolMap;

/// How old the venue's own timestamp on a quote may be when the market is demonstrably CLOSED
/// ([`MarketState::Closed`]): a weekend is about 48 h and the first spot after a subscription is the latest
/// price "even if market is closed", so this has to outlast a closed market and a bank holiday and still catch
/// a frozen price. A judgement call, UNMEASURED.
///
/// shortcut: [`market_state`] reads the symbol's own `schedule` only where the schedule's time zone is spelled
/// UTC (no time-zone database is linked into an order-signing binary for this). For any other zone, or no
/// schedule, the market is [`MarketState::Unknown`] and the STRICT bound [`OPEN_MARKET_MAX_SPOT_AGE_MS`] stands
/// (never this one: a feed frozen for less than 72 h on an open market would pass, with the rate stamped fresh
/// at the ask), so on such a broker the rate is refused over a weekend rather than guessed. Upgrade (resolve the
/// zone) once the live smoke's `CTSYM` line shows which zone names the broker really sends.
pub const MAX_SPOT_AGE_MS: i64 = 72 * 3_600 * 1_000;

/// How old the venue's own timestamp on a quote may be while the market is demonstrably OPEN: the engine's own
/// staleness window (a rate the engine would call stale a minute after it is taken is no rate to hand it). The
/// rate is stamped at the ASK, so the engine's window runs from the ask, on top of this.
pub const OPEN_MARKET_MAX_SPOT_AGE_MS: i64 = vike_exec::DEFAULT_QUOTE_RATE_TTL_MS;

/// How far AFTER the ask the venue's stamp on a quote may lie: the quote is read for up to
/// `conn::SPOT_WAIT` (five seconds) after the subscription, so a live tick can be stamped that late, plus five
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

/// **Account-currency units per ONE unit of a quote currency.** Multiply a quote-currency amount by it
/// ([`Self::to_account`]); never divide. Finite and strictly positive, or it does not exist.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccountPerQuote(f64);

impl AccountPerQuote {
    /// The account currency converting into itself. The only literal `1.0` in this module.
    const IDENTITY: Self = Self(1.0);

    /// `None` unless `account_units_per_quote_unit` is finite and greater than zero.
    #[must_use]
    pub fn new(account_units_per_quote_unit: f64) -> Option<Self> {
        (account_units_per_quote_unit.is_finite() && account_units_per_quote_unit > 0.0)
            .then_some(Self(account_units_per_quote_unit))
    }

    /// The factor: account-currency units per one quote-currency unit.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }

    /// A quote-currency amount in account currency (`quote_amount * factor`).
    #[must_use]
    pub fn to_account(self, quote_amount: f64) -> f64 {
        quote_amount * self.0
    }
}

/// What the node hands the engine for ONE quote currency: the scalar and when it was asked.
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteRate {
    /// The quote currency (upper-case).
    pub quote_ccy: String,
    /// The account's deposit currency THE SESSION THAT PRICED THIS READ (upper-case): the currency
    /// `account_per_quote` is in. The caller holds a listing of its own (kept minutes) with a deposit
    /// currency, and refuses a rate whose currency is another one: credentials swapped under one label would
    /// otherwise book a rate for one account on another's ledger.
    pub account_ccy: String,
    /// Account-currency units per one `quote_ccy` unit.
    pub account_per_quote: AccountPerQuote,
    /// When the rate was asked, epoch ms: the moment the subscription was SENT, not when the quote came
    /// back, so the rate is no older than this says (the conservative side; OANDA's rate is stamped alike).
    pub as_of_ms: i64,
}

impl QuoteRate {
    /// The account's own currency converting into itself (no pair, no quote).
    #[must_use]
    pub fn par(account_ccy: &str, as_of_ms: i64) -> Self {
        Self {
            quote_ccy: account_ccy.trim().to_ascii_uppercase(),
            account_ccy: account_ccy.trim().to_ascii_uppercase(),
            account_per_quote: AccountPerQuote::IDENTITY,
            as_of_ms,
        }
    }
}

/// Why no rate: every variant is a refusal, none carries a number to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateError {
    /// The session could not be made or failed (no credentials, a transport or venue error, no deposit
    /// currency named, no quote in time). Carries the reason; never a token.
    Unavailable(String),
    /// The account's symbol list holds no single forex pair between the two currencies (none, or two).
    NoPair(String),
    /// A quote that cannot be used: a side absent or zero, crossed, non-finite, untimed, stamped in an
    /// unknown unit, or older than [`OPEN_MARKET_MAX_SPOT_AGE_MS`] / [`MAX_SPOT_AGE_MS`] (by the market's
    /// state); or a pair the broker has suspended.
    BadQuote(String),
}

impl fmt::Display for RateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(why) | Self::NoPair(why) | Self::BadQuote(why) => {
                write!(f, "ctrader: {why}")
            }
        }
    }
}

impl std::error::Error for RateError {}

/// Which side of the conversion pair the ACCOUNT's currency is, which fixes the orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leg {
    /// The pair is `ACCOUNT/QUOTE` (USDJPY on a USD account): its price is quote units per account unit,
    /// so the rate is `1 / mid`.
    AccountIsBase,
    /// The pair is `QUOTE/ACCOUNT` (GBPUSD on a USD account): its price is account units per quote unit,
    /// so the rate is `mid`.
    QuoteIsBase,
}

/// The one symbol of the account's list that converts `quote` into `account`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
    /// The symbol id on THIS account's server (ids are per server, so it is never stored).
    pub symbol_id: i64,
    /// The broker's name for it (case kept).
    pub name: String,
    /// Which way round the pair is.
    pub leg: Leg,
}

/// **The conversion pair of `quote` into `account`** in the account's own symbol list: a symbol of the broker's
/// FOREX asset class (never a name guess) whose base and quote assets are the two currencies, in either order.
/// Exactly one, or a refusal: none (a currency the broker lists no direct pair for; a cross through a third
/// currency is not composed) and two (which of them the rate is read from would be a guess) are each named.
/// An empty currency is never the account's.
pub fn conversion_symbol(
    map: &SymbolMap,
    account: &str,
    quote: &str,
) -> Result<Conversion, RateError> {
    let (account, quote) = (account.trim(), quote.trim());
    if account.is_empty() || quote.is_empty() {
        return Err(RateError::Unavailable(
            "the account's currency or the quote currency is empty, so no conversion pair can be chosen"
                .into(),
        ));
    }
    let mut found: Vec<Conversion> = map
        .ids()
        .into_iter()
        .filter_map(|id| {
            let name = map.name_of(id).filter(|n| !n.is_empty())?;
            let forex = map.category(id).and_then(|c| c.asset_class).and_then(listed_kind)
                == Some(SymbolKind::Forex);
            let (base, quoted) = (map.base_asset(id)?, map.quote_asset(id)?);
            let leg = if !forex {
                return None;
            } else if base.eq_ignore_ascii_case(account) && quoted.eq_ignore_ascii_case(quote) {
                Leg::AccountIsBase
            } else if base.eq_ignore_ascii_case(quote) && quoted.eq_ignore_ascii_case(account) {
                Leg::QuoteIsBase
            } else {
                return None;
            };
            Some(Conversion { symbol_id: id, name: name.to_string(), leg })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    match found.len() {
        0 => Err(RateError::NoPair(format!(
            "the account's symbol list has no forex pair between {quote} and the account's currency \
             {account}, so {quote} has no rate (a cross through a third currency is not composed)"
        ))),
        1 => Ok(found.remove(0)),
        _ => Err(RateError::NoPair(format!(
            "the account's symbol list has {} forex pairs between {quote} and {account} ({}), and which \
             one the rate is read from would be a guess",
            found.len(),
            found.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// **The rate from a two-sided quote** (`bid` and `ask` as the wire states them, 1/100000 of a price unit):
/// the mid, turned the right way round for `leg`. A zero side or a crossed quote is no quote.
///
/// shortcut: the MID, where the vendor's sample converts at the bid for a long and the ask for a short (see the
/// module doc, "The mid, and why it is not side-aware"); a spread-sized error, upgrade with the engine.
pub fn rate_of_quote(leg: Leg, bid: u64, ask: u64) -> Result<AccountPerQuote, RateError> {
    if bid == 0 || ask == 0 {
        return Err(RateError::BadQuote(format!(
            "the conversion pair's quote has a zero side (bid {bid}, ask {ask}), so it is no price"
        )));
    }
    if bid > ask {
        return Err(RateError::BadQuote(format!(
            "the conversion pair's quote is crossed (bid {bid} above ask {ask}), so it is no price"
        )));
    }
    let mid = (bid as f64 + ask as f64) / 2.0 / RELATIVE_PRICE_SCALE;
    let rate = match leg {
        Leg::AccountIsBase => 1.0 / mid,
        Leg::QuoteIsBase => mid,
    };
    AccountPerQuote::new(rate)
        .ok_or_else(|| RateError::BadQuote(format!("the conversion pair's mid {mid} is no rate")))
}

/// Whether a symbol's market is open at an instant, from the symbol's OWN schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketState {
    /// Inside one of the symbol's trading intervals.
    Open,
    /// Outside every trading interval.
    Closed,
    /// Not known: no schedule, or its time zone is not one this crate can resolve ([`market_state`]). The
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

/// Names of the time zone [`market_state`] can evaluate without a time-zone database: a fixed UTC.
const UTC_ZONES: [&str; 4] = ["UTC", "Etc/UTC", "GMT", "Etc/GMT"];

/// Seconds from the Unix epoch (a Thursday, 00:00) back to the Sunday before it: the schedule counts "seconds
/// starting from SUNDAY 00:00 in specified time zone" (`ProtoOASymbol.schedule`).
const EPOCH_SECONDS_PAST_SUNDAY: i64 = 4 * 86_400;

/// **Is `symbol`'s market open at `now_ms`?** Read from its `schedule` (intervals of seconds since Sunday 00:00
/// in `schedule_time_zone`), but only where that zone is spelled UTC (`UTC`, `Etc/UTC`, `GMT`, `Etc/GMT`): any other zone, an empty
/// schedule or a clock before 1970 is [`MarketState::Unknown`], never a guess. A holiday (`ProtoOASymbol.
/// holiday`) is not read: on one the market can be closed while the schedule says open, and the stale-quote
/// check then refuses at [`OPEN_MARKET_MAX_SPOT_AGE_MS`], the safe side.
#[must_use]
pub fn market_state(symbol: &ProtoOaSymbol, now_ms: i64) -> MarketState {
    let zone_is_utc = symbol
        .schedule_time_zone
        .as_deref()
        .map(str::trim)
        .is_some_and(|zone| UTC_ZONES.iter().any(|utc| zone.eq_ignore_ascii_case(utc)));
    if !zone_is_utc || symbol.schedule.is_empty() || now_ms < 0 {
        return MarketState::Unknown;
    }
    let second = u32::try_from((now_ms / 1_000 + EPOCH_SECONDS_PAST_SUNDAY).rem_euclid(604_800))
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
pub fn trading_refusal(symbol: &ProtoOaSymbol, name: &str) -> Option<RateError> {
    let mode = symbol.trading_mode.and_then(|m| ProtoOaTradingMode::try_from(m).ok())?;
    matches!(
        mode,
        ProtoOaTradingMode::DisabledWithoutPendingsExecution
            | ProtoOaTradingMode::DisabledWithPendingsExecution
    )
    .then(|| {
        RateError::BadQuote(format!(
            "{name} is not enabled for trading (tradingMode {}), so its last price is no rate",
            mode.as_str_name()
        ))
    })
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
) -> Option<RateError> {
    let Some(stamp) = timestamp_ms else {
        return Some(RateError::BadQuote(
            "the conversion pair's quote carries no timestamp, so its age is unknown and it is no rate"
                .into(),
        ));
    };
    if stamp < MIN_SPOT_MS_EPOCH {
        return Some(RateError::BadQuote(format!(
            "the conversion pair's quote is stamped {stamp}, which is not a millisecond epoch (it reads \
             as seconds): the unit of the venue's spot timestamp is assumed to be milliseconds, so the \
             quote is refused and not guessed at"
        )));
    }
    if stamp.saturating_sub(asked_ms) > MAX_SPOT_AFTER_ASK_MS {
        return Some(RateError::BadQuote(format!(
            "the conversion pair's quote is stamped {stamp}, {} after the ask ({asked_ms}), which no live \
             quote can be: the unit of the venue's spot timestamp is assumed to be milliseconds and this \
             reads as a finer unit (or a clock far ahead), so the quote is refused and not guessed at",
            span(stamp.saturating_sub(asked_ms))
        )));
    }
    let limit = match market {
        MarketState::Closed => MAX_SPOT_AGE_MS,
        MarketState::Open | MarketState::Unknown => OPEN_MARKET_MAX_SPOT_AGE_MS,
    };
    let age = asked_ms.saturating_sub(stamp);
    (age > limit).then(|| {
        RateError::BadQuote(match market {
            MarketState::Unknown => format!(
                "the conversion pair's last price is {} old and {}; refusing a quote older than {}, so \
                 it is no rate",
                span(age),
                match schedule_zone {
                    Some(zone) => format!("the market schedule zone {zone} is not understood"),
                    None => "the market schedule is not stated, so the market state is not understood"
                        .to_string(),
                },
                span(limit)
            ),
            _ => format!(
                "the conversion pair's last price is {} old (more than {} with the market {market:?}), \
                 so it is no rate",
                span(age),
                span(limit)
            ),
        })
    })
}

/// The two sides of ONE symbol's quote, accumulated over the spot events a subscription delivers: the first
/// ("technical") event may carry one side only, and a later one fills the other in.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SpotBook {
    bid: Option<u64>,
    ask: Option<u64>,
    timestamp: Option<i64>,
}

impl SpotBook {
    /// Fold one spot event in. An event for another symbol changes nothing.
    pub fn on_event(&mut self, ev: &ProtoOaSpotEvent, symbol_id: i64) {
        if ev.symbol_id != symbol_id {
            return;
        }
        self.bid = ev.bid.or(self.bid);
        self.ask = ev.ask.or(self.ask);
        self.timestamp = ev.timestamp.or(self.timestamp);
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
    use crate::proto::{
        ProtoOaAsset, ProtoOaAssetClass, ProtoOaLightSymbol, ProtoOaSymbolCategory,
    };
    use crate::symbols::SymbolLists;

    /// A USD account's list as the demo broker publishes it (category 1 -> asset class `Forex`, category 3
    /// -> `Indices`): USDJPY (USD is the BASE), GBPUSD (the quote currency is the base), EURGBP (a cross),
    /// USDCHF, and a `JPYUSD` index CFD named like a pair but in the wrong class.
    fn usd_account(extra: Vec<ProtoOaLightSymbol>) -> SymbolMap {
        let sym = |id, name: &str, category, base, quote| ProtoOaLightSymbol {
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
            sym(1, "EURUSD", 1, 5, 16),
            sym(2, "USDJPY", 1, 16, 7),
            sym(3, "GBPUSD", 1, 8, 16),
            sym(4, "EURGBP", 1, 5, 8),
            sym(5, "USDCHF", 1, 16, 9),
            sym(6, "JPYUSD", 3, 7, 16),
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
            deposit_asset_id: Some(16),
            ..Default::default()
        })
    }

    fn pick(map: &SymbolMap, quote: &str) -> Result<Conversion, RateError> {
        conversion_symbol(map, map.deposit_currency().expect("USD"), quote)
    }

    /// The conversion pair is chosen from the broker's own list by its base and quote assets, with the
    /// orientation of the ACCOUNT's side: USD is the base of USDJPY and the quote of GBPUSD.
    #[test]
    fn the_pair_is_found_by_its_assets_and_knows_which_side_the_account_is() {
        let map = usd_account(vec![]);
        let jpy = pick(&map, "JPY").expect("USDJPY");
        assert_eq!((jpy.name.as_str(), jpy.leg), ("USDJPY", Leg::AccountIsBase));
        let gbp = pick(&map, "gbp").expect("GBPUSD");
        assert_eq!((gbp.name.as_str(), gbp.leg), ("GBPUSD", Leg::QuoteIsBase));
        assert_eq!(pick(&map, "CHF").expect("USDCHF").leg, Leg::AccountIsBase);
        assert_eq!(map.id_of(&jpy.name), Some(jpy.symbol_id));
    }

    /// No direct pair (EURGBP is not a USD pair; a cross is not composed), a pair in the wrong class
    /// (`JPYUSD` is an index CFD here: the class is the broker's word, never the name) and two pairs
    /// between the same currencies are each refused in words.
    #[test]
    fn no_pair_a_wrong_class_pair_and_two_pairs_are_refused_by_name() {
        let map = usd_account(vec![]);
        for ccy in ["NZD", "ZAR"] {
            let why = pick(&map, ccy).expect_err(ccy);
            assert!(matches!(why, RateError::NoPair(_)), "{ccy}: {why}");
            assert!(why.to_string().contains("no forex pair"), "{ccy}: {why}");
        }
        // `JPYUSD` (category 3, Indices) is not a pair: USDJPY alone answers JPY, the CFD never counts.
        assert_eq!(pick(&map, "JPY").expect("one").name, "USDJPY");

        let twin = ProtoOaLightSymbol {
            symbol_id: 7,
            symbol_name: Some("USDJPY.x".to_string()),
            symbol_category_id: Some(1),
            base_asset_id: Some(16),
            quote_asset_id: Some(7),
            ..Default::default()
        };
        let two = usd_account(vec![twin]);
        let why = pick(&two, "JPY").expect_err("two pairs");
        let text = why.to_string();
        assert!(text.contains("2 forex pairs") && text.contains("USDJPY, USDJPY.x"), "{text}");
        assert!(text.contains("a guess"), "{text}");
    }

    /// An empty currency is never the account's: no pair is chosen for it.
    #[test]
    fn an_empty_currency_chooses_nothing() {
        let map = usd_account(vec![]);
        assert!(matches!(conversion_symbol(&map, "", "JPY"), Err(RateError::Unavailable(_))));
        assert!(matches!(conversion_symbol(&map, "USD", "  "), Err(RateError::Unavailable(_))));
    }

    /// **The orientation, by value.** USDJPY at 150.10 (the account is the BASE): a yen is a FRACTION of a
    /// dollar. 100,000 units are 15,010,000 yen = 100,000 dollars, and a 0.10 yen move on them is 10,000 yen
    /// = 66.62 dollars, NOT 10,000.
    #[test]
    fn a_yen_quote_on_a_usd_account_is_a_fraction_of_a_dollar_per_yen() {
        // 150.09 / 150.11 in the wire's 1/100000: mid 150.10
        let r = rate_of_quote(Leg::AccountIsBase, 15_009_000, 15_011_000).expect("a quote");
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
        let r = rate_of_quote(Leg::QuoteIsBase, 134_999, 135_001).expect("a quote");
        assert!(r.get() > 1.0, "{} is pounds per dollar, the flipped rate", r.get());
        assert!((r.to_account(85_000.0) - 114_750.0).abs() < 1e-6);
    }

    /// The two legs are each other's inverse on the same quote: a flipped leg is the 22,500x bug.
    #[test]
    fn the_two_legs_of_one_quote_are_reciprocal() {
        let (bid, ask) = (15_009_000, 15_011_000);
        let a = rate_of_quote(Leg::AccountIsBase, bid, ask).expect("a").get();
        let b = rate_of_quote(Leg::QuoteIsBase, bid, ask).expect("b").get();
        assert!((a * b - 1.0).abs() < 1e-12, "{a} * {b}");
    }

    /// A zero side (either), a crossed quote and a locked-but-sane quote: only the sane one is a rate.
    #[test]
    fn a_zero_or_crossed_quote_is_no_rate_and_a_locked_one_is() {
        for (bid, ask) in [(0, 135_001), (134_999, 0), (0, 0), (135_002, 135_001)] {
            for leg in [Leg::AccountIsBase, Leg::QuoteIsBase] {
                assert!(
                    matches!(rate_of_quote(leg, bid, ask), Err(RateError::BadQuote(_))),
                    "{bid}/{ask} {leg:?}"
                );
            }
        }
        assert_eq!(rate_of_quote(Leg::QuoteIsBase, 135_000, 135_000).expect("locked").get(), 1.35);
    }

    #[test]
    fn a_rate_cannot_be_built_from_a_non_rate() {
        for bad in [0.0, -1.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(AccountPerQuote::new(bad), None, "{bad}");
        }
        assert_eq!(AccountPerQuote::new(0.0066).map(AccountPerQuote::get), Some(0.0066));
        let par = QuoteRate::par(" usd ", 5);
        assert_eq!(
            (
                par.quote_ccy.as_str(),
                par.account_ccy.as_str(),
                par.account_per_quote.get(),
                par.as_of_ms
            ),
            ("USD", "USD", 1.0, 5)
        );
    }

    /// The first ("technical") event may carry one side; the quote is usable only when both were seen, for
    /// THE symbol asked, and the venue's timestamp is the latest one stated.
    #[test]
    fn a_quote_is_two_sided_only_after_both_sides_arrived_for_that_symbol() {
        let ev = |symbol_id, bid, ask, timestamp| ProtoOaSpotEvent {
            ctid_trader_account_id: 99,
            symbol_id,
            bid,
            ask,
            timestamp,
            ..Default::default()
        };
        let mut book = SpotBook::default();
        book.on_event(&ev(2, Some(15_009_000), None, Some(10)), 2);
        assert_eq!(book.two_sided(), None, "one side is no quote");
        book.on_event(&ev(3, Some(1), Some(2), Some(99)), 2);
        assert_eq!(book.two_sided(), None, "another symbol's event changes nothing");
        book.on_event(&ev(2, None, Some(15_011_000), Some(11)), 2);
        assert_eq!(book.two_sided(), Some((15_009_000, 15_011_000)));
        assert_eq!(book.timestamp_ms(), Some(11));
        book.on_event(&ev(2, Some(15_010_000), None, None), 2);
        assert_eq!(
            book.two_sided(),
            Some((15_010_000, 15_011_000)),
            "a later bid replaces the old"
        );
        assert_eq!(book.timestamp_ms(), Some(11), "an event without a stamp keeps the last");
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
        let text = why.to_string();
        assert!(text.contains("73 h old") && text.contains("72 h"), "{text}");
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
        let text = why.to_string();
        assert!(matches!(&why, RateError::BadQuote(_)), "{text}");
        assert!(
            text.contains("20 h old")
                && text.contains(
                    "the market schedule zone Europe/London is not understood; refusing a quote older \
                     than 120 s"
                ),
            "{text}"
        );
        assert_eq!(spot_age_refusal(Some(ago(120_000)), ASKED, MarketState::Unknown, zone), None);
        assert!(spot_age_refusal(Some(ago(121_000)), ASKED, MarketState::Unknown, zone).is_some());
        // No schedule at all: the same strict bound, worded without a zone.
        let why = spot_age_refusal(Some(ago(48 * 3_600_000)), ASKED, MarketState::Unknown, None)
            .expect("no schedule, no weekend allowance");
        assert!(why.to_string().contains("market schedule is not stated"), "{why}");
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
                let text = why.to_string();
                assert!(
                    matches!(&why, RateError::BadQuote(_))
                        && text.contains("after the ask")
                        && text.contains("assumed to be milliseconds"),
                    "{stamp}: {text}"
                );
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
        let text = why.to_string();
        assert!(
            text.contains("121 s old") && text.contains("120 s") && text.contains("Open"),
            "{text}"
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
            assert!(matches!(&why, RateError::BadQuote(w) if w.contains("no timestamp")), "{why}");
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
                let text = why.to_string();
                assert!(
                    text.contains("not a millisecond epoch") && text.contains("assumed"),
                    "{stamp}: {text}"
                );
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
                .map(|(start_second, end_second)| crate::proto::ProtoOaInterval {
                    start_second: *start_second,
                    end_second: *end_second,
                })
                .collect(),
            ..Default::default()
        }
    }

    /// 2023-11-14 22:13:20 UTC (`ASKED`) is a Tuesday: 2 days 22 h 13 m 20 s past Sunday 00:00 UTC.
    const TUESDAY_SECOND: u32 = 2 * 86_400 + 22 * 3_600 + 13 * 60 + 20;

    /// The market is read from the symbol's OWN schedule, in UTC only: inside an interval it is open (start
    /// inclusive, end exclusive), outside every one closed, and any other zone, no zone or no schedule is
    /// unknown, never a guess. Sunday 00:00 is second 0 and the epoch (a Thursday) is 4 days past it.
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
            let text = why.to_string();
            assert!(
                text.contains("USDJPY is not enabled") && text.contains(off.as_str_name()),
                "{text}"
            );
        }
        let unknown = ProtoOaSymbol { symbol_id: 2, trading_mode: Some(99), ..Default::default() };
        assert_eq!(trading_refusal(&unknown, "USDJPY"), None, "a mode this crate does not know");
    }

    /// The book is complete only with a timestamp: a quote whose events carried none is not, and the next
    /// event that does completes it.
    #[test]
    fn a_book_is_timed_only_once_an_event_carried_a_stamp() {
        let ev = |bid, ask, timestamp| ProtoOaSpotEvent {
            ctid_trader_account_id: 99,
            symbol_id: 2,
            bid,
            ask,
            timestamp,
            ..Default::default()
        };
        let mut book = SpotBook::default();
        book.on_event(&ev(Some(15_009_000), Some(15_011_000), None), 2);
        assert_eq!(book.two_sided(), Some((15_009_000, 15_011_000)));
        assert_eq!(book.timed_two_sided(), None, "two sides and no stamp");
        book.on_event(&ev(None, None, Some(ASKED)), 2);
        assert_eq!(book.timed_two_sided(), Some((15_009_000, 15_011_000)));
    }
}
