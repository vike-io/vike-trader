//! **The venue facts of ANY symbol of an exchange, from its PUBLIC instrument list** (`TradeUp::SymbolProperties`,
//! decision 0173): what the node needs to admit a symbol the account's engine does not hold yet.
//!
//! * **Credential-free.** The rows come from the instrument catalog the daemon already serves (the same
//!   rows the bridges' exec grid parsers read), handed in as an
//!   [`InstrumentList`] by the composition root, so this module names no bridge and no account secret is
//!   touched.
//! * **Bounded.** A full listing per order would be abusive: each venue's listing is kept for
//!   [`LISTING_TTL`], and the fetch is SINGLE-FLIGHT per venue (a second asker waits on the slot's lock and
//!   then reads the fresh listing). A failed fetch is remembered for [`FAILED_TTL`] (a failing or rate-limited venue is not asked again by a flood of orders and previews: each fetch is a whole listing, a heavy request). The credentialed sources whose every fetch is a whole authenticated session or connect remember a failure for [`SESSION_FAILED_TTL`] instead.
//! * **Rate-bounded per venue.** A per-symbol lookup is one upstream request, so each venue has a token
//!   bucket ([`LOOKUP_BURST`], one token back per [`LOOKUP_REFILL`]: 20 lookups per 10 s) in front of it. A
//!   refused token answers the transient [`PROPERTIES_BUSY`] with no network call and is NOT remembered as the
//!   symbol's failure; a symbol already kept (or in its failure memory) costs no token. The slot table is
//!   a hard cap too ([`MAX_SLOTS`]).
//! * **One symbol at a time where the venue filters** (`SymbolPropertiesListSpec::one_at_a_time`, a
//!   [`SymbolLister`]): a symbol the venue's API answers alone (a few hundred bytes against a listing of several
//!   MB) is read alone; which symbols those are is the venue's spec's answer.
//!   Such a symbol is kept and single-flighted under its OWN slot, so a slow read holds up no other symbol and no
//!   other venue; "the venue has no such symbol" is remembered for [`FAILED_TTL`] only. Every other ask still
//!   reads one list per venue (and per network).
//! * **A failed or half read is a failure, never an unknown symbol.** A source that could not read all it
//!   needed returns an `Err` (not a partial `Ok`): it is worded [`PROPERTIES_LIST_UNAVAILABLE`] (transient, so the
//!   node does not remember it as a verdict), kept for [`FAILED_TTL`] and never stored as the listing. The
//!   picker's catalog is tolerant of one endpoint failing on purpose; this module's sources are not.
//! * **One generic answer, each venue's facts in its bridge** (owner ruling 2026-11-12). `facts_via` names no
//!   venue: it finds the venue's `vike_bridge_core::symbol_properties::SymbolPropertiesListSpec` in
//!   `symbol_facts_specs.rs`'s table and asks it, in the trait's order, how the symbol is spelled, keyed and read,
//!   which network's listing answers it and what the row is (each venue's spellings, classes and refusals are its
//!   spec's module doc). What every venue shares stays here: the not-found words, the `.P` lane's linear rule (a
//!   perpetual whose row is not linear is refused with the venue's own word rather than priced as linear, unless
//!   the spec judged the row whole), the fully-funded default (a spot lane's pair and a prediction market's outcome
//!   share are answered `Cash` at a margin rate of 1) and the tick and step checks. A venue with no spec answers
//!   [`NOT_ANSWERED`].
//! * **Whose list.** A venue whose list is PUBLIC is answered from the lists the composition root hands in. A venue
//!   whose list is NOT public is listed by [`SymbolPropertiesCacheAccount`] with the asking account's own credentials, under the
//!   same bounds (`docs/ops/any-symbol-remaining-venues-plan.md`); a venue whose spec `covers` only the account's
//!   own listing is answered ONLY so (a public list of its rows would admit what its fences refuse), fenced by its
//!   bridge's spec over [`SymbolPropertiesCacheAccountListed`] (for a node that converts a quote currency:
//!   [`SymbolPropertiesCacheAccountListed::facts_quoted`]). A venue whose facts are per key is not here
//!   (`keyed_facts.rs`). A market NAME the venue spells another way is resolved to its symbol BEFORE the facts are
//!   asked, by the lane (`TradeLaneBridge`, then [`SymbolPropertiesCache::facts_by_name`]). Which venue reads which way is its
//!   spec's: `crates/vike-gatewayhub/src/trade/adapters/symbol_facts_specs.rs`'s `SPECS` and each bridge's
//!   `SymbolPropertiesListSpec` impl.
//!   An answer that states a margin mode reaches only a node that said it books one
//!   (`TradeUp::SymbolProperties::modes`, `session.rs`'s `mode_refusal`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesAccountSpec, SymbolPropertiesAnswer, SymbolPropertiesError,
    SymbolPropertiesListAnswer, SymbolPropertiesListSpec, SymbolPropertiesWords,
};
use vike_gatewayhub_client::proto::{
    PROPERTIES_BUSY, PROPERTIES_EXPIRED, PROPERTIES_LIST_EMPTY, PROPERTIES_LIST_UNAVAILABLE,
    PROPERTIES_NO_INSTRUMENT, PROPERTIES_RATE_UNAVAILABLE, SymbolPropertiesWire,
};
use vike_model::{Contract, ContractValue, Instrument, Margining, QuoteRate, SymbolProperties};

use super::symbol_facts_specs::list_spec;

/// The whole instrument list of one venue, or the reason it could not be had. Called with the venue slug.
pub type InstrumentList = Arc<dyn Fn(&str) -> Result<Vec<Instrument>, String> + Send + Sync>;

