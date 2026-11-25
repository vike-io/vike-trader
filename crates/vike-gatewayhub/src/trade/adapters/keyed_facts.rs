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
//!   remembered for seconds, never the failed keep time ([`SymbolPropertiesCacheKeyedRateMemory`]): the engine calls a rate stale at 120 s,
//!   so a blip that is served from memory for minutes is minutes of denied opening orders;
//! * `refresh_reserve` of the budget is for the RE-READ of a key that has answered with a rate, so a flood
//!   of first looks cannot starve the refresh of a held position.
//!
//! The venue's own fences (what it admits at all, its grid) and its numbers and words are its bridge's spec
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesKeyedSpec`: ig's `vike_ig::symbol_properties`, ibkr's
//! `vike_ibkr::symbol_properties`), which also turns its source's answers into the shared vocabulary; this module only
//! bounds, keeps and words, and names no venue. One of these per account (`TradeLaneBridge` keeps the map, a
//! `PerAccount`).

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesKeyNotes, SymbolPropertiesKeyedError, SymbolPropertiesKeyedSpec,
};
use vike_gatewayhub_client::proto::{
    PROPERTIES_BUSY, PROPERTIES_LIST_UNAVAILABLE, PROPERTIES_RATE_UNAVAILABLE,
    SymbolPropertiesRefusal, SymbolPropertiesRefusalCode, SymbolPropertiesWire,
};

use super::symbol_facts::{LISTING_TTL, SESSION_FAILED_TTL, WireAnswer, coded, not_now, verdict};

/// How long an answer that carries a conversion rate is reused: the node's own reuse window for a rate is 30 s
/// and the engine calls one stale at 120 s, so the daemon's copy must be a good deal fresher than either.
pub const RATE_KEEP: Duration = Duration::from_secs(5);

/// How long a failed READ on the rate path is remembered ([`SymbolPropertiesKeyedError::NotNow`]): a blip (a 503, a timeout, a
/// closed socket) must not outlive the node's own retry (10 s) by minutes, because the engine calls a rate
/// stale at 120 s and denies opening orders from then on. Long enough that a burst of orders on one epic shares
/// one failing read instead of repeating it.
pub const RATE_READ_FAILED_KEEP: Duration = Duration::from_secs(5);

/// How long a rate that IG cannot give or that no price confirms is remembered ([`SymbolPropertiesKeyedError::RateUnavailable`]:
/// no conversion pair listed, a shut market that cannot witness the rate, a table no reading of which agrees).
/// Longer than a failed read because asking again within a few seconds cannot change the answer, and NOT longer
/// than the node's own retry of a rate (`RATE_RETRY_AFTER`, 10 s): it was 30 s, so a transient answer (the Sunday
/// open, a pair that blinked) could keep refusing for up to 20 s after the market was back and a retry had come.
pub const NO_RATE_KEEP: Duration = Duration::from_secs(10);

/// How long the two failures of the RATE path are remembered. Never longer than the venue's `failed_ttl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolPropertiesCacheKeyedRateMemory {
    /// A read that failed ([`SymbolPropertiesKeyedError::NotNow`]).
    pub read_failed: Duration,
    /// A rate that could not be had or believed ([`SymbolPropertiesKeyedError::RateUnavailable`]).
    pub no_rate: Duration,
}

impl Default for SymbolPropertiesCacheKeyedRateMemory {
    fn default() -> Self {
        Self { read_failed: RATE_READ_FAILED_KEEP, no_rate: NO_RATE_KEEP }
    }
}

/// One key's state: the last answer and when it was taken.
#[derive(Default)]
struct Slot {
    answer: Option<Kept>,
    /// This key has answered with a conversion rate at least once: a later ask is a REFRESH of a symbol the
    /// engine holds a rate for, not a first look (the budget's reserve, [`SymbolPropertiesCacheKeyed::with_refresh_reserve`]).
    rated: bool,
}

/// The last answer of a key, with the time it is good for (chosen when it was written, by what it was).
struct Kept {
    at: Instant,
    keep: Duration,
    answer: Result<SymbolPropertiesWire, SymbolPropertiesRefusal>,
}

