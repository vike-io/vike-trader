//! **What a bridge says about a symbol it is asked to admit, below the gatewayhub that serves it** (decision 0173, decision
//! 0183; owner ruling 2026-11-12: one-venue code lives in its bridge, and the shared crate holds the one type).
//!
//! A bridge says WHAT kind of "no" it gives ([`SymbolPropertiesError`]): a verdict on the symbol or an instrument that
//! is gone, each in its whole sentence, or one of the two "not now" kinds in its own detail. The gatewayhub gives each
//! kind its wire code (and words the two transient ones `<venue>: <marker>: <detail>`), so the node remembers a verdict,
//! gives a gone instrument back and asks again after a not-now, and no bridge names the wire.
//!
//! [`SymbolPropertiesAccountSpec`] is the venue's half of the facts of a venue whose instrument list and currency are one
//! ACCOUNT's, read with its own credentials (ctrader, oanda): the gatewayhub keeps the listing, its bounds and the rate
//! read (the engine); the bridge says which row it admits, for which node, and what a rate is asked for (the spec).
//! [`SymbolPropertiesKeyedSpec`] is the same split for a venue looked up one key at a time (ig, ibkr), with
//! [`SymbolPropertiesKeyNotes`] for what a venue keeps beside a key's answer, and [`SymbolPropertiesAnswer`] the shape
//! a bridge writes its answer into. [`SymbolPropertiesListSpec`] is the same split for a venue answered from an
//! instrument LIST (a public catalog, or the asking account's own list): the gatewayhub keeps the listing, its bounds
//! and the checks every venue shares; the bridge says how its symbols are spelled and read, and what a row is.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_model::{ContractValue, Instrument, MarginMode, QuoteRate, SymbolProperties};
use vike_utility::errors::CarriedRefusal;

/// **The words of the gatewayhub's wire that a bridge's sentence carries**, handed in per ask so no bridge names the
/// wire: the person reads them; the kind of "no" is the [`SymbolPropertiesError`] variant the sentence travels in.
#[derive(Debug, Clone, Copy)]
pub struct SymbolPropertiesWords {
    /// The answer to a kind of symbol the venue's lane does not take.
    pub not_answered: &'static str,
    /// What stands between the venue and the symbol of "the venue lists no such instrument" (`<venue><this><symbol>`).
    pub no_instrument: &'static str,
    /// The words of a contract whose delivery has passed.
    pub expired: &'static str,
}

/// **What a venue's spec says a listed row is**, before the gatewayhub's shared checks: the grid it answers on
/// (`None` = the row's own), what one unit of quantity stands for (`None` = linear 1.0), the margin mode and rate its
/// positions open in where the venue states them, and whether the spec judged the row WHOLE.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SymbolPropertiesListAnswer {
    /// The grid answered, `None` = the row's own.
    pub properties: Option<SymbolProperties>,
    /// What one unit of quantity stands for, `None` = linear 1.0.
    pub contract_value: Option<ContractValue>,
    /// The margin mode the symbol's positions open in, where the venue states one.
    pub margin_mode: Option<MarginMode>,
    /// The symbol's own margin rate, where the venue states one.
    pub margin_rate: Option<f64>,
    /// `true` when the spec judged every class rule of the row itself (bybit's rows, binance's COIN-M rows): the
    /// gatewayhub then skips the `.P` lane's linear rule and the fully-funded default and runs only the grid checks.
    pub whole: bool,
}

