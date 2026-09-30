//! `session` — the subscription driver: keep a venue feed subscribed to exactly the symbols the
//! profile currently wants, and no others.
//!
//! **Why this is a type and not three lines in the daemon loop.** A family's membership rotates
//! (`membership`): Polymarket opens a new up/down window every 5 minutes, so "what should be
//! subscribed" changes 288 times a day per family. Each of those ticks has to answer three
//! questions that are easy to get wrong in a loop body, and whose wrong answers are all silent:
//!
//! - **the symbols that left** must be UNSUBSCRIBED. Every `subscribe_*` on a real venue client owns
//!   a feed thread ([`vike_data::live`]'s `FeedRegistry`) that lives until its id is passed back.
//!   Forgetting costs ~6 threads per rotation, forever — a recorder that has run a day is holding
//!   over a thousand threads on dead markets. Nothing errors; it just degrades.
//! - **the symbols that stayed** must NOT be re-subscribed. A re-subscribe drops the venue book
//!   state and forces a resnapshot, so a "just resubscribe everything each tick" driver punches a
//!   gap into the tape every rotation, in the exact series the customer subscribed to record.
//! - **a stream this venue does not serve** must be asked for ONCE. `Unsupported` is a capability
//!   answer (spec D7, capabilities-not-obligations) and cannot change while the client lives; a
//!   transient `Subscribe` failure is the opposite and MUST be retried. Treating them alike either
//!   spams a venue 288 times a day with a verb it will never serve, or silently gives up on a symbol
//!   because the socket happened to be reconnecting on the tick it was first seen.
//!
//! This type is venue-agnostic on purpose: it drives the [`DataClient`] trait, so it is tested here
//! against a scripted client with no network, and the concrete venue is chosen by the binary.
//!
//! ## Relationship to `vike_polymarket::universe::UniverseManager` (there are two diffs, deliberately)
//!
//! That type also diffs a token set, and it is NOT what this replaces. It answers **which tokens
//! should I want** — a venue-local, intent-level rule shared by the liquidity ranking and
//! `discovery::RollingWindowPlanner`. This answers **what is actually subscribed on the client right
//! now**: it is venue-agnostic, owns the [`SubscriptionId`]s, and its bookkeeping is driven by what
//! the client ACCEPTED, not by what the caller intended (`UniverseManager::commit`'s own docs say it
//! is called "once the caller has — or is about to — apply" the diff, so a `subscribe_*` that fails
//! is already recorded there as subscribed and never retried; here it is not).
//!
//! They compose by handing the FULL desired set across, never a delta: a Polymarket caller takes
//! `RollingTick::target` and passes it to [`SubscriptionSet::reconcile`]. It deliberately does not
//! commit the planner's own diff — two stateful views of "what is subscribed" that can drift apart
//! is the bug this arrangement avoids.

use std::collections::{BTreeMap, BTreeSet};

use vike_data::live::{DataClient, LiveDataError, SubscriptionId};

/// The tick-lane streams a recorder can subscribe. Bars are deliberately absent: a recorder records
/// what the venue SENT, and bars are derived — recording them would store a second, redundant, and
/// possibly disagreeing view of the same trades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stream {
    Quotes,
    Trades,
    /// The LOSSLESS L2 lane (`subscribe_book`) — every delta, contiguous `seq`, detectable gaps.
    /// Recorded as `kind=book`.
    Book,
    /// The CONFLATING L2 lane (`subscribe_depth`) — a full snapshot on a cadence, with every
    /// intermediate state discarded. Recorded as `kind=depth`, deliberately NOT `kind=book`.
    ///
    /// A separate stream rather than a fallback for [`Book`](Stream::Book), because the two make
    /// different promises — and because on the venues that matter they are not alternatives at all:
    /// binance/bybit/okx declare `book: false, depth: true` and REFUSE `subscribe_book`, so depth is
    /// the only L2 they serve. Without this variant the recorder never asked, and their L2 was
    /// unobtainable in this workspace by ANY means — no venue backfill serves `book` either, and the
    /// crypto L2 archive is rights-blocked.
    ///
    /// A venue serving BOTH is asked for both and records both, which is correct: they are different
    /// data with different guarantees, not two views of one thing.
    Depth,
}

