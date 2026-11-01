//! **The bounds on a venue whose facts are looked up ONE KEY AT A TIME and have no list to keep** (ig's epics,
//! ibkr's stock names): [`super::symbol_facts`]'s bounds applied PER KEY, for one account, whose own
//! credentials pay for the reads (`docs/ops/any-symbol-remaining-venues-plan.md`).
//!
//! * a key's answer is kept for `LISTING_TTL` (a refusal for `FAILED_TTL`, the same bounds as a public
//!   list's), and the lookup is SINGLE-FLIGHT per key (a second asker waits on the slot's lock);
//! * the account makes at most `budget` upstream lookups per `window`, so a peer asking about a hundred
//!   made-up keys costs the venue a handful of reads a minute, not a hundred (the rest answer "busy", a
//!   not-now the node does not remember);
//! * at most `max_keys` keys are tracked; past that the expired ones are dropped, and if none is, the ask
//!   is answered "busy";
//! * a failure on the RATE path (a venue whose answer carries a quote-currency rate, decision 0183) is
//!   remembered for seconds, never the failed keep time ([`RateMemory`]): the engine calls a rate stale at 120 s,
//!   so a blip that is served from memory for minutes is minutes of denied opening orders;
//! * `refresh_reserve` of the budget is for the RE-READ of a key that has answered with a rate, so a flood
//!   of first looks cannot starve the refresh of a held position.
//!
//! The venue's own fences (what it admits at all, its grid) are its bridge's; this module only bounds and
//! words. One of these per account (`BridgeTrade` keeps the map, a `FactsByAccount`).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{
    FACTS_BUSY, FACTS_LIST_UNAVAILABLE, FACTS_RATE_UNAVAILABLE, WireSymbolFacts,
};

/// How long an answer that carries a conversion rate is reused: the node's own reuse window for a rate is 30 s
/// and the engine calls one stale at 120 s, so the daemon's copy must be a good deal fresher than either.
pub const RATE_KEEP: Duration = Duration::from_secs(5);

/// How long a failed READ on the rate path is remembered ([`KeyedError::NotNow`]): a blip (a 503, a timeout, a
/// closed socket) must not outlive the node's own retry (10 s) by minutes, because the engine calls a rate
/// stale at 120 s and denies opening orders from then on. Long enough that a burst of orders on one epic shares
/// one failing read instead of repeating it.
pub const RATE_READ_FAILED_KEEP: Duration = Duration::from_secs(5);

/// How long a rate that IG cannot give or that no price confirms is remembered ([`KeyedError::RateUnavailable`]:
/// no conversion pair listed, a shut market that cannot witness the rate, a table no reading of which agrees).
/// Longer than a failed read because asking again within a few seconds cannot change the answer, and NOT longer
/// than the node's own retry of a rate (`RATE_RETRY_AFTER`, 10 s): it was 30 s, so a transient answer (the Sunday
/// open, a pair that blinked) could keep refusing for up to 20 s after the market was back and a retry had come.
pub const NO_RATE_KEEP: Duration = Duration::from_secs(10);

/// How long the two failures of the RATE path are remembered. Never longer than the venue's `failed_ttl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateMemory {
    /// A read that failed ([`KeyedError::NotNow`]).
    pub read_failed: Duration,
    /// A rate that could not be had or believed ([`KeyedError::RateUnavailable`]).
    pub no_rate: Duration,
}

impl Default for RateMemory {
    fn default() -> Self {
        Self { read_failed: RATE_READ_FAILED_KEEP, no_rate: NO_RATE_KEEP }
    }
}

/// How a venue's lookup of one key failed.
pub enum KeyedError {
    /// The venue answered and the key is not admitted: a verdict, kept for the failed keep time.
    Refused(String),
    /// The venue could not be asked (a login, a socket, a timeout): worded as the node's "not now" and kept
    /// for the failed keep time (a login is what such a failure costs to repeat).
    Unavailable(String),
    /// The same "not now" for a lookup whose answer is a quote-currency RATE: worded as [`Self::Unavailable`]
    /// but kept for [`RateMemory::read_failed`] only, so the node's retry after a blip finds the venue asked
    /// again and not a cached error.
    NotNow(String),
    /// The key is fine but the quote-currency RATE that converts it could not be had or believed (decision
    /// 0183): worded as the node's "not now" about the rate (`FACTS_RATE_UNAVAILABLE`), so nothing is
    /// remembered about the symbol itself, and kept for [`RateMemory::no_rate`].
    RateUnavailable(String),
}

