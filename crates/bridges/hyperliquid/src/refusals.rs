//! hyperliquid's refusals, declared once (`vike_utility::refusal_enum!`, decision 0301): each has its id and its ONE
//! text, and code and tests decide on the variant, never on the words. Every id is published in the workspace's
//! `refusals.json` (`crates/vike-backend/tests/refusal_catalog.rs`).
//!
//! A refusal leaves this crate as its text where the shared seam carries a `String`
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesListSpec::symbol_refusal` and `SymbolPropertiesError`,
//! `vike_data::source::SourceError`, `vike_bridge_core::data_client::ws::DataClientSpec::plan`, the gatewayhub's
//! leverage lane, `venue_read`, the reconcile client): its `Display`, byte for byte the words it had before it was
//! typed. The venue's OWN words (an `/exchange` refusal, `crate::execution::event_mapper`'s status words) and another
//! crate's (`vike_venues::venue_urls::UrlRefusal`, the kept list's words) are not here: they travel in a hole.
//!
//! The enums sit outside the `execution` gate, so a feeds-only build declares every id too.

use vike_model::{AssetClass, USDC};

vike_utility::refusal_enum! {
    /// Why a symbol the gatewayhub asks this venue's facts of is not admitted
    /// (`crate::symbol_properties::SymbolPropertiesListSpecHyperliquid`, decisions 0218 and 0290). `not_answered` is
    /// the gatewayhub's own head (`vike_bridge_core::symbol_properties::SymbolPropertiesWords`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HyperliquidPropertiesRefusal with ids HyperliquidPropertiesRefusalId {
        /// The venue's wire name `@N` of a spot pair, refused by its shape before any read.
        WireName { not_answered: &'static str, symbol: String } => "hyperliquid.properties.wire_name":
            "{not_answered}: hyperliquid `{symbol}` is the venue's wire name of a spot pair; write the pair's base \
             and quote (for example HYPEUSDC)",
        /// No kept row (or two) answers the USER name; `why` is the kept list's words.
        NoKeptRow { not_answered: &'static str, why: String } => "hyperliquid.properties.no_kept_row":
            "{not_answered}: hyperliquid {why}",
        /// A perpetual the venue has delisted.
        Delisted { symbol: String } => "hyperliquid.properties.delisted":
            "hyperliquid: `{symbol}` is delisted; no order can open on it",
        /// A spot pair quoted in another token than USDC.
        SpotNotUsdcQuoted { symbol: String, quote: String } => "hyperliquid.properties.spot_not_usdc_quoted":
            "hyperliquid: `{symbol}` is quoted in {quote}, and only a pair quoted in {USDC} is admitted (the \
             engine's ledger is one currency)",
        /// A row of a class that is neither a perpetual nor a spot pair.
        NeitherPerpNorSpot { not_answered: &'static str, symbol: String } =>
            "hyperliquid.properties.neither_perp_nor_spot":
            "{not_answered}: hyperliquid `{symbol}` is neither a perpetual nor a spot pair",
    }
}

vike_utility::refusal_enum! {
    /// Why a candle or funding history read (`crate::history`) has no answer: a store name no kept row stores
    /// (nothing was asked), or a page the venue did not answer.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HyperliquidHistoryRefusal with ids HyperliquidHistoryRefusalId {
        /// No kept row (or two) is stored under the series' store name; `why` is the kept list's words.
        UnstoredName { why: String } => "hyperliquid.history.unstored_name":
            "hyperliquid history: {why}; nothing was asked of the venue",
        /// A `candleSnapshot` page failed; `why` is the transport's words.
        CandlesUnread { coin: String, interval: String, why: String } => "hyperliquid.history.candles_unread":
            "hyperliquid candleSnapshot {coin} {interval}: {why}",
        /// A `fundingHistory` page failed; `why` is the transport's words.
        FundingUnread { why: String } => "hyperliquid.history.funding_unread": "fundingHistory: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why a market-data lane is not subscribed (`crate::feed::DataClientSpecHyperliquid`): nothing was sent, and the
    /// raw name is never subscribed as a coin (audit flag HL-3).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HyperliquidFeedRefusal with ids HyperliquidFeedRefusalId {
        /// A mark lane of a spot pair: the venue states no mark for one.
        SpotHasNoMark { symbol: String } => "hyperliquid.feed.spot_has_no_mark":
            "hyperliquid: `{symbol}` is a spot pair, and the venue states no mark price for one",
        /// A lossless book lane: `l2Book` is a full snapshot every frame.
        BookSnapshotOnly => "hyperliquid.feed.book_snapshot_only": "hyperliquid l2Book is snapshot-only",
        /// The loaded list holds no row of the user name; `fix` is `; <the refresh's words>` or empty.
        NoInstrument { symbol: String, fix: String } => "hyperliquid.feed.no_instrument":
            "hyperliquid: `{symbol}` names no instrument of the venue's list (decision 0290's names: a perpetual \
             `BTCUSDC.P`, a spot pair `HYPEUSDC`){fix}; nothing was subscribed",
        /// The list the coin is read off was not loaded within the lane's wait.
        ListNotLoaded { symbol: String, wait_ms: u128 } => "hyperliquid.feed.list_not_loaded":
            "hyperliquid: `{symbol}`'s wire coin is read off the venue's list (meta, spotMeta), which this feed has \
             not loaded within {wait_ms} ms; nothing was subscribed",
    }
}