impl Stream {
    /// Every stream, in the order a recorder subscribes them.
    pub const ALL: [Stream; 4] = [Stream::Quotes, Stream::Trades, Stream::Book, Stream::Depth];

    pub fn as_str(self) -> &'static str {
        match self {
            Stream::Quotes => "quotes",
            Stream::Trades => "trades",
            Stream::Book => "book",
            Stream::Depth => "depth",
        }
    }

    /// The store `kind=` partition this stream's rows land in.
    ///
    /// NOT [`as_str`](Self::as_str): that is the SUBSCRIPTION verb (plural, venue-facing), this is
    /// the persisted series kind (singular). They differ for quotes/trades — exactly the near-miss
    /// that would make a liveness key silently match nothing.
    pub fn store_kind(self) -> &'static str {
        match self {
            Stream::Quotes => "quote",
            Stream::Trades => "trade",
            Stream::Book => "book",
            Stream::Depth => "depth",
        }
    }
}

/// What one [`SubscriptionSet::reconcile`] pass did — the daemon's status line, and what the Data
/// Manager will render.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// `(symbol, stream)` newly subscribed this pass.
    pub started: Vec<(String, Stream)>,
    /// `(symbol, stream)` unsubscribed because the symbol left the desired set.
    pub stopped: Vec<(String, Stream)>,
    /// `(symbol, stream, error)` — a transient start failure. These are RETRIED next pass, so a
    /// non-empty list is not yet a fault; the same pair reappearing pass after pass is.
    pub failed: Vec<(String, Stream, String)>,
    /// Streams this client answered `Unsupported` for, reported only on the pass that LEARNED it.
    /// A venue serving no trade feed says so once, not 288 times a day.
    pub learned_unsupported: Vec<Stream>,
}

impl ReconcileReport {
    /// Nothing changed — the steady state between rotations, which is most passes.
    pub fn is_quiet(&self) -> bool {
        self.started.is_empty()
            && self.stopped.is_empty()
            && self.failed.is_empty()
            && self.learned_unsupported.is_empty()
    }
}

/// The live subscription state for ONE venue client.
///
/// Holds every issued [`SubscriptionId`] so that teardown is exact: [`SubscriptionSet::reconcile`]
/// stops precisely the streams that left, and [`SubscriptionSet::stop_all`] stops the rest.
#[derive(Debug, Default)]
pub struct SubscriptionSet {
    live: BTreeMap<(String, Stream), SubscriptionId>,
    /// Streams this client has told us it does not serve. Learned once, never re-asked — the
    /// capability/transient split this type exists to keep straight.
    unsupported: BTreeSet<Stream>,
}