/// See the module doc.
pub struct SymbolPropertiesCacheKeyed<S: SymbolPropertiesKeyedSpec> {
    ttl: Duration,
    failed_ttl: Duration,
    budget: usize,
    window: Duration,
    /// How long the rate path's failures are remembered ([`Self::with_rate_memory`]).
    rate_memory: SymbolPropertiesCacheKeyedRateMemory,
    /// How many of `budget`'s lookups only a REFRESH of a rated key may spend ([`Self::with_refresh_reserve`]).
    refresh_reserve: usize,
    slots: Mutex<HashMap<String, Arc<Mutex<Slot>>>>,
    fetches: Mutex<VecDeque<Instant>>,
    /// What the spec keeps beside a granted first-kind answer, for as long as the answer lives.
    notes: SymbolPropertiesKeyNotes<S::Note>,
    spec: PhantomData<fn() -> S>,
}

/// The spec's bounds: a granted answer kept for [`LISTING_TTL`], a refusal for [`SESSION_FAILED_TTL`] (each lookup is
/// a login's or a data connect's worth), the spec's budget per window and its refresh reserve.
impl<S: SymbolPropertiesKeyedSpec> Default for SymbolPropertiesCacheKeyed<S> {
    fn default() -> Self {
        Self::with_bounds(LISTING_TTL, SESSION_FAILED_TTL, S::BUDGET, S::WINDOW)
            .with_refresh_reserve(S::REFRESH_RESERVE)
    }
}

impl<S: SymbolPropertiesKeyedSpec> SymbolPropertiesCacheKeyed<S> {
    /// The spec's facts with explicit bounds and no refresh reserve (the tests'): a granted answer kept for `ttl`, a
    /// refusal for `failed_ttl`, `budget` upstream lookups per `window`.
    #[must_use]
    pub fn with_bounds(
        ttl: Duration,
        failed_ttl: Duration,
        budget: usize,
        window: Duration,
    ) -> Self {
        Self {
            ttl,
            failed_ttl,
            budget,
            window,
            rate_memory: SymbolPropertiesCacheKeyedRateMemory::default(),
            refresh_reserve: 0,
            slots: Mutex::new(HashMap::new()),
            fetches: Mutex::new(VecDeque::new()),
            notes: SymbolPropertiesKeyNotes::new(ttl),
            spec: PhantomData,
        }
    }

    /// These facts remembering the rate path's failures for `memory` (the defaults are [`SymbolPropertiesCacheKeyedRateMemory::default`]).
    #[must_use]
    pub fn with_rate_memory(mut self, memory: SymbolPropertiesCacheKeyedRateMemory) -> Self {
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
    /// kept for [`SymbolPropertiesCacheKeyedRateMemory`] (seconds, never the failed keep time).
    pub fn facts(
        &self,
        key: &str,
        fetch: impl FnOnce() -> S::Fetched,
    ) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
        self.kept(key, || {
            let (WireAnswer(wire), note) = S::answer::<WireAnswer>(fetch())?;
            if let Some(note) = note {
                self.notes.note(key, note);
            }
            Ok(wire)
        })
    }

