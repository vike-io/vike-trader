//! cTrader's refusals, declared once (`vike_utility::refusal_enum!`, decision 0301): each has its id and its ONE text,
//! and code and tests decide on the variant, never on the words. Every id is published in the workspace's
//! `refusals.json` (`crates/vike-backend/tests/refusal_catalog.rs`).
//!
//! A refusal leaves this crate as its text where the shared seam carries a `String` (the symbol facts' `Option<String>`
//! fences and `vike_bridge_core::symbol_properties::SymbolPropertiesError`, `vike_catalog::CatalogError`,
//! `vike_data::source::SourceError`, `venue_read`, the reconcile client): its `Display`, byte for byte the words it had
//! before it was typed. A RATE refusal ([`CtraderRateRefusal`]) is worded without the venue's head, so a cross through
//! USD can name its failed leg inside its own sentence; `From<CtraderRateRefusal> for QuoteRateError` puts `ctrader: `
//! before it, once, and picks the kind (`Unavailable`, `NoPair`, `BadQuote`) by the variant.
//!
//! `crate::history::HistoryConfigError` is declared beside its source, where the gatewayhub names it.

use vike_model::{CONVERSION_HUB, QuoteRateError};

use crate::account::MarketState;
use crate::transport::token_store::REAUTHORIZE_COMMAND;

vike_utility::refusal_enum! {
    /// Why a symbol the gatewayhub asks this venue's facts of is not admitted: the account-listed fences
    /// (`crate::catalog::any_symbol_refusal`, `quoted_symbol_refusal`) and the verdicts and failures of its rate
    /// (`crate::symbol_properties::quoted_rate`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CtraderPropertiesRefusal with ids CtraderPropertiesRefusalId {
        /// A CFD of a class whose unit of volume was never measured; `class` names it (`a share CFD`).
        UnmeasuredCfd { name: String, class: &'static str } => "ctrader.properties.unmeasured_cfd":
            "ctrader: {name} is {class}, and this adapter admits forex pairs and the CFD classes whose unit of volume \
             it has measured (metals, indices, oil and commodities, crypto) only: what one unit of volume means for \
             this class has not been measured, so a position in it would be valued on a guess",
        /// Neither a forex pair nor a CFD.
        NotFxOrCfd { name: String } => "ctrader.properties.not_fx_or_cfd":
            "ctrader: {name} is neither a forex pair nor a CFD, which is all this adapter admits on demand",
        /// The broker names no quote asset for the row.
        NoQuoteCurrency { name: String, deposit: String } => "ctrader.properties.no_quote_currency":
            "ctrader: {name} names no quote currency, so it cannot be checked against this account's currency \
             ({deposit})",
        /// Quoted in another currency than the deposit's, on a node that converts nothing.
        ForeignQuote { name: String, quote: String, deposit: String } => "ctrader.properties.foreign_quote":
            "ctrader: {name} is quoted in {quote} but this account's currency is {deposit}, and the engine books \
             money in ONE currency (nothing converts a quote currency), so a position in it would be mis-valued; \
             only instruments quoted in {deposit} are admitted",
        /// No minimum volume published.
        NoMinimumVolume { name: String } => "ctrader.properties.no_minimum_volume":
            "ctrader: {name}: the broker publishes no minimum volume for it (missing or zero), so an order below \
             the minimum could not be judged",
        /// A converting node's row names no quote currency to convert.
        InstrumentNamesNoQuote { deposit: String } => "ctrader.properties.instrument_names_no_quote":
            "ctrader: the instrument names no quote currency, so it cannot be converted into the account's \
             currency ({deposit})",
        /// The account lists no single pair for the quote currency: a verdict the node remembers. `no_pair` is the
        /// rate read's own words.
        NoPair { no_pair: String } => "ctrader.properties.no_pair":
            "{no_pair}; the symbol is NOT admitted (no rate, no admission: it is never valued at par)",
        /// The rate read failed now; `why` is its words.
        RateUnavailable { quote: String, deposit: String, why: String } => "ctrader.properties.rate_unavailable":
            "no rate for {quote} in {deposit} ({why})",
        /// The read answered for another currency than the one asked (the flipped-rate class of bug).
        RateForAnotherCurrency { quote: String, deposit: String, answered: String } =>
            "ctrader.properties.rate_for_another_currency":
            "no rate for {quote} in {deposit} (the read answered for {answered})",
        /// The session that priced the rate is for an account in another currency than the listing's.
        RateForAnotherAccount { quote: String, deposit: String, account: String } =>
            "ctrader.properties.rate_for_another_account":
            "no rate for {quote} in {deposit} (the session that priced it is for an account in {account}, and this \
             account's listing says {deposit})",
        /// The broker's asset list names no deposit currency (`crate::symbol_properties::deposit_named`).
        DepositUnnamed => "ctrader.properties.deposit_unnamed":
            "ctrader: the broker's asset list does not name this account's deposit currency",
    }
}