/// How long a venue's listing is kept.
pub const LISTING_TTL: Duration = Duration::from_secs(600);

/// How long a venue's FAILED listing fetch is remembered (the answer is the failure, served again).
pub const FAILED_TTL: Duration = Duration::from_secs(30);

/// How long a FAILED fetch is remembered by a CREDENTIALED source whose every fetch costs a whole
/// authenticated session or connect (an application and account auth, a login, or a data connect from a small
/// hourly budget). At [`FAILED_TTL`] an outage would dial a new session every 30 s
/// (about twenty in ten minutes); at this one, about two. The public and per-symbol REST sources keep
/// [`FAILED_TTL`].
pub const SESSION_FAILED_TTL: Duration = Duration::from_secs(300);

/// The answer of a venue this module does not serve (also the trait default's words).
pub const NOT_ANSWERED: &str = "this venue's adapter does not answer symbol facts";

/// One venue's rows for ONE symbol (the venue's own spelling, see `SymbolPropertiesListSpec::one_at_a_time`): the venue
/// slug and the symbol it is asked for. An empty `Ok` is the venue's "no such symbol"; a failed or partial
/// read MUST be an `Err` (it is remembered briefly and worded as a failure, never as an unknown symbol).
pub type SymbolLister = Arc<dyn Fn(&str, &str) -> Result<Vec<Instrument>, String> + Send + Sync>;

/// Slots kept (one slot per venue listing or per looked-up symbol): a HARD cap, so a flood of distinct
/// symbols cannot grow the map without bound. A full map drops the expired slots, then the OLDEST finished
/// one; a slot whose read is in flight is never dropped (and if every slot is, the ask is answered "busy").
const MAX_SLOTS: usize = 512;

/// Names kept by [`SymbolPropertiesCache::facts_by_name`]: a hard cap, like [`MAX_SLOTS`].
const MAX_NAMES: usize = 256;

/// How many per-symbol upstream lookups one venue may make in a burst, from a full bucket.
pub const LOOKUP_BURST: u32 = 10;

/// One lookup token comes back this often: with [`LOOKUP_BURST`], 20 lookups per 10 s per venue. Every
/// per-symbol lookup is one upstream request against a per-IP weight the gatewayhub's data REST shares, and only the
/// number IN FLIGHT was bounded before (`session.rs`'s `MAX_SYMBOL_FACTS_INFLIGHT`), not how many in a minute.
pub const LOOKUP_REFILL: Duration = Duration::from_millis(500);

/// A venue's lookup tokens: `tokens` as of `at`, topped up at one per refill, never above the burst.
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    /// Take one token as of `now`; `false` = none left (nothing is spent). `now` is a parameter so a test
    /// drives the clock.
    fn take(&mut self, now: Instant, burst: u32, refill: Duration) -> bool {
        if !refill.is_zero() {
            let gained =
                now.saturating_duration_since(self.at).as_secs_f64() / refill.as_secs_f64();
            self.tokens = (self.tokens + gained).min(f64::from(burst));
        } else {
            self.tokens = f64::from(burst);
        }
        self.at = now;
        let granted = self.tokens >= 1.0;
        if granted {
            self.tokens -= 1.0;
        }
        granted
    }
}

type Listing = Arc<HashMap<String, Instrument>>;
/// One slot's state: the kept rows (taken at, kept for, rows), and the last failure.
#[derive(Default)]
struct SlotState {
    listing: Option<(Instant, Duration, Listing)>,
    failed: Option<(Instant, String)>,
}

impl SlotState {
    /// When this slot last finished a read (rows or failure); `None` = never.
    fn taken_at(&self) -> Option<Instant> {
        self.listing.as_ref().map(|(at, ..)| *at).max(self.failed.as_ref().map(|(at, _)| *at))
    }
}

type Slot = Arc<Mutex<SlotState>>;

/// The names an operator typed, by `venue \u{1f} text`, and the symbol each resolved to (or the refusal), with
/// the moment it was learned ([`SymbolPropertiesCache::facts_by_name`]).
type NameBook = HashMap<String, (Instant, Result<String, String>)>;

/// See the module doc.
pub struct SymbolPropertiesCache {
    list: InstrumentList,
    /// How a venue that can answer for ONE symbol is asked (`None` = every venue reads its whole list).
    one: Option<SymbolLister>,
    ttl: Duration,
    failed_ttl: Duration,
    slots: Mutex<HashMap<String, Slot>>,
    /// Per venue, the tokens a PER-SYMBOL upstream lookup spends (`burst`, one back per `refill`).
    buckets: Mutex<HashMap<String, Bucket>>,
    burst: u32,
    refill: Duration,
    /// What an operator TYPED for a symbol the venue spells another way (a market's name) and the symbol
    /// the lane resolved it to, or the refusal: kept for [`Self::ttl`] (a refusal for `failed_ttl`) so the same name
    /// costs one venue read, however many times the node asks ([`Self::facts_by_name`]).
    names: Mutex<NameBook>,
    /// Whether this source is an [`SymbolPropertiesCacheAccount`]' inner one, the only kind a spec whose `covers` asks for an
    /// account's own listing answers: its fences (the account's currency, the instrument class) are its spec over
    /// [`SymbolPropertiesCacheAccountListed`], so a PUBLIC list of its rows handed to a plain `SymbolPropertiesCache` would
    /// admit what it refuses.
    account_scoped: bool,
}

