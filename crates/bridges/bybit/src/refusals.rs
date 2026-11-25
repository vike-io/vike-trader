//! bybit's refusals, declared once (`vike_utility::refusal_enum!`, decision 0301): each has its id and its ONE text,
//! and code and tests decide on the variant, never on the words. Every id is published in the workspace's
//! `refusals.json` (`crates/vike-backend/tests/refusal_catalog.rs`).
//!
//! A refusal leaves this crate as its text where the seam it crosses carries a `String`
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesError`, `vike_data::source::SourceError`, the adapter's
//! `venue_read` and `heal_modes`, `vike_bridge_core::leverage_read::LeverageWriteError`, the `LeverageReader` reads,
//! `crate::instrument::class::venue_symbol`): its `Display`, byte for byte the words it had before it was typed.

use vike_model::AssetClass;

vike_utility::refusal_enum! {
    /// Why a symbol is not admitted or not mounted: the gatewayhub's symbol facts
    /// (`crate::symbol_properties::SymbolPropertiesListSpecBybit`), the name's kept row
    /// (`crate::instrument::class::venue_symbol`) and the mounted row (`crate::execution`'s `mount_money` and
    /// `non_linear_perpetual_refusal`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BybitSymbolRefusal with ids BybitSymbolRefusalId {
        /// A dated contract inside its delivery guard's window: the venue closes every position at delivery, and this
        /// adapter does not fold that settlement. `expiry` is the delivery, in ms; `window_hours` the guard's window.
        Delivered { symbol: String, expiry: i64, window_hours: i64 } => "bybit.symbol.delivered":
            "bybit: {symbol} delivers in 0 h (at {expiry} ms): the venue closes every position at delivery and this \
             adapter does not fold that settlement, so no order opens on a contract within {window_hours} h of it; \
             trade the next one",
        /// A quanto contract, whose notional is not valued.
        Quanto { symbol: String } => "bybit.symbol.quanto":
            "bybit: {symbol} is a quanto contract, whose notional is not valued",
        /// A row of no kind this gatewayhub trades (an option, a kind the venue added); `word` is its `contractType`.
        NotTraded { symbol: String, word: String } => "bybit.symbol.not_traded":
            "bybit: {symbol} is {word}, which this gatewayhub does not trade (it trades linear and coin-margined \
             perpetuals, dated futures of either, and spot pairs; an option needs a payoff and short-margin model the \
             engine does not have)",
        /// The user name has no kept row (or two): `why` is the catalog's own sentence.
        NoKeptRow { why: String } => "bybit.symbol.no_kept_row": "bybit: {why}; nothing was sent",
        /// The kept row is of no book this bridge signs or streams; `wire` is the venue's name of it.
        NoBook { user: String, wire: String } => "bybit.symbol.no_book":
            "bybit: `{user}` (the venue's `{wire}`) is of no book this bridge signs or streams (spot, linear, \
             inverse); nothing was sent",
        /// The exec refusal of a contract whose `contractType` (`word`) names no book this adapter signs.
        NotSigned { symbol: String, word: String } => "bybit.symbol.not_signed":
            "bybit exec REFUSES {symbol:?}: the venue reports contractType {word:?}, which is not a book this \
             adapter signs (it signs linear and coin-margined perpetuals and dated futures, and spot pairs; an \
             option needs a payoff and short-margin model the engine does not have), so nothing is being sent. The \
             market-data side of this instrument charts and backfills.",
        /// A coin-margined mount whose instrument row could not be read (`row` says so) or names no coin-margined
        /// contract and settle coin: its grid, value and coin are unknown.
        CoinMarginedRowUnknown { symbol: String, row: &'static str } => "bybit.symbol.coin_margined_row_unknown":
            "bybit: cannot mount {symbol:?}: it is coin-margined, and its instrument row {row}, so its grid, value \
             and settle coin are unknown (a linear grid would size every order wrong); nothing is sent",
        /// A name the one-symbol spot read cannot ask for (`crate::catalog`'s `spot_symbol`).
        NotSpotSpelling { symbol: String } => "bybit.symbol.not_spot_spelling":
            "bybit: `{symbol}` is not a spot pair spelling",
        /// A name the one-symbol derivative read cannot ask for (`crate::catalog`'s `perp_symbol`).
        NotPerpetualSpelling { symbol: String } => "bybit.symbol.not_perpetual_spelling":
            "bybit: `{symbol}` is not a perpetual symbol spelling",
        /// A one-symbol `instruments-info` read answered a non-2xx status; `head` is the body's head.
        ReadHttp { category: String, status: u16, head: String } => "bybit.symbol.read_http":
            "bybit {category}: HTTP {status}: {head}",
        /// A one-symbol `instruments-info` read whose body is no JSON; `why` is the parser's words.
        ReadJson { category: String, why: String } => "bybit.symbol.read_json": "bybit {category}: JSON: {why}",
        /// A one-symbol `instruments-info` read the venue refused (`retCode` not 0); `msg` is its `retMsg`.
        ReadRefused { category: String, msg: String } => "bybit.symbol.read_refused":
            "bybit {category}: the venue refused the read: {msg}",
    }
}

vike_utility::refusal_enum! {
    /// A history or live-feed route this venue refuses, or a kline or funding answer it cannot read
    /// (`crate::history`, `crate::instrument::listing`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BybitHistoryRefusal with ids BybitHistoryRefusalId {
        /// An interval the venue's word table has no code for.
        UnsupportedInterval { interval: String } => "bybit.history.unsupported_interval":
            "bybit: unsupported interval {interval:?}",
        /// A claimed class this venue's kline path does not address; `classes` are the ones it does.
        ClassNotAddressed { classes: &'static [AssetClass], class: AssetClass, symbol: String } =>
            "bybit.history.class_not_addressed":
            "bybit: this venue's kline path addresses {classes:?}, and {class:?} is not one of them (symbol \
             {symbol:?})",
        /// A perpetual marker (`suffix`) and a class claim that disagree.
        SuffixContradictsClaim { symbol: String, suffix: &'static str, class: AssetClass } =>
            "bybit.history.suffix_contradicts_claim":
            "bybit: {symbol:?} carries the {suffix:?} perpetual marker but the caller claimed {class:?} — two \
             claims that disagree, so neither is obeyed. Drop the suffix or drop the claim.",
        /// The name's kept row (`book`, `wire`) and a class claim that disagree.
        KeptRowContradictsClaim { symbol: String, book: &'static str, wire: String, class: AssetClass } =>
            "bybit.history.kept_row_contradicts_claim":
            "bybit: {symbol:?} is the {book} book's `{wire}` by its kept row, but the caller claimed {class:?} — two \
             claims that disagree, so neither is obeyed",
        /// A perpetual claim on a symbol the venue lists in both derivative books.
        BothDerivativeBooks { wire: String } => "bybit.history.both_derivative_books":
            "bybit lists {wire:?} Trading under BOTH category=linear AND category=inverse, so a perpetual claim on it \
             names two books and neither is being guessed. This cannot happen on the venue as MEASURED 2026-09-16 \
             (`linear` and `inverse` share no symbol); it firing means the listings changed, and \
             `crates/bridges/bybit/src/instrument/listing.rs`'s route predicate needs a fact this call does not have.",
        /// An unclaimed bare symbol the venue lists as a spot pair AND an inverse perpetual.
        AmbiguousBareSymbol { wire: String } => "bybit.history.ambiguous_bare_symbol":
            "bybit lists {wire:?} as a Trading SPOT pair AND a Trading INVERSE PERPETUAL, and this call named no \
             asset class — so which book was meant cannot be decided here, and it is not being guessed. The caller \
             must name the class: CryptoSpot for the spot pair, CryptoPerp for the perpetual \
             (`vike_bybit::history::fetch_klines_range_classed`). This is worth naming rather than defaulting \
             because the spot side of such a collision is the near-dead one — MEASURED on `BTCUSD`: four of six bars \
             carried zero volume against a price ~$70 off the perpetual's, on a listing whose status is nonetheless \
             `Trading`. ⚠ Do NOT reach for the `.P` suffix here: it says PERPETUAL, not inverse. It happens to land \
             on the inverse book for this symbol because the quote asset is USD rather than USDT and because bybit \
             resolves the request leniently — a property of the venue's API, not a claim anybody made.",
        /// A funding series asked of a name that is no perpetual; `suffix` is the store's perpetual marker.
        FundingNoPerpetual { symbol: String, suffix: &'static str } => "bybit.history.funding_no_perpetual":
            "bybit: {symbol:?} names no perpetual (a bare name is the SPOT pair here, and a dated future has no \
             funding) — ask for `{symbol}{suffix}`, the store's spelling of the perpetual, linear or inverse",
        /// A kline answer whose `retCode` is not 0: the venue's code and message.
        KlinesRetCode { code: Option<i64>, msg: String } => "bybit.history.klines_ret_code":
            "bybit klines retCode {code:?}: {msg}",
        /// A kline answer with no `result.list`.
        KlinesNoList => "bybit.history.klines_no_list": "bybit klines: missing result.list",
        /// A kline row that is not an array.
        KlineRowNotArray => "bybit.history.kline_row_not_array": "bybit kline row not array",
        /// The latest-klines seed answered a non-2xx status; `head` is the body's head.
        KlinesHttp { status: u16, head: String } => "bybit.history.klines_http":
            "bybit klines HTTP {status}: {head}",
        /// A funding answer whose `retCode` is not 0: the venue's code and message.
        FundingRetCode { code: Option<i64>, msg: String } => "bybit.history.funding_ret_code":
            "bybit funding retCode {code:?}: {msg}",
        /// A funding answer with no `result.list`.
        FundingNoList => "bybit.history.funding_no_list": "bybit funding: missing result.list",
        /// An instrument listing that still offered a page after the walk's ceiling: a short list answers nothing.
        ListingNeverEnds { base_url: String, max_pages: usize } => "bybit.history.listing_never_ends":
            "bybit instruments-info: {base_url} still offered a page after {max_pages} — refusing to answer from a \
             list that may be short",
        /// An instrument listing that handed back the cursor just spent.
        ListingRepeatedCursor { base_url: String, page: usize } => "bybit.history.listing_repeated_cursor":
            "bybit instruments-info: {base_url} repeated its page cursor at page {page} — refusing to answer from a \
             list that may be short",
        /// An instrument listing page that could not be read: `why` is the read's own words.
        ListingUnread { why: String } => "bybit.history.listing_unread": "{why}",
    }
}

vike_utility::refusal_enum! {
    /// An operator's read or write of the account that is refused before or after the venue is asked (the adapter's
    /// `venue_read`, `set_symbol_leverage` and `heal_modes`; `crate::reconcile::venue_read`'s balance).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BybitAccountRefusal with ids BybitAccountRefusalId {
        /// The account holds no bybit api key pair.
        NoKeyPair => "bybit.account.no_key_pair": "the account holds no bybit api key pair",
        /// The account holds no bybit api key pair, so an operator's read asked nothing.
        NoKeyPairNothingAsked => "bybit.account.no_key_pair_nothing_asked":
            "the account holds no bybit api key pair; nothing was asked",
        /// The wallet-balance answer carries no `list`.
        WalletNoList => "bybit.account.wallet_no_list": "bybit's wallet-balance answer carries no `list`",
        /// A wallet row with no usable `walletBalance`; `asset` is its coin.
        WalletNoBalance { asset: String } => "bybit.account.wallet_no_balance":
            "bybit states no walletBalance for its `{asset}` row",
        /// `account/info` states no account version this bridge knows (`status` is its `unifiedMarginStatus`), so
        /// which wallet holds the inverse book is not guessed.
        UnknownAccountVersion { status: String } => "bybit.account.unknown_account_version":
            "bybit /v5/account/info states no account version this bridge knows (unifiedMarginStatus {status}): the \
             wallet of the inverse book is not guessed",
    }
}

vike_utility::refusal_enum! {
    /// Why the leverage the gatewayhub's lane asks of a symbol cannot be read or written (`crate::account::leverage`,
    /// `crate::execution`'s `write_symbol_leverage`): the account or the symbol is not the one-way cross linear book
    /// the engine models.
    #[derive(Debug, Clone, PartialEq)]
    pub enum BybitLeverageRefusal with ids BybitLeverageRefusalId {
        /// The account is in isolated margin.
        IsolatedAccount => "bybit.leverage.isolated_account":
            "the account is in isolated margin (marginMode ISOLATED_MARGIN); the engine models cross margin",
        /// The account is in portfolio margin.
        PortfolioAccount => "bybit.leverage.portfolio_account":
            "the account is in portfolio margin (marginMode PORTFOLIO_MARGIN), which has no leverage number per symbol",
        /// A `marginMode` word that is not plain cross margin.
        MarginModeWord { word: String } => "bybit.leverage.margin_mode_word":
            "the account's marginMode is {word:?}, which is not plain cross margin",
        /// `account/info` carries no `marginMode`.
        NoMarginMode => "bybit.leverage.no_margin_mode":
            "GET /v5/account/info carries no marginMode, so the account's margin mode is unknown",
        /// `position/list` carries no `list`.
        NoPositionList => "bybit.leverage.no_position_list": "GET /v5/position/list carries no `list`",
        /// No position row for the symbol.
        NoPositionRow { symbol: String } => "bybit.leverage.no_position_row":
            "the venue answered no position row for {symbol}",
        /// Several position rows for the symbol: hedge mode.
        HedgeRows { rows: usize, symbol: String } => "bybit.leverage.hedge_rows":
            "the venue answered {rows} position rows for {symbol}: the account is in hedge mode, and this adapter \
             orders one-way (positionIdx 0)",
        /// The row's `positionIdx` is not 0: hedge mode.
        HedgeIndex { index: f64 } => "bybit.leverage.hedge_index":
            "the position row is positionIdx {index} (hedge mode), not one-way",
        /// The symbol is held in isolated margin (`tradeMode` 1).
        IsolatedSymbol => "bybit.leverage.isolated_symbol":
            "the position row is tradeMode 1 (isolated margin on this symbol); the engine models cross margin",
        /// The row's `leverage` is empty or no figure of at least 1.
        NoUsableLeverage => "bybit.leverage.no_usable_leverage":
            "the position row carries no usable leverage (portfolio margin answers an empty one)",
        /// The row's `size` is missing or no figure: the open-position guard fails closed.
        NoUsableSize => "bybit.leverage.no_usable_size": "the position row carries no usable size",
        /// `instruments-info` lists no linear contract of the symbol.
        NotListed { symbol: String } => "bybit.leverage.not_listed":
            "instruments-info lists no linear contract {symbol}",
        /// The symbol's row is not a linear perpetual; `kind` is its `contractType`.
        NotLinearPerpetual { symbol: String, kind: String } => "bybit.leverage.not_linear_perpetual":
            "{symbol} is a {kind}; this adapter trades linear perpetuals only",
        /// A spot pair holds no leverage to read: the engine keeps its own margin rate.
        SpotPairRead { symbol: String } => "bybit.leverage.spot_pair_read":
            "bybit: {symbol} is a spot pair, which holds no leverage; the engine keeps its own margin rate",
        /// A spot pair holds no leverage to write.
        SpotPairWrite { symbol: String } => "bybit.leverage.spot_pair_write":
            "bybit: {symbol} is a spot pair, which holds no leverage; nothing was sent",
    }
}
