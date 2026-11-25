//! **IG's refusals, declared once** (`vike_utility::refusal_enum!`, decision 0301): every "no" this bridge answers a
//! person or an agent with, one id and ONE text each, and the errors its own code and tests decide on.
//!
//! - [`IgSymbolRefusal`]: every answer of the facts source (`crate::instrument`'s `IgPropertiesSource`) that is no
//!   facts: an epic's fences, the account row, a read that failed, the market search, the market list and an
//!   operator's row. `crate::instrument`'s `IgPropertiesError` carries one beside the kind of "no" it is.
//! - [`IgRateRefusal`]: why the rate that converts an epic's quote currency could not be had or believed
//!   (`crate::account::quote_rate`), carried boxed by `IgPropertiesError::NoRate`.
//! - [`IgLoginRefusal`]: which of a login's accounts a session trades on (`crate::account::pick`).
//! - [`IgVenueReadRefusal`]: the operator's read of the account (`crate::reconcile::venue_read`).
//! - [`IgHistoryRefusal`]: the history lane's answers (`crate::history::klines`, `crate::credentialed_klines`).
//! - [`IgPricesError`]: why a `/prices` answer could not be read (`crate::history::klines`' `read_rows`), which the
//!   history lane's refusals carry and its tests decide on.
//!
//! Each text is the sentence its site wrote before it was typed, byte for byte. The shared crates' seams
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesKeyedError`, `vike_data::source::SourceError`,
//! `vike_catalog::CatalogError`, the adapter's `venue_read`) are handed the refusal with its id (`carried()`), so the
//! gatewayhub and the datahub read the words they always read and show the id beside them.

use std::time::Duration;

use vike_model::DEFAULT_CURRENCY;

use crate::account::pick::{WANTED_KIND, WHAT_TO_WRITE};
use crate::credentialed_klines::{DEMO_TIER, VENUE};
use crate::history::klines::{REQUEST_SHARE_PERCENT, RESERVE_PERCENT};
use crate::instrument::WIRE_PRICE_STEP;
use crate::market_list::MAX_PAGES;
use crate::operator_row::CURRENCIES;

vike_utility::refusal_enum! {
    /// An answer of IG's facts source that is no facts: a fence of the admission, an account row that says nothing
    /// usable, or a read that failed (a rate that cannot be had or believed is an [`IgRateRefusal`]).
    #[derive(Debug, Clone, PartialEq)]
    pub enum IgSymbolRefusal with ids IgSymbolRefusalId {
        // ---- the admission's fences (`crate::instrument`'s `facts_with`) ----
        NoInstrumentBlock { epic: String } => "ig.symbol.no_instrument_block":
            "ig: the market details of {epic} carry no `instrument` block",
        AnsweredForAnotherEpic { epic: String, answered: String } => "ig.symbol.answered_for_another_epic":
            "ig: GET /markets/{epic} answered for `{answered}`, so this epic's spelling is not the venue's",
        /// A type word whose size was not measured; `what` is that class's own reason, `admitted` the measured words.
        TypeNotMeasured { epic: String, class: String, what: &'static str, admitted: String } =>
            "ig.symbol.type_not_measured":
            "ig: {epic} is a {class} market and {what}; admitted types are {admitted}",
        SpreadBet { epic: String } => "ig.symbol.spread_bet":
            "ig: {epic} is a spread-bet market (expiry `DFB`, a daily funded bet): measured on the demo gateway, a \
             deal on one is refused HTTP 403 (`apiUser has no access to the relevant exchange`) for a CFD \
             account, and a SPREADBET account sizes a deal as a stake per point, which nothing here has measured; \
             trade the same index or commodity as a CFD epic",
        ExpiryUnknown { epic: String, expiry: String } => "ig.symbol.expiry_unknown":
            "ig: {epic} has expiry `{expiry}`, which is neither `-` (undated) nor a dated contract's own word, so \
             what the order must write is not known",
        NoStreamingPrice { epic: String } => "ig.symbol.no_streaming_price":
            "ig: {epic} has no streaming price on this account (`instrument.streamingPricesAvailable` is \
             false), so no price would ever arrive for it",
        NotSizedInContracts { epic: String, unit: String } => "ig.symbol.not_sized_in_contracts":
            "ig: {epic} is sized in `{unit}`, not in CONTRACTS, so its size is not a contract count",
        PairNotNamed { epic: String, base: String, quote: String } => "ig.symbol.pair_not_named":
            "ig: {epic}: the epic does not name the pair {base}/{quote} that `onePipMeans` states, so \
             the quote currency is not certain",
        NoLevelGrid { epic: String, one_pip_means: String } => "ig.symbol.no_level_grid":
            "ig: {epic}: `onePipMeans` (`{one_pip_means}`) is not '<pip> <QUOTE>/<BASE>' (a currency pair) and its \
             snapshot states no scalingFactor, so neither the quote currency nor the level grid is certain",
        QuotedInAnotherCurrency { epic: String, quote: String, account_currency: String } =>
            "ig.symbol.quoted_in_another_currency":
            "ig: {epic} is quoted in {quote} but this account's currency is {account_currency}, and \
             the engine books money in ONE currency (nothing converts a quote currency), so a \
             position in it would be mis-valued; only instruments quoted in {account_currency} are \
             admitted",
        /// The plain path's account fence, shared with `crate::account::quote_rate`'s `wire_account`.
        AccountNotInWireCurrency { account: String } => "ig.symbol.account_not_in_wire_currency":
            "ig: this account's currency is {account}, and the engine's ledger for an IG account is one currency \
             read as {DEFAULT_CURRENCY} (the balance seed is the account's cash in its own currency, and nothing \
             converts an IG account in another: decision 0183), so only a {DEFAULT_CURRENCY} account is admitted",
        NoWireCurrency { epic: String, currency: String } => "ig.symbol.no_wire_currency":
            "ig: {epic} lists no {currency} among its dealing currencies, and this adapter sends \
             currencyCode {currency}",
        NoContractSize { epic: String } => "ig.symbol.no_contract_size":
            "ig: {epic} carries no contractSize or no valueOfOnePip (missing or zero), so what one unit \
             of size is worth is unknown",
        /// `product` is `contract x pip`; `also` the same product in the account's currency, worded, or empty.
        SizeNotContracts { epic: String, contract: f64, pip: f64, product: f64, also: String, per_pip: f64 } =>
            "ig.symbol.size_not_contracts":
            "ig: {epic}: contractSize {contract} x pip {pip} = {product}{also} but valueOfOnePip says {per_pip}, so \
             the market's size is not 'contracts of contractSize units' (UNMEASURED); it is not admitted",
        NoMinimumDeal { epic: String } => "ig.symbol.no_minimum_deal":
            "ig: {epic} publishes no minimum deal size (missing or zero)",
        PipOffWireGrid { epic: String, pip: f64 } => "ig.symbol.pip_off_wire_grid":
            "ig: {epic} has a pip of {pip}, and this adapter writes a level with five decimals, so a \
             pip that is not a whole multiple of {WIRE_PRICE_STEP} is not admitted",
        NoDealingCurrency { epic: String } => "ig.symbol.no_dealing_currency":
            "ig: {epic} lists no dealing currency in instrument.currencies[], so the currency its levels and \
             valueOfOnePip are in is not known",
        SeveralDealingCurrencies { epic: String, count: usize, listed: String } =>
            "ig.symbol.several_dealing_currencies":
            "ig: {epic} lists {count} dealing currencies ({listed}), and which of them its levels and valueOfOnePip are in \
             is not stated (every epic measured lists exactly one)",
        NoContractValue { epic: String } => "ig.symbol.no_contract_value":
            "ig: the facts of {epic} state no contract value",

        // ---- the account row (`GET /accounts`) ----
        AccountNotListed { account_id: String } => "ig.symbol.account_not_listed":
            "ig: GET /accounts lists no account `{account_id}`",
        AccountCurrencyUnreadable { account_id: String } => "ig.symbol.account_currency_unreadable":
            "ig: the account `{account_id}` carries no readable currency",
        NotACfdAccount { kind: String } => "ig.symbol.not_a_cfd_account":
            "ig: this account is a {kind} account, and only a CFD account is admitted (a spread \
             bet's size is a stake per point and a physical account's a share count: UNMEASURED)",
        AccountTypeUnstated => "ig.symbol.account_type_unstated":
            "ig: GET /accounts states no accountType for this account, and only a CFD account is \
             admitted (a spread bet's size is a stake per point and a physical account's a share \
             count: UNMEASURED), so an account whose type is not stated is not assumed to be one",

        // ---- the source's reads ----
        /// The USER name has no epic (`crate::epic`'s `epic_of`), or the name is no epic
        /// (`crate::execution::lifecycle`'s `non_epic_refusal`): the words those shared rules state.
        NoEpic { why: String } => "ig.symbol.no_epic": "{why}",
        LoginFailed { why: String } => "ig.symbol.login_failed": "ig login: {why}",
        AccountsReadFailed { why: String } => "ig.symbol.accounts_read_failed": "ig GET /accounts: {why}",
        MarketReadFailed { epic: String, why: String } => "ig.symbol.market_read_failed":
            "ig GET /markets/{epic}: {why}",
        SearchReadFailed { why: String } => "ig.search.read_failed": "ig GET /markets: {why}",
        ReadCapSpent { cap: usize } => "ig.symbol.read_cap_spent":
            "ig: this adapter reads at most {cap} times a minute for an account (a cap of its own, \
             below the allowance IG meters); retry shortly",
        FirstLookCapSpent { cap: usize, first_look_cap: usize } => "ig.symbol.first_look_cap_spent":
            "ig: this adapter reads at most {cap} times a minute for an account, and at most {first_look_cap} of \
             them on first looks at an epic (the rest is kept for re-reading the rates already held) (a cap of \
             its own, below the allowance IG meters); retry shortly",
        CurrencyReadOverrun { limit: Duration } => "ig.symbol.currency_read_overrun":
            "ig: the account's currency was not read within {limit:?} (the read goes on in the \
             background and is kept for the next ask)",
        LookupOverrun { epic: String, limit: Duration } => "ig.symbol.lookup_overrun":
            "ig: the lookup of {epic} did not finish within {limit:?} (the read goes on in the background and \
             its login and rows are kept for the next ask)",
        LookupNotStarted { what: String, why: String } => "ig.symbol.lookup_not_started":
            "ig: could not start the {what}: {why}",
        LookupEndedUnanswered { what: String } => "ig.symbol.lookup_ended_unanswered":
            "ig: the {what} ended without an answer",
        /// The converting door's answer from a kept epic's details asks for no conversion pair: never shown.
        KeptEpicNotRead => "ig.symbol.kept_epic_not_read": "not read for a kept epic",

        // ---- the market list (`crate::market_list`) ----
        ListNoCategories => "ig.list.no_categories_list":
            "ig: GET /categories carries no `categories` list",
        ListCategoryNotAWord { code: String } => "ig.list.category_not_a_word":
            "ig: GET /categories names a category `{code}` that is not one word",
        ListNoCategory => "ig.list.no_category": "ig: GET /categories lists no category",
        ListPageStatesNoTotal { code: String } => "ig.list.page_states_no_total":
            "ig: a page of {code} states no metadata.totalPages, so the end of the category cannot be told",
        ListPageCarriesNoRows { code: String } => "ig.list.page_carries_no_rows":
            "ig: a page of {code} carries no `instruments` list",
        ListTooManyPages { code: String, total_pages: u64 } => "ig.list.too_many_pages":
            "ig: {code} states {total_pages} pages, more than the {MAX_PAGES} the list reads; nothing was kept",
        ListReadFailed { path: String, why: String } => "ig.list.read_failed": "ig GET {path}: {why}",

        // ---- an operator's row (`crate::operator_row`) ----
        NoHeldSession => "ig.operator_row.no_session":
            "ig: no ig demo account has started in this gatewayhub since it started, so \
             there is no session to ask IG with (nothing logs in for this); start the account, then ask again",
        NoStandardLot { pair: String, named: String, listed: String } => "ig.operator_row.no_standard_lot":
            "ig: IG's search for `{pair}` lists no standard lot {named} (its forms of the pair: [{listed}]), so \
             nothing was added",
        NoRollingPair { pair: String, listed: String } => "ig.operator_row.no_rolling_pair":
            "ig: IG's search for `{pair}` lists no rolling CFD currency pair named {pair} (its forms of the \
             pair: [{listed}]), so nothing was added",
        SeveralRollingForms { pair: String, count: usize, listed: String, named: String } =>
            "ig.operator_row.several_rolling_forms":
            "ig: IG's search for `{pair}` lists {count} rolling CFD forms of it ([{listed}]) and none is the FX Mini \
             ({named}) the name {pair} names, so which one is meant is not certain; nothing was added",
        RowNoInstrumentBlock { epic: String } => "ig.operator_row.no_instrument_block":
            "ig: the market details of {epic} carry no `instrument` block, so nothing was added",
        RowAnsweredForAnotherEpic { epic: String, answered: String } =>
            "ig.operator_row.answered_for_another_epic":
            "ig: the market details of {epic} answer for `{answered}`, so nothing was added",
        RowNotCurrencies { epic: String, kind: String } => "ig.operator_row.not_currencies":
            "ig: the market details of {epic} type it `{kind}`, not {CURRENCIES}, so nothing was added",
        RowPipUnreadable { epic: String, one_pip_means: String } => "ig.operator_row.pip_unreadable":
            "ig: the market details of {epic} state `onePipMeans` `{one_pip_means}`, not '<pip> <QUOTE>/<BASE>', \
             so nothing was added",
        RowPairNotNamed { epic: String, base: String, quote: String } => "ig.operator_row.pair_not_named":
            "ig: the market details of {epic} name the pair {base}/{quote}, which the epic does not, so nothing \
             was added",
        RowNoContractSize { epic: String } => "ig.operator_row.no_contract_size":
            "ig: the market details of {epic} state no contractSize, so nothing was added",
        RowNoMinimumDeal { epic: String } => "ig.operator_row.no_minimum_deal":
            "ig: the market details of {epic} state no minimum deal size, so nothing was added",
    }
}

vike_utility::refusal_enum! {
    /// Why the RATE that converts an epic's quote currency into the account's could not be had or believed
    /// (`crate::account::quote_rate`): "not now", and nothing about the epic itself. Its own enum, BOXED in
    /// `crate::instrument`'s `IgPropertiesError::NoRate`, because its sentences carry every candidate reading and
    /// would make every facts error large (clippy's `result_large_err`).
    #[derive(Debug, Clone, PartialEq)]
    pub enum IgRateRefusal with ids IgRateRefusalId {
        NoCoherentTable {
            epic: String,
            quote: String,
            account: String,
            own: Option<f64>,
            theirs: Option<Option<f64>>,
            base_own: Option<Option<f64>>,
            base_theirs: Option<Option<f64>>,
        } => "ig.symbol.rate.no_coherent_table":
            "ig: {epic}: no coherent table to read a {quote} rate in {account} from: the {account} row \
             says exchangeRate {own:?} and the {quote} row {theirs:?} (one of the two must say 1, and the \
             other must be a usable number that is not 1: two rows at 1, or an account row at 1 beside a \
             {quote} row that is not listed, state no rate; the baseExchangeRate column reads \
             {base_own:?} and {base_theirs:?}), so IG states no rate this adapter can use",
        CannotWitnessOwnRate { epic: String, why: String } => "ig.symbol.rate.cannot_witness_own_rate":
            "ig: {epic} cannot witness its own rate: {why}",
        /// `what` names the epic and its money (`{epic} is quoted in GBP on a USD account`).
        NoPairFamily { what: String } => "ig.symbol.rate.no_pair_family":
            "ig: {what} and its name does not say which family a conversion pair would be in, so its rate \
             cannot be checked against a price",
        NoPairListed { what: String, first: String, second: String } => "ig.symbol.rate.no_pair_listed":
            "ig: {what} and IG lists neither {first} nor {second}, so its rate cannot be checked against a price",
        PairAnsweredAsOther { name: String, answered: String, kind: String } =>
            "ig.symbol.rate.pair_answered_as_other":
            "ig: the conversion pair {name} answered as `{answered}` of type `{kind}`, not a CURRENCIES market of \
             that name",
        PairPipUnreadable { name: String, one_pip_means: String } => "ig.symbol.rate.pair_pip_unreadable":
            "ig: the conversion pair {name} has an unreadable onePipMeans `{one_pip_means}`, so its direction is \
             not known",
        PairCannotWitness { name: String, why: String } => "ig.symbol.rate.pair_cannot_witness":
            "ig: the conversion pair {name} cannot witness the rate: {why}",
        PairUnrelated { name: String, pair_base: String, pair_quote: String, quote: String, account: String } =>
            "ig.symbol.rate.pair_unrelated":
            "ig: the conversion pair {name} is {pair_base}/{pair_quote}, which does not relate {quote} and \
             {account}",
        PriceNotARate { epic: String, how: String, account: String, quote: String, priced: f64 } =>
            "ig.symbol.rate.price_not_a_rate":
            "ig: {epic}: {how} gives {account} per {quote} as {priced}, which is not a finite positive number, \
             so the symbol is NOT admitted",
        PriceNotInPriceUnits { epic: String, account: String, quote: String, how: String, why: String } =>
            "ig.symbol.rate.price_not_in_price_units":
            "ig: {epic}: its currencies table lists no {account} row to read a {quote} rate from, and {how} \
             cannot stand in for it: {why}. The symbol is NOT admitted",
        /// `tolerance_percent` is the agreement bound in percent; `stated` every reading tried, worded.
        NoReadingAgrees {
            epic: String,
            account: String,
            quote: String,
            tolerance_percent: f64,
            how: String,
            priced: f64,
            stated: String,
            scale: String,
        } => "ig.symbol.rate.no_reading_agrees":
            "ig: {epic}: no reading of IG's currencies table gives {account} per {quote} within \
             {tolerance_percent:.0}% of {how}, which says {priced} ({stated}; snapshot.scalingFactor {scale}), so \
             the table's orientation or field is not one this adapter knows (or the levels are scaled) and the \
             symbol is NOT admitted (never valued at par)",
        /// `count` readings agree with the price and not with each other; `both` are they, worded.
        ReadingsDisagree {
            epic: String,
            count: usize,
            how: String,
            priced: f64,
            tolerance_percent: f64,
            both: String,
        } => "ig.symbol.rate.readings_disagree":
            "ig: {epic}: {count} readings of IG's currencies table agree with {how} ({priced}) within \
             {tolerance_percent:.0}% but not with each other ({both}), so which one IG states cannot be told \
             apart near par and the symbol is NOT admitted (never valued at a guess)",
        NoQuoteCurrency { epic: String, quote: String } => "ig.symbol.rate.no_quote_currency":
            "ig: {epic} names no quote currency to rate (`{quote}`), so the symbol is NOT admitted",
    }
}

vike_utility::refusal_enum! {
    /// Why a login's session cannot be put on an account (`crate::account::pick`'s `pick`): each lists the accounts
    /// found (`listing`) and ends in what to write.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum IgLoginRefusal with ids IgLoginRefusalId {
        BookNotListed { pin: String, listing: String } => "ig.login.book_not_listed":
            "ig: the account row names the book `{pin}`, and this login lists no such account. \
             Accounts found: {listing}. {WHAT_TO_WRITE}",
        /// `book` is the account worded (`crate::account::pick`'s `ListedAccount::words`).
        BookCannotDeal { book: String, listing: String } => "ig.login.book_cannot_deal":
            "ig: the account row names the book {book}, and IG does not let it deal. Accounts found: \
             {listing}. {WHAT_TO_WRITE}",
        /// `session_account` is the session's own account, worded.
        NoUsdCfdAccount { session_account: String, listing: String } => "ig.login.no_usd_cfd_account":
            "ig: this login's default account is {session_account}, and none of its accounts is an enabled {WANTED_KIND} \
             account in {DEFAULT_CURRENCY}, which is what this adapter trades unless told otherwise (what a \
             deal's size means is measured on {WANTED_KIND} epics only). Accounts found: {listing}. Open or enable \
             one in IG's platform, or name a {WANTED_KIND} account in another currency as the book: it is \
             then converted for under a `demo` exposure ceiling. {WHAT_TO_WRITE}",
        SeveralUsdCfdAccounts { session_account: String, listing: String } => "ig.login.several_usd_cfd_accounts":
            "ig: this login's default account is {session_account}, which this adapter does not trade, and SEVERAL \
             of its accounts are enabled {WANTED_KIND} accounts in {DEFAULT_CURRENCY}, so nothing says \
             which one. Accounts found: {listing}. {WHAT_TO_WRITE}",
    }
}

vike_utility::refusal_enum! {
    /// Why the operator's read of an IG account (`trade venue ig open-orders`) gave no rows.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum IgVenueReadRefusal with ids IgVenueReadRefusalId {
        /// No order session of the account runs in this daemon, and the read opens no login of its own.
        NotRunning => "ig.venue_read.not_running":
            "ig: this account's order session is not running in this daemon, and this read \
             opens no IG login of its own (IG meters logins and warns that concurrent sessions may get the key \
             suspended); read it while the account runs",
        ReadFailed { why: String } => "ig.venue_read.read_failed":
            "ig: the working orders could not be read: {why}",
    }
}

vike_utility::refusal_enum! {
    /// An answer of IG's history lane that is no bars: a request that cannot be asked, an allowance this lane
    /// or IG holds, a login refused, or an answer that is not stored. `chunk` is always the chunk in words
    /// (`crate::history::klines`' `chunk_words`), `reset` and `when` the reset in words (`reset_sentence`).
    #[derive(Debug, Clone, PartialEq)]
    pub enum IgHistoryRefusal with ids IgHistoryRefusalId {
        // ---- the allowance (`crate::history::klines`, `crate::credentialed_klines`' ledger) ----
        ReserveReached {
            chunk: String,
            est_points: u32,
            remaining: u32,
            total: u32,
            reserve: u32,
            reset: String,
        } => "ig.history.reserve_reached":
            "ig: {chunk} were NOT requested. They could use up to {est_points} points of IG's historical-data \
             allowance (one point a row); {remaining} of this week's {total} points are left, and {reserve} \
             ({RESERVE_PERCENT} percent) are kept in reserve for the account's other reads, so the fetch stops \
             here. {reset} Run the same command again after the reset to carry on.",
        /// `cap` is the request's cap in points; `left` the points left this week, worded, or empty.
        RequestShareSpent { chunk: String, spent: u32, est_points: u32, cap: u32, total: u32, left: String } =>
            "ig.history.request_share_spent":
            "ig: {chunk} were NOT requested. This request has already used {spent} points and they could \
             use up to {est_points} more; one fetch is capped at {REQUEST_SHARE_PERCENT} percent of the \
             week's allowance ({cap} of {total} points) so that a wrong window cannot spend the week.{left} \
             Run the same command again to carry on from here: what is stored is skipped, and the new \
             request starts its own count.",
        /// IG's own `403` allowance refusal: `code` its words, `which` the allowance, `when` what to do and when.
        AllowanceSpent { code: String, which: &'static str, when: String } => "ig.history.allowance_spent":
            "ig: IG refused the request with {code}: {which}. Nothing is retried. {when} The chunks before \
             this one are stored, and the same command resumes here.",

        // ---- the login and the credential ----
        LoginFailed { why: String } => "ig.history.login_failed":
            "ig: the history lane could not log in ({why}); nothing was asked of the price endpoint",
        LoginHeld { why: String, minutes: i64 } => "ig.history.login_held":
            "ig: IG refused a login a moment ago ({why}); the history lane will not try another for \
             {minutes} minutes, because IG locks an account after repeated failed logins and that would also \
             block the trading side's own re-login. Nothing was asked of IG. Check the stored fields, \
             then run the command again after the wait.",
        /// `all` names the three fields, `account` the demo account in words, `missing` the absent ones.
        CredentialMissing { all: String, account: String, missing: String } =>
            "ig.history.credential_missing":
            "ig history needs {all} of {account} in this gatewayhub's credential store, and the \
             store lacks: {missing}. Nothing was asked of IG. On THIS box, `vike-cli secrets set --id \
             <ID> <FIELD>` stores one field (`vike-cli account ls` lists the rows; `vike-cli \
             account add --venue {VENUE} --tier {DEMO_TIER} --no-label` creates the \
             account). The fields are read per connection, so there is nothing to restart",
        CredentialStoreUnreadable { account: String, all: String } =>
            "ig.history.credential_store_unreadable":
            "ig history could not read this gatewayhub's credential store, so whether {account} \
             holds {all} cannot be told. This is not the same as the fields being absent; the \
             store's error is in the gatewayhub's log. Nothing was asked of IG",

        // ---- what is refused before IG is asked ----
        NotAnEpic { symbol: String } => "ig.history.not_an_epic":
            "ig: {symbol:?} is not spelled as an IG epic (five upper-case dotted segments, like \
             CS.D.EURUSD.MINI.IP). A series is stored under the symbol the request carried, so a \
             second spelling of one instrument would be a second series; this lane maps no pair \
             name to an epic. Nothing was asked of IG.",
        IntervalNotServed { interval: String } => "ig.history.interval_not_served":
            "ig: the history serves minute, hour and day bars (1m 2m 3m 5m 10m 15m 30m 1h 2h 3h \
             4h 1d); {interval:?} is not one of them. Nothing was asked of IG.",
        WindowInverted { start_ms: i64, end_ms: i64 } => "ig.history.window_inverted":
            "ig: the window [{start_ms}, {end_ms}] is inverted. Nothing was asked of IG.",
        WindowOutsideClockYears { start_ms: i64, end_ms: i64 } => "ig.history.window_outside_clock_years":
            "ig: the window [{start_ms}, {end_ms}] lies outside the years (1996 to 9999) the \
             account's clock rule is known for. Nothing was asked of IG.",
        WindowHasNoYear { start_ms: i64, end_ms: i64 } => "ig.history.window_has_no_year":
            "ig: the window [{start_ms}, {end_ms}] has no four-digit year on the account's \
             clock. Nothing was asked of IG.",
        Cancelled => "ig.history.cancelled": "ig: the history fetch was cancelled",
        CancelledWaiting => "ig.history.cancelled_waiting":
            "ig: the history fetch was cancelled while it waited for its turn",

        // ---- what IG answered ----
        /// `may_be_counted` says, or not, that the request may have been charged.
        PriceRequestFailed { why: String, may_be_counted: &'static str } => "ig.history.price_request_failed":
            "ig: the price request failed: {why}.{may_be_counted}",
        FirstReadUnusable { epic: String, why: IgPricesError } => "ig.history.first_read_unusable":
            "ig: the first read of {epic} is unusable: {why}",
        FirstReadEmpty { epic: String } => "ig.history.first_read_empty":
            "ig: the first read of {epic} returned no daily bar, so the account's clock (which \
             `from` and `to` are read in) was not seen to agree with the lane's rule and no \
             window can be asked",
        /// `kind` is the epic's class in words; `why` why the snapshot could not witness the scale.
        ScaleUnchecked { epic: String, kind: String, why: String } => "ig.history.scale_unchecked":
            "ig: {epic} is {kind}, and the scale of its history could not be checked: {why}. \
             Only currencies, indices and commodities were read in price units by the 2026-10-23 \
             probe, so for any other class the history is stored only when the market snapshot \
             confirms the scale. It is REFUSED, never guessed: a wrong scale stored once is \
             permanent. Nothing was stored; try again while the market is open.",
        ScaleDisagrees { epic: String, newest_close: f64, price: f64, tolerance_percent: f64 } =>
            "ig.history.scale_disagrees":
            "ig: {epic}'s newest history close is {newest_close} but its market snapshot prices it at \
             {price} (level x scalingFactor x pip), more than {tolerance_percent} percent apart, so the history \
             is not in price units. It is REFUSED, never rescaled on a guess: a wrong scale stored once is \
             permanent.",
        /// A later piece of a chunk failed (`why`, its own refusal's words) after an earlier one was charged.
        EarlierPieceNotStored { why: String, chunk: String } => "ig.history.earlier_piece_not_stored":
            "{why} The earlier piece of {chunk} WAS asked and charged and is not stored: a \
             chunk is stored whole or not at all.",
        NoBars { chunk: String, weekdays: u64 } => "ig.history.no_bars":
            "ig: IG returned no bars for {chunk}, a window that holds {weekdays} weekdays. An \
             empty chunk is final once a later chunk holds data (its key is spent for good), \
             so this one is not stored. If the instrument was not listed yet, start the window \
             after its first trading day; otherwise run the command again later.",
        AnswerUnusable { chunk: String, why: IgPricesError, est: u32 } => "ig.history.answer_unusable":
            "ig: the answer for {chunk} is unusable and is not stored: {why}. It may have \
             been charged up to {est} points, and the same window asked again is charged again.",
        /// `at` is the stray bar's open, `from` and `to` the window's ends, each to the minute in UTC.
        BarOutsideWindow { chunk: String, at: String, from: String, to: String, charged: u32 } =>
            "ig.history.bar_outside_window":
            "ig: the answer for {chunk} holds a bar at {at} outside the window it was asked for \
             ({from} to {to}), so the window was shifted or the answer is not what was asked: the chunk \
             is not stored. The answer WAS charged ({charged} points).",
    }
}

vike_utility::refusal_enum! {
    /// Why a `/prices` answer is not a series this lane can store (`crate::history::klines`' `read_rows`). `at` is the
    /// row's place, counted from 1; `utc_text` its `snapshotTimeUTC` as IG wrote it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum IgPricesError with ids IgPricesErrorId {
        NoPrices => "ig.prices.no_prices": "the answer carries no `prices` array",
        Paged { pages: u64 } => "ig.prices.paged":
            "the answer is paged ({pages} pages) although pageSize 0 was asked: only the first \
             page is here, and a chunk stored from it would be short for good",
        Truncated { size: u64, carried: usize } => "ig.prices.truncated":
            "the answer says {size} rows and carries {carried}: a truncated body",
        NoStamp { at: usize, key: String } => "ig.prices.no_stamp": "row {at}: no `{key}`",
        NotAStamp { at: usize, key: String, text: String } => "ig.prices.not_a_stamp":
            "row {at}: `{key}` {text:?} is not a stamp",
        OffsetNotWholeMinutes { at: usize, utc_text: String, difference: i64 } =>
            "ig.prices.offset_not_whole_minutes":
            "row {at} ({utc_text}): `snapshotTime` is {difference} ms from `snapshotTimeUTC`, \
             not a whole number of minutes, so the account's clock offset is not understood",
        OffsetNoTimeZone { at: usize, utc_text: String, minutes: i64 } => "ig.prices.offset_no_time_zone":
            "row {at} ({utc_text}): `snapshotTime` is {minutes} minutes from `snapshotTimeUTC`, \
             which no time zone is",
        /// The row's offset is not the account clock rule's (`crate::history::clock`): the window was asked shifted.
        OffsetAgainstRule { at: usize, utc_text: String, minutes: i32, rule: i32 } =>
            "ig.prices.offset_against_rule":
            "row {at} ({utc_text}): the account's clock is {minutes} minutes from UTC here, \
             but the account clock rule (`account_clock.rs`: the login's 8 hours, plus the \
             UK's summer hour at the row, less the UK's summer hour on the day asked) is \
             {rule}, and this lane asks every window by that rule, so the window was asked \
             in the wrong clock and is shifted; the lane cannot fetch until the rule is \
             corrected",
        OutsideClockYears { at: usize, utc_text: String } => "ig.prices.outside_clock_years":
            "row {at} ({utc_text}): this stamp is outside the years (1996 to 9999) the \
             account's clock rule is known for",
        NotIncreasing { at: usize, utc_text: String } => "ig.prices.not_increasing":
            "row {at} ({utc_text}): the stamps are not strictly increasing, so the answer is \
             not a series this source can store",
        OneSided { at: usize, utc_text: String, field: String } => "ig.prices.one_sided":
            "row {at} ({utc_text}): `{field}` has no two-sided price",
    }
}