impl SymbolPropertiesCache {
    /// Facts over `list`, each venue's listing kept for [`LISTING_TTL`].
    #[must_use]
    pub fn new(list: InstrumentList) -> Self {
        Self::with_ttl(list, LISTING_TTL)
    }

    /// [`SymbolPropertiesCache::new`] with an explicit keep time (the tests').
    #[must_use]
    pub fn with_ttl(list: InstrumentList, ttl: Duration) -> Self {
        Self::with_ttls(list, ttl, FAILED_TTL)
    }

    /// [`SymbolPropertiesCache::with_ttl`] with an explicit time to remember a failed fetch too.
    #[must_use]
    pub fn with_ttls(list: InstrumentList, ttl: Duration, failed_ttl: Duration) -> Self {
        Self {
            list,
            one: None,
            ttl,
            failed_ttl,
            slots: Mutex::new(HashMap::new()),
            buckets: Mutex::new(HashMap::new()),
            burst: LOOKUP_BURST,
            refill: LOOKUP_REFILL,
            names: Mutex::new(HashMap::new()),
            account_scoped: false,
        }
    }

    /// These facts with an explicit per-venue lookup rate (the tests'): `burst` tokens, one back per `refill`.
    #[must_use]
    pub fn with_lookup_rate(mut self, burst: u32, refill: Duration) -> Self {
        self.burst = burst;
        self.refill = refill;
        self
    }

    /// Take one of `venue`'s lookup tokens as of `now`.
    fn take_lookup(&self, venue: &str, now: Instant) -> bool {
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        let bucket = buckets
            .entry(venue.to_string())
            .or_insert_with(|| Bucket { tokens: f64::from(self.burst), at: now });
        bucket.take(now, self.burst, self.refill)
    }

    /// These facts, with `one` answering the asks the venue's spec reads one at a time
    /// (`SymbolPropertiesListSpec::one_at_a_time`; a symbol is then
    /// kept and single-flighted per `(venue, symbol)`, so an unrelated symbol never waits on it).
    #[must_use]
    pub fn with_symbol_lister(mut self, one: SymbolLister) -> Self {
        self.one = Some(one);
        self
    }

    /// This source as an [`SymbolPropertiesCacheAccount`]' inner one (the only kind that answers a spec covering only the
    /// account's own listing).
    fn account_scoped(mut self) -> Self {
        self.account_scoped = true;
        self
    }

    /// The facts of `symbol` on `venue`, or the operator's sentence for why there are none.
    pub fn facts(&self, venue: &str, symbol: &str) -> Result<SymbolPropertiesWire, String> {
        self.facts_via(venue, venue, symbol, self.one.as_ref(), None, &|_| Ok(()))
    }

    /// [`Self::facts`] for an account on `network` (`"mainnet"` or `"testnet"`, the words of
    /// `vike_model::VenueNetwork::word`): the asking ACCOUNT's network chooses the universe, never
    /// the daemon's. The venue's spec names the listing (`SymbolPropertiesListSpec::list_key`): a test or demo
    /// network with a listing of its own has a key of its own, kept and failure-remembered apart; every other
    /// network is answered exactly as [`Self::facts`] answers.
    pub fn facts_on(
        &self,
        venue: &str,
        network: &str,
        symbol: &str,
    ) -> Result<SymbolPropertiesWire, String> {
        let list_key = list_spec(venue).map_or(venue, |spec| spec.list_key(network));
        self.facts_via(venue, list_key, symbol, self.one.as_ref(), None, &|_| Ok(()))
    }

    /// **The facts of a symbol the operator may have typed as a NAME** (a name the venue spells another way, which
    /// `resolve` turns into the venue's symbol with one read). The name is resolved first
    /// ([`Self::named`]), the facts of the RESOLVED symbol are the answer, and the answer says which symbol they
    /// are for ([`SymbolPropertiesWire::symbol`], only when it differs from `typed`), so the node admits and orders under
    /// the venue's one spelling. A symbol already in the venue's spelling resolves to itself.
    pub fn facts_by_name(
        &self,
        venue: &str,
        typed: &str,
        resolve: &dyn Fn(&str) -> Result<String, String>,
    ) -> Result<SymbolPropertiesWire, String> {
        let symbol = self.named(venue, typed, resolve)?;
        let mut wire = self.facts(venue, &symbol)?;
        wire.symbol = (symbol != typed).then_some(symbol);
        Ok(wire)
    }

    /// `typed` resolved to `venue`'s symbol by `resolve`, kept (a refusal too, for a shorter time) so a name asked
    /// again within the keep time costs no venue read. NOT single-flight (two askers of one new name make two reads;
    /// the lease's own bound on running lookups, `MAX_SYMBOL_FACTS_INFLIGHT`, caps them), and a full table is
    /// emptied of its expired names and then cleared (`MAX_NAMES`): a flood of distinct names costs a read each, as
    /// before the table existed, and never grows it.
    fn named(
        &self,
        venue: &str,
        typed: &str,
        resolve: &dyn Fn(&str) -> Result<String, String>,
    ) -> Result<String, String> {
        let key = format!("{venue}\u{1f}{typed}");
        let kept = |at: &Instant, answer: &Result<String, String>| {
            at.elapsed() < if answer.is_ok() { self.ttl } else { self.failed_ttl }
        };
        if let Some((at, answer)) =
            self.names.lock().unwrap_or_else(PoisonError::into_inner).get(&key)
            && kept(at, answer)
        {
            return answer.clone();
        }
        // The read runs with no lock held.
        let answer = resolve(typed);
        let mut names = self.names.lock().unwrap_or_else(PoisonError::into_inner);
        if names.len() >= MAX_NAMES && !names.contains_key(&key) {
            names.retain(|_, (at, answer)| kept(at, answer));
            if names.len() >= MAX_NAMES {
                names.clear();
            }
        }
        names.insert(key, (Instant::now(), answer.clone()));
        answer
    }