impl SubscriptionSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Symbols currently subscribed on at least one stream.
    pub fn symbols(&self) -> BTreeSet<&str> {
        self.live.keys().map(|(s, _)| s.as_str()).collect()
    }

    /// Every live subscription as a store-series key, `"{kind}/{venue}/{symbol}"` — the identity
    /// [`vike_data::RecorderHandle::liveness`] reports under, so the two can be diffed.
    ///
    /// This is "what I believe I subscribed", which is the half the recorder knows and the sink
    /// does not: a series that has never received a single row has no liveness entry at all, so
    /// the never-started case is only visible against this set.
    pub fn series_keys(&self, venue: &str) -> Vec<String> {
        self.series_keys_with_symbols(venue).into_iter().map(|(key, _)| key).collect()
    }

    /// [`series_keys`](Self::series_keys)' twin, carrying the SYMBOL beside each key.
    ///
    /// It exists because a caller that has to ask `Membership` about a series (which is keyed on
    /// `(venue, symbol)`) would otherwise have to take the formatted key apart again —
    /// `crates/vike-recorder/src/runtime.rs`'s `expected_families` is the one such caller. Both
    /// spellings of the key format would then exist, and the whole cost of this feature landing in
    /// the wrong place is that they drift; there is ONE `format!` here and the plain version
    /// delegates to it.
    pub fn series_keys_with_symbols(&self, venue: &str) -> Vec<(String, String)> {
        self.live
            .keys()
            .map(|(sym, stream)| (format!("{}/{venue}/{sym}", stream.store_kind()), sym.clone()))
            .collect()
    }

    /// How many streams are live — subscriptions, not symbols.
    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Is this exact `(symbol, stream)` subscribed right now?
    pub fn contains(&self, symbol: &str, stream: Stream) -> bool {
        self.live.contains_key(&(symbol.to_string(), stream))
    }

    /// Drive `client` to exactly `desired` on `streams`.
    ///
    /// Stops what left FIRST, then starts what arrived — so a rotation never holds both windows'
    /// subscriptions at once, which on a venue with a per-connection subscription cap is the
    /// difference between rotating cleanly and being refused the new window.
    pub fn reconcile(
        &mut self,
        client: &mut dyn DataClient,
        desired: &BTreeSet<String>,
        streams: &[Stream],
    ) -> ReconcileReport {
        let mut report = ReconcileReport::default();
        let wanted: BTreeSet<Stream> = streams.iter().copied().collect();

        // 1. Stop everything no longer wanted — a symbol that left the family, or a stream that was
        //    dropped from the profile.
        let doomed: Vec<(String, Stream)> = self
            .live
            .keys()
            .filter(|(sym, st)| !desired.contains(sym) || !wanted.contains(st))
            .cloned()
            .collect();
        for key in doomed {
            if let Some(id) = self.live.remove(&key) {
                client.unsubscribe(id);
                report.stopped.push(key);
            }
        }

        // 2. Start what is wanted and not already live. Already-live pairs are left strictly alone:
        //    re-subscribing a surviving symbol would resnapshot its book for no reason.
        for symbol in desired {
            for &stream in streams {
                if self.unsupported.contains(&stream) {
                    continue;
                }
                let key = (symbol.clone(), stream);
                if self.live.contains_key(&key) {
                    continue;
                }
                match subscribe(client, symbol, stream) {
                    Ok(id) => {
                        self.live.insert(key, id);
                        report.started.push((symbol.clone(), stream));
                    }
                    Err(LiveDataError::Unsupported(_)) => {
                        // A capability answer: true for every symbol on this client, forever.
                        self.unsupported.insert(stream);
                        report.learned_unsupported.push(stream);
                    }
                    // `Subscribe`, and any variant added later — `LiveDataError` is
                    // `#[non_exhaustive]`. Treated as TRANSIENT and retried, which is the safe
                    // default of the two: retrying a permanent failure costs one call per pass,
                    // while permanently giving up on a symbol loses its tape in silence.
                    Err(e) => {
                        // Not recorded as live, so the next pass tries again.
                        report.failed.push((symbol.clone(), stream, e.to_string()));
                    }
                }
            }
        }

        report
    }

    /// Stop every stream this set owns, leaving it empty. The `unsupported` learning is KEPT: it
    /// describes the client, and the client outlives one profile edit.
    pub fn stop_all(&mut self, client: &mut dyn DataClient) -> Vec<(String, Stream)> {
        let stopped: Vec<(String, Stream)> = self.live.keys().cloned().collect();
        for (_, id) in std::mem::take(&mut self.live) {
            client.unsubscribe(id);
        }
        stopped
    }
}

fn subscribe(
    client: &mut dyn DataClient,
    symbol: &str,
    stream: Stream,
) -> Result<SubscriptionId, LiveDataError> {
    match stream {
        Stream::Quotes => client.subscribe_quotes(symbol),
        Stream::Trades => client.subscribe_trades(symbol),
        Stream::Book => client.subscribe_book(symbol),
        Stream::Depth => client.subscribe_depth(symbol),
    }
}

#[path = "session_tests.rs"]
#[cfg(test)]
mod session_tests;
