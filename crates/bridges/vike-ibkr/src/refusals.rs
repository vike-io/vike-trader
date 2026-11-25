//! **IBKR's refusals, declared once** (`vike_utility::refusal_enum!`, decision 0301): each has a stable id and ONE
//! text, and code and tests decide on the variant, never on the words.
//!
//! - [`IbkrSymbolRefusal`]: the answers to the gatewayhub's facts of a name (a stock, a forex pair, a dated
//!   future), the grammar of a contract name, the kept row a user name reads, the conversion rate and the account's
//!   base currency. [`crate::instrument::any_symbol::IbkrPropertiesError`] carries one in either outcome (a verdict
//!   or a "not now").
//! - [`IbkrVenueReadRefusal`]: an operator's `venue_read` of the account.
//! - [`IbkrConfigRefusal`]: the tier's gateway address rows, the login's accounts and the gateway the gatewayhub
//!   watches, the reasons an account does NOT RUN.
//! - [`IbkrReadError`]: a bounded read of one `ibapi` subscription (`crate::reconcile::socket`'s `drain`), the error a
//!   reconcile pass and the operator's read carry.
//!
//! Every text is byte for byte the sentence the bridge said before it had an id. The seams of `vike-bridge-core`
//! (`SymbolPropertiesError`, `VenueAdapter::venue_read`) are handed the refusal with its id (`carried()`); the id does
//! not cross the String seams of `vike-execution`'s `ReconcileClient` or the rate errors (`QuoteRateError`): there the
//! bridge hands over the bare text (`Display`), never `rendered()`, so what another crate reads is unchanged.

use vike_execution::MAX_QUOTE_RATE_STEP;
use vike_model::DEFAULT_CURRENCY;
use vike_venues::venue_urls::UrlRefusal;

use crate::instrument::any_symbol::forex_future::{IDEALPRO, OPTION_REFUSAL};
use crate::instrument::any_symbol::{FOREX_REFUSAL, MAX_MARKET_RULES, ROUTE, STOCK_NAME_HINT};
use crate::transport::error_codes::IbkrError;

/// A wait in whole milliseconds, as the texts print it: a `u64`, never `Duration::as_millis`' `u128`, whose 16-byte
/// alignment would make every facts error too large to return (clippy's `result_large_err`).
#[must_use]
pub fn millis_of(wait: std::time::Duration) -> u64 {
    u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)
}