    /// [`Self::facts`] with the per-symbol source (`one`) or the whole-list source (`whole`, instead of
    /// this source's own) supplied by the caller (the credentialed venue's, which holds the asking
    /// account's credentials and so cannot be kept here), and `check` run on the
    /// instrument's row once it is found, before any grid check: the hook the account-scoped venues' own
    /// fences (a currency, an instrument class) use, so a refusal names the fundamental reason instead
    /// of a grid detail. `list_key` names the whole listing asked for (the venue's slug, or the network's own
    /// key, `SymbolPropertiesListSpec::list_key`): the slot it is kept under and the key the [`InstrumentList`] is
    /// called with.
    ///
    /// Generic over the venue's [`SymbolPropertiesListSpec`] (`symbol_facts_specs::list_spec`), asked in the trait's
    /// order; what every venue shares stays here: the not-found words, the `.P` lane's linear rule, the fully-funded
    /// default and the tick and step checks.
    fn facts_via(
        &self,
        venue: &str,
        list_key: &str,
        symbol: &str,
        one: Option<&SymbolLister>,
        whole: Option<&InstrumentList>,
        check: &dyn Fn(&Instrument) -> Result<(), String>,
    ) -> Result<SymbolPropertiesWire, String> {
        let Some(spec) = list_spec(venue) else { return Err(NOT_ANSWERED.to_string()) };
        let Some(covers) = spec.covers(list_key, self.account_scoped) else {
            return Err(NOT_ANSWERED.to_string());
        };
        if let Some(why) = spec.symbol_refusal(symbol, &WORDS) {
            return Err(why);
        }
        let symbol =
            if spec.case_kept(symbol) { symbol.to_string() } else { symbol.to_ascii_uppercase() };
        let key = spec.key(&symbol);
        if let Some(answer) = spec.answered_elsewhere(
            &key,
            list_key,
            vike_model::now_ms(),
            &WORDS,
            &|listing: &str, key: &str| Ok(self.listing(listing, &self.list)?.get(key).cloned()),
        ) {
            let answer = answer?;
            return answered(spec, &symbol, answer.properties.unwrap_or_default(), answer, false);
        }
        let single = one.filter(|_| spec.one_at_a_time(&key));
        let listing = match single {
            Some(lister) => {
                self.symbol_listing(spec.lister_key(list_key), &spec.wanted(&symbol, &key), lister)?
            }
            None => self.listing(list_key, whole.unwrap_or(&self.list))?,
        };
        let row =
            listing.get(&key).or_else(|| spec.row_fallback(&key).and_then(|k| listing.get(k)));
        let Some(row) = row else {
            return Err(single.and_then(|_| spec.not_found(&symbol)).unwrap_or_else(|| {
                format!(
                    "{venue}{PROPERTIES_NO_INSTRUMENT}{symbol} in the venue's instrument list ({covers})"
                )
            }));
        };
        check(row)?;
        let answer = spec.row_answer(&symbol, &key, row, vike_model::now_ms(), &WORDS)?;
        if !answer.whole
            && key.ends_with(".P")
            && row.contract.margining() != Some(Margining::Linear)
        {
            return Err(format!(
                "{venue}: {symbol} is {}, not a linear perpetual; the gatewayhub trades linear \
                 perpetuals only",
                row.contract_type.as_deref().unwrap_or("a non-linear contract")
            ));
        }
        // A FULLY FUNDED row is answered `Cash` at a margin rate of 1, never liquidated: a SPOT lane's pair (the
        // mode the node books a spot MOUNT in, `vike_catalog::names_spot_pair`; a venue whose spec states the mode
        // already keeps its own) and a prediction market's outcome share, bought outright for its price.
        let fully_funded = !answer.whole
            && (matches!(row.contract, Contract::PredictionOutcome(_))
                || (matches!(row.contract, Contract::CryptoSpot)
                    && vike_catalog::names_spot_pair(venue, &symbol)));
        answered(spec, &symbol, answer.properties.unwrap_or(row.properties), answer, fully_funded)
    }

    /// The whole listing `key` names (a venue's slug, or a network's own key): the kept one while
    /// fresh, else fetched with its slot held, so a concurrent asker waits for this fetch instead of making
    /// its own.
    fn listing(&self, key: &str, list: &InstrumentList) -> Result<Listing, String> {
        self.kept(key, key, false, || list(key))
    }

    /// `venue`'s rows for the ONE symbol `wanted`, kept and single-flighted under their own slot: the
    /// slot's lock is held across this symbol's fetch only, so an ask for any other symbol (or venue) never
    /// waits on it. A venue that answers with no row is remembered for [`FAILED_TTL`] only (the symbol may
    /// be listed a minute later), not for the listing's keep time.
    fn symbol_listing(
        &self,
        venue: &str,
        wanted: &str,
        lister: &SymbolLister,
    ) -> Result<Listing, String> {
        self.kept(&format!("{venue}\u{1f}{wanted}"), venue, true, || lister(venue, wanted))
    }

