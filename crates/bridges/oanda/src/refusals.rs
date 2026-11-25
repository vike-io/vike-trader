//! OANDA's refusals, declared once (`vike_utility::refusal_enum!`, decision 0301): each has its id and its ONE text,
//! and code and tests decide on the variant, never on the words. Every id is published in the workspace's
//! `refusals.json` (`crates/vike-backend/tests/refusal_catalog.rs`).
//!
//! A refusal leaves this crate as its text where the shared seam carries a `String`
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesError`, `vike_catalog::CatalogError`,
//! `vike_data::source::SourceError`, `vike_data::DataClientError`, `venue_read`, the reconcile client): its `Display`,
//! byte for byte the words it had before it was typed. A RATE refusal ([`OandaRateRefusal`]) becomes
//! `vike_model::QuoteRateError` through `From`, which picks the kind (`Unavailable`, `BadQuote`) by the variant.
//!
//! `crate::history::klines::HistoryTokenError` is declared beside its source, where the gatewayhub names it. The venue's
//! OWN words (its `errorMessage`, `crate::transport`'s `is_order_does_not_exist`) and the shared demo-only sentences
//! (`vike_bridge_core`'s `DemoOnlyArm`) are not here.

use vike_catalog::CatalogError;
use vike_model::QuoteRateError;

use crate::instrument::{LEGACY_TICK, MAX_DECIMALS};

vike_utility::refusal_enum! {
    /// Why an operator's read of the account (`crate::adapter`'s `VenueAdapterOanda::venue_read`) has no answer.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OandaVenueReadRefusal with ids OandaVenueReadRefusalId {
        /// A `live` account holding its pair: this build reaches no fxTrade host.
        LiveTier => "oanda.venue_read.live_tier":
            "this build reaches no fxTrade host, so an account on the live tier has no host to read; nothing was \
             asked",
        /// The account holds no complete pair.
        NoPair => "oanda.venue_read.no_pair":
            "the account holds no oanda API_KEY and ACCOUNT_ID pair; nothing was asked",
        /// The `pendingOrders` read failed; `why` is the read's words.
        PendingUnread { why: String } => "oanda.venue_read.pending_unread":
            "oanda: the pending orders could not be read: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why an instrument the gatewayhub asks this venue's facts of is not admitted (`crate::symbol_properties`'s
    /// `SymbolPropertiesAccountSpecOanda` and `quoted_rate`, `crate::instrument`'s `grid_refusal`), or its rate is not
    /// had now.
    #[derive(Debug, Clone, PartialEq)]
    pub enum OandaPropertiesRefusal with ids OandaPropertiesRefusalId {
        /// No kept row (or two) answers the USER name (decision 0290); `why` is the kept list's words.
        NotKept { why: String } => "oanda.properties.not_kept": "oanda: {why}",
        /// The plain answer: quoted in another currency than the account's.
        ForeignQuote { instrument: String, quote: String, home: String } => "oanda.properties.foreign_quote":
            "oanda: {instrument} is quoted in {quote} but this account's currency is {home}, and the engine books \
             money in ONE currency (nothing converts a quote currency), so a position in it would be mis-valued; \
             only instruments quoted in {home} are admitted",
        /// The plain answer: the wire name states no quote currency.
        NoQuoteCurrency { instrument: String, home: String } => "oanda.properties.no_quote_currency":
            "oanda: {instrument} names no quote currency, so it cannot be checked against this account's currency \
             ({home})",
        /// A converting node's answer: the wire name states no quote currency to convert.
        NoQuoteToConvert { instrument: String, home: String } => "oanda.properties.no_quote_to_convert":
            "oanda: {instrument} names no quote currency, so it cannot be converted into the account's currency \
             ({home})",
        /// The rate could not be had now; `why` is the rate read's words.
        NoRate { quote: String, home: String, why: String } => "oanda.properties.no_rate":
            "no rate for {quote} in {home} ({why})",
        /// The pricing read converts into another home currency than the listing's.
        RateForAnotherHome { quote: String, home: String, converts: String } =>
            "oanda.properties.rate_for_another_home":
            "no rate for {quote} in {home} (the pricing read converts into {converts}, not the account's {home})",
        /// A currency pair whose price grid is no whole number of decimals.
        GridNotDecimal { symbol: String, tick: f64 } => "oanda.properties.grid_not_decimal":
            "oanda: {symbol} has a price grid of {tick}, which is not a whole number of decimal places from 0 to \
             {MAX_DECIMALS} (the instrument list's displayPrecision is missing, malformed or out of range), so this \
             adapter cannot write its price and the instrument is not admitted",
        /// Any other class off the five decimals the adapter was proven on.
        GridNotLegacy { symbol: String, tick: f64 } => "oanda.properties.grid_not_legacy":
            "oanda: {symbol} has a price grid of {tick}, and only a currency pair is admitted on its own price \
             precision: any other instrument must sit on the five decimals the adapter was proven on (a grid of \
             {LEGACY_TICK}), so an instrument with another price precision is not admitted",
        /// A unit step that is not the whole unit.
        NotWholeUnits { symbol: String, step: f64 } => "oanda.properties.not_whole_units":
            "oanda: {symbol} trades in steps of {step} units, and this adapter rounds every order to WHOLE units, so \
             an instrument whose unit step is not 1 is not admitted",
    }
}

vike_utility::refusal_enum! {
    /// Why a `homeConversions` rate could not be had (`crate::account::quote_rate`, `crate::catalog`'s
    /// `CatalogProviderOanda::home_conversions_assuming`). It crosses as a `QuoteRateError`: `Unavailable` for a read
    /// that could not be made, `BadQuote` for an answer that names no usable rate.
    #[derive(Debug, Clone, PartialEq)]
    pub enum OandaRateRefusal with ids OandaRateRefusalId {
        /// The account's catalog read refused first.
        Unavailable { why: OandaCatalogRefusal } => "oanda.rate.unavailable":
            "oanda home conversion unavailable: {why}",
        /// No home currency to convert into.
        HomeUnknown => "oanda.rate.home_unknown":
            "oanda home conversion unavailable: the account's home currency is not known",
        /// No instrument name, or one outside the name alphabet: nothing was asked.
        NoInstrumentNames => "oanda.rate.no_instrument_names":
            "oanda home conversion unavailable: oanda: home conversions need one or more instrument names (EUR_USD)",
        /// The pricing read failed; `why` is the transport's words.
        PricingUnread { why: String } => "oanda.rate.pricing_unread":
            "oanda home conversion unavailable: oanda /v3/pricing: {why}",
        /// The body carries no (or an empty) `homeConversions` array.
        NoConversions => "oanda.rate.no_conversions":
            "oanda pricing carried no homeConversions (includeHomeConversions=true)",
        /// No entry for the currency (or no currency at all).
        NoConversionFor { currency: String } => "oanda.rate.no_conversion_for":
            "oanda sent no home conversion for {currency}, so it has no rate (never 1.0)",
        /// The currency is listed twice.
        ListedTwice { currency: String } => "oanda.rate.listed_twice":
            "oanda homeConversions entry for {currency} is unusable: listed twice",
        /// A factor that is no finite decimal; `raw` is what the venue sent (`absent`).
        FactorUnread { currency: String, field: &'static str, raw: String } => "oanda.rate.factor_unread":
            "oanda homeConversions entry for {currency} is unusable: {field} is {raw} (want a finite decimal)",
        /// A factor that is zero or negative.
        FactorNotPositive { currency: String, field: &'static str, x: f64 } => "oanda.rate.factor_not_positive":
            "oanda homeConversions entry for {currency} is unusable: {field} = {x} is not a positive rate",
        /// The home currency's own entry does not convert at 1: the home read is not the one pricing converts into.
        HomeNotOne { currency: String, gain: f64, loss: f64, position: f64 } => "oanda.rate.home_not_one":
            "oanda homeConversions entry for {currency} is unusable: the account's own currency must convert at 1, \
             but OANDA states {gain} / {loss} / {position} (gain / loss / position value): the home currency read \
             from the account is not the one pricing converts into",
    }
}

impl From<OandaRateRefusal> for QuoteRateError {
    fn from(refusal: OandaRateRefusal) -> Self {
        match &refusal {
            OandaRateRefusal::Unavailable { .. }
            | OandaRateRefusal::HomeUnknown
            | OandaRateRefusal::NoInstrumentNames
            | OandaRateRefusal::PricingUnread { .. } => Self::Unavailable(refusal.to_string()),
            _ => Self::BadQuote(refusal.to_string()),
        }
    }
}

vike_utility::refusal_enum! {
    /// Why the account's own catalog reads (`crate::catalog`'s `CatalogProviderOanda`) have no answer.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OandaCatalogRefusal with ids OandaCatalogRefusalId {
        /// The instrument list read failed; `why` is the transport's words.
        InstrumentsUnread { why: String } => "oanda.catalog.instruments_unread": "oanda /v3/instruments: {why}",
        /// The account summary read failed; `why` is the transport's words.
        SummaryUnread { why: String } => "oanda.catalog.summary_unread": "oanda /v3/summary: {why}",
        /// The summary states no currency (never a guessed `USD`).
        SummaryNoCurrency => "oanda.catalog.summary_no_currency":
            "oanda /v3/summary: the account summary carries no currency",
        /// The account holds no usable pair.
        NoCredentials => "oanda.catalog.no_credentials":
            "oanda: this account holds no usable credentials (API_KEY and ACCOUNT_ID)",
    }
}

impl From<OandaCatalogRefusal> for CatalogError {
    fn from(refusal: OandaCatalogRefusal) -> Self {
        Self(refusal.carried())
    }
}

vike_utility::refusal_enum! {
    /// Why a history candles fetch (`crate::history::klines`) has no answer: a request this source cannot express,
    /// or a page that failed or that the venue answered in a shape it does not document.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OandaHistoryRefusal with ids OandaHistoryRefusalId {
        /// An interval OANDA has no candle granularity for: nothing is asked.
        NotServed { interval: String } => "oanda.history.not_served":
            "oanda history: {interval:?} is not an interval OANDA serves candles for",
        /// A week or a month: no fixed step to page by.
        CalendarWidth { interval: String } => "oanda.history.calendar_width":
            "oanda history: {interval:?} candles are calendar-width, and this source pages by a fixed step. Fetch \
             1d and resample",
        /// A symbol that is no instrument name (it would steer the request path); `shown` is its head.
        NotAnInstrumentName { shown: String } => "oanda.history.not_an_instrument_name":
            "oanda history: {shown:?} is not an OANDA instrument name (letters, digits and `_`, like EUR_USD)",
        /// A page request failed; `why` is the venue error's words.
        PageFailed { why: String, from_s: i64 } => "oanda.history.page_failed": "{why} (candles page from={from_s})",
        /// A page that could not be decoded.
        PageUndecoded { why: OandaCandleRefusal, from_s: i64 } => "oanda.history.page_undecoded":
            "oanda history: {why} (candles page from={from_s})",
        /// A venue that served the same full page again: the cursor did not move.
        NoProgress { from_s: i64, last_ms: i64 } => "oanda.history.no_progress":
            "oanda history: the candles endpoint made no progress past from={from_s} (its last candle was at \
             {last_ms} ms)",
    }
}

vike_utility::refusal_enum! {
    /// Why one candles page is not decoded (`crate::history::klines`'s `decode_page`): strict where a shortcut would
    /// store something wrong.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OandaCandleRefusal with ids OandaCandleRefusalId {
        /// The body has no `candles` array.
        NoCandles => "oanda.candles.no_candles": "the response has no `candles` array",
        /// A candle whose `time` does not parse.
        NoTime => "oanda.candles.no_time": "a candle has no parseable `time`",
        /// Candles out of order within one page.
        OutOfOrder { ts: i64 } => "oanda.candles.out_of_order":
            "candle {ts} is not after the one before it in the same page",
        /// A complete candle without usable mid prices.
        NoMid { ts: i64 } => "oanda.candles.no_mid": "complete candle {ts} has no usable `mid` prices",
        /// A complete candle with a non-finite price.
        NonFinite { ts: i64 } => "oanda.candles.non_finite": "complete candle {ts} carries a non-finite price",
    }
}

vike_utility::refusal_enum! {
    /// Why a live feed subscription (`crate::feed`'s `Feeds`) is refused before any thread exists.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OandaFeedRefusal with ids OandaFeedRefusalId {
        /// The feed was built without a config: absent credentials are the live gate. `label` names the ask
        /// (`EURUSD quotes`, `EURUSD@1m bars`).
        NoConfig { label: String } => "oanda.feed.no_config":
            "oanda {label}: no credentials configured (absent credentials ARE the live gate) — build the feed with \
             Feeds::with_config(load_oanda_config_from(…))",
        /// An interval with no live candle granularity.
        NoGranularity { symbol: String, interval: String } => "oanda.feed.no_granularity":
            "oanda {symbol}@{interval}: no OANDA candle granularity for this interval",
        /// No kept row (or two) answers the USER name (decision 0290); `subject` names the ask, `why` is the kept
        /// list's words.
        NotKept { subject: String, why: String } => "oanda.feed.not_kept": "oanda {subject}: {why}",
        /// The OS refused the subscription's thread; `why` is its words.
        ThreadRefused { subject: String, why: String } => "oanda.feed.thread_refused": "oanda {subject}: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why one entry of the REST pricing reply seeds no first price (`crate::feed::bar::seed`'s `seed_tick` and
    /// `serve`): the stream prices the pair instead.
    #[derive(Debug, Clone, PartialEq)]
    pub enum OandaSeedRefusal with ids OandaSeedRefusalId {
        /// The entry is no JSON object.
        NotAnObject => "oanda.seed.not_an_object": "the entry is not an object",
        /// The venue marks the market closed or halted.
        NotTradeable => "oanda.seed.not_tradeable": "the venue marks it non-tradeable (market closed or halted)",
        /// One side missing, or unreadable.
        OneSided => "oanda.seed.one_sided": "the price is one-sided or unreadable",
        /// The bid above the ask, or a side not positive.
        Crossed { bid: f64, ask: f64 } => "oanda.seed.crossed": "the price is crossed or not positive ({bid} / {ask})",
        /// The venue's time is past the age bound (or ahead of this clock); `bound` is the bound behind, in ms.
        Stale { age: i64, bound: i64 } => "oanda.seed.stale":
            "the venue's time on it is {age} ms from now (it must be within {bound} ms behind)",
        /// The reply names no price for the instrument.
        NoPrice => "oanda.seed.no_price": "the reply carries no price for it",
    }
}

vike_utility::refusal_enum! {
    /// Why a reconcile read (`crate::reconcile`'s `ReconcileClientOanda`) fails the fetch: a symbol with no kept row,
    /// a read that failed or answered without its array, or a connect probe that has not answered.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OandaReconcileRefusal with ids OandaReconcileRefusalId {
        /// No kept row (or two) answers a symbol the pass reads (decision 0290); `why` is the kept list's words.
        NotKept { why: String } => "oanda.reconcile.not_kept": "oanda reconcile: {why}; nothing was read",
        /// A 200 whose body lacks the array its read exists for: never an empty report.
        NoArray { what: &'static str, key: &'static str } => "oanda.reconcile.no_array":
            "{what}: OANDA answered without its `{key}` array; nothing is read from that",
        /// A report read failed; `what` names it, `why` is the venue error's words.
        Unread { what: &'static str, why: String } => "oanda.reconcile.unread": "{what}: {why}",
        /// No probe is kept although none is owed (a race the lock rules out).
        NoProbe => "oanda.reconcile.no_probe": "oanda reconcile: no connect probe",
        /// The connect probe is owed and not due yet.
        ProbePending { wait_ms: u128 } => "oanda.reconcile.probe_pending":
            "oanda reconcile: the connect probe has not answered yet; the next one is in {wait_ms} ms",
        /// OANDA refused a later connect probe: no pass probes again for this mount.
        ProbeRefused { why: String } => "oanda.reconcile.probe_refused":
            "oanda reconcile: OANDA refused the connect probe ({why}); asking again cannot cure a refusal, so no \
             pass probes again for this mount",
        /// A later connect probe got no answer; the next is due after `wait_ms`.
        ProbeUnanswered { why: String, wait_ms: u128 } => "oanda.reconcile.probe_unanswered":
            "oanda reconcile: the connect probe did not answer ({why}); the next one is in {wait_ms} ms",
    }
}
