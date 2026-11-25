//! binance's refusals, declared once (`vike_utility::refusal_enum!`, decision 0301): each has its id and its ONE text,
//! and code and tests decide on the variant, never on the words. Every id is published in the workspace's
//! `refusals.json` (`crates/vike-backend/tests/refusal_catalog.rs`).
//!
//! A refusal leaves this crate as its text where the shared seam carries a `String`
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesError`, `vike_bridge_core::leverage_read::LeverageReader`,
//! `vike_bridge_core::venue_adapter::ModeHealOutcome`, `vike_data::source::SourceError`, a `pub fn` the gatewayhub
//! calls): its `Display`, byte for byte the words it had before it was typed. `family/` (aster's code too) keeps its
//! own words.

use vike_bridge_core::transport::VenueApiError;
use vike_model::{AssetClass, Contract};

vike_utility::refusal_enum! {
    /// Why a symbol the gatewayhub asks this venue's facts of is not admitted
    /// (`crate::symbol_properties::SymbolPropertiesListSpecBinance`). `expired` and `no_instrument` are the
    /// gatewayhub's own words (`vike_bridge_core::symbol_properties::SymbolPropertiesWords`).
    #[derive(Debug, Clone, PartialEq)]
    pub enum BinancePropertiesRefusal with ids BinancePropertiesRefusalId {
        /// A dated future whose delivery has passed, though the venue's list still shows it TRADING.
        Delivered { venue: String, symbol: String, date: String, expired: &'static str } =>
            "binance.properties.delivered":
            "{venue}: {symbol} was delivered on {date}; the venue's list still shows it as TRADING, but {expired}, \
             so the symbol is not admitted",
        /// A COIN-M symbol the COIN-M list of the account's network does not hold; `host` names that list's host.
        CoinMNotListed { no_instrument: &'static str, key: String, host: &'static str } =>
            "binance.properties.coin_m_not_listed":
            "binance{no_instrument}{key} in the venue's instrument list (the COIN-M contracts that are TRADING on \
             {host})",
        /// A COIN-M row with no positive `contractSize`: its face is the row's alone, never a guessed one.
        CoinMValueUnknown { key: String, face: f64, contract: Contract } =>
            "binance.properties.coin_m_value_unknown":
            "binance: {key} is a COIN-M row whose value is unknown (contractSize {face}, {contract:?}): its face is \
             the row's alone, so it is not admitted",
    }
}

vike_utility::refusal_enum! {
    /// Why a kline or funding history ask is refused before (or instead of) a page (`crate::history`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BinanceHistoryRefusal with ids BinanceHistoryRefusalId {
        /// A class claim this venue's data path does not address (`vike_catalog::addressing_for`); `addressed` is
        /// the classes it does, `suffix` the futures lane's (`vike_venues::PERP_SUFFIX`).
        UnaddressableClass {
            addressed: &'static [AssetClass],
            class: AssetClass,
            symbol: String,
            suffix: &'static str,
        } => "binance.history.unaddressable_class":
            "binance: this venue's kline path addresses {addressed:?}, and {class:?} is not one of them (symbol \
             {symbol:?}). ⚠ A dated future (CryptoFuture, USDⓈ-M or COIN-M) is reached with no claim, through the \
             futures lane suffix, as `<venue symbol>{suffix}` (e.g. `BTCUSD_261225{suffix}`), and so is a COIN-M \
             perpetual (`BTCUSD_PERP{suffix}`).",
        /// A symbol whose perpetual marker and the caller's class claim disagree: neither is obeyed.
        ContradictingClaim { symbol: String, marker: &'static str, class: AssetClass } =>
            "binance.history.contradicting_claim":
            "binance: {symbol:?} carries the {marker:?} perpetual marker but the caller claimed {class:?} — two \
             claims that disagree, so neither is obeyed. Drop the suffix or drop the claim.",
        /// A dated COIN-M delivery contract has no funding rate.
        DatedHasNoFunding { wire: String, suffix: &'static str } => "binance.history.dated_has_no_funding":
            "binance: {wire:?} is a DATED COIN-M delivery contract — a dated delivery contract has no funding rate \
             to ask for. The COIN-M PERPETUALS (`<PAIR>_PERP`, e.g. `BTCUSD_PERP`) do, through the store's \
             `{suffix}` suffix.",
        /// The COIN-M listing that picks a futures symbol's book could not be had (`crate::instrument::listing`).
        Listing { refusal: BinanceListingRefusal } => "binance.history.listing": "{refusal}",
    }
}

vike_utility::refusal_enum! {
    /// Why binance's COIN-M listing (`crate::instrument::listing`) gives no answer: never read as "not COIN-M".
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BinanceListingRefusal with ids BinanceListingRefusalId {
        /// The list could not be read: the read's own words.
        Unread { why: String } => "binance.listing.unread": "{why}",
        /// The list read but named no TRADING COIN-M contract: the parser no longer matches the venue's shape.
        ParserMismatch { url: String } => "binance.listing.parser_mismatch":
            "binance: {url} answered but named no TRADING COIN-M contract. That is not a delisting — the venue \
             listed 30 (20 PERPETUAL, 10 dated) when this parser was written — so it is read as this parser no \
             longer matching the venue's shape, and it is refused rather than cached. \
             `vike_binance::instrument::listing::parse_coin_m_contracts` is the map; the row key it gates on is \
             `contractStatus`, which is NOT the `status` key every other binance host spells.",
    }
}

vike_utility::refusal_enum! {
    /// Why a USER name has no lane name (`crate::instrument::names::lane_name_of`): `why` is the kept list's words
    /// (`vike_catalog::kept_raw_of_user_name`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BinanceNameRefusal with ids BinanceNameRefusalId {
        /// No list kept, no row of that name, or two.
        NotKept { why: String } => "binance.name.not_kept": "binance: {why}; nothing was sent",
    }
}

vike_utility::refusal_enum! {
    /// Why an account's key pair asks nothing (`crate::adapter`'s `VenueAdapterBinance`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BinanceAccountRefusal with ids BinanceAccountRefusalId {
        /// The account holds no binance key pair: no leverage written, no mode healed, no reader built.
        NoKeyPair => "binance.account.no_key_pair": "the account holds no binance api key pair",
        /// The same, for an operator's read of the account.
        NoKeyPairNothingAsked => "binance.account.no_key_pair_nothing_asked":
            "the account holds no binance api key pair; nothing was asked",
    }
}

vike_utility::refusal_enum! {
    /// Why the account's leverage for a symbol cannot be read or written (`crate::account::leverage`,
    /// `crate::account::class_prep`, `crate::execution::write_symbol_leverage`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BinanceLeverageRefusal with ids BinanceLeverageRefusalId {
        /// A spot symbol has no leverage.
        SpotSymbol { symbol: String } => "binance.leverage.spot_symbol":
            "{symbol} is a binance SPOT symbol: leverage is a USDⓈ-M futures setting (a perpetual is spelled with \
             the .P suffix)",
        /// A signed read failed: `what` is the read, `why` the transport's words.
        Unread { what: &'static str, why: VenueApiError } => "binance.leverage.unread":
            "{what} could not be read: {why}",
        /// `positionRisk` answered no array.
        NoArray => "binance.leverage.no_array": "positionRisk answered no array",
        /// `positionRisk` answered no row for the symbol.
        NoRow { symbol: String } => "binance.leverage.no_row":
            "the venue answered no positionRisk row for {symbol}",
        /// Several rows: the account is in hedge mode.
        HedgeRows { count: usize, symbol: String } => "binance.leverage.hedge_rows":
            "the venue answered {count} positionRisk rows for {symbol}: the account is in hedge mode, and this \
             adapter orders one-way (positionSide BOTH): choose One-way Mode in the Binance Futures preferences (a \
             DEMO account that is flat is switched by the bridge at its start, and the figure then applies after \
             the account's `active` is set off and on; a live account is never switched by the bridge)",
        /// One row that is a hedge-mode side.
        HedgeSide => "binance.leverage.hedge_side":
            "the positionRisk row is a hedge-mode side (LONG/SHORT), not BOTH",
        /// The symbol is on a margin mode the engine does not model.
        NotCross { mode: String } => "binance.leverage.not_cross":
            "the symbol is on {mode} margin; the engine models cross margin (choose Cross in the Binance Futures \
             preferences; a DEMO symbol that is flat is switched by the bridge at its start, and the figure then \
             applies after the account's `active` is set off and on; a live account is never switched by the \
             bridge)",
        /// No margin word at all.
        NoMarginType => "binance.leverage.no_margin_type":
            "positionRisk carries no marginType, so the margin mode is unknown",
        /// The row's leverage is no figure of at least 1.
        NoLeverage => "binance.leverage.no_leverage": "the positionRisk row carries no usable leverage",
        /// The row's amount is unreadable, so it is not read as flat.
        NoPositionAmt => "binance.leverage.no_position_amt":
            "the positionRisk row carries no usable positionAmt",
        /// A never-traded symbol (leverage `0`) whose first write the venue refused (`crate::account::class_prep`).
        FirstWriteRefused { symbol: String, figure: i64, error: VenueApiError } =>
            "binance.leverage.first_write_refused":
            "binance: {symbol} reads leverage 0 on this account (never traded) and the venue refused the bridge's \
             write of {figure}x ({error}); the venue's default can exceed the symbol's maximum, which makes it \
             refuse the first order (-2027): set a leverage for the symbol in the Binance app",
    }
}

vike_utility::refusal_enum! {
    /// An account mode the bridge's switch could not put right (`crate::account::modes`): what is wrong, what blocks
    /// the fix, what to do.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BinanceModeRefusal with ids BinanceModeRefusalId {
        /// The account is in HEDGE position mode and the switch to one-way did not land.
        Hedge { blocker: BinanceModeBlocker } => "binance.mode.hedge":
            "binance: this account is in HEDGE position mode and this adapter orders ONE-WAY (positionSide BOTH), \
             so the venue refuses every order (-4061). The bridge tried to switch it to one-way and could not: \
             {blocker}. Binance switches the mode only while the account holds no position and no resting order \
             on ANY symbol: close the positions and cancel the orders, then send an order again (the bridge asks \
             the venue again at a refused order, at most every 30 seconds) or set the account's `active` off and \
             on (or choose One-way Mode in the Binance Futures preferences)",
        /// The symbol is on ISOLATED margin and the switch to cross did not land.
        Isolated { symbol: String, blocker: BinanceModeBlocker } => "binance.mode.isolated":
            "binance: {symbol} is on ISOLATED margin and the engine's margin math models CROSS. The bridge tried to \
             switch it to cross and could not: {blocker}. Orders are NOT refused and trade on isolated margin \
             meanwhile; Binance switches a symbol's margin type only while it holds no position and no resting \
             order: close or cancel them and restart the account, or choose Cross in the Binance Futures \
             preferences",
    }
}

vike_utility::refusal_enum! {
    /// What blocked a mode switch ([`BinanceModeRefusal`]'s `blocker`), from the venue's own refusal (its `code` and
    /// `msg` always ride along).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BinanceModeBlocker with ids BinanceModeBlockerId {
        /// The switch's own "resting orders" code.
        RestingOrders { code: i64, msg: String } => "binance.mode.blocker.resting_orders":
            "it holds RESTING ORDERS (the venue answered {code}: {msg})",
        /// The switch's own "open position" code.
        OpenPosition { code: i64, msg: String } => "binance.mode.blocker.open_position":
            "it holds an OPEN POSITION (the venue answered {code}: {msg})",
        /// The key may not change settings (`-2014`/`-2015`).
        KeyRefused { code: i64, msg: String } => "binance.mode.blocker.key_refused":
            "this key may not change account settings (the venue answered {code}: {msg}): enable Futures trading \
             on the key and whitelist this box's IP",
        /// The write got no verdict and the read-back shows the old mode.
        NoVerdict { code: i64, msg: String } => "binance.mode.blocker.no_verdict":
            "the switch got no verdict (the venue answered {code}: {msg}) and reading the account back shows the \
             old mode still",
        /// Any other refusal.
        Refused { code: i64, msg: String } => "binance.mode.blocker.refused":
            "the venue refused the switch (the venue answered {code}: {msg})",
    }
}