    /// The rows kept under `slot_key` while fresh, else `fetch`ed with that slot held. `one_symbol`: an
    /// empty answer is "no such symbol" (kept briefly), not an empty LIST (a failure).
    fn kept(
        &self,
        slot_key: &str,
        venue: &str,
        one_symbol: bool,
        fetch: impl FnOnce() -> Result<Vec<Instrument>, String>,
    ) -> Result<Listing, String> {
        let slot = self.slot(slot_key, venue)?;
        let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, keep, rows)) = held.listing.as_ref()
            && at.elapsed() < *keep
        {
            return Ok(Arc::clone(rows));
        }
        if let Some((at, why)) = held.failed.as_ref()
            && at.elapsed() < self.failed_ttl
        {
            return Err(why.clone());
        }
        // A per-symbol lookup is one upstream request: spend the venue's token first. A refusal makes no
        // network call and is NOT remembered (`held.failed` stays as it was): it is "not now", not a verdict.
        if one_symbol && !self.take_lookup(venue, Instant::now()) {
            return Err(format!(
                "{venue}: {PROPERTIES_BUSY} (per-symbol lookups are rate-limited to protect the venue's \
                 request budget; retry shortly)"
            ));
        }
        let fetched = fetch()
            .map_err(|why| format!("{venue}: {PROPERTIES_LIST_UNAVAILABLE}: {why}"))
            .and_then(|rows| {
                if rows.is_empty() && !one_symbol {
                    Err(format!("{venue}: {PROPERTIES_LIST_EMPTY}"))
                } else {
                    Ok(rows)
                }
            });
        match fetched {
            Ok(rows) => {
                let keep = if rows.is_empty() { self.failed_ttl } else { self.ttl };
                let rows: Listing =
                    Arc::new(rows.into_iter().map(|row| (row.raw_symbol.clone(), row)).collect());
                held.listing = Some((Instant::now(), keep, Arc::clone(&rows)));
                held.failed = None;
                Ok(rows)
            }
            Err(why) => {
                held.failed = Some((Instant::now(), why.clone()));
                Err(why)
            }
        }
    }

    /// The slot named `key`, made if absent. Only the map is locked here, and only briefly (never across
    /// a fetch). At [`MAX_SLOTS`] the slots whose rows and failure have both expired are dropped first,
    /// then (all still fresh) the OLDEST finished one; a slot whose lock is taken is mid-fetch and stays,
    /// and if every slot is, the ask is answered "busy" (not now, not remembered).
    fn slot(&self, key: &str, venue: &str) -> Result<Slot, String> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if slots.len() >= MAX_SLOTS && !slots.contains_key(key) {
            slots.retain(|_, slot| match slot.try_lock() {
                Ok(state) => {
                    state.listing.as_ref().is_some_and(|(at, keep, _)| at.elapsed() < *keep)
                        || state
                            .failed
                            .as_ref()
                            .is_some_and(|(at, _)| at.elapsed() < self.failed_ttl)
                }
                Err(_) => true,
            });
            if slots.len() >= MAX_SLOTS {
                let oldest = slots
                    .iter()
                    .filter_map(|(k, slot)| match slot.try_lock() {
                        Ok(state) => Some((k, state.taken_at())),
                        Err(std::sync::TryLockError::Poisoned(p)) => {
                            Some((k, p.into_inner().taken_at()))
                        }
                        Err(std::sync::TryLockError::WouldBlock) => None,
                    })
                    .min_by_key(|(_, at)| *at)
                    .map(|(k, _)| k.clone());
                let Some(oldest) = oldest else {
                    return Err(format!(
                        "{venue}: {PROPERTIES_BUSY} (every slot of the symbol table is mid-lookup; retry \
                         shortly)"
                    ));
                };
                slots.remove(&oldest);
            }
        }
        Ok(Arc::clone(slots.entry(key.to_string()).or_default()))
    }
}

/// The words of the wire a bridge's spec puts in its sentences (no bridge names the wire).
const WORDS: SymbolPropertiesWords = SymbolPropertiesWords {
    not_answered: NOT_ANSWERED,
    no_instrument: PROPERTIES_NO_INSTRUMENT,
    expired: PROPERTIES_EXPIRED,
};

/// **The shared tail of every list-answered venue's facts**: the grid answered must carry a tick and a step, then the
/// venue's own grid fence (`SymbolPropertiesListSpec::grid_refusal`), then the answer on the wire, a fully funded
/// row's `Cash` mode and margin rate of 1 filling what the spec left unstated.
fn answered(
    spec: &dyn SymbolPropertiesListSpec,
    symbol: &str,
    properties: SymbolProperties,
    answer: SymbolPropertiesListAnswer,
    fully_funded: bool,
) -> Result<SymbolPropertiesWire, String> {
    let venue = spec.venue();
    for (what, value) in [("tick size", properties.tick_size), ("step size", properties.step_size)]
    {
        if !value.is_finite() || value <= 0.0 {
            return Err(format!(
                "{venue}: instrument {symbol}: the venue's instrument list carries no {what} \
                 (missing or zero)"
            ));
        }
    }
    if let Some(why) = spec.grid_refusal(symbol, &properties) {
        return Err(why);
    }
    Ok(SymbolPropertiesWire {
        properties,
        contract_value: answer.contract_value,
        margin_mode: answer.margin_mode.or(fully_funded.then_some(vike_model::MarginMode::Cash)),
        margin_rate: answer.margin_rate.or(fully_funded.then_some(1.0)),
        ..SymbolPropertiesWire::default()
    })
}

/// One account's way to read ONE symbol's rows from its venue (the argument is the venue's symbol): it
/// owns what the call needs (for example a catalog holding the account's sandbox config), so it can be kept
/// past the borrow of the credentials.
pub type AccountLister = Arc<dyn Fn(&str) -> Result<Vec<Instrument>, String> + Send + Sync>;

