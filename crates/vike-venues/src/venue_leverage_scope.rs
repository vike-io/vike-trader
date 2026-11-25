//! `LeverageScope`: **what the VENUE offers on the leverage axis**, as one NAMED row per roster venue. A row says
//! where the venue keeps a leverage figure ([`LeverageGrain`]), where it publishes the figure's maximum
//! ([`LeverageMaximumSource`]) and whether a program may set the figure through the venue's API ([`LeverageSetter`]).
//!
//! **One table, one subject** (decision 0009's split, the leverage twin of `crate::venue_margin_support`). It says
//! nothing about what vike SENDS: which venue's account leverage the platform makes real, at which tiers, and
//! whether the venue takes a whole number, is `crate::LEVERAGE_VENUES` (`crate::LeverageVenue::whole_only`), and
//! no fact of that table is restated here. `the_venue_lets_a_program_set_what_vike_makes_real` pins the one relation
//! between the two: what vike makes real is a figure the venue lets a program set.
//!
//! **No maximum NUMBER, anywhere.** A row names WHERE the maximum is published, never its value: the real
//! ceilings are per instrument and, on most venues, fall with the position's size, so one figure here would be
//! wrong for most instruments while reading as fact. A cap this workspace cannot READ is a cap it does not claim
//! (`crates/vike-bridge-core/src/leverage.rs`'s `clamp_to_venue_cap`).
//!
//! **Evidence.** Each row's doc cites the venue's official API documentation (read 2026-11-23), and each crypto
//! row is cross-checked against what its bridge actually writes and reads, cited by symbol. A fact neither
//! source confirms is `Unknown`, never a guess. A row describes the venue's leverage-bearing lane (its
//! derivatives, or a broker's margin account); a spot pair takes no leverage on any venue vike trades.
//!
//! **Step 1 of the playbook** (`crates/vike-venues/CLAUDE.md`'s "Per-venue capability maps (the playbook)"):
//! declared, pinned and complete over `vike_model::VENUES`, and read by nothing yet, so it changes no behaviour.
//! An unknown venue string answers [`LeverageScope::UNKNOWN`].

/// Where a venue keeps the leverage figure an account trades at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeverageGrain {
    /// One figure for the whole account.
    PerAccount,
    /// One figure per symbol, for both sides alike.
    PerSymbol,
    /// One figure per symbol and side (a buy and a sell figure).
    PerSymbolAndSide,
    /// One figure per instrument and margin mode (cross and isolated hold separate figures).
    PerInstrumentAndMarginMode,
    /// One figure per asset, set together with the asset's cross or isolated flag.
    PerAsset,
    /// No leverage figure: the account's margin model decides the margin, and leverage follows from size.
    WholeAccountMarginModel,
    /// No leverage at all: every position is paid for in full.
    FullyCollateralized,
    /// The broker sets a margin rate per instrument; the account holds no leverage figure of its own.
    BrokerMarginRatePerInstrument,
    /// Not confirmed.
    Unknown,
}

/// Where a venue publishes the maximum of its leverage figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeverageMaximumSource {
    /// A field of the venue's instrument listing (the figure itself, or the margin rate it is the inverse of).
    InstrumentField,
    /// An authenticated request that answers the instrument's brackets, the maximum falling as size grows.
    AuthenticatedBracketRequest,
    /// A field of the account.
    AccountField,
    /// Nowhere: the venue has no leverage to bound.
    Nowhere,
    /// Not confirmed.
    Unknown,
}

/// Whether a program may set the leverage figure through the venue's API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeverageSetter {
    /// A program sets it through the venue's API.
    VenueApi,
    /// No API call sets it: only the broker changes it (on the client's request, through its portal or its
    /// agreement, or at its own discretion).
    BrokerOnly,
    /// There is no figure to set (the grain is `WholeAccountMarginModel` or `FullyCollateralized`).
    NothingToSet,
    /// Not confirmed.
    Unknown,
}