/// **The venue's half of a LIST-answered venue's facts** (the public catalogs of binance, aster, bybit, okx, hyperliquid,
/// deribit and polymarket; alpaca's asset read with the account's key; ctrader's and oanda's account lists). The
/// gatewayhub's `SymbolFacts` keeps every listing (ten minutes, single-flight, a failure remembered), rate-bounds every
/// one-symbol read and runs the checks every venue shares; it finds a venue's spec by [`Self::venue`] and asks it, in
/// this order:
///
/// 1. [`Self::covers`]: whether this source answers the venue at all, and what its list covers (the not-found words);
/// 2. [`Self::symbol_refusal`]: a spelling refused before any read;
/// 3. [`Self::case_kept`], then [`Self::key`]: the catalog key the row is listed under;
/// 4. [`Self::listed_elsewhere`], then [`Self::answered_elsewhere`]: a symbol answered from ANOTHER listing of the venue,
///    whole (the gatewayhub reads that listing, so its failure never passes through the spec);
/// 5. [`Self::one_at_a_time`] (with a one-symbol lister), [`Self::wanted`] and [`Self::lister_key`]: how the row is read;
/// 6. [`Self::row_fallback`], then [`Self::not_found`]: the row, or the venue's own not-found words;
/// 7. [`Self::row_answer`]: what the row is, or why it is not admitted; then the shared `.P` linear rule (unless the
///    answer is whole), the fully-funded default, the tick and step checks, and last [`Self::grid_refusal`].
///
/// Object-safe: the gatewayhub keeps one table of `&dyn` specs.
pub trait SymbolPropertiesListSpec: Send + Sync {
    /// The venue's slug, as the node names it.
    fn venue(&self) -> &'static str;

    /// The key of the listing an account on `network` (`mainnet` or `testnet`) is answered from: the venue's slug,
    /// or a key of its own where the network's list differs (it keeps its own slot, failures included).
    fn list_key(&self, _network: &str) -> &'static str {
        self.venue()
    }

    /// What the listing under `list_key` covers, in the not-found sentence; `None` = this source does not answer the
    /// venue (`account_listed`: whether the source is one account's own listing).
    fn covers(&self, list_key: &str, account_listed: bool) -> Option<&'static str>;

    /// Why `symbol` (as the node spelled it) is refused before anything is read; `None` reads on.
    fn symbol_refusal(
        &self,
        _symbol: &str,
        _words: &SymbolPropertiesWords,
    ) -> Option<CarriedRefusal> {
        None
    }

    /// Whether `symbol`'s case is part of its name (else it is upper-cased).
    fn case_kept(&self, _symbol: &str) -> bool {
        false
    }

    /// The key `symbol` (its case settled) is listed under.
    fn key(&self, symbol: &str) -> String {
        symbol.to_string()
    }

    /// **The key of ANOTHER listing of the venue that answers `key`** (binance's COIN-M book), for an account answered
    /// from `list_key`; `None` = the venue's own list answers. The gatewayhub reads that listing through its cache and
    /// hands the row to [`Self::answered_elsewhere`].
    fn listed_elsewhere(&self, _key: &str, _list_key: &str) -> Option<&'static str> {
        None
    }

    /// **The answer of a symbol listed ELSEWHERE** ([`Self::listed_elsewhere`]): `row` is its row in that listing,
    /// `None` when the listing has none. The answer is judged whole; the grid checks still run. The default refuses
    /// (a spec that names another listing judges its rows).
    ///
    /// # Errors
    /// The refusal, of its kind.
    fn answered_elsewhere(
        &self,
        _key: &str,
        _list_key: &str,
        _row: Option<&Instrument>,
        _now_ms: i64,
        words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, SymbolPropertiesError> {
        Err(SymbolPropertiesError::Refused(words.not_answered.into()))
    }

    /// Whether the row of `key` is read ONE at a time (when the source has a one-symbol lister) instead of out of the
    /// whole list.
    fn one_at_a_time(&self, _key: &str) -> bool {
        false
    }

    /// The name the one-symbol lister is asked for.
    fn wanted(&self, _symbol: &str, key: &str) -> String {
        key.to_string()
    }

    /// The listing key the one-symbol lister is told (the venue's slug, or the network's `list_key`).
    fn lister_key<'k>(&self, _list_key: &'k str) -> &'k str {
        self.venue()
    }

    /// A second key the row may be listed under when `key` is not.
    fn row_fallback<'k>(&self, _key: &'k str) -> Option<&'k str> {
        None
    }

    /// The venue's own words for a symbol its one-symbol read did not find; `None` = the shared ones.
    fn not_found(&self, _symbol: &str) -> Option<CarriedRefusal> {
        None
    }

    /// **What `row` (found for `symbol` under `key`) is to the engine admitted it**, or why it is not admitted, in the
    /// venue's order of checks; at `now_ms`.
    ///
    /// # Errors
    /// The refusal in the operator's words, of its kind (a delivered contract is [`SymbolPropertiesError::Expired`]).
    fn row_answer(
        &self,
        _symbol: &str,
        _key: &str,
        _row: &Instrument,
        _now_ms: i64,
        _words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, SymbolPropertiesError> {
        Ok(SymbolPropertiesListAnswer::default())
    }

    /// Why the adapter cannot write an order on `properties` (the grid answered, after the shared grid checks); `None`
    /// admits it.
    fn grid_refusal(
        &self,
        _symbol: &str,
        _properties: &SymbolProperties,
    ) -> Option<CarriedRefusal> {
        None
    }
}