/// **The facts of a venue whose instruments sit behind the asking account's OWN credentials** (decision 0174
/// verdict 1 says "public list", which such a venue has none of). It is [`SymbolPropertiesCache`] with the symbol source
/// supplied per ask, and everything
/// that bounds a public lookup bounds this one: an asset is kept for [`LISTING_TTL`] and fetched
/// single-flight per `(account, symbol)`, a failed fetch is remembered for [`FAILED_TTL`], and "no such
/// asset" for [`FAILED_TTL`] too. ⚠ The WHOLE asset list (several MB) is never read here: one lookup is
/// one small request. Everything is kept PER ACCOUNT (the caller's `account` key): a failure (an account
/// with no credentials reads nothing, and that refusal is its own) is never handed to another account, and
/// a fetch always runs with the credentials of the account it is for. The credentials never leave the
/// lister.
///
/// **Two ways to read, one set of bounds.** [`Self::facts`] reads ONE symbol per request;
/// [`Self::listed`] reads the account's WHOLE list per request, with the account's currency (a venue with no
/// per-symbol filter, whose currency is the account's too). Both are kept per account.
pub struct SymbolPropertiesCacheAccount {
    accounts: Mutex<HashMap<String, Arc<AccountSlot>>>,
    /// How long a failed fetch is remembered (by account slot): [`FAILED_TTL`], or [`SESSION_FAILED_TTL`]
    /// for a source whose fetch is a whole authenticated session.
    failed_ttl: Duration,
}

impl Default for SymbolPropertiesCacheAccount {
    fn default() -> Self {
        Self::with_failed_ttl(FAILED_TTL)
    }
}

/// One account's list of ALL its instruments and its currency, read with its own credentials the way its
/// venue's bridge reads them (one session, or an account summary and an instrument list).
pub type CurrencyLister = Arc<dyn Fn() -> Result<(String, Vec<Instrument>), String> + Send + Sync>;

/// One account's state in [`SymbolPropertiesCacheAccount`]: the bounded source and the currency of its last whole listing.
/// ⚠ It holds NO reader of the account's list: that holds the account's grant, so [`SymbolPropertiesCacheAccount::listed`]
/// takes it per ask and drops it when the ask returns.
struct AccountSlot {
    facts: SymbolPropertiesCache,
    /// The currency of the last successful whole listing (written by the per-ask reader `listed` builds).
    currency: Arc<Mutex<Option<String>>>,
}

impl AccountSlot {
    fn open(failed_ttl: Duration) -> Self {
        let facts = SymbolPropertiesCache::with_ttls(
            // Reached only by a venue asked through neither reader: `facts` answers per symbol, `listed`
            // brings its own list.
            Arc::new(|venue| {
                Err(format!("{venue}: this venue's assets are read one symbol at a time"))
            }),
            LISTING_TTL,
            failed_ttl,
        )
        .account_scoped();
        Self { facts, currency: Arc::default() }
    }
}

impl SymbolPropertiesCacheAccount {
    /// A source with no account yet: [`Self::facts`] opens one per account key it is asked for.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// [`Self::new`] remembering a failed fetch for `failed_ttl` (a source whose fetch is a session).
    #[must_use]
    pub fn with_failed_ttl(failed_ttl: Duration) -> Self {
        Self { accounts: Mutex::new(HashMap::new()), failed_ttl }
    }

    /// The slot of `account`, opened on first use.
    fn slot(&self, account: &str) -> Arc<AccountSlot> {
        let mut accounts = self.accounts.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            accounts
                .entry(account.to_string())
                .or_insert_with(|| Arc::new(AccountSlot::open(self.failed_ttl))),
        )
    }

    /// The facts of `symbol` on `venue` for `account` (any stable text that names ONE account: label and
    /// tier), reading the symbol through `lister` when that account's kept rows are stale.
    pub fn facts(
        &self,
        account: &str,
        venue: &str,
        symbol: &str,
        lister: AccountLister,
    ) -> Result<SymbolPropertiesWire, String> {
        let one: SymbolLister = Arc::new(move |_venue, symbol| lister(symbol));
        self.slot(account).facts.facts_via(venue, venue, symbol, Some(&one), None, &|_| Ok(()))
    }

    /// The facts of `symbol` on `venue` for `account`, from the account's WHOLE list read through
    /// `lister` when the kept one is stale (one read per [`LISTING_TTL`], single-flight, a failure
    /// remembered). `check` runs on the instrument's row with the account's currency once the row is
    /// found, before any grid check (the venue's own fence: `SymbolPropertiesCache::facts_via`); with no currency
    /// known, no row is admitted. ⚠ `lister` holds the account's grant: it is used for THIS ask and
    /// dropped when it returns (nothing in a slot keeps it).
    pub fn listed(
        &self,
        account: &str,
        venue: &str,
        symbol: &str,
        lister: CurrencyLister,
        check: &dyn Fn(&Instrument, &str) -> Result<(), String>,
    ) -> Result<SymbolPropertiesWire, String> {
        let slot = self.slot(account);
        let currency = Arc::clone(&slot.currency);
        let whole: InstrumentList = Arc::new(move |_venue| {
            let (deposit, rows) = lister()?;
            *currency.lock().unwrap_or_else(PoisonError::into_inner) = Some(deposit);
            Ok(rows)
        });
        slot.facts.facts_via(venue, venue, symbol, None, Some(&whole), &|row| {
            let held = slot.currency.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let Some(currency) = held else {
                // Unreachable after a listing, and fail-closed if it ever is: no currency, no admission.
                return Err(format!(
                    "{venue}: the account's currency is not known, so a quote currency cannot be \
                     checked against it"
                ));
            };
            check(row, &currency)
        })
    }
}