vike_utility::refusal_enum! {
    /// Why the rate of a quote currency in the account's deposit currency could not be had (`crate::account`, the
    /// one-shot quote session `crate::transport::connection::fetch_conversion_quote_assuming`). Worded without the
    /// venue's head: see the module doc.
    #[derive(Debug, Clone, PartialEq)]
    pub enum CtraderRateRefusal with ids CtraderRateRefusalId {
        /// Two or more pairs between one leg's currencies: which one the rate is read from would be a guess.
        ManyPairs { count: usize, from: String, to: String, names: String } => "ctrader.rate.many_pairs":
            "the account's symbol list has {count} forex pairs between {from} and {to} ({names}), and which one \
             the rate is read from would be a guess",
        /// An empty currency code on either side.
        EmptyCurrency => "ctrader.rate.empty_currency":
            "the account's currency or the quote currency is empty, so no conversion pair can be chosen",
        /// The quote currency is the account's own.
        SameCurrency { quote: String } => "ctrader.rate.same_currency":
            "{quote} is the account's own currency, so there is nothing to convert",
        /// No pair between the two currencies (`through` names the hub that was tried too, or is empty).
        NoPair { quote: String, account: String, through: String } => "ctrader.rate.no_pair":
            "the account's symbol list has no forex pair between {quote} and the account's currency \
             {account}{through}, so {quote} has no rate",
        /// A zero bid or ask.
        ZeroSide { bid: u64, ask: u64 } => "ctrader.rate.zero_side":
            "the conversion pair's quote has a zero side (bid {bid}, ask {ask}), so it is no price",
        /// The bid above the ask.
        Crossed { bid: u64, ask: u64 } => "ctrader.rate.crossed":
            "the conversion pair's quote is crossed (bid {bid} above ask {ask}), so it is no price",
        /// A mid no rate can be built from.
        MidNoRate { mid: f64 } => "ctrader.rate.mid_no_rate": "the conversion pair's mid {mid} is no rate",
        /// The broker suspended the pair (`tradingMode` disabled).
        Suspended { name: String, mode: &'static str } => "ctrader.rate.suspended":
            "{name} is not enabled for trading (tradingMode {mode}), so its last price is no rate",
        /// The quote carries no timestamp.
        Untimed => "ctrader.rate.untimed":
            "the conversion pair's quote carries no timestamp, so its age is unknown and it is no rate",
        /// A stamp below a millisecond epoch (it reads as seconds).
        StampInSeconds { stamp: i64 } => "ctrader.rate.stamp_in_seconds":
            "the conversion pair's quote is stamped {stamp}, which is not a millisecond epoch (it reads as \
             seconds): the unit of the venue's spot timestamp is assumed to be milliseconds, so the quote is refused \
             and not guessed at",
        /// A stamp too far after the ask; `ahead` is the span (`3 h`).
        StampAfterAsk { stamp: i64, ahead: String, asked_ms: i64 } => "ctrader.rate.stamp_after_ask":
            "the conversion pair's quote is stamped {stamp}, {ahead} after the ask ({asked_ms}), which no live quote \
             can be: the unit of the venue's spot timestamp is assumed to be milliseconds and this reads as a finer \
             unit (or a clock far ahead), so the quote is refused and not guessed at",
        /// Too old for the strict bound, the schedule's zone not one this crate reads.
        StaleZoneNotUnderstood { age: String, zone: String, limit: String } =>
            "ctrader.rate.stale_zone_not_understood":
            "the conversion pair's last price is {age} old and the market schedule zone {zone} is not understood; \
             refusing a quote older than {limit}, so it is no rate",
        /// Too old for the strict bound, no schedule stated.
        StaleNoSchedule { age: String, limit: String } => "ctrader.rate.stale_no_schedule":
            "the conversion pair's last price is {age} old and the market schedule is not stated, so the market \
             state is not understood; refusing a quote older than {limit}, so it is no rate",
        /// Too old for the market's own state.
        Stale { age: String, limit: String, market: MarketState } => "ctrader.rate.stale":
            "the conversion pair's last price is {age} old (more than {limit} with the market {market:?}), so it is \
             no rate",
        /// The session's handshake or symbol lists failed.
        SymbolsUnread { why: String } => "ctrader.rate.symbols_unread": "symbols: {why}",
        /// The broker's asset list names no deposit currency and none was assumed.
        DepositUnnamed => "ctrader.rate.deposit_unnamed":
            "the broker's asset list does not name this account's deposit currency, so a quote currency cannot be \
             converted into it",
        /// One leg of a cross through the hub currency has no rate; its kind is the leg's. Its words are boxed
        /// `str`s, so the variant stays under clippy's `result_large_err` bound.
        ThroughHub {
            quote: Box<str>,
            account: Box<str>,
            from: Box<str>,
            to: Box<str>,
            pair: Box<str>,
            leg: Box<CtraderRateRefusal>
        } => "ctrader.rate.through_hub":
            "{quote} is converted into {account} through {CONVERSION_HUB}, and the {from} to {to} leg ({pair}) has no \
             rate: {leg}",
        /// The two legs' product is no rate.
        ComposedNotFinite { quote: String, account: String } => "ctrader.rate.composed_not_finite":
            "the rate of {quote} in {account} composed through {CONVERSION_HUB} is not a finite positive number",
        /// The subscription to the pair could not be sent.
        Subscribe { name: String, why: String } => "ctrader.rate.subscribe": "subscribing to {name}: {why}",
        /// The pair's spots could not be read.
        ReadSpots { name: String, why: String } => "ctrader.rate.read_spots": "reading {name}: {why}",
        /// No two-sided quote within the wait.
        NoTwoSidedQuote { name: String, secs: u64 } => "ctrader.rate.no_two_sided_quote":
            "{name} sent no two-sided quote within {secs} s of the subscription",
    }
}

impl CtraderRateRefusal {
    /// The kind of `QuoteRateError` this refusal is: a failed moment, no single pair (a verdict), or a quote that is
    /// no rate. A cross is its failed leg's kind.
    fn kind(&self) -> fn(String) -> QuoteRateError {
        match self {
            Self::EmptyCurrency
            | Self::SymbolsUnread { .. }
            | Self::DepositUnnamed
            | Self::Subscribe { .. }
            | Self::ReadSpots { .. }
            | Self::NoTwoSidedQuote { .. } => QuoteRateError::Unavailable,
            Self::ManyPairs { .. } | Self::SameCurrency { .. } | Self::NoPair { .. } => {
                QuoteRateError::NoPair
            }
            Self::ZeroSide { .. }
            | Self::Crossed { .. }
            | Self::MidNoRate { .. }
            | Self::Suspended { .. }
            | Self::Untimed
            | Self::StampInSeconds { .. }
            | Self::StampAfterAsk { .. }
            | Self::StaleZoneNotUnderstood { .. }
            | Self::StaleNoSchedule { .. }
            | Self::Stale { .. }
            | Self::ComposedNotFinite { .. } => QuoteRateError::BadQuote,
            Self::ThroughHub { leg, .. } => leg.kind(),
        }
    }
}

impl From<CtraderRateRefusal> for QuoteRateError {
    fn from(refusal: CtraderRateRefusal) -> Self {
        refusal.kind()(format!("ctrader: {refusal}"))
    }
}

vike_utility::refusal_enum! {
    /// Why the account's own listing (or its deposit currency) could not be read for the symbol facts
    /// (`crate::CatalogProviderCtrader`), carried as `vike_catalog::CatalogError`'s text.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CtraderCatalogRefusal with ids CtraderCatalogRefusalId {
        /// The account holds no usable grant: nothing is dialled.
        NoCredentials => "ctrader.catalog.no_credentials":
            "ctrader: this account holds no usable credentials (the OAuth grant and the application pair)",
        /// The one-shot session that reads the lists failed; `why` is its words.
        SymbolsUnread { why: String } => "ctrader.catalog.symbols_unread": "ctrader symbols: {why}",
        /// The broker's asset list names no deposit currency, and the fence needs one.
        DepositNotNamed => "ctrader.catalog.deposit_not_named":
            "ctrader: the broker's asset list does not name this account's deposit currency, so a quote currency \
             cannot be checked against it",
    }
}