/// **The venue's half of an account-listed venue's facts** (ctrader's account symbol list, oanda's account instruments).
/// The gatewayhub's per-account facts run it in this order: the row is found in the account's listing, the refusal of
/// the asking node ([`Self::plain_refusal`] or [`Self::quoted_refusal`]) judges it with the account's currency BEFORE
/// any grid check (so a refusal names the fundamental reason, not a grid detail), the generic grid checks follow, then
/// [`Self::grid_refusal`] on the grid the answer carries, and a converting node's rate is asked for
/// [`Self::rate_subject`] in the account's currency.
pub trait SymbolPropertiesAccountSpec: Send + Sync + 'static {
    /// The venue's slug, the first word of the gatewayhub's own refusals.
    const VENUE: &'static str;

    /// Whether one read of the account's listing is a whole authenticated session (ctrader: application auth and
    /// account auth), so a failure is remembered for minutes, or plain REST reads (oanda), remembered for seconds.
    const LISTING_IS_A_SESSION: bool;

    /// Why `row` (found for `symbol`, as the node spelled it) is not admitted for a node that converts NOTHING, with
    /// the account's `currency`; `None` admits it.
    fn plain_refusal(symbol: &str, row: &Instrument, currency: &str) -> Option<CarriedRefusal>;

    /// The same for a node that CONVERTS a quote currency (decision 0183): every refusal but the foreign quote.
    fn quoted_refusal(symbol: &str, row: &Instrument, currency: &str) -> Option<CarriedRefusal>;

    /// Why the adapter cannot write an order on `properties` (the grid the answer carries) for `symbol`'s `row`; `None`
    /// admits it. Runs after the generic grid checks, for either node.
    fn grid_refusal(
        symbol: &str,
        row: &Instrument,
        properties: &SymbolProperties,
    ) -> Option<CarriedRefusal>;

    /// What a converting node's rate is asked for, beside the account's currency: the first argument of the venue's
    /// rate source (oanda: the instrument; ctrader: the row's quote currency).
    fn rate_subject(symbol: &str, row: &Instrument) -> String;
}

/// Why a bridge gives no properties (or no rate) for a symbol. Each sentence is the bridge's refusal with its id kept
/// ([`CarriedRefusal`]: `refusal.carried()`, or free text `.into()`), so the gatewayhub hands the id on beside the words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolPropertiesError {
    /// A verdict on the symbol, its whole sentence: the node remembers it.
    Refused(CarriedRefusal),
    /// The venue's own list, read whole, has no instrument of that name: its whole sentence (the node gives the symbol
    /// back).
    NoInstrument(CarriedRefusal),
    /// A dated contract whose delivery has passed, on a venue whose list still shows it: its whole sentence (the node
    /// gives the symbol back).
    Expired(CarriedRefusal),
    /// The venue's list or terms of the symbol could not be read NOW: the detail the gatewayhub words as the
    /// list's failure (`<venue>: <list unavailable>: <detail>`), which the node does not remember.
    ListUnavailable(CarriedRefusal),
    /// The quote-currency rate that converts the symbol could not be had NOW: the detail the gatewayhub words as
    /// the rate's failure (`<venue>: <rate unavailable>: <detail>`), which the node does not remember.
    RateUnavailable(CarriedRefusal),
}