vike_utility::refusal_enum! {
    /// Why IBKR gives no facts for a name, or no rate for its currency.
    #[derive(Debug, Clone, PartialEq)]
    pub enum IbkrSymbolRefusal with ids IbkrSymbolRefusalId {
        // --- the grammar of a contract name (`crate::instrument::contract`'s `parse_simplified`)
        ContractNameTooShort { canonical: String } => "ibkr.symbol.contract_name.too_few_fields":
            "IBKR symbol {canonical:?}: expected at least SYMBOL.EXCHANGE.CURRENCY with no empty \
             field (a symbol or exchange code containing a '.' cannot be written in this grammar)",
        UnknownEquityExchange { canonical: String, exchange: String, currency: String } =>
            "ibkr.symbol.contract_name.unknown_equity_exchange":
            "IBKR symbol {canonical:?}: the three-field form SYMBOL.EXCHANGE.CURRENCY means a CASH \
             EQUITY, and {exchange:?} is not a known cash-equity exchange — it is refused rather than \
             defaulted, because reading a derivatives exchange as equity places the order in the wrong \
             instrument. Name the security type: SYMBOL.{exchange}.{currency}.STK|CASH|IND|CRYPTO, \
             SYMBOL.{exchange}.{currency}.FUT.YYYYMMDD[.MULTIPLIER], or \
             SYMBOL.{exchange}.{currency}.OPT.YYYYMMDD.STRIKE.C|P[.MULTIPLIER]",
        UnknownSecurityType { canonical: String, claim: String, accepted: String } =>
            "ibkr.symbol.contract_name.unknown_security_type":
            "IBKR symbol {canonical:?}: {claim:?} is not a security type this venue accepts in a \
             canonical — one of {accepted} (an unrecognised code is refused rather than forwarded, because \
             the wire would take it and mean something by it)",
        FutureWithoutExpiry { canonical: String } => "ibkr.symbol.contract_name.future_without_expiry":
            "IBKR symbol {canonical:?}: a FUT canonical is \
             SYMBOL.EXCHANGE.CURRENCY.FUT.YYYYMMDD[.MULTIPLIER] — a futures contract \
             without its expiry names a whole ladder of contracts rather than one, and the \
             exchange's own month code (the Z5 in ESZ5) is NOT decoded because its \
             single-digit year is ambiguous across decades",
        OptionWithoutTerms { canonical: String } => "ibkr.symbol.contract_name.option_without_terms":
            "IBKR symbol {canonical:?}: an OPT canonical is \
             SYMBOL.EXCHANGE.CURRENCY.OPT.YYYYMMDD.STRIKE.C|P[.MULTIPLIER] — expiry, \
             strike and right are all required, because any two of them name many contracts",
        StockTrailingFields { canonical: String, extra: usize } =>
            "ibkr.symbol.contract_name.stock_trailing_fields":
            "IBKR symbol {canonical:?}: a STK canonical is SYMBOL.EXCHANGE.CURRENCY.STK[.CONID] — \
             at most one field after the security type, the contract id (got {extra} extra)",
        ClaimTakesNoField { canonical: String, claim: String, extra: usize } =>
            "ibkr.symbol.contract_name.claim_takes_no_field":
            "IBKR symbol {canonical:?}: a {claim} canonical takes no field after the \
             security type (got {extra} extra) — only FUT and OPT do",
        NotACurrencyCode { canonical: String, currency: String } =>
            "ibkr.symbol.contract_name.not_a_currency_code":
            "IBKR symbol {canonical:?}: {currency:?} is not a three-letter currency code (IBKR names \
             the settlement currency, e.g. USD / EUR / GBP) — check the field order",
        ExpiryNotADate { canonical: String, expiry: String } => "ibkr.symbol.contract_name.expiry_not_a_date":
            "IBKR symbol {canonical:?}: expiry {expiry:?} is not YYYYMM or YYYYMMDD. The \
             exchange's month code (Z5, H6, …) is deliberately NOT accepted: its single-digit year \
             names December 2025, 2035 and 2015 alike, and guessing which is how an order lands on \
             the wrong contract",
        StrikeNotPositive { canonical: String, strike: String } =>
            "ibkr.symbol.contract_name.strike_not_positive":
            "IBKR symbol {canonical:?}: strike {strike:?} is not a positive decimal number",
        OptionRightUnknown { canonical: String, right: String } =>
            "ibkr.symbol.contract_name.option_right_unknown":
            "IBKR symbol {canonical:?}: option right {right:?} must be C / CALL or P / PUT",
        ContractIdNotPositive { canonical: String, id: String } =>
            "ibkr.symbol.contract_name.contract_id_not_positive":
            "IBKR symbol {canonical:?}: contract id {id:?} must be a positive whole number (IBKR's conId, \
             e.g. 265598 for AAPL)",
        MultiplierNotWhole { canonical: String, multiplier: String } =>
            "ibkr.symbol.contract_name.multiplier_not_whole":
            "IBKR symbol {canonical:?}: multiplier {multiplier:?} must be a whole number of units (IBKR sends \
             it as a numeric string, e.g. 50 for the E-mini or 100 for a listed option)",

        // --- the kept row a user name reads (`crate::instrument::contract`'s `operator_row_of`, `contract_of_row`)
        /// A WIRE name typed where a user name belongs.
        WireNameTyped { typed: String, name: String } => "ibkr.symbol.kept_row.wire_name_typed":
            "ibkr: `{typed}` is the venue's spelling of `{name}`: an instrument is named by its user name, \
             `{name}` (decision 0290)",
        /// `why` is `vike_catalog::kept_row_of_user_name`'s sentence.
        NoKeptRow { why: String } => "ibkr.symbol.kept_row.missing":
            "ibkr: {why}. IBKR lists nothing, so an instrument is the operator's row: `vike-cli data \
             instruments add --venue ibkr <SYMBOL.EXCHANGE.CURRENCY> --asset-class equity|fx|future --base <BASE> \
             --quote <CCY> --tick <T> --point-scale <1/T>` (docs/ops/operator-instruments.md)",
        /// No ibkr list is kept in this process: `why` is `vike_catalog::kept_row_of_user_name`'s sentence.
        NoKeptList { why: String } => "ibkr.symbol.kept_row.no_list": "ibkr: {why}",
        /// `stated` is what the row says it is, `read` what its name reads as.
        KeptRowDisagrees { wire: String, stated: String, read: String } => "ibkr.symbol.kept_row.disagrees":
            "ibkr: the kept row `{wire}` says it is {stated}, and its name reads as {read}: the row is refused \
             until the operator states it again (`vike-cli data instruments rm --venue ibkr {wire}`, then `add`)",

        // --- a stock by name (`crate::instrument::any_symbol`)
        ForexIsNotAStock { name: String } => "ibkr.symbol.stock.forex_is_not_a_stock":
            "`{name}` is not an IBKR stock name ({STOCK_NAME_HINT}). {FOREX_REFUSAL}",
        StockContractIdNotPositive { name: String, id: String } =>
            "ibkr.symbol.stock.contract_id_not_positive":
            "`{name}` is not an IBKR stock name: the contract id {id:?} is not a positive whole \
             number ({STOCK_NAME_HINT})",
        NotAStockName { name: String } => "ibkr.symbol.stock.not_a_stock_name":
            "`{name}` is not an IBKR stock name ({STOCK_NAME_HINT})",
        StockOfAnotherCurrency { name: String, currency: String, account_currency: String } =>
            "ibkr.symbol.stock.another_currency":
            "`{name}` is a {currency} stock and this account's money is {account_currency}: the \
             ledger is one currency and nothing converts, so only a {account_currency} stock is admitted",
        NoContractForTicker { name: String, ticker: String } => "ibkr.symbol.stock.no_contract":
            "{name}: the gateway's symbol search names no contract with the ticker {ticker}",
        /// `kinds` is the secType words the ticker names, joined.
        TickerNamesNoStock { name: String, ticker: String, kinds: String } => "ibkr.symbol.stock.no_stock":
            "{name}: the ticker {ticker} names only {kinds} contracts here and no stock; this adapter \
             admits a stock by name",
        StockInOtherCurrencies { name: String, ticker: String, others: String, currency: String } =>
            "ibkr.symbol.stock.listed_in_other_currencies":
            "{name}: the stock {ticker} is listed here in {others} only, not in {currency}; a stock of \
             another currency is another contract and is not admitted",
        /// `first` is the lowest conId, the example spelled.
        AmbiguousStock { name: String, ticker: String, count: usize, currency: String, listed: String, first: i32 } =>
            "ibkr.symbol.stock.ambiguous":
            "{name} is ambiguous: the ticker {ticker} names {count} stocks in {currency} ({listed}); an order on it \
             would draw TWS error 200, so it is refused and not picked from. Name the one you mean by its \
             contract id: `{ticker}.{ROUTE}.{currency}.STK.<conId>`, for example `{ticker}.{ROUTE}.{currency}.STK.{first}`",
        NoStockDetails { name: String, conid: i32 } => "ibkr.symbol.stock.no_details":
            "{name}: the gateway returned no contract details for conId {conid}",
        StockDetailsDisagree { name: String, row: String, conid: i32, chosen: i32 } =>
            "ibkr.symbol.stock.details_disagree":
            "{name}: the contract details name {row} (conId {conid}) and not the stock the search \
             chose (conId {chosen}); nothing is admitted on a disagreement",
        StockMultiplier { name: String, multiplier: String } => "ibkr.symbol.stock.multiplier":
            "{name}: the contract carries a multiplier of {multiplier}, and the engine books a \
             share as quantity times price",
        NoSizeRules { name: String } => "ibkr.symbol.stock.no_size_rules":
            "{name}: size rules unavailable (the contract details carry neither a minimum size nor a \
             size increment), so the order size grid and minimum are unknown",
        StockNoSizeIncrement { name: String } => "ibkr.symbol.stock.no_size_increment":
            "{name}: the contract details carry no size increment (missing or zero), so the order \
             size grid is unknown",
        /// Judged for a FOREIGN-currency stock only (`crate::instrument::any_symbol`'s `price_magnifier_refusal`).
        ForeignPriceMagnifier { name: String, magnifier: i32, currency: String } =>
            "ibkr.symbol.stock.foreign_price_magnifier":
            "ibkr: {name}: the contract carries a price magnifier of {magnifier} (a share quoted in whole {currency} \
             units carries 1), so the unit its prices are reported in is not known and a price in a minor unit \
             (pence, cents, agorot) would be booked 100 times too high: a stock of another currency than the \
             account's is not admitted on it",
        PriceMagnifierNotRead { name: String } => "ibkr.symbol.stock.price_magnifier_not_read":
            "ibkr: {name}: the contract's price magnifier was not read, so the unit its prices are reported in \
             is not known and a stock of another currency than the account's is not admitted on it",

        // --- the grid of any class (`crate::instrument::any_symbol`'s `grid_tick_of`) and its size rules
        NoMinimumTick { name: String } => "ibkr.symbol.grid.no_minimum_tick":
            "{name}: the contract details carry no minimum tick (missing or zero)",
        MarketRuleIdNotANumber { name: String, word: String } => "ibkr.symbol.grid.market_rule_id_not_a_number":
            "{name}: the market rule id {word:?} is not a number",
        NoMarketRule { name: String, min_tick: f64 } => "ibkr.symbol.grid.no_market_rule":
            "{name}: the contract names no market rule, so the price bands behind its minimum tick \
             ({min_tick}) cannot be read",
        TooManyMarketRules { name: String, count: usize } => "ibkr.symbol.grid.too_many_market_rules":
            "{name}: the contract names {count} distinct market rules (more than {MAX_MARKET_RULES}); \
             its price grid is not read",
        MarketRuleNoBand { name: String, id: i32 } => "ibkr.symbol.grid.market_rule_no_band":
            "{name}: market rule {id} has no price band",
        MarketRuleIncrement { name: String, id: i32, increment: f64 } =>
            "ibkr.symbol.grid.market_rule_increment":
            "{name}: market rule {id} has a band with an increment of {increment}",
        MarketRuleLowEdge { name: String, id: i32, low_edge: f64 } => "ibkr.symbol.grid.market_rule_low_edge":
            "{name}: market rule {id} has a band with a low edge of {low_edge}",
        RulesDoNotNestAtPrice { name: String, price: f64, increment: f64, coarsest: f64 } =>
            "ibkr.symbol.grid.rules_do_not_nest_at_price":
            "{name}: the market rules of its exchanges do not nest at the price {price} ({increment} does \
             not divide {coarsest}), so no single tick is valid at every exchange",
        BandsDoNotNest { name: String, finest: f64, increment: f64 } => "ibkr.symbol.grid.bands_do_not_nest":
            "{name}: the price bands of its market rules do not nest ({finest} does not divide \
             {increment}), so the finest band's grid is not a grid of the others",
        NoSizeIncrement { name: String } => "ibkr.symbol.grid.no_size_increment":
            "{name}: the contract details carry no size increment, so the order size grid is unknown",
        MinimumSize { name: String, min_qty: f64 } => "ibkr.symbol.grid.minimum_size":
            "{name}: the contract details carry a minimum size of {min_qty}",

        // --- a forex pair or a dated future by name (`crate::instrument::any_symbol::forex_future`)
        OptionNotAdmitted { name: String } => "ibkr.symbol.class.option": "`{name}`: {OPTION_REFUSAL}",
        /// `sec_type` is IB's word for the class the name claims.
        ClassNotAdmitted { name: String, sec_type: String } => "ibkr.symbol.class.not_admitted":
            "`{name}` is a {sec_type} contract and this adapter admits a stock, a forex pair on IDEALPRO or a dated \
             cash-settled future by name",
        ForexNotOnIdealpro { name: String, exchange: String } => "ibkr.symbol.forex.not_on_idealpro":
            "`{name}`: a forex pair is admitted on {IDEALPRO} only, not on {exchange}",
        NotAForexPair { name: String } => "ibkr.symbol.forex.not_a_pair":
            "`{name}` is not a forex pair: a pair is two different three-letter currencies, `EUR.USD.IDEALPRO`",
        FutureNamesNoExpiry { name: String } => "ibkr.symbol.future.no_expiry":
            "`{name}`: a future names its expiry, `ES.CME.USD.FUT.20261218`",
        NotAFuturesRoot { name: String, root: String } => "ibkr.symbol.future.not_a_root":
            "`{name}`: {root:?} is not a futures root",
        NoDetails { name: String } => "ibkr.symbol.class.no_details":
            "{name}: the gateway returned no contract details",
        DetailsNameAnotherKind { name: String, sec_type: String, conid: i32, wanted: String } =>
            "ibkr.symbol.class.another_kind":
            "{name}: the contract details name a {sec_type} contract (conId {conid}) and not a {wanted}",
        /// `listed` names each contract, at most eight.
        AmbiguousContract { name: String, count: usize, listed: String } => "ibkr.symbol.class.ambiguous":
            "{name} is ambiguous: it names {count} contracts ({listed}); state the one you mean (a future's multiplier \
             as the last field of its name)",
        PriceMagnifierUnknown { name: String, magnifier: i32 } => "ibkr.symbol.class.price_magnifier":
            "{name}: the contract carries a price magnifier of {magnifier}, so the unit its prices are reported in is not known",
        /// `pair` is the details' `SYMBOL.CURRENCY`.
        PairDisagrees { name: String, pair: String, conid: i32 } => "ibkr.symbol.forex.details_disagree":
            "{name}: the contract details name {pair} (conId {conid}) and not the pair asked for; nothing is admitted on a disagreement",
        IdealproMinimumCurrency { name: String, size: f64, currency: String, base: String } =>
            "ibkr.symbol.forex.idealpro_minimum_currency":
            "{name}: the venue states its IdealPro minimum as {size} {currency}, which is not the pair's base currency {base}; the order size floor is unknown",
        /// `roots` is the operator's `ibkr_cash_settled_root` rows, joined.
        RootNotCashSettled { name: String, root: String, roots: String } => "ibkr.symbol.future.root_not_cash_settled":
            "{name}: the root {root} is not admitted: only the cash-settled equity-index futures ({roots}) are, because \
             IB closes a position in a future that settles by delivery before its last trading day, at prices the \
             engine would carry as a position the venue no longer holds",
        NoLastTradingDay { name: String, conid: i32 } => "ibkr.symbol.future.no_last_trading_day":
            "{name}: the contract details state no last trading day (conId {conid}), so its expiry cannot be guarded",
        LastTradesAnotherDay { name: String, day: String, conid: i32, expiry: String } =>
            "ibkr.symbol.future.last_trades_another_day":
            "{name}: the contract of that name last trades on {day} (conId {conid}), not on {expiry}",
        LastTradingDayNotADay { name: String, day: String } => "ibkr.symbol.future.last_trading_day_not_a_day":
            "{name}: the last trading day {day} is not a calendar day",
        FutureDelivered { name: String, day: String } => "ibkr.symbol.future.delivered":
            "{name}: the contract last traded on {day}, which has begun or passed, and a future at its last trading \
             day is not admitted (nothing in the engine rolls or closes it at expiry); name the next month",
        NoUsableMultiplier { name: String, multiplier: String } => "ibkr.symbol.future.no_usable_multiplier":
            "{name}: the contract carries no usable multiplier ({multiplier:?}), so what one contract is worth is unknown",
        MultiplierDisagrees { name: String, claimed: f64, multiplier: f64 } =>
            "ibkr.symbol.future.multiplier_disagrees":
            "{name}: the name claims a multiplier of {claimed} and the contract carries {multiplier}",
        /// `typed` is the name as asked, `symbol` the one this contract is named by.
        NotTheCanonicalName { typed: String, symbol: String } => "ibkr.symbol.future.not_the_canonical_name":
            "{typed}: this contract is named `{symbol}` here (the venue's exchange and its last trading day): \
             spell it that way, so that one position has one name",
        /// A node that converts nothing (`crate::symbol_properties`'s `ledger_refusal`).
        LedgerCurrency { name: String, money: String } => "ibkr.symbol.ledger_currency":
            "ibkr: `{name}` counts its money in {money} and this account's ledger is \
             {DEFAULT_CURRENCY}: only a node that converts a quote currency admits it",

        // --- the gateway, not answering (a "not now")
        /// `what` names the request, `why` the gateway's error.
        DidNotAnswer { what: String, why: String } => "ibkr.symbol.gateway.did_not_answer":
            "ibkr: {what} did not answer: {why}",
        /// A cpapi account opens no ibapi socket: refused by name before any dial.
        CpapiBackend { why: IbkrError } => "ibkr.symbol.gateway.cpapi_backend": "{why}",
        /// The data lane refused the dial, or the read outlived its bound.
        DataConnection { why: IbkrError } => "ibkr.symbol.gateway.data_connection": "{why}",

        // --- the conversion rate (`crate::account::quote_rate`)
        MinorUnitCurrency { listing: String, minor: String, currency: String, major: String } =>
            "ibkr.symbol.rate.minor_unit_currency":
            "ibkr: IBKR is known to quote {listing} stocks in {minor} while the contract says {currency}, and which \
             listings and whether this adapter's prices are in that minor unit is not measured: a rate for \
             {major} would value such a stock 100 times too high, so a {currency} stock is not admitted",
        RateCodeNotACurrency { code: String } => "ibkr.symbol.rate.not_a_currency_code":
            "`{code}` is not a currency code",
        RateOfTheLedgersOwn { currency: String } => "ibkr.symbol.rate.ledger_own_currency":
            "{currency} is the ledger's own currency: nothing converts",
        /// `why` is the pair read's own words.
        PairReadFailed { base: String, counter: String, why: String } => "ibkr.symbol.rate.pair_read_failed":
            "{base}.{counter}: {why}",
        NoForexPair { quote: String, account: String, base: String, counter: String } =>
            "ibkr.symbol.rate.no_forex_pair":
            "IBKR lists no forex pair between {quote} and {account} (neither {base}.{counter} \
             nor {counter}.{base}), so there is no rate to convert {quote} into {account}",
        QuoteNotRealTime { pair: String, delay: String } => "ibkr.symbol.rate.quote_not_real_time":
            "the {pair} quote is {delay} data, not real-time, so it is not a rate",
        QuoteNotTwoSided { pair: String, bid: f64, ask: f64 } => "ibkr.symbol.rate.quote_not_two_sided":
            "the {pair} quote has no usable two-sided price (bid {bid}, ask {ask})",
        BookCrossed { pair: String, bid: f64, ask: f64 } => "ibkr.symbol.rate.book_crossed":
            "the {pair} book is crossed (bid {bid} above ask {ask})",
        SpreadTooWide { pair: String, bid: f64, ask: f64, percent: f64 } => "ibkr.symbol.rate.spread_too_wide":
            "the {pair} spread ({bid} / {ask}) is wider than {percent}% of the mid, which is not a market",
        /// `per_quote` is the rate the mid gave.
        MidGivesNoRate { pair: String, mid: f64, per_quote: f64 } => "ibkr.symbol.rate.mid_gives_no_rate":
            "the {pair} mid {mid} gives no usable rate ({per_quote})",
        /// The sanity gate (`crate::account::quote_rate`'s `check_against_previous`).
        RateMovedTooFar {
            pair: String,
            now: f64,
            currency: String,
            age_secs: u64,
            was: f64,
            previous_pair: String,
            step: f64,
        } => "ibkr.symbol.rate.moved_too_far":
            "the {pair} read gives {now} ledger units per {currency} where the read {age_secs} s ago gave {was} \
             ({previous_pair}): a move of {step:.1} times in one read is past the engine's {MAX_QUOTE_RATE_STEP}x \
             step, so it is a flipped or frozen quote and not a rate",
        NoTwoSidedQuote { millis: u64 } => "ibkr.symbol.rate.no_two_sided_quote":
            "no two-sided quote arrived within {millis} ms",
        QuoteLinkGone { code: i32, message: String } => "ibkr.symbol.rate.link_gone":
            "the gateway's link is gone (notice {code}: {message})",
        QuoteStreamSilent { millis: u64 } => "ibkr.symbol.rate.quote_stream_silent":
            "the quote stream ended or stayed silent for {millis} ms with no two-sided price",
        /// A failed read remembered (`crate::instrument::any_symbol::socket`'s `RATE_FAIL_KEEP`): `why` is its words.
        RateReadFailedRecently { why: String, age_secs: u64, keep_secs: u64 } =>
            "ibkr.symbol.rate.failed_recently":
            "{why} (the same read failed {age_secs} s ago and is not asked again for {keep_secs} s)",
        /// The rate's failure as the node reads it (`crate::symbol_properties`' `rate_refusal`).
        NoRateIn { currency: String, base: String, why: String } => "ibkr.symbol.rate.none_in_base":
            "no rate for {currency} in {base} ({why})",
        NoRate { currency: String, why: String } => "ibkr.symbol.rate.none": "no rate for {currency} ({why})",

        // --- the account's base currency (the account summary's `NetLiquidation` rows)
        SummaryCurrencyNotACode { tag: String, currency: String } => "ibkr.symbol.base.not_a_currency_code":
            "ibkr: the account summary's {tag} row names `{currency}` as its currency, which is no \
             currency code, so the account's base currency is not known",
        SummaryNoRow { tag: String } => "ibkr.symbol.base.no_row":
            "ibkr: the account summary carries no {tag} row for the configured account, so its base \
             currency is not known",
        SummaryManyCurrencies { count: usize, currencies: String, tag: String } =>
            "ibkr.symbol.base.many_currencies":
            "ibkr: the account summary states {count} different currencies ({currencies}) for {tag}, so no single \
             base currency is known",
        SummaryDidNotEnd { millis: u64 } => "ibkr.symbol.base.summary_did_not_end":
            "ibkr: the account summary did not end within {millis} ms",
        /// `why` is `ibapi`'s error.
        SummaryFailed { why: String } => "ibkr.symbol.base.summary_failed": "ibkr: the account summary: {why}",
        SummaryCut => "ibkr.symbol.base.summary_cut": "ibkr: the account summary ended before its end marker",
        BaseCurrencyChanged { read: String, was: String } => "ibkr.symbol.base.changed":
            "ibkr: the account's base currency reads {read} where this gatewayhub read {was} before, and \
             the engine's ledger is anchored to {was}: no rate is stated until the account is started \
             again (set its `account.active` off and on)",
    }
}

