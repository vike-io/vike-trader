//! dukascopy's refusals, declared once (`vike_utility::refusal_enum!`, decision 0301): each has its id and its ONE
//! text, and code and tests decide on the variant, never on the words. Every id is published in the workspace's
//! `refusals.json` (`crates/vike-backend/tests/refusal_catalog.rs`). The account-resolution refusal is
//! `crate::account::pick`'s `DukascopyRefusal`, declared where it is resolved.
//!
//! A refusal leaves this crate as its text where the shared seam carries a `String`
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesError`, `vike_model::QuoteRateError`,
//! `vike_bridge_core::ticks::TicksError`, the reconcile client, `venue_read`, a `ProcessExclusive` sentence): its
//! `Display`, byte for byte the words it had before it was typed.

vike_utility::refusal_enum! {
    /// Why an account does NOT RUN, in the words the start logs and the gatewayhub's status reads.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DukascopyStartRefusal with ids DukascopyStartRefusalId {
        /// Another account of this process holds the one JForex sidecar (`ProcessExclusive::held_by_another`).
        SidecarHeldByAnother { label: String, holder: String } => "dukascopy.start.sidecar_held_by_another":
            "dukascopy account `{label}` is NOT RUNNING: this process runs ONE JForex sidecar and `{holder}` holds it. \
             Two sidecars share one JForex platform cache (the Java bridge sets no cache directory) and a corrupted \
             cache makes EVERY login fail until the directory is deleted, so exactly one dukascopy account is armed \
             per process. WHICH one is the account table's to choose: an ACTIVE labelled dukascopy row above `paper` \
             takes it, and with no such row the DEFAULT account keeps it. To trade both at once, give the second \
             account its own project folder (its own VIKE_SETTINGS_DIR and credential store) and run a second \
             process there; `crates/vike-gatewayhub/src/trade/adapters/exclusive.rs`'s `pick_holder` carries the rule \
             and `crates/bridges/dukascopy/src/adapter.rs`'s module doc names the measurement that retires it",
        /// The process-level backstop: a second account asks for a sidecar (`ProcessExclusive::already_claimed`).
        SidecarAlreadyClaimed { label: String } => "dukascopy.start.sidecar_already_claimed":
            "dukascopy account `{label}` is NOT RUNNING: this process already runs a JForex sidecar and a SECOND one \
             is refused. Two sidecars share one JForex platform cache (the Java bridge sets no cache directory), and a \
             corrupted cache makes EVERY login fail until the directory is deleted — so admitting the second account \
             could take the FIRST one off the venue. Run the second account in its own project folder (its own \
             VIKE_SETTINGS_DIR and credential store) until per-sidecar cache directories are proven; \
             `crates/bridges/dukascopy/src/adapter.rs`'s module doc names the measurement that retires this",
        /// The JNLP address row is no JNLP: the sidecar would hang on it, so nothing is started.
        JnlpNotAJnlp { url: String } => "dukascopy.start.jnlp_not_a_jnlp":
            "dukascopy: the demo login rest jnlp address row `{url}` is not a JNLP URL (it does not end in `.jnlp`); \
             the sidecar would hang on it, so nothing is started. Fix it with `vike-cli config url unset dukascopy \
             demo login rest --product jnlp` or `config url set`",
        /// The account's sidecar is down; `why` is what it said when it ended (`crate::feed::start_failure`).
        SidecarWouldNotStart { why: String } => "dukascopy.start.sidecar_would_not_start":
            "the JForex sidecar would not start: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why an instrument the gatewayhub asks this venue's facts of is not admitted, or its facts cannot be had now
    /// (`crate::instrument::any_symbol`, `crate::ledger`).
    #[derive(Debug, Clone, PartialEq)]
    pub enum DukascopyPropertiesRefusal with ids DukascopyPropertiesRefusalId {
        /// The account's list carries no row of the wire name; `count` is the list's size.
        NotOnList { wire: String, count: usize } => "dukascopy.properties.not_on_list":
            "dukascopy: `{wire}` is not on the account's instrument list ({count} instruments, as the JForex sidecar \
             stated them at its login), so there is no statement of its price grid and quote currency; it is not an \
             instrument this account can trade",
        /// A coin, which the SDK types `kind`; `group` is the venue's grouping of it.
        Coin { pair: String, kind: &'static str, group: String } => "dukascopy.properties.coin":
            "dukascopy: `{pair}` is a coin typed {kind} in the SDK (the venue lists it in {group}), and the SDK's FOREX \
             amount defaults are not a statement about a coin's unit",
        /// Quoted in a minor unit (pence) no conversion pair carries; `unit_words` names it.
        MinorUnit { pair: String, unit_words: String } => "dukascopy.properties.minor_unit":
            "dukascopy: `{pair}` is quoted in {unit_words} and no conversion pair carries it, so a position in it \
             would be booked a hundred times off; it is not admitted",
        /// A metal or a CFD (`class`) asked of an answer that has no sidecar to read the venue's terms from.
        NeedsVenueTerms { pair: String, class: &'static str } => "dukascopy.properties.needs_venue_terms":
            "dukascopy: `{pair}` is a {class}: its amounts and its price grid are the venue's own, which the JForex \
             sidecar states once the instrument is subscribed, and this answer has no sidecar to read them from; it is \
             admitted only for a `demo` account with an account-wide exposure ceiling armed (the converting lane) on a \
             sidecar jar that states the venue's terms",
        /// The row's tick scale gives no price step.
        NoPriceStep { pair: String } => "dukascopy.properties.no_price_step":
            "dukascopy: `{pair}`: the list's tick scale gives no price step",
        /// A currency pair quoted in another money than the ledger's, on a node that converts nothing.
        QuoteNotLedger { pair: String, quote: String, ledger: String } => "dukascopy.properties.quote_not_ledger":
            "dukascopy: `{pair}` is quoted in {quote} and this account's money is {ledger} (the ledger is one currency \
             and nothing converts); only {ledger}-quoted instruments are admitted",
        /// The venue's terms state another class than the account's list row.
        TermsClass { name: String, stated: String, class: &'static str } => "dukascopy.properties.terms_class":
            "dukascopy: `{name}`: the sidecar states it as a `{stated}` and the account's list has it as a `{class}`; \
             it is not admitted until the two agree",
        /// A figure of the venue's terms (`what`) is missing or not a positive number.
        TermsNotUsable { name: String, what: &'static str, figure: Option<f64> } =>
            "dukascopy.properties.terms_not_usable":
            "dukascopy: `{name}`: the venue's terms state no usable {what} ({figure:?}), so it is not admitted on them",
        /// More than one unit of the underlying per contract.
        PerContract { name: String, per_contract: f64 } => "dukascopy.properties.per_contract":
            "dukascopy: `{name}`: the venue states {per_contract} units of the underlying per contract; what one \
             contract is worth per price point is unmeasured (no demo fill has shown it), so only an instrument stated \
             at one per contract is admitted",
        /// The terms state no pip scale or tick scale.
        TermsNoScale { name: String } => "dukascopy.properties.terms_no_scale":
            "dukascopy: `{name}`: the venue's terms state no pip scale or tick scale, so its price grid is not known",
        /// A tick grid that is not the SDK's (tick scale = pip scale + 1).
        TermsGrid { name: String, pip_scale: u32, tick_scale: u32, pip: f64 } => "dukascopy.properties.terms_grid":
            "dukascopy: `{name}`: the venue states pip scale {pip_scale} and tick scale {tick_scale} (pip {pip}); the \
             SDK's tick is a tenth of a pip, so this is not a grid the engine can round to",
        /// The minimum is not a whole number of increments.
        MinNotWhole { name: String, min: f64, step: f64 } => "dukascopy.properties.min_not_whole":
            "dukascopy: `{name}`: the minimum {min} is not a whole number of the increment {step}, so the venue's \
             sizes ({min}, {min} + {step}, ...) are not multiples of a step the engine can round to",
        /// The largest order is below the smallest.
        MaxBelowMin { name: String, max: f64, min: f64 } => "dukascopy.properties.max_below_min":
            "dukascopy: `{name}`: the venue's largest order {max} is below its smallest {min}",
        /// The terms state another quote currency than the list row.
        TermsQuote { name: String, stated: String, quote: String } => "dukascopy.properties.terms_quote":
            "dukascopy: `{name}`: the sidecar states the quote currency {stated} and its list row has {quote}; it is \
             not admitted until the two agree",
        /// Only a `demo` account is answered.
        TierNotDemo => "dukascopy.properties.tier_not_demo":
            "dukascopy: any-symbol admission is wired for a `demo` account only (the JForex sidecar logs into a demo \
             server only), so no instrument is admitted for any other tier",
        /// The stated money is not the mount's, and the mount is no pair (`crate::ledger::Ledger::Elsewhere`).
        MountNotAPair { symbol: String, stated: String, mounted: String } => "dukascopy.properties.mount_not_a_pair":
            "dukascopy: this account's money is {stated} but the engine is mounted on {mounted}, which is not a pair, \
             so the money it is booked in is not known; its numbers are not {stated}, so `{symbol}` and every other \
             symbol beyond the mounted one are NOT admitted for it. Use an account mounted on a pair quoted in {stated}",
        /// The venue's terms of a metal or a CFD could not be read NOW (the price lane's words).
        TermsUnread { pair: String, why: String } => "dukascopy.properties.terms_unread":
            "the terms of `{pair}`: {why}",
        /// No sidecar is attached, so the ledger's currency is not known YET.
        LedgerNotKnownYet { symbol: String } => "dukascopy.properties.ledger_not_known_yet":
            "the JForex sidecar is not up (it is starting or restarting, or this account is not armed), so the \
             ledger's currency is not known yet and `{symbol}` cannot be placed against it",
        /// No rate of the quote currency in the ledger's could be had NOW (the rate's words).
        NoRate { quote: String, ledger: String, why: String } => "dukascopy.properties.no_rate":
            "no rate for {quote} in {ledger} ({why})",
    }
}

vike_utility::refusal_enum! {
    /// Why the rate that converts a quote currency into the account's has none (`crate::account::quote_rate`).
    #[derive(Debug, Clone, PartialEq)]
    pub enum DukascopyRateRefusal with ids DukascopyRateRefusalId {
        /// Not both currency codes.
        NotCurrencyCodes { quote_ccy: String, account_ccy: String } => "dukascopy.rate.not_currency_codes":
            "dukascopy: `{quote_ccy}` / `{account_ccy}` are not both currency codes, so no pair converts them",
        /// One currency: nothing to convert.
        SameCurrency { quote: String } => "dukascopy.rate.same_currency":
            "dukascopy: {quote} is the account's own currency; there is nothing to convert",
        /// The list carries the pair both ways round.
        BothOrientations { from: String, to: String } => "dukascopy.rate.both_orientations":
            "dukascopy: the account's instrument list carries both {from}/{to} and {to}/{from}, so which one the rate \
             is read from would be a guess",
        /// No pair, nor two through USD, converts the two.
        NoPair { quote: String, account: String } => "dukascopy.rate.no_pair":
            "dukascopy: the account's instrument list carries neither {quote}/{account} nor {account}/{quote}, nor a \
             pair of each with USD, so no pair converts {quote} into the account's {account}",
        /// A tick of another symbol than the leg's pair.
        WrongTick { tick: String, pair: String } => "dukascopy.rate.wrong_tick":
            "dukascopy: a tick of {tick} cannot price the conversion pair {pair}",
        /// The tick's mid is not a finite positive number.
        UnusableTick { pair: String, bid: f64, ask: f64 } => "dukascopy.rate.unusable_tick":
            "dukascopy: the tick of {pair} (bid {bid}, ask {ask}) states no usable rate",
        /// The tick is stamped further ahead of this box's clock than a rate may be.
        TickAhead { pair: String, ahead_secs: f64, max_secs: i64 } => "dukascopy.rate.tick_ahead":
            "dukascopy: the tick of {pair} is stamped {ahead_secs:.1} s AHEAD of this box's clock (a skewed or \
             drifting clock); a rate may be stamped at most {max_secs} s ahead of it, so this tick is not used as a rate",
        /// One leg of a cross through USD has no rate; `why` is the leg's.
        LegHasNoRate { from: String, to: String, leg_from: String, leg_to: String, why: String } =>
            "dukascopy.rate.leg_has_no_rate":
            "dukascopy: {from} is converted into {to} through USD, and the {leg_from} to {leg_to} leg has no rate: {why}",
        /// The product of the two legs is not a finite positive number.
        ComposedNotFinite { from: String, to: String } => "dukascopy.rate.composed_not_finite":
            "dukascopy: the rate of {from} in {to} composed through USD is not a finite positive number",
        /// The quote currency is no code.
        NotACurrencyCode { quote_ccy: String } => "dukascopy.rate.not_a_currency_code":
            "dukascopy: `{quote_ccy}` is not a currency code, so no pair converts it",
    }
}

vike_utility::refusal_enum! {
    /// Why the price lane (`crate::feed::QuoteBus`) prices nothing for a symbol, or reads no terms of it: the data
    /// lane's subscribe refusal, and the "not now" of a rate or a facts lookup. `purpose` ends "`SYMBOL` cannot be ..."
    /// (`priced`, `priced for a conversion`, `subscribed to read its trade terms`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DukascopyPriceLaneRefusal with ids DukascopyPriceLaneRefusalId {
        /// This stream was replaced by a newer one (or ended after a fault in its consumer).
        NotCurrent => "dukascopy.price_lane.not_current":
            "dukascopy: this price stream was replaced by a newer one that still holds the lane (or ended after a \
             fault in its consumer, see the log), so it prices nothing",
        /// No sidecar runs on this node.
        NoSidecar { symbol: String, purpose: &'static str } => "dukascopy.price_lane.no_sidecar":
            "dukascopy: no JForex sidecar is running on this node, so `{symbol}` cannot be {purpose} (the account is \
             not armed at `demo`, its sidecar did not start, or it is not up yet); the price lane never starts one",
        /// The jar predates the price lane (its `ready` announces no `ticks`: `crate::feed::FEATURE_TICKS`).
        JarPredatesTicks { symbol: String, purpose: &'static str } => "dukascopy.price_lane.jar_predates_ticks":
            "dukascopy: the installed JForex sidecar jar predates the price lane (its ready line announces no \
             `ticks`), so `{symbol}` cannot be {purpose}; install the jar from this release (`just fetch-tools`) and \
             restart",
        /// The jar predates the venue's terms (its `ready` announces no `terms`: `crate::feed::FEATURE_TERMS`).
        JarPredatesTerms { symbol: String } => "dukascopy.price_lane.jar_predates_terms":
            "dukascopy: the installed JForex sidecar jar predates the venue's terms (its ready line announces no \
             `terms`), so the terms of `{symbol}` cannot be read; install the jar from this release (`just \
             fetch-tools`) and restart",
        /// Every one of the sidecar's `max` places is taken.
        AtMostPriced { max: usize, symbol: String } => "dukascopy.price_lane.at_most_priced":
            "dukascopy: at most {max} instruments are priced at once (the JForex subscription limit is unmeasured); \
             `{symbol}` was not subscribed",
        /// Every one of the rate reader's `max` pairs has a lookup waiting on its tick.
        RatedPairsBusy { max: usize, symbol: String } => "dukascopy.price_lane.rated_pairs_busy":
            "dukascopy: {max} conversion pairs are being priced right now, each with a lookup waiting on its tick; \
             `{symbol}` was not subscribed (ask again)",
        /// The sidecar's command pipe is closed.
        SidecarGone { symbol: String } => "dukascopy.price_lane.sidecar_gone":
            "dukascopy: the JForex sidecar is gone (its command pipe is closed), so `{symbol}` was not subscribed",
        /// The sidecar declined to price the symbol, in its words.
        DeclinedToPrice { symbol: String, reason: String } => "dukascopy.price_lane.declined_to_price":
            "dukascopy: the JForex sidecar declined to price `{symbol}`: {reason}",
        /// The sidecar declined to subscribe the symbol whose terms were asked, in its words.
        DeclinedToSubscribe { symbol: String, reason: String } => "dukascopy.price_lane.declined_to_subscribe":
            "dukascopy: the JForex sidecar declined to subscribe `{symbol}`: {reason}",
        /// The sidecar ended while the symbol was being priced.
        WentAwayPricing { symbol: String } => "dukascopy.price_lane.went_away_pricing":
            "dukascopy: the JForex sidecar went away while `{symbol}` was being priced",
        /// The sidecar ended while the symbol's terms were being read.
        WentAwayReadingTerms { symbol: String } => "dukascopy.price_lane.went_away_reading_terms":
            "dukascopy: the JForex sidecar went away while the terms of `{symbol}` were being read",
        /// No tick arrived within `millis` of asking for it.
        NoTickInTime { symbol: String, millis: u128 } => "dukascopy.price_lane.no_tick_in_time":
            "dukascopy: no tick of `{symbol}` arrived within {millis} ms of asking the sidecar for it (a closed \
             market, or its price feed is not live yet)",
        /// No `terms` line arrived within `millis` of asking for it.
        NoTermsInTime { symbol: String, millis: u128 } => "dukascopy.price_lane.no_terms_in_time":
            "dukascopy: the sidecar stated no terms of `{symbol}` within {millis} ms of asking it to subscribe it (a \
             closed market: the SDK states an instrument's terms at its first tick; or an instrument this account \
             cannot trade; or its price feed is not live yet)",
        /// The stream's key names no kept row; `why` is the name translator's words.
        NotPriced { why: String } => "dukascopy.price_lane.not_priced": "{why}; it is not priced",
    }
}

vike_utility::refusal_enum! {
    /// Why a `.bi5` history fetch has no answer (`crate::history`, `crate::ticks`, the point value of
    /// `crate::instrument::grid::history_point_value`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DukascopyHistoryRefusal with ids DukascopyHistoryRefusalId {
        /// A 429, or a connect that did not complete (the ban that follows a 429).
        Throttled => "dukascopy.history.throttled":
            "Dukascopy's datafeed throttled this box (429); try again in ~20 min or use the S3 archive",
        /// The ban this process saw at `at` (`HH:MM` UTC) still stands.
        ThrottledSince { at: String } => "dukascopy.history.throttled_since":
            "Dukascopy's datafeed throttled this box (429); try again in ~20 min or use the S3 archive (banned at {at} \
             UTC)",
        /// An hour still failed after every retry; `failure` is the last try's answer.
        Overloaded { tries: u32, failure: String } => "dukascopy.history.overloaded":
            "Dukascopy's datafeed is overloaded: the last of {tries} tries answered {failure}",
        /// Any other HTTP status, in the feed's number.
        Status { status: u16 } => "dukascopy.history.status": "dukascopy {status}",
        /// An hour file that does not decompress; `why` is the format's words (`vike_bi5::decompress`).
        Undecodable { why: String } => "dukascopy.history.undecodable": "{why}",
        /// The name is not on the account's list with a price step; `count` is the list's size.
        NotOnList { name: String, count: usize } => "dukascopy.history.not_on_list":
            "dukascopy: `{name}` is not on the account's instrument list with a price step ({count} instruments, as \
             the JForex sidecar stated them), so the point value of its .bi5 prices is not known and its history is \
             not decoded",
        /// The symbol is no plain code (`bytes` were sent); the gatewayhub's words, which reach its peer.
        SymbolNotPlain { max: usize, bytes: usize } => "dukascopy.history.symbol_not_plain":
            "gatewayhub: a tick symbol is 1 to {max} ASCII letters, digits, `_` or `-`; {bytes} bytes were sent; \
             nothing was asked of the venue",
        /// The window is wider than one fetch may name.
        SpanTooWide { hours: i128, max_hours: i64 } => "dukascopy.history.span_too_wide":
            "gatewayhub: a tick fetch spans {hours} hours and one request may name at most {max_hours} (one hour file \
             is one CDN request); ask in chunks; nothing was asked of the venue",
    }
}

vike_utility::refusal_enum! {
    /// Why the venue's orders and positions were NOT read (`crate::reconcile::query`, the reconcile client and the
    /// operator's `venue_read`). Never an empty answer: an empty one reads as a flat venue.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DukascopyReconcileRefusal with ids DukascopyReconcileRefusalId {
        /// The jar does not announce `recon`.
        JarPredatesRecon => "dukascopy.reconcile.jar_predates_recon":
            "dukascopy: the installed JForex sidecar jar predates the reconcile query (its `ready` line does not \
             announce `recon`), so the venue's orders and positions cannot be read; install the jar of this release \
             (`just fetch-tools`) and restart",
        /// The write to the sidecar failed.
        NotSent => "dukascopy.reconcile.not_sent":
            "dukascopy: the reconcile query was NOT sent: the write to the JForex sidecar failed (it has exited and \
             the node is restarting it)",
        /// No answer within `wait`.
        NoAnswer { wait: std::time::Duration } => "dukascopy.reconcile.no_answer":
            "dukascopy: the JForex sidecar did not answer the reconcile query within {wait:?}; the venue's orders and \
             positions were NOT read",
        /// The question was dropped before it was answered.
        Dropped => "dukascopy.reconcile.dropped":
            "dukascopy: the reconcile query was dropped before it was answered",
        /// The sidecar ended with the question open; `why` says how.
        SidecarEnded { why: String } => "dukascopy.reconcile.sidecar_ended":
            "dukascopy: {why}; the venue's orders and positions were NOT read",
        /// The sidecar answered `error`, in its words.
        VenueUnread { error: String } => "dukascopy.reconcile.venue_unread":
            "dukascopy: the JForex sidecar could not read the venue: {error}",
        /// The answer carried neither its lists nor an error.
        AnswerMalformed => "dukascopy.reconcile.answer_malformed":
            "dukascopy: the JForex sidecar's reconcile answer carried neither its lists nor an error, so it is not \
             read as an empty venue",
        /// No sidecar runs (the supervisor is starting another); `words` are its down state's.
        SidecarDown { words: String } => "dukascopy.reconcile.sidecar_down":
            "dukascopy: bridge unavailable: {words}; the venue's orders and positions were NOT read",
        /// A symbol the node holds names no kept row; `why` is the name translator's words.
        SymbolUnnamed { why: String, symbol: String } => "dukascopy.reconcile.symbol_unnamed":
            "dukascopy recon: {why}, so the orders, fills and position of {symbol} cannot be read",
        /// The operator's read of an account whose exec client is not running in this daemon.
        NotRunningHere => "dukascopy.reconcile.not_running_here":
            "dukascopy: this account's JForex sidecar is not running in this daemon, and this read starts none of its \
             own (a process holds one sidecar); read it while the account runs",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The feature words the price-lane refusals spell are the jar's (`crate::feed`'s constants).
    #[test]
    fn the_jar_words_the_refusals_spell_are_the_features() {
        let symbol = String::new();
        let ticks =
            DukascopyPriceLaneRefusal::JarPredatesTicks { symbol: symbol.clone(), purpose: "" };
        assert!(ticks.to_string().contains(&format!("`{}`", crate::feed::FEATURE_TICKS)));
        let terms = DukascopyPriceLaneRefusal::JarPredatesTerms { symbol };
        assert!(terms.to_string().contains(&format!("`{}`", crate::feed::FEATURE_TERMS)));
    }
}