vike_utility::refusal_enum! {
    /// Why a USER name has no broker name (decision 0290, `crate::instrument::names::names_in`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CtraderNameRefusal with ids CtraderNameRefusalId {
        /// No list is kept, no kept row answers the name, or two do; `why` is the translator's own words
        /// (`vike_catalog::names_of_user_name`), which name the fix when no list is kept.
        Untranslated { why: String } => "ctrader.name.untranslated": "ctrader: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why a candle-history fetch was refused or failed (`crate::history::KlineSourceCtrader`), carried as
    /// `vike_data::source::SourceError`'s text.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CtraderHistoryRefusal with ids CtraderHistoryRefusalId {
        /// An interval the venue serves no candles for; `served` lists the ones it does.
        IntervalNotServed { interval: String, served: String } => "ctrader.history.interval_not_served":
            "ctrader history: {interval:?} is not an interval cTrader serves candles for (served: {served})",
        /// A calendar-width interval this source cannot page by a fixed step.
        CalendarWidth { interval: String } => "ctrader.history.calendar_width":
            "ctrader history: {interval:?} candles are calendar-width, and this source pages by a fixed step. Fetch \
             1d and resample",
        /// The window needs more requests than one fetch may send.
        TooManyRequests { most: usize } => "ctrader.history.too_many_requests":
            "ctrader history: the window needs more than {most} requests; ask for a narrower one",
        /// The venue said more bars follow and sent none inside the window.
        MoreWithoutBars => "ctrader.history.more_without_bars":
            "ctrader history: the venue said more bars follow but sent none inside the window",
        /// The session dropped again after its one reopen.
        DroppedTwice { why: String } => "ctrader.history.dropped_twice":
            "ctrader history: the session dropped twice in a row ({why})",
        /// The session failed on the wire (a drop, a refused handshake, an answer that does not decode).
        Wire { why: String } => "ctrader.history.wire": "ctrader history: {why}",
        /// The venue answered with an error.
        Venue { code: String, description: String } => "ctrader.history.venue":
            "ctrader history: cTrader error {code}: {description}",
        /// The venue refused the account's grant.
        GrantRefused { code: String, description: String } => "ctrader.history.grant_refused":
            "ctrader history: cTrader error {code}: {description}. The account's grant was refused: a running \
             ctrader socket of this account refreshes it (a history session only follows it), otherwise re-issue it \
             with `{REAUTHORIZE_COMMAND}`",
        /// A symbol the account does not list (`shown` is its first 40 characters, `hint` a near name or nothing).
        UnknownSymbol { shown: String, hint: String } => "ctrader.history.unknown_symbol":
            "ctrader history: {shown:?} is not a symbol this account lists. Names are the broker's own and \
             case-significant (like EURUSD), so one instrument can never be stored under two spellings.{hint}",
        /// The answer is for another symbol or period than the one asked.
        Crossed => "ctrader.history.crossed":
            "ctrader history: the venue answered for another symbol or period than the one asked",
        /// A trendbar with no low price or no stamp.
        TrendbarUnread { minute: Option<u32> } => "ctrader.history.trendbar_unread":
            "ctrader history: a trendbar (stamp minute {minute:?}) has no low price or no stamp, so it has no reading",
        /// A trendbar that opens or closes above its high.
        TrendbarAboveHigh { minute: u32, open: u64, close: u64, high: u64 } =>
            "ctrader.history.trendbar_above_high":
            "ctrader history: a trendbar (stamp minute {minute}) opens or closes above its high (deltas {open} and \
             {close} against {high} from the low)",
        /// A stamp off its period's grid: the venue's convention is not the one this source reads.
        OffGrid { minute: i64, width: i64 } => "ctrader.history.off_grid":
            "ctrader history: a trendbar stamped minute {minute} is off the {width}-minute grid of its period, so \
             the venue's stamp convention is not the one this source reads. Nothing was stored for the window",
        /// Another history session of this process held the one permit past the wait.
        SessionBusy { waited_ms: u128 } => "ctrader.history.session_busy":
            "another cTrader history session is open in this process, and it was still open after {waited_ms} ms of \
             waiting (one at a time: the venue's per-application connection cap is not measured). Run the same \
             command again when that fetch has finished",
    }
}

