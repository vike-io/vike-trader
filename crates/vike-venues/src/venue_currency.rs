//! `VenueCurrencyRow`: the per-venue CURRENCY facts a hub needs and no bridge may be linked for (model-moves B3.01 and
//! B3.25). Which symbols a venue's gatewayhub lane rates into the account's currency ([`VenueQuoteConversion`],
//! decision 0183 and its mounted-symbol amendment), whether its statement of the account's currency is paid out of a
//! venue budget ([`VenueCurrencyRead`]), whether a rate must say which ledger it is expressed in
//! ([`VenueRateLedgerStamp`]), whether its reconcile balance names a currency ([`VenueBalanceCurrency`]), what the engine
//! counts money in ([`VenueEngineMoney`]), which minor units it may quote in and which coins settle its linear books.
//! Whether an account's currency was stated or assumed is a runtime answer (`vike_model::AccountCurrency`), not a column.
//!
//! Every roster venue is a NAMED row citing where its answers were read (`crates/vike-venues/CLAUDE.md`'s "Per-venue
//! capability maps (the playbook)"). It replaced the tradehub's two converting-venue lists and its three currency
//! predicates, the gatewayhub's twins of two of them, bybit's settle-coin list and hyperliquid's perp quote (B3.01,
//! B3.25). Its readers: the door's symbol admission (`crates/vike-tradehub/src/server/symbol_admission.rs`) and
//! balance gate (`crates/vike-tradehub/src/venue_backend/gateway/balance_gate.rs`), the engine's assembly
//! (`crates/vike-tradehub-core/src/engine/assemble.rs`), the gatewayhub's lane
//! (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`) and the bybit and hyperliquid bridges' settle coins.
//!
//! [`VENUE_CURRENCY`] keeps TODAY'S order of the rate-source venues (oanda, ctrader, ibkr, dukascopy, ig, fxcm: the
//! door's refusal sentence lists them so), then the rest in `vike_model::VENUES` order. An unknown venue string answers
//! [`VenueCurrencyRow::NONE`]. Every currency code is a `vike_model` constant: no currency literal lives here.

use vike_model::{MINOR_UNITS, MinorUnit, USDC, USDT};

/// Which symbols a venue's gatewayhub lane RATES into the account's currency: the rate source. The mount subset is a
/// variant of it, so the subset cannot name a venue the source does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueQuoteConversion {
    /// No rate: every symbol is fenced to the account's own currency, by the venue's own facts.
    None,
    /// Any listed symbol but the engine's own MOUNTED one.
    OtherSymbols,
    /// Any listed symbol, the mounted one included (decision 0183's mounted-symbol amendment).
    OtherSymbolsAndMount,
}

/// Whether the venue's statement of the account's currency is paid for out of a venue budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueCurrencyRead {
    /// Free, so it is read at every start.
    Free,
    /// Paid out of a venue budget, so it is read only for an account that will convert (an exposure ceiling armed).
    Budgeted,
}

/// Whether a RATE of the venue must state the ledger currency it is expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueRateLedgerStamp {
    /// An unstamped rate (an older gatewayhub) is admitted; a stamp that is not the engine's anchor is still refused.
    Optional,
    /// A rate without the stamp is refused (fail closed).
    Required,
}

/// Whether the venue's reconcile BALANCE names the currency it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueBalanceCurrency {
    /// It names one, the venue reports no balance, or its client withholds itself: the balance passes through.
    Named,
    /// The account's cash with no currency beside it: the node holds it back when the engine's label is not what the
    /// venue stated.
    Unnamed,
}

/// What the venue's engine counts money in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueEngineMoney {
    /// The account's ledger currency.
    Ledger,
    /// The currency of its MOUNTED contract (the cash row its balance seed reads): the ledger anchors to no other.
    MountedContract,
    /// The quote of its own mounted pair (decision 0170): a pair in another money is admitted only for an account that
    /// converts.
    MountedPairQuote,
}