vike_utility::refusal_enum! {
    /// Why an operator's `venue_read` of an IBKR account lists nothing (`crate::adapter`'s `venue_read`,
    /// `crate::reconcile::venue_read`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum IbkrVenueReadRefusal with ids IbkrVenueReadRefusalId {
        AccountDoesNotArm => "ibkr.venue_read.account_does_not_arm":
            "the account does not arm here (`vike-cli trade health` says \
             why), so it has no reconcile connection to read on; nothing was asked",
        CpapiBackend => "ibkr.venue_read.cpapi_backend":
            "not offered by ibkr on the cpapi backend: only the socket \
             backend's reconcile connection lists the open orders",
        NoReconcileConnection => "ibkr.venue_read.no_reconcile_connection":
            "ibkr: this account's reconcile connection is not running in this daemon \
             (reconciliation off, the cpapi backend, or its connect failed), and this read opens no IB Gateway \
             connection of its own (each one takes an API client id); read it while the account's reconcile runs",
        /// `why` is `ibapi`'s error.
        AskFailed { why: String } => "ibkr.venue_read.ask_failed": "ibkr open orders: {why}",
        Read { why: IbkrReadError } => "ibkr.venue_read.read_failed": "{why}",
        ConnectionClosed => "ibkr.venue_read.connection_closed":
            "ibkr: the reconcile connection closed during the read; nothing is listed",
    }
}

