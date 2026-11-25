//! **Every "no" this bridge answers in its own words, one id each** (`vike_utility::refusal_enum!`, decision 0301):
//! the symbol facts a converting node asks ([`FxcmSymbolRefusal`]) and the subscriptions behind them
//! ([`FxcmSubscribeRefusal`]), the price lane ([`FxcmPriceRefusal`]), the candle archive's history answers
//! ([`FxcmHistoryRefusal`], the file reasons [`FxcmArchiveFileRefusal`]), an operator's read of the open orders
//! ([`FxcmVenueReadRefusal`]) and the instrument list ([`FxcmCatalogRefusal`]); and, for code and tests to decide on,
//! why the reconcile session's port answered nothing ([`FxcmSessionError`]).
//!
//! Each text is byte-identical to the sentence the site built before. Code and tests decide on the VARIANT, never on
//! the words. The seams this crate answers through (`vike_bridge_core::symbol_properties`'s errors,
//! `vike_data::source::SourceError`, `VenueAdapter::venue_read`, `vike_catalog::CatalogError`) carry the refusal with
//! its id (`carried()`); the price stream's `DataClientError` is still text: there the BARE text (`Display`) is handed
//! over, so the id does not cross it yet.
//!
//! Not here: an order's rejection (its own typed cause, `vike_model::OrderRefusalId`), a log line, and the venue's own
//! words (`crate::reconcile::redial`'s `refuses_credentials`).

use vike_model::CONVERSION_HUB;