vike_utility::refusal_enum! {
    /// Why an operator's ask of the account (its leverage cap, `crate::account::leverage`; its read,
    /// `crate::reconcile::venue_read`) has no answer: no key, an unusable one, a book the venue did not name, or a
    /// keyless `/info` read that failed.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HyperliquidAccountRefusal with ids HyperliquidAccountRefusalId {
        /// The account holds no `PRIVATE_KEY`: nothing was asked.
        NoKey => "hyperliquid.account.no_key": "the account holds no hyperliquid key",
        /// The key does not parse; `why` is the signer's words, which carry no character of the key.
        KeyUnusable { why: String } => "hyperliquid.account.key_unusable": "the account's key is unusable ({why})",
        /// The venue's `userRole` did not confirm the book the key trades: never a read of the agent's empty one.
        MasterUnconfirmed => "hyperliquid.account.master_unconfirmed":
            "the venue did not confirm which account this key trades for",
        /// A read failed; `what` names it (`meta`, `activeAssetData`, `frontendOpenOrders`), `why` is the
        /// transport's words.
        Unread { what: &'static str, why: String } => "hyperliquid.account.unread": "{what} could not be read: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why the gatewayhub's leverage lane has no cap for a coin (`crate::account::leverage`, decision 0181): a coin
    /// the engine cannot margin cross, or an answer of a shape the reader does not know, never a guess.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HyperliquidLeverageRefusal with ids HyperliquidLeverageRefusalId {
        /// `meta` lists no core perpetual of that coin.
        NotListed { coin: String } => "hyperliquid.leverage.not_listed":
            "hyperliquid lists no core perpetual `{coin}` (meta)",
        /// The coin is delisted.
        Delisted { coin: String } => "hyperliquid.leverage.delisted": "hyperliquid has delisted `{coin}`",
        /// The coin trades isolated only (`onlyIsolated`, `strictIsolated`, `noCross`).
        IsolatedOnly { coin: String } => "hyperliquid.leverage.isolated_only":
            "`{coin}` can only be traded isolated; the engine margins cross",
        /// `meta` states no `maxLeverage` of at least 1.
        NoMaxLeverage { coin: String } => "hyperliquid.leverage.no_max_leverage":
            "meta carries no usable maxLeverage for `{coin}`",
        /// `activeAssetData` carries no `leverage` object.
        NoLeverage { coin: String } => "hyperliquid.leverage.no_leverage":
            "activeAssetData carries no leverage for `{coin}`",
        /// The account holds the coin at a leverage that is not cross.
        NotCross { coin: String } => "hyperliquid.leverage.not_cross":
            "this account holds a non-cross leverage on `{coin}`; the engine margins cross, so set it to cross on \
             the venue first",
        /// The cross leverage carries no figure of at least 1.
        NoCrossLeverage { coin: String } => "hyperliquid.leverage.no_cross_leverage":
            "activeAssetData carries no usable cross leverage for `{coin}`",
        /// No core perpetual of `meta` is named the USER name; `fix` is `; <the refresh's words>` or empty.
        NoPerpetualNamed { symbol: String, fix: String } => "hyperliquid.leverage.no_perpetual_named":
            "hyperliquid lists no core perpetual named `{symbol}` (meta){fix}",
    }
}

vike_utility::refusal_enum! {
    /// Why an operator's read of the account (`crate::reconcile::venue_read`) has no answer in the venue's reply.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HyperliquidVenueReadRefusal with ids HyperliquidVenueReadRefusalId {
        /// The balance read answered no figure.
        NoBalance => "hyperliquid.venue_read.no_balance": "hyperliquid states no balance for this account",
        /// `frontendOpenOrders` answered something other than a list.
        NotAList => "hyperliquid.venue_read.not_a_list": "hyperliquid's frontendOpenOrders answer is not a list",
        /// `spotClearinghouseState` answered no `balances` array, so the spot wallet's coins cannot be listed.
        SpotBalancesNotAList => "hyperliquid.venue_read.spot_balances_not_a_list":
            "spotClearinghouseState: expected a `balances` array",
    }
}

vike_utility::refusal_enum! {
    /// Why the venue's listing (`crate::catalog::CatalogProviderHyperliquid`) has no answer: a class this venue lists
    /// none of (`crate::config::unlisted_class`: the catalog, the grid and the reconcile reads branch on `CryptoPerp`
    /// (`meta`) and `CryptoSpot` (`spotMeta`) only), or a keyless `/info` read of the listing that failed.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HyperliquidCatalogRefusal with ids HyperliquidCatalogRefusalId {
        /// Any other class, named.
        UnlistedClass { class: AssetClass } => "hyperliquid.catalog.unlisted_class":
            "hyperliquid lists no {class:?} instrument: its classes are CryptoPerp (meta) and CryptoSpot (spotMeta)",
        /// A listing read failed; `what` names it (`perpDexs read`, `meta/spotMeta load`), `why` is the transport's
        /// words.
        Unread { what: &'static str, why: String } => "hyperliquid.catalog.unread": "hyperliquid {what}: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why the reconcile client reads no fee schedule for the mount (`crate::reconcile`'s `read_fee_rates`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HyperliquidReconcileRefusal with ids HyperliquidReconcileRefusalId {
        /// A HIP-3 builder-dex perpetual: its fee scale is stated by no documented read.
        Hip3Fee { symbol: String } => "hyperliquid.reconcile.hip3_fee":
            "{symbol} is a HIP-3 builder-dex perpetual: its fee is the account's perp rate times the deployer's fee \
             scale (1+s under 1, 2s from 1, a tenth in growth mode), which no read this bridge makes states, so the \
             account's core perp rate is not reported as its fee schedule",
    }
}