/// One venue's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeverageScope {
    pub grain: LeverageGrain,
    pub maximum_source: LeverageMaximumSource,
    pub setter: LeverageSetter,
}

impl LeverageScope {
    /// The answer for a string that is not a roster venue: nothing is known.
    pub const UNKNOWN: LeverageScope =
        scope(LeverageGrain::Unknown, LeverageMaximumSource::Unknown, LeverageSetter::Unknown);
}

const fn scope(
    grain: LeverageGrain,
    maximum_source: LeverageMaximumSource,
    setter: LeverageSetter,
) -> LeverageScope {
    LeverageScope { grain, maximum_source, setter }
}

/// Binance USDⓈ-M: `POST /fapi/v1/leverage {symbol, leverage}`, one figure for both sides even in hedge mode
/// (<https://developers.binance.com/docs/derivatives/usds-margined-futures/trade/rest-api/Change-Initial-Leverage>;
/// the bridge's `crates/bridges/binance/src/execution/rest/perp.rs`'s `PATH_SET_LEVERAGE`). The maximum is bracket
/// 1's `initialLeverage` from the signed `GET /fapi/v1/leverageBracket`, which answers -2014 unauthenticated
/// (`crates/bridges/binance/src/account/leverage.rs`'s `PATH_LEVERAGE_BRACKET`).
const BINANCE: LeverageScope = scope(
    LeverageGrain::PerSymbol,
    LeverageMaximumSource::AuthenticatedBracketRequest,
    LeverageSetter::VenueApi,
);

/// Aster USDⓈ-M perp (a Binance-fapi fork, its Pro mode): `POST /fapi/v3/leverage {symbol, leverage}`
/// (<https://github.com/asterdex/api-docs/blob/master/V3%28Recommended%29/EN/aster-finance-futures-api-v3.md>;
/// the bridge's `crates/bridges/aster/src/execution/rest/perp.rs`'s `PATH_LEVERAGE`). The maximum is the brackets
/// of the signed `GET /fapi/v3/leverageBrackets`; unsigned, the bracket path answers -1102 (measured 2026-08-05,
/// `crates/bridges/aster/src/adapter.rs`'s `start_with`). Simple mode, its own on-chain product, is not this lane.
const ASTER: LeverageScope = scope(
    LeverageGrain::PerSymbol,
    LeverageMaximumSource::AuthenticatedBracketRequest,
    LeverageSetter::VenueApi,
);

/// Bybit V5 linear and inverse: `POST /v5/position/set-leverage {category, symbol, buyLeverage, sellLeverage}`,
/// the two equal in one-way mode (<https://bybit-exchange.github.io/docs/v5/position/leverage>). The bridge writes
/// one figure to both sides per `(category, symbol)`
/// (`crates/bridges/bybit/src/execution/rest/mod.rs`'s `VenueRestBybit::set_leverage`). The maximum is the public
/// instrument's `leverageFilter.maxLeverage` (<https://bybit-exchange.github.io/docs/v5/market/instrument>;
/// `crates/bridges/bybit/src/account/leverage.rs`'s `parse_max_leverage`).
const BYBIT: LeverageScope = scope(
    LeverageGrain::PerSymbolAndSide,
    LeverageMaximumSource::InstrumentField,
    LeverageSetter::VenueApi,
);

/// OKX: `POST /api/v5/account/set-leverage {instId or ccy, lever, mgnMode, posSide}`, keyed by the instrument (the
/// underlying for cross FUTURES, the currency for some cross MARGIN modes) and the margin mode, plus the position
/// side in long/short isolated (<https://www.okx.com/docs-v5/en/>). The bridge writes `mgnMode: cross` per `instId`
/// (`crates/bridges/okx/src/execution/rest/mod.rs`'s `VenueRestOkx::set_leverage`). The maximum is the public
/// instrument's `lever` (`crates/bridges/okx/src/account/leverage.rs`'s `parse_max_leverage`), tiered by the
/// public `GET /api/v5/public/position-tiers`.
const OKX: LeverageScope = scope(
    LeverageGrain::PerInstrumentAndMarginMode,
    LeverageMaximumSource::InstrumentField,
    LeverageSetter::VenueApi,
);