/// **The per-account facts (`T`: [`SymbolPropertiesCacheAccountListed`]) of every account of one venue, each kept for the
/// CREDENTIALS it was read with.** The map is keyed by the wire account text (`""` = the default account),
/// and each entry carries a non-reversible fingerprint of the account id behind it (`None` = the venue has
/// none, or the account holds no credentials). An operator who swaps the credentials under one label
/// (another account, with its own instrument list and its own currency) changes the fingerprint, and the
/// next ask gets a FRESH `T`: otherwise the old account's listing and currency would answer for up to
/// [`LISTING_TTL`], and the currency fence would judge a pair against a currency the account no longer
/// holds. Only the fingerprint is kept: no token, no account id.
pub struct PerAccount<T> {
    held: Mutex<HashMap<String, Held<T>>>,
}

/// One label's facts and the fingerprint of the credentials they were read with.
type Held<T> = (Option<u64>, Arc<T>);

impl<T> Default for PerAccount<T> {
    fn default() -> Self {
        Self { held: Mutex::new(HashMap::new()) }
    }
}

impl<T: Default> PerAccount<T> {
    /// The facts of `account` read with the credentials whose fingerprint is `fingerprint`: the held ones
    /// while the fingerprint is unchanged, a fresh one (replacing the held one, so the map never outgrows
    /// the accounts) when it changed.
    pub fn of(&self, account: &str, fingerprint: Option<u64>) -> Arc<T> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = held.entry(account.to_string()).or_insert_with(|| (fingerprint, Arc::default()));
        if slot.0 != fingerprint {
            *slot = (fingerprint, Arc::default());
        }
        Arc::clone(&slot.1)
    }
}

/// **One account's facts of a venue whose instrument list and currency are the ACCOUNT's** (`S`: the bridge's
/// [`SymbolPropertiesAccountSpec`] of a venue whose spec covers only the account's own listing):
/// [`SymbolPropertiesCacheAccount::listed`] (the account's own listing, one read per ten minutes, single-flight, a failure remembered for
/// [`SESSION_FAILED_TTL`] when one read is a whole authenticated session, else [`FAILED_TTL`]) plus the venue's
/// fences, run in the spec's order: the asking node's row refusal with the account's currency BEFORE any grid check,
/// the generic grid checks, then the adapter's grid fence on the grid the answer carries. Unlike a shared per-symbol
/// listing there is one of these per account ([`PerAccount`], which `TradeLaneBridge` keeps).
pub struct SymbolPropertiesCacheAccountListed<S> {
    facts: SymbolPropertiesCacheAccount,
    spec: PhantomData<fn() -> S>,
}

impl<S: SymbolPropertiesAccountSpec> Default for SymbolPropertiesCacheAccountListed<S> {
    fn default() -> Self {
        Self::with_failed_ttl(if S::LISTING_IS_A_SESSION { SESSION_FAILED_TTL } else { FAILED_TTL })
    }
}

impl<S: SymbolPropertiesAccountSpec> SymbolPropertiesCacheAccountListed<S> {
    /// These facts remembering a failed listing for `failed_ttl` (the tests').
    #[must_use]
    pub fn with_failed_ttl(failed_ttl: Duration) -> Self {
        Self { facts: SymbolPropertiesCacheAccount::with_failed_ttl(failed_ttl), spec: PhantomData }
    }

    /// The facts of `symbol` (the engine's spelling) for a node that converts NOTHING, listing through `lister`
    /// when the kept listing is stale: the account's currency and its instrument rows.
    pub fn facts(
        &self,
        symbol: &str,
        lister: CurrencyLister,
    ) -> Result<SymbolPropertiesWire, String> {
        // The row, kept from the row check for the grid fence below: the wire facts carry the grid, not the class.
        let seen = RefCell::new(None::<Instrument>);
        let wire = self.facts.listed(S::VENUE, S::VENUE, symbol, lister, &|row, currency| {
            *seen.borrow_mut() = Some(row.clone());
            S::plain_refusal(symbol, row, currency).map_or(Ok(()), Err)
        })?;
        if let Some(row) = seen.into_inner()
            && let Some(why) = S::grid_refusal(symbol, &row, &wire.properties)
        {
            return Err(why);
        }
        Ok(wire)
    }

    /// [`Self::facts`] for a node that CONVERTS a quote currency (`TradeUp::SymbolProperties::quoted`, decision 0183,
    /// phase 1): a symbol quoted in the account's own currency is answered exactly as before; one quoted in ANOTHER
    /// is answered with its quote currency and the rate `rate` reads for it (asked NOW: only the listing is kept,
    /// never a rate), instead of being refused for that alone. The row fence is the converting one (every refusal
    /// but the foreign quote), the adapter's grid fence still runs, and a row `rate` cannot price is refused: no
    /// rate, no admission. The answer states the ledger it is in (the listing's currency, which the rate was read
    /// against): the node refuses it when its engine was anchored to another (second review, HIGH-1).
    pub fn facts_quoted(
        &self,
        symbol: &str,
        lister: CurrencyLister,
        rate: &RateSource,
    ) -> Result<SymbolPropertiesWire, String> {
        // The row and the account's currency, kept from the row check.
        let seen = RefCell::new(None::<(Instrument, String)>);
        let mut wire =
            self.facts.listed(S::VENUE, S::VENUE, symbol, lister, &|row, currency| {
                if let Some(why) = S::quoted_refusal(symbol, row, currency) {
                    return Err(why);
                }
                *seen.borrow_mut() = Some((row.clone(), currency.to_string()));
                Ok(())
            })?;
        let (row, currency) = seen.into_inner().ok_or_else(|| {
            format!(
                "{}: the account's currency is not known, so a quote currency cannot be converted",
                S::VENUE
            )
        })?;
        if let Some(why) = S::grid_refusal(symbol, &row, &wire.properties) {
            return Err(why);
        }
        if let Some(quoted) = rate(&S::rate_subject(symbol, &row), &currency)? {
            wire.quote_rate = Some(quoted.account_per_quote.get());
            wire.quote_currency = Some(quoted.quote_currency);
            wire.rate_as_of_ms = Some(quoted.as_of_ms);
        }
        wire.account_currency = Some(currency);
        Ok(wire)
    }
}

