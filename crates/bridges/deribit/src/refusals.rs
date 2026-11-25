//! deribit's refusals, declared once (`vike_utility::refusal_enum!`, decision 0301): each has its id and its ONE text,
//! and code and tests decide on the variant, never on the words. Every id is published in the workspace's
//! `refusals.json` (`crates/vike-backend/tests/refusal_catalog.rs`).
//!
//! A refusal leaves this crate as its text where the shared seam carries a `String`
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesError`, `vike_data::source::SourceError`,
//! `vike_bridge_core::options::OptionsSpec`, `VenueAdapter::venue_read`, the data feed's plan): its `Display`, byte
//! for byte the words it had before it was typed.

vike_utility::refusal_enum! {
    /// Why a symbol the gatewayhub asks this venue's facts of is not admitted
    /// (`crate::symbol_properties::SymbolPropertiesListSpecDeribit`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DeribitPropertiesRefusal with ids DeribitPropertiesRefusalId {
        /// A coin-settled option: its premium is a coin amount the one-currency ledger would book as dollars.
        CoinSettledOption { symbol: String, settle: String } => "deribit.properties.coin_settled_option":
            "deribit: {symbol} is a coin-settled option: its premium is quoted in {settle}, so its notional and \
             PnL are coin amounts that an account's one-currency ledger would book as dollars. Trade the \
             USDC-settled options of the same underlying",
        /// Neither a perpetual, a dated future nor a USDC option (a spot pair has no position at the venue).
        NotADerivative { symbol: String, class: String } => "deribit.properties.not_a_derivative":
            "deribit: {symbol} is {class}, not a perpetual, a dated future or a USDC option; a spot pair has no \
             position at the venue (a holding is only a currency balance), so the gatewayhub does not admit one",
        /// A dated contract whose delivery has passed; `expired` is the gatewayhub's words for it.
        Delivered { symbol: String, expired: String } => "deribit.properties.delivered":
            "deribit: {symbol} was delivered at 08:00 UTC of the day in its name and {expired}, so the symbol is \
             not admitted",
        /// A quanto contract, whose notional is not valued.
        Quanto { symbol: String } => "deribit.properties.quanto":
            "deribit: {symbol} is a quanto contract, whose notional is not valued",
    }
}

vike_utility::refusal_enum! {
    /// Why one instrument row has no contract value, so no account is mounted on it and no account adds it
    /// (`crate::catalog::contract_value_of`, `crate::catalog::coin_option_refusal`; decision 0170, decision 0233).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DeribitContractRefusal with ids DeribitContractRefusalId {
        /// A coin-settled option: its premium is quoted in its coin.
        CoinSettledOption { name: String, settle: String } => "deribit.contract.coin_settled_option":
            "{name} is a coin-settled option: its premium is quoted in {settle}, so its notional and PnL are coin \
             amounts that an account's one-currency ledger would book as dollars. Trade the USDC-settled options \
             of the same underlying",
        /// A contract margined in a third asset.
        Quanto => "deribit.contract.quanto":
            "the instrument is margined in a third asset (quanto), whose notional this build does not value",
        /// A future or perpetual whose row states no margining word the venue is known by.
        MarginingUnstated { name: String } => "deribit.contract.margining_unstated":
            "the {name} row states no instrument_type the venue is known by (`reversed` or `linear`), so whether \
             its amount is dollars or coin cannot be known",
    }
}

vike_utility::refusal_enum! {
    /// Why a USER name has no kept deribit row (`crate::names::venue_name_of`, decision 0290).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DeribitNameRefusal with ids DeribitNameRefusalId {
        /// The catalog's own refusal of the name (`vike_catalog::kept_raw_of_user_name`, `vike_catalog::door_symbol`).
        NotKept { why: String } => "deribit.name.not_kept": "deribit: {why}; nothing was sent",
    }
}

vike_utility::refusal_enum! {
    /// Why an operator's READ of the account has no answer (`crate::adapter::VenueAdapterDeribit`'s `venue_read`,
    /// `crate::reconcile::venue_read`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DeribitVenueReadRefusal with ids DeribitVenueReadRefusalId {
        /// A `live` account: this bridge reaches only the testnet.
        LiveTierNotWired => "deribit.venue_read.live_tier_not_wired":
            "this bridge reaches only deribit's testnet, so an account on the live tier has no host to read; \
             nothing was asked",
        /// The account holds no key pair.
        Unconfigured => "deribit.venue_read.unconfigured":
            "the account holds no deribit API_KEY and API_SECRET pair; nothing was asked",
        /// The authed socket could not be opened (the dial's or the auth's own words).
        Unconnected { why: String } => "deribit.venue_read.unconnected": "{why}",
        /// One private read failed.
        Unread { method: String, code: i64, msg: String } => "deribit.venue_read.unread":
            "deribit {method} could not be read: [{code}] {msg}",
        /// The account summaries list no wallet.
        NoWallet => "deribit.venue_read.no_wallet": "deribit's get_account_summaries answer lists no wallet",
        /// A wallet with no balance figure (never read as zero).
        NoBalance { asset: String } => "deribit.venue_read.no_balance":
            "deribit states no balance for its `{asset}` wallet",
        /// An answer that should be a list is not one.
        NotAList { answer: &'static str } => "deribit.venue_read.not_a_list":
            "deribit's {answer} answer is not a list",
        /// The open orders could not be parsed (the parser's own words).
        Unparsed { why: String } => "deribit.venue_read.unparsed": "{why}",
    }
}

vike_utility::refusal_enum! {
    /// Why the server-time read measured nothing (`crate::adapter`'s clock leg).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DeribitClockRefusal with ids DeribitClockRefusalId {
        /// The host that answered is mainnet's, not the testnet every exec spawn site binds.
        WrongHost => "deribit.clock.wrong_host":
            "the deribit host answered testnet=false, but every exec spawn site binds testnet — this reading is \
             against the wrong host",
        /// The answer carries no stamp (`vike_bridge_core::venue_adapter::missing_time_field`'s words).
        NoStamp { why: String } => "deribit.clock.no_stamp": "{why}",
    }
}

vike_utility::refusal_enum! {
    /// Why a history fetch (bars, the DVOL index, funding, option trades) answers no rows (`crate::history`,
    /// `crate::option_trades`): a request refused before the venue is asked, the venue's own error, or an answer
    /// that cannot be read whole (a short answer would be stored).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DeribitHistoryRefusal with ids DeribitHistoryRefusalId {
        /// An interval the venue's resolution table has no code for.
        UnsupportedInterval { interval: String } => "deribit.history.unsupported_interval":
            "deribit: unsupported interval {interval:?}",
        /// An interval the DVOL index serves no bars of.
        DvolInterval { interval: String } => "deribit.history.dvol_interval":
            "deribit: the DVOL index has no {interval:?} bars (it serves 1s, 1m, 1h, 12h and 1d); nothing was sent",
        /// Funding asked of a symbol that is no perpetual.
        FundingNotPerpetual { user: String } => "deribit.history.funding_not_perpetual":
            "deribit: {user:?} is not a perpetual, and funding is a perpetual's series (`BTCUSD.P`, `ETHUSDC.P`)",
        /// A currency that is no venue word (it is put into a URL).
        NotACurrencyWord { currency: String } => "deribit.history.not_a_currency_word":
            "deribit: {currency:?} is not a deribit currency word (upper-case letters: `BTC`, `ETH`, `USDC`); \
             nothing was sent",
        /// The venue's JSON-RPC error envelope; `what` names the endpoint (`deribit chart`).
        VenueError { what: &'static str, code: i64, msg: String } => "deribit.history.venue_error":
            "{what} error {code}: {msg}",
        /// The same, with the venue's `error.data.reason`.
        VenueErrorWithReason { what: &'static str, code: i64, msg: String, reason: String } =>
            "deribit.history.venue_error_with_reason":
            "{what} error {code}: {msg} ({reason})",
        /// The DVOL endpoint's error envelope.
        DvolVenueError { msg: String, reason: String } => "deribit.history.dvol_venue_error":
            "deribit dvol error: {msg} ({reason})",
        /// The body or the page could not be read (the reader's own words).
        Unread { why: String } => "deribit.history.unread": "{why}",
        /// A chart answer with no `result`.
        ChartMissingResult => "deribit.history.chart_missing_result": "deribit chart: missing result",
        /// A chart answer whose `status` is neither `ok` nor `no_data`.
        ChartStatus { status: String } => "deribit.history.chart_status": "deribit chart status {status:?}",
        /// A chart answer without one of its columns.
        ChartMissingColumn { name: String } => "deribit.history.chart_missing_column":
            "deribit chart: missing column {name:?}",
        /// A chart cell that is no number.
        ChartNotANumber { name: String, index: usize } => "deribit.history.chart_not_a_number":
            "deribit chart: column {name}[{index}] not a number",
        /// A chart `ticks` cell that is no integer.
        ChartNotAnInteger { name: String, index: usize } => "deribit.history.chart_not_an_integer":
            "deribit chart: column {name}[{index}] not an integer",
        /// Chart columns of different lengths (zipping them would fabricate a gap).
        ChartRagged { ticks: usize, name: &'static str, len: usize } => "deribit.history.chart_ragged":
            "deribit chart: ragged columns (ticks={ticks}, {name}={len})",
        /// A DVOL answer with no `result.data`.
        DvolMissingData => "deribit.history.dvol_missing_data": "deribit dvol: missing result.data",
        /// A DVOL row that is not five numbers.
        DvolRow { row: String } => "deribit.history.dvol_row":
            "deribit dvol: a row that is not [timestamp, open, high, low, close]: {row}",
        /// A funding answer with no `result` array.
        FundingMissingResult => "deribit.history.funding_missing_result":
            "deribit funding: missing result array",
        /// An option-trades answer without one of its parts (`result`, `result.trades`, `result.has_more`).
        OptionTradesMissing { what: &'static str } => "deribit.history.option_trades_missing":
            "deribit option trades: missing {what}",
        /// An option trade of the page a row cannot be made of.
        OptionTradesBadTrade { currency: String, why: Box<DeribitHistoryRefusal> } =>
            "deribit.history.option_trades_bad_trade":
            "deribit option trades ({currency}): {why}",
        /// A trade without one of its numbers.
        TradeWithoutNumber { key: String, trade: String } => "deribit.history.trade_without_number":
            "trade without a number `{key}`: {trade}",
        /// A trade without one of its fields.
        TradeWithoutField { field: &'static str, trade: String } => "deribit.history.trade_without_field":
            "trade without `{field}`: {trade}",
        /// A trade whose instrument is no deribit option name.
        NotAnOptionName { instrument: String } => "deribit.history.not_an_option_name":
            "`{instrument}` is not a deribit option name",
        /// A trade whose direction is neither `buy` nor `sell`.
        TradeDirection { direction: Option<String>, trade: String } => "deribit.history.trade_direction":
            "trade with direction {direction:?}: {trade}",
        /// An empty option-trades page that says it has more.
        OptionTradesEmptyPage { from: String } => "deribit.history.option_trades_empty_page":
            "deribit option trades: from {from}, an empty page said it has more; the fetch stopped and answers \
             nothing",
        /// A full option-trades page that did not move past its first millisecond.
        OptionTradesPageStuck { from: String } => "deribit.history.option_trades_page_stuck":
            "deribit option trades: from {from}, a full page did not move past its first millisecond (more \
             trades in one millisecond than a page holds?); the fetch stopped and answers nothing",
    }
}

vike_utility::refusal_enum! {
    /// Why the market-data feed refuses a subscription before any lane spawns (`crate::feed`'s
    /// `DataClientSpecDeribit`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DeribitFeedRefusal with ids DeribitFeedRefusalId {
        /// A bars interval the `chart.trades` resolution table has no code for (owner ruling B-22); `why` is the
        /// table's own refusal.
        BarsIntervalUnserved { interval: String, why: DeribitHistoryRefusal } =>
            "deribit.feed.bars_interval_unserved":
            "deribit streams `chart.trades` by RESOLUTION code and has none for interval {interval:?} ({why} — \
             `vike_deribit::history::resolution_code` is the table, and its enum has gaps: there is no 4h), so \
             the bar lane's thread would set a status line and exit — and closed bars are what fire the \
             margin-call watchdog and the drawdown latch. Name an interval this venue serves",
        /// A mark lane: deribit pairs none with a bars subscription.
        NoMarkStream => "deribit.feed.no_mark_stream": "deribit pairs no mark stream",
    }
}

vike_utility::refusal_enum! {
    /// Why the options lane answers no listing, chain or feed (`crate::options::OptionsSpecDeribit`,
    /// `crate::feed::options::chain::DeribitOptionsProvider`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DeribitOptionsRefusal with ids DeribitOptionsRefusalId {
        /// An address row the lane needs was refused at construction (the row's own refusal).
        RowRefused { why: String } => "deribit.options.row_refused":
            "deribit options: {why}; nothing was asked of the venue",
        /// The listing a feed open checks names against could not be read.
        ListingUnread { why: Box<DeribitOptionsRefusal> } => "deribit.options.listing_unread":
            "deribit options: the venue's option listing could not be read ({why}); nothing was subscribed",
        /// An index of no underlying the daemon serves; `served` is the listing.
        IndexNotServed { index: String, served: String } => "deribit.options.index_not_served":
            "deribit options: `{index}` is not an index of an underlying this daemon serves (an index is \
             `<underlying>_<settlement>`, e.g. `btc_usd`; served: [{served}]); nothing was subscribed",
        /// An instrument that is no option of an underlying the daemon serves.
        InstrumentNotServed { name: String, served: String } => "deribit.options.instrument_not_served":
            "deribit options: `{name}` is not an option instrument of an underlying this daemon serves (served: \
             [{served}]); nothing was subscribed",
        /// An underlying the daemon does not serve.
        UnderlyingNotServed { underlying: String, served: String } => "deribit.options.underlying_not_served":
            "deribit options: `{underlying}` is not an underlying this daemon serves (served: [{served}]); \
             nothing was asked of the venue",
        /// The instrument index of one kind could not be read (the catalog's own words).
        InstrumentsUnread { kind: String, why: String } => "deribit.options.instruments_unread":
            "deribit {kind} instruments: {why}",
        /// An underlying the venue lists no options on.
        NotListed { underlying: String, listed: String } => "deribit.options.not_listed":
            "deribit options: `{underlying}` is not an underlying the venue lists options on (listed: [{listed}])",
        /// No listing read has answered yet, and the next may not be asked yet.
        NoListingYet => "deribit.options.no_listing_yet":
            "deribit option instruments: no listing read has answered yet, and the next is allowed a minute after \
             the last",
        /// The book summary could not be asked.
        BookSummaryUnasked { why: String } => "deribit.options.book_summary_unasked":
            "deribit book summary: {why}",
        /// The book summary's body could not be read.
        BookSummaryBody { why: String } => "deribit.options.book_summary_body":
            "deribit book summary read: {why}",
        /// The book summary's body is no JSON.
        BookSummaryJson { why: String } => "deribit.options.book_summary_json":
            "deribit book summary json: {why}",
        /// A listing answer with no `result` list.
        NoResultList => "deribit.options.no_result_list":
            "deribit option instruments: the answer carries no `result` list",
        /// A listing answer whose rows name no option underlying.
        NoUnderlying { rows: usize } => "deribit.options.no_underlying":
            "deribit option instruments: {rows} rows list no option underlying",
        /// The listing read failed (the reader's own words).
        ListingRead { why: String } => "deribit.options.listing_read": "{why}",
    }
}

vike_utility::refusal_enum! {
    /// Why a reconcile trade read is refused rather than reported as complete (`crate::reconcile`'s trade pages).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DeribitReconcileRefusal with ids DeribitReconcileRefusalId {
        /// A page that says there is more brought no new trade.
        NoNewTrade { scope: String, rows: usize } => "deribit.reconcile.no_new_trade":
            "{scope} says it has more trades after {rows} rows, and its next page brought no new one: the read is \
             refused rather than reported as complete",
        /// The page cap was reached with the venue still paging.
        PageCap { pages: usize, count: usize } => "deribit.reconcile.page_cap":
            "still paging after {pages} pages of {count} trades: the read is refused rather than reported as \
             complete",
    }
}