vike_utility::refusal_enum! {
    /// Why an IBKR account does NOT RUN on its configuration: the tier's gateway rows (`crate::config`'s
    /// `gateway_addresses`, and the gateway the gatewayhub watches, `crate::gateway_watch`) and the login's accounts
    /// (`crate::config`'s `account_from_gateway`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum IbkrConfigRefusal with ids IbkrConfigRefusalId {
        /// The `venue_url` row's own refusal (missing, plaintext, a demo row that is the live one).
        AddressRow { refusal: UrlRefusal } => "ibkr.config.address_row": "{refusal}",
        /// `tier` is the row's type word (`demo`, `live`).
        SocketRowNotHostPort { tier: String, socket: String } => "ibkr.config.socket_row_not_host_port":
            "ibkr: the {tier} trade socket gateway row `{socket}` is not host:port",
        /// `count` is how many accounts the login manages, never their numbers.
        NoAccountStored { count: usize } => "ibkr.config.no_account_stored":
            "the IB Gateway login manages {count} account(s) and this ibkr account stores no `ACCOUNT` \
             field, so nothing says which one to trade and none is: the `ACCOUNT` field picks one \
             (`vike-cli secrets set --id <id> ACCOUNT`, the number on stdin)",
        /// The gatewayhub's watch of the gateway (`crate::gateway_watch`'s `IbkrGateway::supervised`).
        BackendWordUnknown { tier: String, word: String } => "ibkr.config.backend_word_unknown":
            "venue.ibkr.{tier}.backend `{word}` is none of socket, cpapi, oauth",
        GatewayAddressNotHostPort { socket: String } => "ibkr.config.gateway_address_not_host_port":
            "the ibkr gateway address `{socket}` is not host:port",
        AccountNotManaged { count: usize } => "ibkr.config.account_not_managed":
            "the IB Gateway login does not manage this ibkr account's `ACCOUNT` (the login manages {count} \
             account(s)), so nothing is traded: the `ACCOUNT` field must name one of the login's \
             accounts, or stay unset on a login that manages exactly one",
    }
}