/// One venue's currency facts and where they were read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueCurrencyRow {
    pub venue: &'static str,
    pub quote_conversion: VenueQuoteConversion,
    pub currency_read: VenueCurrencyRead,
    pub rate_ledger_stamp: VenueRateLedgerStamp,
    pub balance_currency: VenueBalanceCurrency,
    pub engine_money: VenueEngineMoney,
    /// The minor units the venue may quote a mount's money in (`vike_model::MINOR_UNITS`' rows); a mount in one is
    /// never anchored.
    pub minor_units: &'static [MinorUnit],
    /// The coins that settle the venue's linear books, in the order the bridge walks them.
    pub settle_coins: &'static [&'static str],
    /// Why, citing the code (a path's symbol, never a line number).
    pub why: &'static str,
}

impl VenueCurrencyRow {
    /// The answer for a venue string the roster does not carry: no rate, a free read, an optional stamp, a named
    /// balance, the ledger's money, no minor unit and no settle coin.
    pub const NONE: VenueCurrencyRow = plain("", "not a roster venue");
}

/// A venue none of whose currency facts differ from [`VenueCurrencyRow::NONE`]'s.
const fn plain(venue: &'static str, why: &'static str) -> VenueCurrencyRow {
    VenueCurrencyRow {
        venue,
        quote_conversion: VenueQuoteConversion::None,
        currency_read: VenueCurrencyRead::Free,
        rate_ledger_stamp: VenueRateLedgerStamp::Optional,
        balance_currency: VenueBalanceCurrency::Named,
        engine_money: VenueEngineMoney::Ledger,
        minor_units: &[],
        settle_coins: &[],
        why,
    }
}

/// The `why` of a venue whose lane rates no other currency.
const NO_RATE: &str = "no lane rates another currency: every symbol is fenced to the account's own currency by the \
     venue's own facts, and the reconcile balance names its asset";

pub const OANDA: VenueCurrencyRow = VenueCurrencyRow {
    quote_conversion: VenueQuoteConversion::OtherSymbolsAndMount,
    balance_currency: VenueBalanceCurrency::Unnamed,
    ..plain(
        "oanda",
        "the lane's rate is the venue's own `homeConversions` (`crates/bridges/oanda/src/account/quote_rate.rs`), for \
         any listed symbol and the mount, beside an account summary that is free and always read; the reconcile \
         balance is that summary's cash with no currency beside it (`crates/bridges/oanda/src/reconcile/mod.rs`)",
    )
};

pub const CTRADER: VenueCurrencyRow = VenueCurrencyRow {
    quote_conversion: VenueQuoteConversion::OtherSymbolsAndMount,
    ..plain(
        "ctrader",
        "the lane's rate is the mid of the conversion pair's own quote (`crates/bridges/ctrader/src/catalog.rs`'s \
         `quote_rate`), for any listed symbol and the mount; the reconcile balance answers only a deposit it names \
         (`crates/bridges/ctrader/src/reconcile/mod.rs`'s `parse_balance`)",
    )
};

pub const IBKR: VenueCurrencyRow = VenueCurrencyRow {
    quote_conversion: VenueQuoteConversion::OtherSymbols,
    currency_read: VenueCurrencyRead::Budgeted,
    rate_ledger_stamp: VenueRateLedgerStamp::Required,
    engine_money: VenueEngineMoney::MountedContract,
    minor_units: &MINOR_UNITS,
    ..plain(
        "ibkr",
        "the lane rates a stock, a forex pair or a dated future in another currency off the mid of a live forex quote \
         (`crates/bridges/vike-ibkr/src/account/quote_rate/mod.rs`), never the mount: the engine counts money in its \
         MOUNTED contract's currency (foreign cash is a per-asset ledger, a decision 0183 reopener), and a mount in a \
         money IB may quote in hundredths is not anchored (`minor_unit_refusal` there refuses its rate); the base \
         currency is a dial of the data lane, rationed by the hour, and stamps every rate, so an unstamped one is \
         refused; the client withholds its own balance",
    )
};

pub const DUKASCOPY: VenueCurrencyRow = VenueCurrencyRow {
    quote_conversion: VenueQuoteConversion::OtherSymbolsAndMount,
    engine_money: VenueEngineMoney::MountedPairQuote,
    ..plain(
        "dukascopy",
        "the lane rates any listed pair, the mounted one included, off the mid of the conversion pair on the JForex \
         sidecar's own ticks (`crates/bridges/dukascopy/src/account/quote_rate.rs`'s `quote_rate`), in the currency \
         the sidecar's `ready` line states for free (`crates/bridges/dukascopy/src/feed.rs`); the engine counts money \
         in its own pair's quote (decision 0170); the reconcile leg reports no balance",
    )
};