/// One key's state: the last answer and when it was taken.
#[derive(Default)]
struct Slot {
    answer: Option<Kept>,
    /// This key has answered with a conversion rate at least once: a later ask is a REFRESH of a symbol the
    /// engine holds a rate for, not a first look (the budget's reserve, [`KeyedFacts::with_refresh_reserve`]).
    rated: bool,
}

/// The last answer of a key, with the time it is good for (chosen when it was written, by what it was).
struct Kept {
    at: Instant,
    keep: Duration,
    answer: Result<WireSymbolFacts, String>,
}

/// See the module doc.
pub struct KeyedFacts {
    /// The venue's slug, the opening word of every refusal (`ig`).
    venue: &'static str,
    /// What a key is called in the "table full" sentence (`epics`, `names`).
    keys: &'static str,
    /// What a lookup is called in the "budget spent" sentence (`epic lookups`, `lookups`).
    lookups: &'static str,
    max_keys: usize,
    ttl: Duration,
    failed_ttl: Duration,
    budget: usize,
    window: Duration,
    /// How long the rate path's failures are remembered ([`Self::with_rate_memory`]).
    rate_memory: RateMemory,
    /// How many of `budget`'s lookups only a REFRESH of a rated key may spend ([`Self::with_refresh_reserve`]).
    refresh_reserve: usize,
    slots: Mutex<HashMap<String, Arc<Mutex<Slot>>>>,
    fetches: Mutex<VecDeque<Instant>>,
}

/// The venue's words and bounds, the arguments of [`KeyedFacts::new`].
pub struct KeyedBounds {
    /// The venue's slug, the opening word of every refusal.
    pub venue: &'static str,
    /// What a key is called in the "table full" sentence.
    pub keys: &'static str,
    /// What a lookup is called in the "budget spent" sentence.
    pub lookups: &'static str,
    /// How many keys are tracked at once.
    pub max_keys: usize,
    /// How long a granted answer is kept.
    pub ttl: Duration,
    /// How long a refusal is kept.
    pub failed_ttl: Duration,
    /// Upstream lookups allowed per `window`.
    pub budget: usize,
    /// See `budget`.
    pub window: Duration,
}

impl KeyedFacts {
    /// Facts bounded as `bounds` says.
    #[must_use]
    pub fn new(bounds: KeyedBounds) -> Self {
        Self {
            venue: bounds.venue,
            keys: bounds.keys,
            lookups: bounds.lookups,
            max_keys: bounds.max_keys,
            ttl: bounds.ttl,
            failed_ttl: bounds.failed_ttl,
            budget: bounds.budget,
            window: bounds.window,
            rate_memory: RateMemory::default(),
            refresh_reserve: 0,
            slots: Mutex::new(HashMap::new()),
            fetches: Mutex::new(VecDeque::new()),
        }
    }

    /// These facts remembering the rate path's failures for `memory` (the defaults are [`RateMemory::default`]).
    #[must_use]
    pub fn with_rate_memory(mut self, memory: RateMemory) -> Self {
        self.rate_memory = memory;
        self
    }

    /// These facts keeping `reserve` of the window's `budget` for the REFRESH of a key that has answered with a
    /// conversion rate: a first look at a made-up key may spend at most `budget - reserve` lookups a window, so a
    /// flood of those can never starve the re-read that keeps a held position's rate inside the engine's window
    /// (the engine calls a rate stale at 120 s and denies opening orders). `reserve` is clamped to half the
    /// budget. The daemon cannot tell a refresh from a first look by the request, only by the key's history.
    #[must_use]
    pub fn with_refresh_reserve(mut self, reserve: usize) -> Self {
        self.refresh_reserve = reserve.min(self.budget / 2);
        self
    }