vike_utility::refusal_enum! {
    /// Why one bounded read of an `ibapi` subscription gave no whole answer (`crate::reconcile::socket`'s `drain`): a
    /// list that stopped early is never read as the venue's whole answer.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum IbkrReadError with ids IbkrReadErrorId {
        /// `what` names the read (`positions`, `open orders`).
        NoEnd { what: String, deadline: std::time::Duration } => "ibkr.read.no_end":
            "ibkr {what}: no end of the reply within {deadline:?}",
        /// `why` is `ibapi`'s error.
        Failed { what: String, why: String } => "ibkr.read.failed": "ibkr {what}: {why}",
        CutBeforeMarker { what: String } => "ibkr.read.cut_before_marker":
            "ibkr {what}: the stream ended before its end marker",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The texts are the sentences the bridge said before they had ids**, byte for byte, shown under their id: the
    /// stock-name hint (a `const` hole), the minor-unit sentence (its `ibkr: ` once added at every call site), the
    /// operator read's "not running" and the login's account count.
    #[test]
    fn each_text_is_the_old_sentence_under_its_id() {
        assert_eq!(
            IbkrSymbolRefusal::NotAStockName { name: "AAPL.SMART.US.EXTRA".into() }.rendered(),
            "error [ibkr.symbol.stock.not_a_stock_name]: `AAPL.SMART.US.EXTRA` is not an IBKR stock name (an \
             IBKR stock is admitted as `TICKER.SMART.CCY`, e.g. `AAPL.SMART.USD` (a ticker of letters and digits, \
             `_` for the space of a class share such as `BRK_B`, routed SMART, with its currency; pin one \
             contract with `TICKER.SMART.CCY.STK.<conId>`, e.g. `AAPL.SMART.USD.STK.265598`; forex, futures, \
             options and an explicit exchange are not admitted by name))"
        );
        assert_eq!(
            crate::account::quote_rate::minor_unit_refusal(" gbp ")
                .map(|r| r.to_string())
                .as_deref(),
            Some(
                "ibkr: IBKR is known to quote London stocks in pence (GBX) while the contract says GBP, and which \
                 listings and whether this adapter's prices are in that minor unit is not measured: a rate for \
                 pounds would value such a stock 100 times too high, so a GBP stock is not admitted"
            )
        );
        assert_eq!(
            IbkrVenueReadRefusal::NoReconcileConnection.to_string(),
            "ibkr: this account's reconcile connection is not running in this daemon (reconciliation off, the \
             cpapi backend, or its connect failed), and this read opens no IB Gateway connection of its own (each \
             one takes an API client id); read it while the account's reconcile runs"
        );
        assert_eq!(
            IbkrConfigRefusal::NoAccountStored { count: 2 }.to_string(),
            "the IB Gateway login manages 2 account(s) and this ibkr account stores no `ACCOUNT` field, so nothing \
             says which one to trade and none is: the `ACCOUNT` field picks one (`vike-cli secrets set --id \
             <id> ACCOUNT`, the number on stdin)"
        );
        assert_eq!(
            IbkrReadError::NoEnd {
                what: "positions".into(),
                deadline: std::time::Duration::from_secs(10)
            }
            .to_string(),
            "ibkr positions: no end of the reply within 10s"
        );
    }
}