/// Hyperliquid: the signed `updateLeverage {asset, isCross, leverage}` action, per user and asset
/// (<https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint>;
/// `crates/bridges/hyperliquid/src/signing/action.rs`'s `UpdateLeverageAction`). The maximum is the public `meta`
/// universe's `maxLeverage`, tiered by `marginTables`
/// (<https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint/perpetuals>;
/// `crates/bridges/hyperliquid/src/account/leverage.rs`'s `cap_from_reads`).
const HYPERLIQUID: LeverageScope = scope(
    LeverageGrain::PerAsset,
    LeverageMaximumSource::InstrumentField,
    LeverageSetter::VenueApi,
);

/// Deribit: no method sets a leverage; `private/change_margin_model` picks the (sub)account's model, and leverage
/// follows from size (<https://docs.deribit.com/api-reference/account-management/private-change_margin_model.md>;
/// `crates/vike-venues/src/leverage_venues.rs`'s `leverage_refusal` tells the operator so). The public
/// `public/get_instruments` publishes `max_leverage` per future
/// (<https://docs.deribit.com/api-reference/market-data/public-get_instruments.md>); the bridge reads none.
const DERIBIT: LeverageScope = scope(
    LeverageGrain::WholeAccountMarginModel,
    LeverageMaximumSource::InstrumentField,
    LeverageSetter::NothingToSet,
);

/// Polymarket's prediction markets: fully collateralized, every outcome share paid in full
/// (<https://docs.polymarket.com/concepts/pusd>; `crates/vike-venues/src/venue_margin_support.rs`'s `POLYMARKET`).
/// Polymarket Perps, which carry leverage, are a separate product vike does not trade.
const POLYMARKET: LeverageScope = scope(
    LeverageGrain::FullyCollateralized,
    LeverageMaximumSource::Nowhere,
    LeverageSetter::NothingToSet,
);

/// OANDA v20: the account's `marginRate` override, set by `PATCH /v3/accounts/{accountID}/configuration`; the
/// effective rate is the lesser of it and OANDA's own (<https://developer.oanda.com/rest-live-v20/account-ep/>,
/// <https://developer.oanda.com/rest-live-v20/account-df/>). Each instrument carries OANDA's `marginRate` in
/// `GET /v3/accounts/{accountID}/instruments` (`crates/bridges/oanda/src/catalog.rs`'s `parse_instruments`).
const OANDA: LeverageScope = scope(
    LeverageGrain::PerAccount,
    LeverageMaximumSource::InstrumentField,
    LeverageSetter::VenueApi,
);

/// IG: each market's `marginFactor` and `marginDepositBands` in `GET /markets/{epic}` versions 2 and 3, absent in
/// version 4 (<https://labs.ig.com/reference/markets-epic.html>; `crates/bridges/ig/src/instrument.rs`'s
/// `margin_rate`). No endpoint changes them (<https://labs.ig.com/rest-trading-api-reference.html>): they follow
/// the client's category, and a professional reclassification in My IG is the only route.
const IG: LeverageScope = scope(
    LeverageGrain::BrokerMarginRatePerInstrument,
    LeverageMaximumSource::InstrumentField,
    LeverageSetter::BrokerOnly,
);

/// FXCM ForexConnect: a margin requirement per instrument under the account's leverage profile
/// (`LeverageProfileID`, <https://www.fxcm.com/public-docs/fclite/commonresources/TradingTables/TTAccounts.html>),
/// read per instrument by the session (`crates/bridges/fxcm/src/account/quote_rate.rs`'s `margin_rate_of`;
/// `getMMR` answers -1 on the demo, and a three-level account's margins stand in). FXCM changes leverage at its own
/// discretion
/// (<https://www.fxcm.com/markets/help/trading-basics-how-much-default-leverage-does-fxcm-offer-when-does-this-change/>).
const FXCM: LeverageScope = scope(
    LeverageGrain::BrokerMarginRatePerInstrument,
    LeverageMaximumSource::InstrumentField,
    LeverageSetter::BrokerOnly,
);