vike_utility::refusal_enum! {
    /// Why an account does NOT RUN, or its startup credential probe fails (`crate::adapter`, `crate::config`): the
    /// words an operator reads beside the account's NOT RUNNING row and in its start log.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CtraderStartRefusal with ids CtraderStartRefusalId {
        /// The account's row states no symbol, and the handshake needs one.
        NoAccountSymbol => "ctrader.start.no_account_symbol":
            "the account's row states no symbol (`account.symbol`), and cTrader's handshake needs one in the \
             account's own list: `vike-cli account set-symbol --id <N> <SYMBOL>`",
        /// The probe's handshake failed before the venue gave a verdict.
        ProbeUnanswered { endpoint: String, why: String } => "ctrader.start.probe_unanswered":
            "got no verdict from {endpoint} ({why}) — check reachability and the account's ACCESS_TOKEN/REFRESH_TOKEN \
             grant (re-issue it with `{REAUTHORIZE_COMMAND}`)",
        /// The probe's handshake was refused.
        ProbeRefused { endpoint: String } => "ctrader.start.probe_refused":
            "connect/auth refused at {endpoint} — check reachability and the account's ACCESS_TOKEN/REFRESH_TOKEN \
             grant (re-issue it with `{REAUTHORIZE_COMMAND}`)",
        /// The tier's trade socket row is not `host:port`.
        SocketRowNotHostPort { ty: String, socket: String } => "ctrader.start.socket_row_not_host_port":
            "ctrader: the {ty} trade socket row `{socket}` is not host:port",
    }
}