    /// The facts of `key` (already canonical), looked up with `fetch` when no fresh answer is kept and the
    /// budget allows. A `Refused` answer is kept for the failed keep time; an `Unavailable` one is worded
    /// as the node's "not now" and kept only as a failure of that length, and a failure of the rate path is
    /// kept for [`RateMemory`] (seconds, never the failed keep time).
    pub fn facts(
        &self,
        key: &str,
        fetch: impl FnOnce() -> Result<WireSymbolFacts, KeyedError>,
    ) -> Result<WireSymbolFacts, String> {
        let slot = self.slot(key)?;
        let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(kept) = held.answer.as_ref()
            && kept.at.elapsed() < kept.keep
        {
            return kept.answer.clone();
        }
        self.spend_budget(held.rated)?;
        // The shared markers are what make the node treat a failure as "not now" and remember nothing.
        let (answer, keep) = match fetch() {
            Ok(facts) => {
                // A conversion rate (decision 0183) is never served from the listing's keep time: it is
                // kept for a moment only, so a look and the commit that follows it share one read.
                let keep = if facts.quote_rate.is_some() {
                    held.rated = true;
                    self.ttl.min(RATE_KEEP)
                } else {
                    self.ttl
                };
                (Ok(facts), keep)
            }
            Err(KeyedError::Refused(why)) => (Err(why), self.failed_ttl),
            Err(KeyedError::Unavailable(why)) => {
                (Err(format!("{}: {FACTS_LIST_UNAVAILABLE}: {why}", self.venue)), self.failed_ttl)
            }
            Err(KeyedError::NotNow(why)) => (
                Err(format!("{}: {FACTS_LIST_UNAVAILABLE}: {why}", self.venue)),
                self.rate_memory.read_failed.min(self.failed_ttl),
            ),
            Err(KeyedError::RateUnavailable(why)) => (
                Err(format!("{}: {FACTS_RATE_UNAVAILABLE}: {why}", self.venue)),
                self.rate_memory.no_rate.min(self.failed_ttl),
            ),
        };
        held.answer = Some(Kept { at: Instant::now(), keep, answer: answer.clone() });
        answer
    }

    /// The slot of `key`, made on first ask; a full table drops the expired slots first.
    fn slot(&self, key: &str) -> Result<Arc<Mutex<Slot>>, String> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = slots.get(key) {
            return Ok(Arc::clone(slot));
        }
        if slots.len() >= self.max_keys {
            // A slot being fetched right now is locked: it is not expired, keep it.
            slots.retain(|_, slot| match slot.try_lock() {
                Ok(held) => self.fresh(&held),
                Err(TryLockError::WouldBlock) => true,
                Err(TryLockError::Poisoned(p)) => self.fresh(&p.into_inner()),
            });
            // shortcut: a dropped slot forgets that its key was rated, so its next ask is a first look
            // again; only a full table (MAX_EPICS keys) does this, and it costs one reserve-less lookup.
            if slots.len() >= self.max_keys {
                return Err(format!(
                    "{}: {FACTS_BUSY} (at most {} {} are looked up at once; retry shortly)",
                    self.venue, self.max_keys, self.keys
                ));
            }
        }
        let slot = Arc::default();
        slots.insert(key.to_string(), Arc::clone(&slot));
        Ok(slot)
    }

    /// Whether `slot` still holds an answer within the keep time it was written with.
    fn fresh(&self, slot: &Slot) -> bool {
        slot.answer.as_ref().is_some_and(|kept| kept.at.elapsed() < kept.keep)
    }

    /// Take one lookup from the window's budget, or say the daemon is busy. A `refresh` (a key that has
    /// answered with a rate) may spend the whole budget; a first look only what the reserve leaves.
    fn spend_budget(&self, refresh: bool) -> Result<(), String> {
        let mut fetches = self.fetches.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        while fetches.front().is_some_and(|at| now.duration_since(*at) >= self.window) {
            fetches.pop_front();
        }
        let allowed = if refresh { self.budget } else { self.budget - self.refresh_reserve };
        if fetches.len() >= allowed {
            return Err(format!(
                "{}: {FACTS_BUSY} (at most {} {} per {} s for this account; retry shortly)",
                self.venue,
                allowed,
                self.lookups,
                self.window.as_secs()
            ));
        }
        fetches.push_back(now);
        Ok(())
    }
}

impl std::fmt::Debug for KeyedFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyedFacts").field("venue", &self.venue).finish_non_exhaustive()
    }
}