/// Dukascopy JForex: the account's leverage, fixed by the contract with the bank and read by
/// `IAccount.getLeverage()` (and `getOverWeekEndLeverage()`), with no setter
/// (<https://www.dukascopy.com/client/javadoc3/com/dukascopy/api/IAccount.html>). It changes by agreement with the
/// bank (<https://www.dukascopy.com/swiss/english/forex/forex-trading-accounts/margin/>).
const DUKASCOPY: LeverageScope = scope(
    LeverageGrain::PerAccount,
    LeverageMaximumSource::AccountField,
    LeverageSetter::BrokerOnly,
);

/// cTrader Open API: the account's `ProtoOATrader.leverageInCents`, capped by its `maxLeverage`, with each symbol's
/// dynamic tiers below it (`ProtoOASymbol.leverageId`, answered by `ProtoOAGetDynamicLeverageByIDReq`)
/// (<https://help.ctrader.com/open-api/model-messages/>). No message sets leverage; the broker configures it
/// (<https://help.ctrader.com/open-api/messages/>).
const CTRADER: LeverageScope = scope(
    LeverageGrain::PerAccount,
    LeverageMaximumSource::AuthenticatedBracketRequest,
    LeverageSetter::BrokerOnly,
);

/// Interactive Brokers: no leverage figure. The account TYPE (cash, Reg T, portfolio margin), chosen in the Client
/// Portal, and IBKR's per-product margin decide, and no API call sets either
/// (<https://interactivebrokers.github.io/tws-api/account_summary.html>). Whether the API publishes a maximum per
/// product is not confirmed: a what-if order answers one order's margin, which is how the bridge reads a future's
/// rate (`crates/bridges/vike-ibkr/src/instrument/any_symbol/forex_future.rs`'s `future_margin_rate`).
const IBKR: LeverageScope = scope(
    LeverageGrain::WholeAccountMarginModel,
    LeverageMaximumSource::Unknown,
    LeverageSetter::NothingToSet,
);

/// Alpaca: the account's buying-power multiplier, set by `PATCH /v2/account/configurations`'s
/// `max_margin_multiplier` (<https://docs.alpaca.markets/us/reference/patchaccountconfig-1>,
/// <https://github.com/alpacahq/alpaca-docs/blob/master/content/api-references/broker-api/trading/trading-configurations.md>).
/// Where the account's maximum is published is not confirmed (the account schema could not be read). Crypto is
/// never bought on margin.
const ALPACA: LeverageScope =
    scope(LeverageGrain::PerAccount, LeverageMaximumSource::Unknown, LeverageSetter::VenueApi);

/// The row of `venue`, one NAMED arm per `vike_model::VENUES` entry; any other string is
/// [`LeverageScope::UNKNOWN`].
#[must_use]
pub fn leverage_scope_for(venue: &str) -> LeverageScope {
    match venue {
        "binance" => BINANCE,
        "bybit" => BYBIT,
        "okx" => OKX,
        "deribit" => DERIBIT,
        "oanda" => OANDA,
        "ig" => IG,
        "fxcm" => FXCM,
        "dukascopy" => DUKASCOPY,
        "polymarket" => POLYMARKET,
        "ibkr" => IBKR,
        "ctrader" => CTRADER,
        "alpaca" => ALPACA,
        "aster" => ASTER,
        "hyperliquid" => HYPERLIQUID,
        // vike:new-venue:row // TODO(new-venue: {venue}): a NAMED row read off the venue's API docs and its bridge
        // vike:new-venue:row "{venue}" => LeverageScope::UNKNOWN,
        _ => LeverageScope::UNKNOWN,
    }
}

#[path = "venue_leverage_scope_tests.rs"]
#[cfg(test)]
mod venue_leverage_scope_tests;