vike_utility::refusal_enum! {
    /// Why an operator's read of the account was not made (`crate::adapter`'s `venue_read`,
    /// `crate::reconcile::venue_read`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CtraderVenueReadRefusal with ids CtraderVenueReadRefusalId {
        /// A `live` account: this bridge dials only the demo host.
        LiveTier => "ctrader.venue_read.live_tier":
            "this bridge reaches only cTrader's demo host, so an account on the live tier has no host to read; \
             nothing was asked",
        /// The account holds no complete grant.
        NoGrant => "ctrader.venue_read.no_grant":
            "the account holds no complete ctrader grant (the venue's CLIENT_ID and CLIENT_SECRET, the account's \
             ACCESS_TOKEN and REFRESH_TOKEN); nothing was asked",
        /// The grant could not be read into a config.
        GrantUnread => "ctrader.venue_read.grant_unread":
            "the account's ctrader grant could not be read; nothing was asked",
        /// The read's own socket was refused.
        ConnectRefused { host: String, port: u16 } => "ctrader.venue_read.connect_refused":
            "ctrader: connect/auth refused at {host}:{port}, so the pending orders were not read (check the \
             account's ACCESS_TOKEN/REFRESH_TOKEN grant: `{REAUTHORIZE_COMMAND}`)",
    }
}