/// How [`SymbolPropertiesCacheAccountListed::facts_quoted`] asks the venue for a conversion, handed in by the composition (this module names no bridge): `(the spec's rate subject, the account's
/// currency)`, the subject being what the venue's spec names (an instrument, or the row's QUOTE CURRENCY). `Ok(None)` = the instrument is quoted in the home currency,
/// so nothing converts; `Ok(Some(rate))` = it is quoted in another and this is the rate (`vike_model`'s one record:
/// account-currency units per ONE quote-currency unit, finite and positive by its type, stamped with the moment
/// the venue was asked); `Err` = no rate could be had, or the instrument names no quote currency, in the operator's
/// sentence. The symbol is then NOT admitted: an absent rate is never read as par.
pub type RateSource = Arc<dyn Fn(&str, &str) -> Result<Option<QuoteRate>, String> + Send + Sync>;

/// **A bridge's refusal of a symbol, in the node's words**: a verdict passes as its own sentence; the two "not now"
/// kinds are worded `<venue>: <marker>: <detail>` with the wire's own markers ([`PROPERTIES_LIST_UNAVAILABLE`],
/// [`PROPERTIES_RATE_UNAVAILABLE`]), so the node asks again and remembers nothing about the symbol. The one place a
/// bridge's [`SymbolPropertiesError`] meets the wire.
pub(crate) fn worded(venue: &str, refusal: SymbolPropertiesError) -> String {
    match refusal {
        SymbolPropertiesError::Refused(why) => why,
        SymbolPropertiesError::ListUnavailable(why) => {
            format!("{venue}: {PROPERTIES_LIST_UNAVAILABLE}: {why}")
        }
        SymbolPropertiesError::RateUnavailable(why) => {
            format!("{venue}: {PROPERTIES_RATE_UNAVAILABLE}: {why}")
        }
    }
}

/// **A bridge's answer for a symbol, on the wire**: the grid, what one unit of quantity stands for (`None` = the wire's
/// default, linear 1.0), the symbol's own margin rate when the venue states one, the quote currency with its rate and
/// its stamp when the symbol is quoted in another currency than the account's (absent at par, exactly as an older
/// daemon answers), and the ledger currency the answer is expressed in (`account_currency`, `None` = no statement), so the
/// node can refuse an answer in a currency its engine was not anchored to. The ONE mapping every venue's answer takes
/// (a converted answer and a plain one alike); it names no venue.
pub(crate) fn wire_facts(
    properties: SymbolProperties,
    contract_value: Option<ContractValue>,
    margin_rate: Option<f64>,
    quote: Option<QuoteRate>,
    account_ccy: Option<String>,
) -> SymbolPropertiesWire {
    SymbolPropertiesWire {
        properties,
        contract_value,
        margin_rate,
        quote_currency: quote.as_ref().map(|q| q.quote_currency.clone()),
        quote_rate: quote.as_ref().map(|q| q.account_per_quote.get()),
        rate_as_of_ms: quote.map(|q| q.as_of_ms),
        account_currency: account_ccy,
        ..SymbolPropertiesWire::default()
    }
}

/// **The wire's answer as a bridge writes it** (`vike_bridge_core::symbol_properties::SymbolPropertiesAnswer`): a
/// bridge's spec builds its answer through it, so its mapping of the venue's own types stays in the bridge, and the
/// parts land on the wire through the one [`wire_facts`]. A newtype because neither the trait nor the wire's type is
/// this crate's.
pub(crate) struct WireAnswer(pub(crate) SymbolPropertiesWire);

impl SymbolPropertiesAnswer for WireAnswer {
    fn of(
        properties: SymbolProperties,
        contract_value: Option<ContractValue>,
        margin_rate: Option<f64>,
        quote: Option<QuoteRate>,
        account_ccy: Option<String>,
    ) -> Self {
        Self(wire_facts(properties, contract_value, margin_rate, quote, account_ccy))
    }

    fn with_rate(mut self, rate: QuoteRate, ledger: String) -> Self {
        self.0.quote_currency = Some(rate.quote_currency);
        self.0.quote_rate = Some(rate.account_per_quote.get());
        self.0.rate_as_of_ms = Some(rate.as_of_ms);
        self.0.account_currency = Some(ledger);
        self
    }
}

impl std::fmt::Debug for SymbolPropertiesCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymbolProperties").field("ttl", &self.ttl).finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "symbol_facts_tests.rs"]
mod symbol_facts_tests;

#[cfg(test)]
#[path = "symbol_facts_one_tests.rs"]
mod symbol_facts_one_tests;

#[cfg(test)]
#[path = "symbol_facts_oanda_grid_tests.rs"]
mod symbol_facts_oanda_grid_tests;

#[cfg(test)]
#[path = "symbol_facts_pin_tests.rs"]
mod symbol_facts_pin_tests;