vike_utility::refusal_enum! {
    /// Why the facts of a symbol (its row, its quote currency, its conversion rate, the account's currency) are not
    /// answered: `crate::account::quote_rate`, `crate::account::reads`, `crate::symbol_properties`.
    #[derive(Debug, Clone, PartialEq)]
    pub enum FxcmSymbolRefusal with ids FxcmSymbolRefusalId {
        /// The session did not answer the account's currency: nothing is assumed over it.
        CurrencyUnanswered { why: String } => "fxcm.symbol.currency_unanswered":
            "fxcm: the session did not answer the account's currency ({why}); it is not assumed over a \
             session that said nothing",
        PropertiesNotJson { error: String } => "fxcm.symbol.properties_not_json":
            "fxcm: the venue's system properties are not JSON ({error}); the account's currency is not \
             assumed over a malformed answer",
        PropertiesNotObject => "fxcm.symbol.properties_not_object":
            "fxcm: the venue's system properties are not a JSON object; the account's currency is not \
             assumed over a malformed answer",
        OffersNotJson { error: String } => "fxcm.symbol.offers_not_json":
            "fxcm: the Offers table is not JSON ({error})",
        OffersNotArray => "fxcm.symbol.offers_not_array": "fxcm: the Offers table is not a JSON array",
        /// The session's own failure to read the Offers table, in its words.
        OffersUnread { why: String } => "fxcm.symbol.offers_unread": "{why}",
        UnknownType { instrument: String, instrument_type: i32 } => "fxcm.symbol.unknown_type":
            "fxcm: `{instrument}` is an instrument of type {instrument_type}, which is none of the nine the SDK \
             names, so its quote currency is not determinable and it is not admitted",
        ForexMultiplier { instrument: String, multiplier: f64 } => "fxcm.symbol.forex_multiplier":
            "fxcm: `{instrument}` has a contract multiplier of {multiplier} (a forex instrument's is 1), so one \
             unit is not one unit of its base currency and its money is not converted",
        NotAPair { instrument: String } => "fxcm.symbol.not_a_pair":
            "fxcm: `{instrument}` is not a `BASE/QUOTE` pair of two currency codes, so it names no quote currency",
        ContractCurrencyDisagrees { instrument: String, contract_currency: String, base: String } =>
            "fxcm.symbol.contract_currency_disagrees":
            "fxcm: `{instrument}` states ContractCurrency `{contract_currency}` (the base currency of the \
             instrument) and its name says {base}; the two disagree, so its quote currency is not trusted",
        CfdMultiplier { instrument: String, class: &'static str, multiplier: f64 } =>
            "fxcm.symbol.cfd_multiplier":
            "fxcm: `{instrument}` ({class}) has a contract multiplier of {multiplier}, which is not a positive \
             number, so what one unit of it is worth is unknown and it is not admitted",
        CfdCurrencyNotACode { instrument: String, class: &'static str, contract_currency: String } =>
            "fxcm.symbol.cfd_currency_not_a_code":
            "fxcm: `{instrument}` ({class}) states ContractCurrency `{contract_currency}`, which is not a \
             currency code, so its quote currency is not determinable and it is not admitted",
        CfdNameDisagrees { instrument: String, class: &'static str, named: String, native: String } =>
            "fxcm.symbol.cfd_name_disagrees":
            "fxcm: `{instrument}` ({class}) is named like a pair quoted in {named} and states ContractCurrency \
             `{native}`; the two disagree, so its quote currency is not trusted",
        NotInOffers { symbol: String } => "fxcm.symbol.not_in_offers":
            "fxcm: `{symbol}` is not in the venue's Offers table",
        MountTypeUnknown { instrument: String, instrument_type: i32 } => "fxcm.symbol.mount_type_unknown":
            "fxcm: `{instrument}` is of InstrumentType {instrument_type}, a class this bridge does not know, so \
             what one unit of it stands for is unknown",
        SeveralRows { symbol: String, first: String, second: String } => "fxcm.symbol.several_rows":
            "fxcm: `{symbol}` names more than one row of the Offers table (`{first}` and `{second}`), so it is \
             not admitted",
        MarketNotOpen { instrument: String, status: String } => "fxcm.symbol.market_not_open":
            "fxcm: the market for `{instrument}` is not open (trading status `{status}`), so it states no live \
             rate",
        SideNotTradable { instrument: String, bid: String, ask: String } => "fxcm.symbol.side_not_tradable":
            "fxcm: `{instrument}` is not available for trading on a side (bid `{bid}`, ask `{ask}`; only `T` is \
             tradable, not indicative `I` or unavailable `N`), so its quote is not a price or a rate",
        NotTwoSided { instrument: String, bid: f64, ask: f64 } => "fxcm.symbol.not_two_sided":
            "fxcm: `{instrument}` has no two-sided positive quote (bid {bid}, ask {ask})",
        Crossed { instrument: String, bid: f64, ask: f64 } => "fxcm.symbol.crossed":
            "fxcm: the quote of `{instrument}` is crossed (bid {bid} above ask {ask})",
        QuoteUntimed { instrument: String } => "fxcm.symbol.quote_untimed":
            "fxcm: the quote of `{instrument}` carries no time of its own, so its age is not known and it is \
             not a rate",
        QuoteStale { instrument: String, age_s: i64, limit_s: i64 } => "fxcm.symbol.quote_stale":
            "fxcm: the quote of `{instrument}` is {age_s} s old by its own time (the limit is {limit_s} s), so \
             it is not a live rate",
        QuoteAhead { instrument: String, ahead_s: i64 } => "fxcm.symbol.quote_ahead":
            "fxcm: the quote of `{instrument}` is stamped {ahead_s} s ahead of the server clock; the row time \
             and the server time do not share a time frame, so no age is trusted and it is not a rate",
        /// FXCM lists no pair of `from` and `to` in either orientation.
        NoPair { from: String, to: String } => "fxcm.symbol.no_pair":
            "fxcm lists no {from}/{to} or {to}/{from} pair, so one {from} cannot be converted to {to}",
        BothListed { from: String, to: String } => "fxcm.symbol.both_listed":
            "fxcm lists BOTH {from}/{to} and {to}/{from}, so the conversion pair is ambiguous and no rate is \
             chosen",
        NotCurrencyCodes { quote: String, account: String } => "fxcm.symbol.not_currency_codes":
            "fxcm: `{quote}` / `{account}` are not both currency codes, so no pair converts them",
        SameCurrency { quote: String } => "fxcm.symbol.same_currency":
            "fxcm: {quote} is the account's own currency; there is nothing to convert",
        /// A rate composed through the hub whose leg `from -> to` has none, and why.
        ThroughHub { quote: String, account: String, from: String, to: String, why: Box<FxcmSymbolRefusal> } =>
            "fxcm.symbol.through_hub":
            "fxcm: {quote} is converted to {account} through {CONVERSION_HUB} (no {quote}/{account} or \
             {account}/{quote} pair is listed), and the {from} to {to} leg has no rate: {why}",
        ComposedRateNotFinite { quote: String, account: String } => "fxcm.symbol.composed_rate_not_finite":
            "fxcm: the rate of {quote} in {account} composed through {CONVERSION_HUB} is not a finite \
             positive number",
        PairNotSubscribed { from: String, to: String, instrument: String, subscription: String } =>
            "fxcm.symbol.pair_not_subscribed":
            "fxcm's conversion pair for {from} and {to} is not subscribed for trading: {instrument} \
             (subscription `{subscription}`)",
        DerivedRateNotFinite { instrument: String } => "fxcm.symbol.derived_rate_not_finite":
            "fxcm: the rate derived from `{instrument}` is not a finite positive number",
        NotSubscribed { name: String, subscription: String } => "fxcm.symbol.not_subscribed":
            "fxcm: `{name}` is not subscribed for trading on this login (subscription `{subscription}`)",
        /// Not subscribed, and of a kind the bridge does not subscribe itself (`why`).
        NotSubscribedNotOurs { name: String, subscription: String, why: Box<FxcmSymbolRefusal> } =>
            "fxcm.symbol.not_subscribed_not_ours":
            "fxcm: `{name}` is not subscribed for trading on this login (subscription `{subscription}`); the \
             bridge does not subscribe it: {why}",
        UnusablePrecision { name: String, digits: i32 } => "fxcm.symbol.unusable_precision":
            "fxcm: `{name}` reports an unusable price precision ({digits})",
        NoBaseUnit { name: String } => "fxcm.symbol.no_base_unit":
            "fxcm: `{name}` has no base unit size on the tradable account, so it has no lot",
        NoTerms { name: String, class: &'static str } => "fxcm.symbol.no_terms":
            "fxcm: `{name}` is {class} and the installed shim sends no trading terms (the minimum and maximum \
             order), so it is not admitted: install the shim of this release",
        NoForexForQuote { name: String, class: &'static str, quote: String, account: String } =>
            "fxcm.symbol.no_forex_for_quote":
            "fxcm: `{name}` ({class}) is quoted in {quote}, which no forex pair FXCM lists carries, so it has no \
             conversion to {account} and is not admitted",
        /// A refusal, with what FXCM said about the subscriptions it needed (`said`, joined).
        WithSubscriptionRefusals { why: Box<FxcmSymbolRefusal>, said: String } =>
            "fxcm.symbol.with_subscription_refusals": "{why}; {said}",
        NoReconcileSession => "fxcm.symbol.no_reconcile_session":
            "fxcm: no reconcile session is running for this account, so the venue's quotes cannot be \
             read (a second login is not opened: FXCM sessions are scarce), and a symbol quoted in \
             another currency stays refused",
        DemoOnly => "fxcm.symbol.demo_only":
            "fxcm: this adapter trades its demo account only (a `demo` account), so no \
             instrument is looked up for any other tier",
        NotAnAccountLabel { text: String } => "fxcm.symbol.not_an_account_label":
            "fxcm: `{text}` is not an account label, so there is no session to ask",
    }
}

vike_utility::refusal_enum! {
    /// Why a subscription (`SetSubscriptionStatus`, `crate::instrument::subscription`) did not come to pass, or why a
    /// session cannot make one (the gap words, [`Self::GapConnection`] and [`Self::GapOldShim`]).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FxcmSubscribeRefusal with ids FxcmSubscribeRefusalId {
        NotListed { instrument: String } => "fxcm.subscribe.not_listed":
            "fxcm: `{instrument}` is not in the venue's Offers table, so it cannot be subscribed",
        Unbuildable { instrument: String, said: String } => "fxcm.subscribe.unbuildable":
            "fxcm: ForexConnect could not build the request that subscribes `{instrument}`: {said}",
        NotNow { instrument: String, said: String } => "fxcm.subscribe.not_now":
            "fxcm: FXCM could not subscribe `{instrument}` for trading just now (an internal error, a code its \
             list does not name, or the account locked or in the daily rollover); the venue said: {said}",
        Refused { instrument: String, said: String } => "fxcm.subscribe.refused":
            "fxcm: FXCM refused to subscribe `{instrument}` for trading; the venue said: {said}",
        TypeRefused { instrument: String, said: String } => "fxcm.subscribe.type_refused":
            "fxcm: the shim refused to subscribe `{instrument}` for trading ({said}): either its instrument type \
             is none of the nine the SDK names, or the installed shim is older than this binary and subscribes \
             regular forex pairs only (install the shim of this release, or subscribe it in the FXCM platform)",
        OrdersNotPermitted { instrument: String, said: String } => "fxcm.subscribe.orders_not_permitted":
            "fxcm: the account may not trade `{instrument}`: {said}; the bridge does not subscribe an instrument \
             FXCM refuses this account's orders on, so use an account FXCM has enabled for it",
        NotKnownToHaveApplied { instrument: String, said: String } =>
            "fxcm.subscribe.not_known_to_have_applied":
            "fxcm: the subscription of `{instrument}` is not known to have applied: {said}",
        StillReads { instrument: String, status: String } => "fxcm.subscribe.still_reads":
            "fxcm: FXCM answered the request that subscribes `{instrument}` but its Offers row still reads \
             {status} after the wait",
        FailedWithCode { instrument: String, code: i32, said: String } => "fxcm.subscribe.failed_with_code":
            "fxcm: the subscription of `{instrument}` failed with code {code}: {said}",
        Undone { instrument: String, status: String } => "fxcm.subscribe.undone":
            "fxcm: `{instrument}` was subscribed by this bridge earlier in this session and its Offers row \
             reads {status} again, so it was changed at the venue since; the bridge does not subscribe it a \
             second time",
        SessionCannot { instrument: String } => "fxcm.subscribe.session_cannot":
            "fxcm: this session cannot subscribe `{instrument}` for trading",
        CannotItself { instrument: String, gap: String } => "fxcm.subscribe.cannot_itself":
            "fxcm: the bridge cannot subscribe `{instrument}` for trading itself: {gap}",
        NoExport => "fxcm.subscribe.no_export":
            "fxcm: the shim exports no `forex_connect_set_subscription`",
        /// The instrument or the status could not be handed to the shim (an interior NUL), in that error's words.
        BadName { error: String } => "fxcm.subscribe.bad_name": "{error}",
        OtherAnswer => "fxcm.subscribe.other_answer": "fxcm reconcile: expected a subscription answer",
        GapConnection { connection: String, write_connection: &'static str } => "fxcm.subscribe.gap.connection":
            "the session is on the `{connection}` connection and the bridge writes a subscription on \
             `{write_connection}` only (whether a real account may change its own subscription is not \
             measured), so a symbol that is not tradable must be subscribed in the FXCM platform",
        GapOldShim { shim: String } => "fxcm.subscribe.gap.old_shim":
            "the shim this process opened — {shim} — is older than this binary and exports no \
             `forex_connect_set_subscription`; the deploy installs the release's shim from the next deploy on \
             (`docs/ops/fxcm-forexconnect.md`, \"How the shim reaches a box\"), and until then a symbol that is \
             not tradable must be subscribed in the FXCM platform",
    }
}

vike_utility::refusal_enum! {
    /// Why the price lane (`crate::feed`) prices nothing for a symbol, or refuses a stream's ask.
    #[derive(Debug, Clone, PartialEq)]
    pub enum FxcmPriceRefusal with ids FxcmPriceRefusalId {
        NotAUserName { symbol: String } => "fxcm.price.not_a_user_name":
            "fxcm: `{symbol}` is not an instrument's user name (letters, digits and dots, like EURUSD, XAUUSD or \
             US30.CFD), the name the price lane finds its Offers row by, so it is not priced",
        NotListed { symbol: String } => "fxcm.price.not_listed":
            "fxcm: `{symbol}` is not in the venue's Offers table, so it has no price",
        UnknownType { name: String, instrument_type: i32 } => "fxcm.price.unknown_type":
            "fxcm: `{name}` is an instrument of type {instrument_type}, which is none of the nine the SDK names: \
             the price lane does not price it",
        NotSubscribed { name: String, subscription: String } => "fxcm.price.not_subscribed":
            "fxcm: `{name}` is not subscribed for trading on this login (subscription `{subscription}`), so it \
             has no price",
        /// The row is no live quote (or names several rows), in the facts' own words.
        Row { why: FxcmSymbolRefusal } => "fxcm.price.row_refused": "{why}",
        Frozen { symbol: String, bid: f64, ask: f64, seconds: u64 } => "fxcm.price.frozen":
            "fxcm: `{symbol}` has shown the same bid {bid} and ask {ask} for {seconds} s while its market reads \
             open: a frozen quote (the row's own time may be a clock that kept moving), so it has no price until \
             either side moves",
        NoSession => "fxcm.price.no_session":
            "fxcm: no reconcile session is running for the default account, so the venue's quotes cannot be read \
             (the price lane asks that session and never opens a login of its own: FXCM sessions are scarce); \
             reconciliation is off, its login failed, or the box's only FXCM account is labelled",
        /// The desk's source has no session to ask, in its own words.
        SourceNotReady { why: String } => "fxcm.price.source_not_ready": "{why}",
        NotCurrent => "fxcm.price.not_current":
            "fxcm: this price stream was replaced by a newer one that still holds the desk (or ended \
             after a fault in its consumer, see the log), so it prices nothing",
        AtCap { cap: usize, symbol: String } => "fxcm.price.at_cap":
            "fxcm: the price lane already prices {cap} pairs (the most it reads at once); `{symbol}` is not \
             priced until one is let go",
        PollerNotStarted { error: String } => "fxcm.price.poller_not_started":
            "fxcm: the price poller could not start ({error})",
    }
}

vike_utility::refusal_enum! {
    /// The candle archive's answer to a history fetch (`crate::history`): a REFUSAL before anything is sent
    /// ([`Self::is_refused`]), else a fetch that was made and failed.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FxcmHistoryRefusal with ids FxcmHistoryRefusalId {
        IntervalNotHeld { interval: String } => "fxcm.history.interval_not_held":
            "fxcm candle archive: {interval:?} is not an interval it holds (only 1m, 1h and 1d: \
             `m1` and `H1` weekly files, `D1` yearly ones)",
        NotAPairName { shown: String } => "fxcm.history.not_a_pair_name":
            "fxcm candle archive: {shown:?} is not a pair name the archive could hold (six upper-case \
             letters, like EURUSD; the store symbol is the archive's folder name)",
        BeforeArchive { start: String } => "fxcm.history.before_archive":
            "fxcm candle archive: the window starts at {start}, before the archive does: its earliest \
             sample is the week of Monday 2012-01-02 (`2012/1`, first row 2012-01-02 03:00 UTC) and \
             nothing was found for 2011. Start the window on or after 2012-01-01 \
             (`--from 2012-01-01`)",
        AfterLastYear { last_year: i64, held_until: i64, to: String } => "fxcm.history.after_last_year":
            "fxcm candle archive: the window reaches {last_year}, and FXCM's yearly 1d files \
             stop at {held_until}: it holds none for a later year.\nFrom the command line: \
             end the window on or before {to} (`--to {to}`), with a `--from` before it; \
             `--days N` windows always end now and always land here.",
        TooManyWeeks {
            start_ms: i64, end_ms: i64, weeks: i64, interval: String, max: i64, span: &'static str
        } => "fxcm.history.too_many_weeks":
            "fxcm candle archive: [{start_ms}, {end_ms}) needs {weeks} weekly {interval} files \
             and one fetch holds at most {max} (about {span}): the call cannot be cancelled once \
             started, so it holds a gatewayhub worker and its bars until the last file is in. \
             Ask for a shorter window, and keep the pieces from overlapping, because a \
             different window over the same days stores them again",
        UnpublishedWeek { end: String, monday: String, hours: i64, minutes: i64, saturday: String } =>
            "fxcm.history.unpublished_week":
            "fxcm candle archive: the window ends at {end}, in or after the trading week of Monday {monday}, \
             which FXCM has not published yet: it publishes a week on the Monday after it ends, and \
             this source counts it as there from {hours:02}:{minutes:02} UTC that Monday.\nFrom the \
             command line: end the window on or before the Saturday before \
             it, {saturday} (`--to {saturday}`); `--days N` windows always end now and always land here.",
        Overlap { what: String, next: i64, previous: i64 } => "fxcm.history.overlap":
            "fxcm candle archive {what}: its first row ({next}) is not after the previous file's last \
             ({previous}); two files overlap, and storing both would duplicate bars",
        /// One file asked could not be used, and why.
        File { url: String, why: FxcmArchiveFileRefusal } => "fxcm.history.file":
            "fxcm candle archive {url}: {why}",
        /// Weeks or years of the window with no usable file (`count` of them, `listed`, with how many more were not
        /// spelled out), the walk's stop (`not_asked`, empty when it reached the end) and the pieces the archive holds.
        Holes { count: usize, listed: String, not_asked: String, pieces: String } =>
            "fxcm.history.holes":
            "fxcm candle archive: {count} NOT AVAILABLE: {listed}{not_asked}. That is not proof that \
             there were no candles: the archive has holes and truncated weeks, and publishes a week only after \
             it ends. Nothing was stored for this window. The pieces of it the archive holds, as \
             `vike-cli data history fetch` arguments (UTC; keep them from overlapping, because a \
             different window over the same days stores them again): {pieces}",
        /// [`Self::Holes`] where every file asked answered 404 and none was usable: `pair` may be NOT ARCHIVED.
        HolesNotArchived { count: usize, listed: String, not_asked: String, pieces: String, pair: String } =>
            "fxcm.history.holes_not_archived":
            "fxcm candle archive: {count} NOT AVAILABLE: {listed}{not_asked}. That is not proof that \
             there were no candles: the archive has holes and truncated weeks, and publishes a week only after \
             it ends. Nothing was stored for this window. The pieces of it the archive holds, as \
             `vike-cli data history fetch` arguments (UTC; keep them from overlapping, because a \
             different window over the same days stores them again): {pieces}. {pair}: Every file asked \
             answered 404: a symbol the archive does not hold answers exactly this for every window, so it \
             may be NOT ARCHIVED at all (FXCM's public archive holds a few major and cross pairs, the 21 its \
             README lists); a pair it holds answers so only for a window inside one of its holes",
    }
}

impl FxcmHistoryRefusal {
    /// Whether this is a refusal made before anything was sent (`SourceError::Refused`), not a fetch that failed.
    #[must_use]
    pub fn is_refused(&self) -> bool {
        matches!(
            self,
            Self::IntervalNotHeld { .. }
                | Self::NotAPairName { .. }
                | Self::BeforeArchive { .. }
                | Self::AfterLastYear { .. }
                | Self::TooManyWeeks { .. }
                | Self::UnpublishedWeek { .. }
        )
    }
}

vike_utility::refusal_enum! {
    /// Why one archive file (or the request for it) gave no usable rows: inside a [`FxcmHistoryRefusal::File`] or a
    /// hole's listing.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FxcmArchiveFileRefusal with ids FxcmArchiveFileRefusalId {
        NotGzip { error: String } => "fxcm.history.file.not_gzip": "not a readable gzip text file ({error})",
        DecompressesPast { cap: u64 } => "fxcm.history.file.decompresses_past": "decompresses past {cap} bytes",
        UnexpectedHeader { got: String, want: &'static str } => "fxcm.history.file.unexpected_header":
            "unexpected header {got:?} (want {want:?})",
        FieldCount { row: usize, fields: usize } => "fxcm.history.file.field_count":
            "row {row}: {fields} fields, want 9",
        BadTime { row: usize, stamp: String } => "fxcm.history.file.bad_time": "row {row}: bad time {stamp:?}",
        BadPrice { row: usize, raw: String } => "fxcm.history.file.bad_price": "row {row}: bad price {raw:?}",
        NotAfter { row: usize, stamp: String } => "fxcm.history.file.not_after":
            "row {row}: time {stamp:?} is not after the previous row's",
        NotNewYorkClose { row: usize, stamp: String } => "fxcm.history.file.not_new_york_close":
            "row {row}: D1 stamp {stamp:?} is not 17:00 New York (21:00 UTC in US summer \
             time, 22:00 otherwise); the file's convention changed or it predates 2007",
        PastOneWeek => "fxcm.history.file.past_one_week": "its rows run past one trading week; not a weekly file",
        TooLarge { max: u64 } => "fxcm.history.file.too_large": "more than {max} bytes",
        Status { status: u16 } => "fxcm.history.file.status": "HTTP {status}",
        /// Every attempt failed transiently; the last failure's words (`HTTP <status>` or the transport's).
        GaveUp { last: String, retries: usize } => "fxcm.history.file.gave_up": "{last} (after {retries} retries)",
        NoRowsKept => "fxcm.history.file.no_rows_kept": "no rows kept: a truncated week",
        ShortWeek { rows: usize, hours: i64, min_hours: i64 } => "fxcm.history.file.short_week":
            "{rows} rows over {hours} hours: a truncated week, a week runs at least {min_hours} hours",
        EndsEarly { end: String } => "fxcm.history.file.ends_early":
            "a week that opens on Sunday but ends {end} UTC: a truncated week, a whole week \
             reaches at least Thursday 12:00 UTC",
        TruncatedYear { bars: usize } => "fxcm.history.file.truncated_year": "{bars} bars kept: a truncated year",
    }
}

vike_utility::refusal_enum! {
    /// Why an operator's read of the account's open orders (`crate::reconcile::venue_read`) is not answered.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FxcmVenueReadRefusal with ids FxcmVenueReadRefusalId {
        NotRunning => "fxcm.venue_read.not_running":
            "fxcm: this account's reconcile session is not running in this daemon \
             (reconciliation off, or its login has not opened), and this read opens no ForexConnect login of its \
             own (FXCM sessions are scarce); read it while the account's reconcile runs",
        OrdersUnread { error: String } => "fxcm.venue_read.orders_unread":
            "fxcm: the account's orders could not be read: {error}",
        /// The Orders table could not be parsed, in the parser's words.
        OrdersUnparsed { error: String } => "fxcm.venue_read.orders_unparsed": "{error}",
        OrdersNotList => "fxcm.venue_read.orders_not_list": "fxcm's Orders table is not a list",
        CannotListOrders => "fxcm.venue_read.cannot_list_orders":
            "fxcm: this session cannot list the account's orders",
    }
}

vike_utility::refusal_enum! {
    /// Why the instrument list (`crate::catalog`) cannot be read.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FxcmCatalogRefusal with ids FxcmCatalogRefusalId {
        NoSession => "fxcm.catalog.no_session":
            "no fxcm account's reconcile session is open in this gatewayhub (no fxcm account runs, its \
             reconciliation is off, or its login has not opened yet), so FXCM's Offers table cannot be read; \
             a login of its own is never opened for a list",
    }
}

vike_utility::refusal_enum! {
    /// Why the reconcile session's port answered nothing (`crate::reconcile`'s `Port`): the session's OWN answer, a
    /// failed read included, is not this.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FxcmSessionError with ids FxcmSessionErrorId {
        ClientDropping => "fxcm.session.client_dropping": "fxcm reconcile client is being dropped",
        ThreadGone => "fxcm.session.thread_gone": "fxcm reconcile session thread is gone",
        ReplyDropped => "fxcm.session.reply_dropped": "fxcm reconcile session dropped the reply",
        Unanswered { seconds: u64 } => "fxcm.session.unanswered":
            "fxcm reconcile session did not answer within {seconds} s (it is busy or stuck in a call into \
             the venue's library); nothing is assumed about the answer",
    }
}