vike_utility::refusal_enum! {
    /// Why a reconcile pass claims nothing about a symbol, or adopts no balance (`crate::reconcile`): a fetch error
    /// the pass manager skips the pass on, which its tests decide on by variant.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CtraderReconcileRefusal with ids CtraderReconcileRefusalId {
        /// Position rows of a status this build cannot classify.
        PositionRowsUnclassified { unreadable: usize, symbol: String } =>
            "ctrader.reconcile.position_rows_unclassified":
            "ctrader recon: {unreadable} position row(s) of {symbol} carry a status this build cannot classify (not an \
             OPEN position with a volume, nor a CLOSED or CREATED one), so its position cannot be claimed read",
        /// A deposit currency that is not USD: the bare balance is not adopted into the USD ledger.
        DepositNotUsd { currency: String } => "ctrader.reconcile.deposit_not_usd":
            "ctrader recon: the account's deposit currency is {currency}, and a reconcile balance carries no \
             currency, so it is not adopted into the engine's USD ledger",
        /// The broker's asset list names no deposit currency.
        DepositUnknown => "ctrader.reconcile.deposit_unknown":
            "ctrader recon: the account's deposit currency is unknown (the broker's asset list does not name it), so \
             its balance is not adopted",
        /// The trader record states no money scale; `why` is the handshake's words.
        MoneyDigits { why: String } => "ctrader.reconcile.money_digits": "{why}",
        /// The USER name has no broker name; `why` is the translator's refusal.
        NameUnread { why: String, name: String } => "ctrader.reconcile.name_unread":
            "ctrader recon: {why}, so the orders, fills and position of {name} cannot be read",
        /// The broker dropped the name from the account's list.
        NotInList { name: String, wire: String } => "ctrader.reconcile.not_in_list":
            "ctrader recon: symbol {name} (the broker's `{wire}`) is not in the account's symbol list, so its orders, \
             fills and position cannot be read",
        /// The deal list stayed truncated past the paging bound.
        DealListTruncated => "ctrader.reconcile.deal_list_truncated":
            "ctrader recon: the deal list has more rows than one reply carries (`has_more`) and paging did not reach \
             its end, so the fills of every admitted symbol cannot be claimed read",
        /// One deal page says it has more rows than it carries.
        DealPageHasMore => "ctrader.reconcile.deal_page_has_more":
            "ctrader recon: the deal list has more rows than one reply carries (`has_more`); narrow the window",
        /// The exec's reconnect has not read the venue's lists yet: the pass waits for the next one.
        Settling => "ctrader.reconcile.settling":
            "ctrader recon: the exec socket died and its reconnect has not yet read the venue's lists for what it left \
             unanswered, so this pass would set the venue against a book that has not heard it; it waits for the next \
             pass",
    }
}

vike_utility::refusal_enum! {
    /// Why a symbol's commission terms give no fee schedule (`crate::instrument::symbols::CommissionTerms::fee_schedule`):
    /// the static fee row then stands.
    #[derive(Debug, Clone, PartialEq)]
    pub enum CtraderFeeRefusal with ids CtraderFeeRefusalId {
        /// A per-lot commission on a symbol that states no lot size.
        PerLotNoLotSize { amount: f64 } => "ctrader.fees.per_lot_no_lot_size":
            "a per-lot commission of {amount} on a symbol that states no lot size",
        /// A USD-per-lot commission on a symbol quoted in another currency.
        UsdPerLotForeignQuote { usd: f64, quote: String } => "ctrader.fees.usd_per_lot_foreign_quote":
            "a commission of {usd} USD per lot on a symbol quoted in {quote}: no schedule in the quote currency",
        /// A commission type the vendored proto has no word for.
        UnknownType { kind: i32 } => "ctrader.fees.unknown_type":
            "a commission type this bridge does not know ({kind})",
    }
}
