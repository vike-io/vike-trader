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
        self.live
            .keys()
            .map(|(sym, stream)| format!("{}/{venue}/{sym}", stream.store_kind()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::live::SubscriptionId;

    /// A scripted [`DataClient`] recording every call — the whole point of driving the trait rather
    /// than a concrete venue is that the rotation logic is testable with no network.
    #[derive(Default)]
    struct FakeClient {
        next: u64,
        /// Streams this fake refuses as a CAPABILITY (`Unsupported`).
        unsupported: BTreeSet<Stream>,
        /// Streams this fake fails TRANSIENTLY, until `fail_transient` is cleared.
        fail_transient: BTreeSet<Stream>,
        subscribed: Vec<(String, Stream)>,
        unsubscribed: Vec<SubscriptionId>,
        shutdowns: usize,
    }

    impl FakeClient {
        fn issue(&mut self, symbol: &str, stream: Stream) -> Result<SubscriptionId, LiveDataError> {
            if self.unsupported.contains(&stream) {
                return Err(LiveDataError::Unsupported("fake: no such feed"));
            }
            if self.fail_transient.contains(&stream) {
                return Err(LiveDataError::Subscribe("fake: socket reconnecting".into()));
            }
            self.next += 1;
            self.subscribed.push((symbol.to_string(), stream));
            Ok(SubscriptionId(self.next))
        }
        /// Subscribe calls made since the last `take_calls`.
        fn take_calls(&mut self) -> Vec<(String, Stream)> {
            std::mem::take(&mut self.subscribed)
        }
    }

    impl DataClient for FakeClient {
        fn subscribe_bars(
            &mut self,
            _symbol: &str,
            _interval: &str,
        ) -> Result<SubscriptionId, LiveDataError> {
            Err(LiveDataError::Unsupported("fake: recorder never asks for bars"))
        }
        fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            self.issue(symbol, Stream::Quotes)
        }
        fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            self.issue(symbol, Stream::Trades)
        }
        fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            self.issue(symbol, Stream::Book)
        }
        fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            self.issue(symbol, Stream::Depth)
        }
        fn unsubscribe(&mut self, id: SubscriptionId) {
            self.unsubscribed.push(id);
        }
        fn shutdown(&mut self) {
            self.shutdowns += 1;
        }
    }

    fn set(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_first_pass_subscribes_every_symbol_on_every_stream() {
        let mut c = FakeClient::default();
        let mut s = SubscriptionSet::new();
        let r = s.reconcile(&mut c, &set(&["UP", "DOWN"]), &Stream::ALL);

        assert_eq!(s.len(), 8, "2 symbols x 4 streams");
        assert_eq!(r.started.len(), 8);
        assert!(r.stopped.is_empty());
        assert!(s.contains("UP", Stream::Book));
    }

    /// The rotation case, and the reason this type exists. When the 14:05 window replaces the 14:00
    /// one, the old tokens' feed threads must be released — otherwise the recorder accumulates
    /// threads on dead markets for as long as it runs, silently.
    #[test]
    fn a_rotation_stops_exactly_the_symbols_that_left() {
        let mut c = FakeClient::default();
        let mut s = SubscriptionSet::new();
        s.reconcile(&mut c, &set(&["OLD_UP", "OLD_DOWN"]), &Stream::ALL);
        c.take_calls();

        let r = s.reconcile(&mut c, &set(&["NEW_UP", "NEW_DOWN"]), &Stream::ALL);

        assert_eq!(r.stopped.len(), 8, "both old tokens, all four streams");
        assert_eq!(c.unsubscribed.len(), 8, "the client was actually told");
        assert_eq!(r.started.len(), 8);
        assert_eq!(s.symbols(), ["NEW_DOWN", "NEW_UP"].into_iter().collect());
    }

    /// A partial rotation — the case that catches a "stop everything, resubscribe everything"
    /// driver. Re-subscribing a SURVIVING symbol resnapshots its book, punching a gap into the tape
    /// the customer is paying attention to.
    #[test]
    fn a_surviving_symbol_is_never_resubscribed() {
        let mut c = FakeClient::default();
        let mut s = SubscriptionSet::new();
        s.reconcile(&mut c, &set(&["KEEP", "LEAVE"]), &Stream::ALL);
        c.take_calls();

        let r = s.reconcile(&mut c, &set(&["KEEP", "ARRIVE"]), &Stream::ALL);

        assert_eq!(r.stopped.len(), 4, "only LEAVE");
        assert!(r.stopped.iter().all(|(sym, _)| sym == "LEAVE"));
        assert_eq!(r.started.len(), 4, "only ARRIVE");
        assert!(
            c.take_calls().iter().all(|(sym, _)| sym == "ARRIVE"),
            "KEEP was not re-subscribed"
        );
    }

    /// Steady state: between rotations most passes must do nothing at all, or the recorder is
    /// churning subscriptions under a feed it is supposed to be quietly recording.
    #[test]
    fn an_unchanged_desired_set_is_a_no_op() {
        let mut c = FakeClient::default();
        let mut s = SubscriptionSet::new();
        s.reconcile(&mut c, &set(&["TOK"]), &Stream::ALL);
        c.take_calls();

        let r = s.reconcile(&mut c, &set(&["TOK"]), &Stream::ALL);

        assert!(r.is_quiet(), "{r:?}");
        assert!(c.take_calls().is_empty());
        assert!(c.unsubscribed.is_empty());
    }

    /// `Unsupported` is a CAPABILITY answer — true for every symbol, permanently. Asking again is
    /// 288 pointless calls a day per family, and on a venue that logs refusals, 288 log lines.
    #[test]
    fn an_unsupported_stream_is_asked_once_and_never_again() {
        let mut c = FakeClient::default();
        c.unsupported.insert(Stream::Trades);
        let mut s = SubscriptionSet::new();

        let r1 = s.reconcile(&mut c, &set(&["A", "B"]), &Stream::ALL);
        assert_eq!(r1.learned_unsupported, vec![Stream::Trades], "reported once");
        assert_eq!(r1.started.len(), 6, "quotes+book+depth for both symbols (trades refused)");

        // A later rotation brings new symbols: still no trade attempt.
        let r2 = s.reconcile(&mut c, &set(&["C"]), &Stream::ALL);
        assert!(r2.learned_unsupported.is_empty(), "not re-reported");
        assert_eq!(
            r2.started,
            vec![
                ("C".into(), Stream::Quotes),
                ("C".into(), Stream::Book),
                ("C".into(), Stream::Depth)
            ]
        );
    }

    /// A depth-serving venue subscribes it like any other stream — the variant is not special-cased
    /// anywhere in the driver, which is the point of adding it here rather than as a `Book` fallback.
    #[test]
    fn a_depth_serving_venue_gets_a_depth_subscription() {
        let mut c = FakeClient::default();
        // The real shape of binance/bybit/okx: book REFUSED, depth served.
        c.unsupported.insert(Stream::Book);
        let mut s = SubscriptionSet::new();

        let r = s.reconcile(&mut c, &set(&["BTCUSDT"]), &Stream::ALL);

        assert_eq!(r.learned_unsupported, vec![Stream::Book]);
        assert!(s.contains("BTCUSDT", Stream::Depth), "depth is subscribed");
        assert!(!s.contains("BTCUSDT", Stream::Book));
        assert_eq!(s.len(), 3, "quotes + trades + depth");
    }

    /// The mirror of the above, and the reason the two error variants are NOT collapsed: a socket
    /// that happened to be reconnecting must not cost the symbol its recording for the rest of the
    /// daemon's life.
    #[test]
    fn a_transient_subscribe_failure_is_retried_next_pass() {
        let mut c = FakeClient::default();
        c.fail_transient.insert(Stream::Book);
        let mut s = SubscriptionSet::new();

        let r1 = s.reconcile(&mut c, &set(&["TOK"]), &Stream::ALL);
        assert_eq!(r1.failed.len(), 1);
        assert_eq!(r1.started.len(), 3, "quotes+trades+depth got through");
        assert!(!s.contains("TOK", Stream::Book), "a failure is not recorded as live");

        c.fail_transient.clear(); // the socket came back
        let r2 = s.reconcile(&mut c, &set(&["TOK"]), &Stream::ALL);
        assert_eq!(r2.started, vec![("TOK".into(), Stream::Book)], "retried and got it");
        assert!(r2.failed.is_empty());
        assert_eq!(s.len(), 4);
    }

    /// Narrowing the profile's streams (say, dropping `book` to save disk) must actually stop the
    /// book feed, not merely stop writing it.
    #[test]
    fn dropping_a_stream_from_the_profile_unsubscribes_it() {
        let mut c = FakeClient::default();
        let mut s = SubscriptionSet::new();
        s.reconcile(&mut c, &set(&["TOK"]), &Stream::ALL);

        let r = s.reconcile(&mut c, &set(&["TOK"]), &[Stream::Quotes, Stream::Trades]);

        assert_eq!(
            r.stopped,
            vec![("TOK".into(), Stream::Book), ("TOK".into(), Stream::Depth)],
            "BOTH L2 lanes released — they are separate streams, so narrowing the profile to \
             quotes+trades drops each of them"
        );
        assert_eq!(c.unsubscribed.len(), 2);
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn stop_all_releases_everything_and_keeps_the_capability_learning() {
        let mut c = FakeClient::default();
        c.unsupported.insert(Stream::Trades);
        let mut s = SubscriptionSet::new();
        s.reconcile(&mut c, &set(&["A", "B"]), &Stream::ALL);

        let stopped = s.stop_all(&mut c);

        assert_eq!(stopped.len(), 6);
        assert_eq!(c.unsubscribed.len(), 6);
        assert!(s.is_empty());

        // Re-subscribing must still not ask for trades: the client has not changed.
        let r = s.reconcile(&mut c, &set(&["A"]), &Stream::ALL);
        assert!(r.learned_unsupported.is_empty());
        assert_eq!(r.started.len(), 3);
    }

    /// An empty desired set is legitimate — between Polymarket windows a family can resolve to no
    /// live token — and must release the previous window rather than hold it.
    #[test]
    fn an_empty_desired_set_stops_everything() {
        let mut c = FakeClient::default();
        let mut s = SubscriptionSet::new();
        s.reconcile(&mut c, &set(&["TOK"]), &Stream::ALL);

        let r = s.reconcile(&mut c, &BTreeSet::new(), &Stream::ALL);

        assert_eq!(r.stopped.len(), 4);
        assert!(s.is_empty());
    }
}