impl SymbolPropertiesError {
    /// Whether this is a "not now" (the node asks again) rather than a verdict.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::ListUnavailable(_) | Self::RateUnavailable(_))
    }

    /// The bridge's own words: a verdict's or a gone instrument's whole sentence, a not-now's detail.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Refused(s)
            | Self::NoInstrument(s)
            | Self::Expired(s)
            | Self::ListUnavailable(s)
            | Self::RateUnavailable(s) => s.text(),
        }
    }
}

impl fmt::Display for SymbolPropertiesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

/// **What a bridge's answer for one symbol is written into** (the gatewayhub's wire answer): the grid, what one unit
/// of quantity stands for (`None` = linear 1.0), the symbol's own margin rate, the rate of its quote currency when it
/// needs one, and the ledger currency the answer is expressed in (`None` = no statement). The bridge builds its answer
/// through this, so its mapping of the venue's own types stays in the bridge and names no wire.
pub trait SymbolPropertiesAnswer: Sized {
    /// The answer of these parts.
    fn of(
        properties: SymbolProperties,
        contract_value: Option<ContractValue>,
        margin_rate: Option<f64>,
        quote: Option<QuoteRate>,
        account_ccy: Option<String>,
    ) -> Self;

    /// This answer converted: the quote currency, its rate and the moment it was asked, in `ledger`'s money.
    #[must_use]
    fn with_rate(self, rate: QuoteRate, ledger: String) -> Self;
}

/// **How a venue's lookup of ONE KEY failed** (a venue with no list to keep: ig's epics, ibkr's names), so the gatewayhub's
/// per-key bounds know how long to remember it and how to word it. Each detail keeps its refusal's id, as
/// [`SymbolPropertiesError`]'s do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolPropertiesKeyedError {
    /// The venue answered and the key is not admitted: a verdict, kept for the failed keep time.
    Refused(CarriedRefusal),
    /// The venue could not be asked (a login, a socket, a timeout): worded as the node's "not now" and kept
    /// for the failed keep time (a login is what such a failure costs to repeat).
    Unavailable(CarriedRefusal),
    /// The same "not now" for a lookup whose answer is a quote-currency RATE: worded as [`Self::Unavailable`]
    /// but kept for the rate path's short memory only, so the node's retry after a blip finds the venue asked
    /// again and not a cached error.
    NotNow(CarriedRefusal),
    /// The key is fine but the quote-currency RATE that converts it could not be had or believed (decision
    /// 0183): worded as the node's "not now" about the rate, so nothing is remembered about the symbol itself,
    /// and kept for the rate path's memory of a rate nobody can give.
    RateUnavailable(CarriedRefusal),
}

/// **The venue's half of the gatewayhub's per-key bounds** (a venue whose facts are looked up ONE KEY AT A TIME and have no
/// list to keep, with the asking account's own credentials paying): its words, how many keys it tracks, its upstream
/// lookup budget, the share of it a rate refresh keeps, and how its source's answers become the shared vocabulary. The
/// gatewayhub's `SymbolPropertiesCacheKeyed<S>` keeps every answer, single-flights every key and spends the budget; it names no venue.
pub trait SymbolPropertiesKeyedSpec: Send + Sync + 'static {
    /// The venue's slug, the opening word of every refusal (`ig`).
    const VENUE: &'static str;
    /// What a key is called in the "table full" sentence (`epics`, `names`).
    const KEYS: &'static str;
    /// What a lookup is called in the "budget spent" sentence (`epic lookups`, `lookups`).
    const LOOKUPS: &'static str;
    /// How many keys are tracked at once.
    const MAX_KEYS: usize;
    /// Upstream lookups allowed per [`Self::WINDOW`], per account.
    const BUDGET: usize;
    /// See [`Self::BUDGET`].
    const WINDOW: Duration;
    /// How many of [`Self::BUDGET`] only the RE-READ of a key that has answered with a rate may spend (`0`: none kept).
    const REFRESH_RESERVE: usize;

    /// What the venue's source hands back for one key on the first kind of ask.
    type Fetched;
    /// ... on the second kind (ig: a node that converts; ibkr: a forex pair or a dated future).
    type FetchedOther;
    /// What is kept beside a granted first-kind answer, for as long as the answer lives ([`SymbolPropertiesKeyNotes`]).
    type Note: Clone + Send;

    /// The first kind's answer, with the note kept beside it, or how it failed.
    ///
    /// # Errors
    /// The failure in the shared vocabulary.
    fn answer<A: SymbolPropertiesAnswer>(
        fetched: Self::Fetched,
    ) -> Result<(A, Option<Self::Note>), SymbolPropertiesKeyedError>;

    /// The second kind's answer, or how it failed.
    ///
    /// # Errors
    /// The failure in the shared vocabulary.
    fn answer_other<A: SymbolPropertiesAnswer>(
        fetched: Self::FetchedOther,
    ) -> Result<A, SymbolPropertiesKeyedError>;

    /// The key the second kind is kept under, so the two kinds of one key never answer for each other where they could.
    fn other_key(key: &str) -> String;
}