pub const IG: VenueCurrencyRow = VenueCurrencyRow {
    quote_conversion: VenueQuoteConversion::OtherSymbolsAndMount,
    currency_read: VenueCurrencyRead::Budgeted,
    balance_currency: VenueBalanceCurrency::Unnamed,
    ..plain(
        "ig",
        "the lane rates any listed epic, the mounted one included, from the market details' `currencies[]` checked \
         against a price (`crates/bridges/ig/src/account/quote_rate.rs`); the account's currency costs a login and \
         `GET /accounts` out of the account's non-trading allowance; the reconcile balance is the account row's cash \
         with no currency beside it",
    )
};

pub const FXCM: VenueCurrencyRow = VenueCurrencyRow {
    quote_conversion: VenueQuoteConversion::OtherSymbolsAndMount,
    balance_currency: VenueBalanceCurrency::Unnamed,
    engine_money: VenueEngineMoney::MountedPairQuote,
    ..plain(
        "fxcm",
        "the lane rates any listed pair, the mounted one included, off the mid of the conversion pair in the running \
         session's Offers table, in the account's currency read at login from its system property \
         (`crates/bridges/fxcm/src/account/quote_rate.rs`'s `BASE_CURRENCY_PROPERTY`); the engine counts money in its \
         own pair's quote; the Accounts table's balance names no currency",
    )
};

pub const BINANCE: VenueCurrencyRow = plain("binance", NO_RATE);

pub const BYBIT: VenueCurrencyRow = VenueCurrencyRow {
    settle_coins: &[USDT, USDC],
    ..plain(
        "bybit",
        "no lane rates another currency; the linear books settle in the two coins the account-mode probe walks, in its \
         order (`crates/bridges/bybit/src/account/modes.rs`)",
    )
};

pub const OKX: VenueCurrencyRow = plain("okx", NO_RATE);

pub const DERIBIT: VenueCurrencyRow = plain("deribit", NO_RATE);

pub const POLYMARKET: VenueCurrencyRow = plain("polymarket", NO_RATE);

pub const ALPACA: VenueCurrencyRow = plain("alpaca", NO_RATE);

pub const ASTER: VenueCurrencyRow = plain("aster", NO_RATE);

pub const HYPERLIQUID: VenueCurrencyRow = VenueCurrencyRow {
    settle_coins: &[USDC],
    ..plain(
        "hyperliquid",
        "no lane rates another currency; every perp is quoted and settled in one coin \
         (`crates/bridges/hyperliquid/src/catalog.rs`'s `PERP_QUOTE`)",
    )
};

// vike:new-venue:row /// TODO(new-venue: {venue}): read its currency facts off its bridge and both hubs; `plain` is the default row.
// vike:new-venue:row pub const {VENUE}: VenueCurrencyRow = plain("{venue}", "the default currency facts");
// vike:new-venue:row

/// Every roster venue's row: the six rate-source venues in today's order, then the rest in `vike_model::VENUES` order.
#[rustfmt::skip]
pub const VENUE_CURRENCY: &[VenueCurrencyRow] = &[
    OANDA,
    CTRADER,
    IBKR,
    DUKASCOPY,
    IG,
    FXCM,
    BINANCE,
    BYBIT,
    OKX,
    DERIBIT,
    POLYMARKET,
    ALPACA,
    ASTER,
    HYPERLIQUID,
    // vike:new-venue:row {VENUE}, // TODO(new-venue: {venue}): a rate-source venue's row goes after FXCM's, in the order the refusal lists it
];

/// The currency row of `venue`; an unknown venue string answers [`VenueCurrencyRow::NONE`].
#[must_use]
pub fn venue_currency_for(venue: &str) -> &'static VenueCurrencyRow {
    VENUE_CURRENCY.iter().find(|row| row.venue == venue).unwrap_or(&VenueCurrencyRow::NONE)
}

#[path = "venue_currency_tests.rs"]
#[cfg(test)]
mod tests;