    /// [`Self::facts`] for the spec's second kind of ask, kept under its own key (`S::other_key`).
    pub fn facts_other(
        &self,
        key: &str,
        fetch: impl FnOnce() -> S::FetchedOther,
    ) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
        self.kept(&S::other_key(key), || {
            S::answer_other::<WireAnswer>(fetch()).map(|WireAnswer(wire)| wire)
        })
    }

    /// What the spec noted beside `key`'s first-kind answer, or `None` when it was never looked up or is older than the
    /// keep. Asked AFTER [`Self::facts`] answered, so a kept answer's note is no older than the keep unless something is
    /// wrong, and `None` is then a refusal, not a guess.
    #[must_use]
    pub fn note(&self, key: &str) -> Option<S::Note> {
        self.notes.get(key)
    }

    /// The answer kept under `key` while fresh, else `fetch`ed when the budget allows.
    fn kept(
        &self,
        key: &str,
        fetch: impl FnOnce() -> Result<SymbolPropertiesWire, SymbolPropertiesKeyedError>,
    ) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
        let slot = self.slot(key)?;
        let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(kept) = held.answer.as_ref()
            && kept.at.elapsed() < kept.keep
        {
            return kept.answer.clone();
        }
        self.spend_budget(held.rated)?;
        // The code is what makes the node treat a failure as "not now" and remember nothing; the markers are the words.
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
            Err(SymbolPropertiesKeyedError::Refused(why)) => (Err(verdict(why)), self.failed_ttl),
            Err(SymbolPropertiesKeyedError::Unavailable(why)) => (
                Err(coded(
                    SymbolPropertiesRefusalCode::ListUnavailable,
                    not_now(S::VENUE, PROPERTIES_LIST_UNAVAILABLE, &why),
                )),
                self.failed_ttl,
            ),
            Err(SymbolPropertiesKeyedError::NotNow(why)) => (
                Err(coded(
                    SymbolPropertiesRefusalCode::ListUnavailable,
                    not_now(S::VENUE, PROPERTIES_LIST_UNAVAILABLE, &why),
                )),
                self.rate_memory.read_failed.min(self.failed_ttl),
            ),
            Err(SymbolPropertiesKeyedError::RateUnavailable(why)) => (
                Err(coded(
                    SymbolPropertiesRefusalCode::RateUnavailable,
                    not_now(S::VENUE, PROPERTIES_RATE_UNAVAILABLE, &why),
                )),
                self.rate_memory.no_rate.min(self.failed_ttl),
            ),
        };
        held.answer = Some(Kept { at: Instant::now(), keep, answer: answer.clone() });
        answer
    }

    /// The slot of `key`, made on first ask; a full table drops the expired slots first.
    fn slot(&self, key: &str) -> Result<Arc<Mutex<Slot>>, SymbolPropertiesRefusal> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = slots.get(key) {
            return Ok(Arc::clone(slot));
        }
        if slots.len() >= S::MAX_KEYS {
            // A slot being fetched right now is locked: it is not expired, keep it.
            slots.retain(|_, slot| match slot.try_lock() {
                Ok(held) => self.fresh(&held),
                Err(TryLockError::WouldBlock) => true,
                Err(TryLockError::Poisoned(p)) => self.fresh(&p.into_inner()),
            });
            // shortcut: a dropped slot forgets that its key was rated, so its next ask is a first look
            // again; only a full table (MAX_EPICS keys) does this, and it costs one reserve-less lookup.
            if slots.len() >= S::MAX_KEYS {
                return Err(coded(
                    SymbolPropertiesRefusalCode::Busy,
                    format!(
                        "{}: {PROPERTIES_BUSY} (at most {} {} are looked up at once; retry shortly)",
                        S::VENUE,
                        S::MAX_KEYS,
                        S::KEYS
                    ),
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
    fn spend_budget(&self, refresh: bool) -> Result<(), SymbolPropertiesRefusal> {
        let mut fetches = self.fetches.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        while fetches.front().is_some_and(|at| now.duration_since(*at) >= self.window) {
            fetches.pop_front();
        }
        let allowed = if refresh { self.budget } else { self.budget - self.refresh_reserve };
        if fetches.len() >= allowed {
            return Err(coded(
                SymbolPropertiesRefusalCode::Busy,
                format!(
                    "{}: {PROPERTIES_BUSY} (at most {} {} per {} s for this account; retry shortly)",
                    S::VENUE,
                    allowed,
                    S::LOOKUPS,
                    self.window.as_secs()
                ),
            ));
        }
        fetches.push_back(now);
        Ok(())
    }
}

impl<S: SymbolPropertiesKeyedSpec> std::fmt::Debug for SymbolPropertiesCacheKeyed<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymbolPropertiesCacheKeyed")
            .field("venue", &S::VENUE)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "keyed_facts_ig_tests.rs"]
mod keyed_facts_ig_tests;

#[cfg(all(test, feature = "ibkr"))]
#[path = "keyed_facts_ibkr_tests.rs"]
mod keyed_facts_ibkr_tests;

#[cfg(all(test, feature = "ibkr"))]
#[path = "keyed_facts_ibkr_rate_tests.rs"]
mod keyed_facts_ibkr_rate_tests;