/// **A note per key kept beside a keyed answer, for as long as the answer lives** (ibkr's contract price magnifier: the
/// kept answer has no room for it, and the converting path judges it AFTER the rate read says the stock is foreign). A
/// note older than `ttl` belongs to an answer that is gone and reads as `None`; noting drops the expired ones, so the
/// table is as small as the keep and the lookup budget allow.
#[derive(Debug)]
pub struct SymbolPropertiesKeyNotes<N> {
    ttl: Duration,
    kept: Mutex<HashMap<String, (Instant, N)>>,
}

impl<N: Clone> SymbolPropertiesKeyNotes<N> {
    /// Notes that live `ttl` (the keyed answers' own keep time).
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self { ttl, kept: Mutex::new(HashMap::new()) }
    }

    /// Remember `note` for `key`, dropping the notes that have expired.
    pub fn note(&self, key: &str, note: N) {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.retain(|_, (at, _)| at.elapsed() < self.ttl);
        kept.insert(key.to_string(), (Instant::now(), note));
    }

    /// The note of `key`, or `None` when it was never noted or is older than the keep.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<N> {
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.get(key).filter(|(at, _)| at.elapsed() < self.ttl).map(|(_, note)| note.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_is_remembered_and_the_two_not_nows_are_not() {
        let refused = SymbolPropertiesError::Refused("ctrader: no".into());
        assert!(!refused.is_transient());
        assert_eq!(refused.to_string(), "ctrader: no");
        for gone in [
            SymbolPropertiesError::NoInstrument("binance: no instrument X".into()),
            SymbolPropertiesError::Expired("deribit: delivered".into()),
        ] {
            assert!(!gone.is_transient(), "{gone:?}");
        }
        for not_now in [
            SymbolPropertiesError::ListUnavailable("the terms".into()),
            SymbolPropertiesError::RateUnavailable("no rate".into()),
        ] {
            assert!(not_now.is_transient(), "{not_now:?}");
        }
        assert_eq!(SymbolPropertiesError::RateUnavailable("no rate".into()).text(), "no rate");
        // A carried id rides beside the bare text, which `text()` and `Display` give unchanged.
        let carried = CarriedRefusal::new(Some("ibkr.symbol.x".into()), "ibkr: no".into());
        let refused = SymbolPropertiesError::Refused(carried.clone());
        assert_eq!((refused.text(), refused.to_string()), ("ibkr: no", "ibkr: no".to_string()));
        assert_eq!(refused, SymbolPropertiesError::Refused(carried));
    }

    /// A note lives as long as its keep, and noting drops the expired ones.
    #[test]
    fn a_key_note_lives_as_long_as_its_keep() {
        let notes = SymbolPropertiesKeyNotes::new(Duration::from_secs(3600));
        assert_eq!(notes.get("A"), None, "never noted");
        notes.note("A", 100);
        assert_eq!(notes.get("A"), Some(100));
        let gone = SymbolPropertiesKeyNotes::new(Duration::ZERO);
        gone.note("A", 1);
        assert_eq!(gone.get("A"), None, "older than a zero keep");
    }
}
