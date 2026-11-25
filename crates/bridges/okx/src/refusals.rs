//! okx's refusals, declared once (`vike_utility::refusal_enum!`, decision 0301): each has its id and its ONE text,
//! and code and tests decide on the variant, never on the words. Every id is published in the workspace's
//! `refusals.json` (`crates/vike-backend/tests/refusal_catalog.rs`).
//!
//! A refusal leaves this crate as its text where the shared seam carries a `String`
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesError`, `vike_data::source::SourceError`,
//! `vike_bridge_core::leverage_read::LeverageReader`, `vike_bridge_core::venue_adapter::ModeHealOutcome`, a venue
//! read's `Err`): its `Display`, byte for byte the words it had before it was typed. The venue's OWN words (its codes
//! and messages, `crate::transport::error_codes`) are not here: they are the venue's, read where they arrive.

vike_utility::refusal_enum! {
    /// Why a symbol the gatewayhub asks this venue's facts of is not admitted
    /// (`crate::symbol_properties::SymbolPropertiesListSpecOkx`, decision 0232).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OkxPropertiesRefusal with ids OkxPropertiesRefusalId {
        /// A dated future past its delivery: the engine books no settlement.
        FutureDelivered { symbol: String } => "okx.properties.future_delivered":
            "okx: {symbol} is delivered already (08:00 UTC of the date in its name); the venue settles a position at \
             the delivery and the engine books no settlement, so a future is admitted only until its delivery. Trade \
             the next expiry",
        /// A spot pair quoted in another asset than the USDT ledger.
        SpotNotUsdtQuoted { symbol: String, quote: String } => "okx.properties.spot_not_usdt_quoted":
            "okx: {symbol} is quoted in {quote}, and the engine's ledger is USDT, so only a USDT-quoted pair is \
             admitted (a pair quoted in another asset would be valued at par)",
        /// An option: the engine has no option payoff or margin model.
        OptionNotAdmitted { symbol: String } => "okx.properties.option_not_admitted":
            "okx: {symbol} is an option, which the gatewayhub does not admit: its premium is quoted in the \
             underlying coin (tick 0.0001 BTC on the demo's BTC-USD family) and a contract is 0.01 of it, so the \
             engine's quantity times price would value it in the wrong money, and the engine has no option payoff \
             or short-option margin model. The venue also wants options trading activated by hand on its app or web \
             before it takes an order (code 51198)",
        /// An instrument of a class the gatewayhub does not trade; `class` is its asset class's word.
        ClassNotTraded { symbol: String, class: &'static str } => "okx.properties.class_not_traded":
            "okx: {symbol} is a {class} instrument, which the gatewayhub does not trade (spot pairs, SWAPs and dated \
             futures only)",
        /// A contract that is neither linear nor coin-margined (a quanto); `kind` is the row's contract type.
        NotLinear { symbol: String, kind: String } => "okx.properties.not_linear":
            "okx: {symbol} is {kind}, neither a linear nor a coin-margined perpetual or future, so its notional is \
             not valued",
        /// A coin-margined contract counted in anything but US dollars.
        InverseNotUsd { symbol: String, currency: String } => "okx.properties.inverse_not_usd":
            "okx: {symbol} is coin-margined and its contract is counted in {currency}, not in US dollars, so its \
             notional is not valued",
        /// The row carries no usable contract size.
        NoContractSize { symbol: String } => "okx.properties.no_contract_size":
            "okx: instrument {symbol}: the venue's instrument list carries no contract size (missing or zero)",
        /// Neither a SWAP nor a dated future, where one is needed.
        NotSwapOrFuture { symbol: String } => "okx.properties.not_swap_or_future":
            "okx: {symbol} is not a SWAP perpetual or a dated future",
        /// A linear contract settled in anything but USDT.
        SettleNotUsdt { symbol: String, settle: String } => "okx.properties.settle_not_usdt":
            "okx: {symbol} settles in {settle}; the gatewayhub admits USDT-settled swaps and futures only for now",
        /// A linear contract counted in another asset than its base.
        NotCountedInBase { symbol: String, currency: String, base: String } =>
            "okx.properties.not_counted_in_base":
            "okx: {symbol}: a contract is counted in {currency}, not in the base asset {base}",
    }
}

vike_utility::refusal_enum! {
    /// Why a candle or funding history read (`crate::history`) has no answer: a request this source cannot express,
    /// or a page the venue answered with an error or a shape it does not document.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OkxHistoryRefusal with ids OkxHistoryRefusalId {
        /// An interval okx has no `bar` code for: the venue is not asked.
        UnsupportedInterval { interval: String } => "okx.history.unsupported_interval":
            "okx: unsupported interval {interval:?}",
        /// A non-zero `code` in the envelope: `what` names the read (`okx candles`, `okx funding`), `code` and `msg`
        /// are the venue's.
        VenueCode { what: &'static str, code: String, msg: String } => "okx.history.venue_code":
            "{what} code {code}: {msg}",
        /// An envelope with no `data` array.
        MissingData { what: &'static str } => "okx.history.missing_data": "{what}: missing data",
        /// A candle row that is not an array.
        RowNotArray => "okx.history.row_not_array": "okx candle row not array",
        /// A candle row the shared row mapper could not read; `why` is its words.
        RowUnread { why: String } => "okx.history.row_unread": "{why}",
        /// A body that is no JSON; `why` is the parser's words.
        NotJson { why: String } => "okx.history.not_json": "{why}",
        /// The latest-candles read answered a non-2xx status; `head` is the body's head.
        Http { status: u16, head: String } => "okx.history.http": "okx candles HTTP {status}: {head}",
        /// A funding read of a name that is no perpetual swap: only a swap has funding, and nothing is asked.
        FundingNotASwap { user: String } => "okx.history.funding_not_a_swap":
            "okx: {user:?} is not a perpetual swap, and funding is a swap's series (`BTCUSDT.P`, `BTCUSD.P`)",
    }
}

vike_utility::refusal_enum! {
    /// Why a name cannot be signed or read as an okx instrument (`crate::instrument`): no kept row names it, or its
    /// instId is of no class this adapter knows. Nothing was sent.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OkxInstrumentRefusal with ids OkxInstrumentRefusalId {
        /// No kept row (or two) answers the USER name (decision 0290); `why` is the kept list's words.
        NoKeptRow { why: String } => "okx.instrument.no_kept_row": "okx: {why}; nothing was sent",
        /// An instId of no known form: its class is never guessed (OKX-1).
        UnknownForm { inst_id: String } => "okx.instrument.unknown_form":
            "okx: `{inst_id}` is no instrument id of a spot pair, a SWAP or a dated future (`BASE-QUOTE`, \
             `BASE-QUOTE-SWAP`, `BASE-QUOTE-YYMMDD`); its class is not guessed and nothing was sent",
    }
}

vike_utility::refusal_enum! {
    /// Why the account cannot be asked for what an operator or the gatewayhub's lane wants of it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OkxAccountRefusal with ids OkxAccountRefusalId {
        /// The account holds no complete key trio: a leverage write or a mode heal is refused.
        NoKeySet => "okx.account.no_key_set":
            "the account holds no complete okx key set (key, secret, passphrase)",
        /// The same, for an operator's read of the account: nothing was asked.
        NoKeySetNothingAsked => "okx.account.no_key_set_nothing_asked":
            "the account holds no complete okx key set (key, secret, passphrase); nothing was asked",
        /// One of the account-wide open-order lists of an operator's read failed; `list` names it
        /// (`orders-pending`, `orders-algo-pending (conditional)`), `why` is the read's words.
        OpenOrdersUnread { list: String, why: String } => "okx.account.open_orders_unread":
            "okx {list} could not be read: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why the gatewayhub's leverage lane cannot read or write a figure on this account (`crate::account::leverage`,
    /// decision 0182): a mode the engine does not model, an answer of a shape the reader does not know, or a read that
    /// failed.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OkxLeverageRefusal with ids OkxLeverageRefusalId {
        /// `acctLv` 1: no swap margin.
        AccountSpotOnly => "okx.leverage.account_spot_only":
            "the account is spot-only (acctLv 1): it has no swap margin",
        /// `acctLv` 4: no cross swap leverage.
        AccountPortfolioMargin => "okx.leverage.account_portfolio_margin":
            "the account is in portfolio margin (acctLv 4), which cannot set or read a cross swap leverage",
        /// An `acctLv` the reader does not know.
        AccountLevelUnknown { level: String } => "okx.leverage.account_level_unknown":
            "the account level acctLv {level} is not one this reader knows",
        /// `account/config` states no `acctLv`.
        AccountLevelAbsent => "okx.leverage.account_level_absent":
            "account/config carries no acctLv, so the account level is unknown",
        /// A position mode other than `net_mode`.
        PositionModeNotNet { mode: String } => "okx.leverage.position_mode_not_net":
            "the account's position mode is {mode:?}; this adapter orders net positions (posSide net)",
        /// `account/config` states no `posMode`.
        PositionModeAbsent => "okx.leverage.position_mode_absent":
            "account/config carries no posMode, so the position mode is unknown",
        /// `account/config` answered no row.
        ConfigNoRow => "okx.leverage.config_no_row": "account/config answered no row",
        /// An answer with no `data` array; `what` names the read.
        NoDataArray { what: &'static str } => "okx.leverage.no_data_array": "{what} carries no data array",
        /// No leverage row for the instrument.
        NoLeverageRow { symbol: String } => "okx.leverage.no_leverage_row":
            "the venue answered no leverage row for {symbol}",
        /// Several leverage rows: a long/short account.
        SeveralLeverageRows { count: usize, symbol: String } => "okx.leverage.several_leverage_rows":
            "the venue answered {count} leverage rows for {symbol}: a long/short account, and this adapter orders \
             net positions",
        /// The one leverage row is a long/short side's.
        LeverageRowNotNet => "okx.leverage.leverage_row_not_net":
            "the leverage row is for a long/short side, not net",
        /// The one leverage row is not cross.
        LeverageRowNotCross => "okx.leverage.leverage_row_not_cross": "the leverage row is not for cross margin",
        /// The leverage row carries no figure of at least 1.
        LeverageRowNoLever => "okx.leverage.leverage_row_no_lever": "the leverage row carries no usable lever",
        /// A position row of the instrument with no usable size: never read as flat.
        PositionNoSize { symbol: String } => "okx.leverage.position_no_size":
            "the {symbol} position row carries no usable pos",
        /// An open position on a long/short side.
        PositionNotNet { symbol: String } => "okx.leverage.position_not_net":
            "an open {symbol} position is a long/short side, not net",
        /// An open isolated position.
        PositionIsolated { symbol: String } => "okx.leverage.position_isolated":
            "an open {symbol} position is isolated; the engine models cross margin",
        /// `public/instruments` lists no SWAP of the instrument.
        NoSwapRow { symbol: String } => "okx.leverage.no_swap_row": "public/instruments lists no SWAP {symbol}",
        /// A contract that is not linear.
        NotLinear { symbol: String, kind: String } => "okx.leverage.not_linear":
            "{symbol} is a {kind} contract; this adapter trades linear swaps only",
        /// A spot pair holds no leverage (the reader's word).
        SpotHoldsNone { symbol: String } => "okx.leverage.spot_holds_none":
            "{symbol} is a spot pair and has no leverage to hold",
        /// A spot pair holds no leverage (the write's word): nothing was sent.
        SpotWriteRefused { symbol: String } => "okx.leverage.spot_write_refused":
            "okx: {symbol} is a spot pair, which holds no leverage; nothing was sent",
        /// A read the reader asked failed; `what` names it, `why` is the transport's or the envelope's words.
        Unread { what: &'static str, why: String } => "okx.leverage.unread": "{what} could not be read: {why}",
    }
}

vike_utility::refusal_enum! {
    /// What the start's account-mode probe (`crate::account::modes`) says when it cannot settle the account's modes,
    /// or names a level the venue may refuse orders on.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OkxModeRefusal with ids OkxModeRefusalId {
        /// A read of the probe failed; `path` names it, `why` is the transport's or the envelope's words.
        Unread { path: &'static str, why: String } => "okx.mode.unread": "{path} could not be read: {why}",
        /// `account/config` answered no row.
        ConfigNoRow { path: &'static str } => "okx.mode.config_no_row": "{path} answered no row",
        /// A `posMode` word the adapter does not know: not guessed at.
        PositionModeUnknown { word: String } => "okx.mode.position_mode_unknown":
            "account/config answers posMode {word:?}, a word this adapter does not know",
        /// `account/config` states no `posMode`.
        PositionModeAbsent => "okx.mode.position_mode_absent": "account/config carries no posMode",
        /// A long/short account the bridge could not switch to net; `blocker` is what stopped it.
        LongShortBlocked { blocker: OkxModeBlocker } => "okx.mode.long_short_blocked":
            "the account is in long/short position mode and this adapter orders net positions, so OKX refuses its \
             orders; the bridge switches the account to net mode only while it is flat, and could not: {blocker}. \
             Close the positions and cancel the orders, then switch this account's engine off and on \
             (account.active) and the bridge switches the mode",
        /// `acctLv` 1 (Simple mode): named, never written.
        SimpleModeLevel => "okx.mode.simple_mode_level":
            "the account is in Simple mode (acctLv 1), which OKX may refuse swap orders on. This bridge does not \
             change an account level: choose single-currency or multi-currency margin mode in the OKX app or web \
             (Settings, Account mode), then restart this account's engine",
    }
}

vike_utility::refusal_enum! {
    /// What kept a long/short account from being switched to net ([`OkxModeRefusal::LongShortBlocked`]).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OkxModeBlocker with ids OkxModeBlockerId {
        /// A flat-check read failed, so the account is not shown flat; `why` is that read's refusal.
        FlatUnread { why: Box<OkxModeRefusal> } => "okx.mode.blocker.flat_unread":
            "{why}, so the account cannot be shown flat",
        /// What the account holds (`1 open position(s) (BTC-USDT-SWAP); 2 resting order(s)`).
        NotFlat { found: String } => "okx.mode.blocker.not_flat": "it is not flat: {found}",
        /// The venue refused the write.
        WriteRefused { code: i64, msg: String } => "okx.mode.blocker.write_refused":
            "OKX refused set-position-mode (code {code}: {msg})",
        /// The write answered success and the read-back still says another mode.
        NotSwitched { mode: String } => "okx.mode.blocker.not_switched":
            "set-position-mode answered success but the account still reads posMode {mode:?}",
        /// The write was sent and the read-back failed.
        ReadBackFailed { why: Box<OkxModeRefusal> } => "okx.mode.blocker.read_back_failed":
            "set-position-mode was sent but the account could not be read back: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why the start does not mount its symbol (`crate::execution::mount_money`): the account is NOT RUNNING, with this
    /// reason, until the row can be read or the contract is one this adapter sizes.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OkxMountRefusal with ids OkxMountRefusalId {
        /// A linear contract with a multiplier: every order would be the multiplier's times its size.
        LinearMultiplied { symbol: String, ct_mult: String } => "okx.mount.linear_multiplied":
            "okx exec REFUSES {symbol:?}: its contract multiplier ctMult is {ct_mult} and this adapter sizes a linear \
             contract by ctVal alone, so every order would be the multiplier's times its size; nothing is being sent",
        /// A coin-margined name whose row could not be read.
        InverseRowUnread { symbol: String } => "okx.mount.inverse_row_unread":
            "okx exec REFUSES {symbol:?}: it is a coin-margined contract and its instrument row could not be \
             fetched, so its contract value and settle coin are unknown; nothing is being sent until the row can be \
             read",
        /// A coin-margined row with no settle coin.
        InverseNoSettleCoin { symbol: String } => "okx.mount.inverse_no_settle_coin":
            "okx exec REFUSES {symbol:?}: its coin-margined row names no settle currency (settleCcy)",
        /// A coin-margined row with no usable contract value.
        InverseNoContractValue { symbol: String, ct_val: String } => "okx.mount.inverse_no_contract_value":
            "okx exec REFUSES {symbol:?}: its coin-margined row has no usable contract value (ctVal {ct_val})",
    }
}

/// The text of an instrument refusal, where the order path's and the reads' `String` errors carry it.
impl From<OkxInstrumentRefusal> for String {
    fn from(refusal: OkxInstrumentRefusal) -> Self {
        refusal.to_string()
    }
}

/// The text of a leverage refusal, where `vike_bridge_core::leverage_read::LeverageReader` carries it.
impl From<OkxLeverageRefusal> for String {
    fn from(refusal: OkxLeverageRefusal) -> Self {
        refusal.to_string()
    }
}
