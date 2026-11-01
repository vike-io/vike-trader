//! The unit-test pins every venue's live market feed (`Feeds`, the `vike_data::DataClient`) ran
//! over its own wrapper, written once (test-support only). Each venue keeps its `#[test]` and hands
//! in its facts — its `Feeds`, spawn path, registry, mark pairing, symbols and interval spelling — so
//! every venue stays proven and only the assertions are shared. [`FeedPins`] drives a
//! registry-backed feed (binance, bybit, okx, deribit, hyperliquid); the free fns pin the declared
//! refusals, the br7 subscribe-ack watchdog on the venue's real pump row and the `mark_streams` knob
//! (decision 0095).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vike_data::{DataClient, FeedRegistry, LiveDataError, SubscriptionId};

use crate::mark_stream::MarkPairings;
use crate::sub_ack::SubscribeAck;

/// A network-free stand-in for a feed thread's body: polls its own stop flag at the real feeds' WS
/// cadence without ever touching a socket, so a lifecycle runs deterministically in `cargo test`.
pub fn idle_until_stopped(stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// One venue's registry-backed `Feeds` as these pins drive it.
pub struct FeedPins<F> {
    /// The venue's private spawn of `(symbol, interval)` with an [`idle_until_stopped`] body.
    pub spawn: fn(&mut F, &str, &str) -> SubscriptionId,
    /// The feed's `FeedRegistry`.
    pub registry: fn(&F) -> &FeedRegistry,
    /// Two of the venue's interval spellings; single-chart pins open the first.
    pub intervals: [&'static str; 2],
    /// A mark-capable venue's pairing (`None`: the venue has no mark lane).
    pub mark: Option<MarkPins<F>>,
}

/// A mark-capable venue's `pair_mark_stream(bars_id, symbol, idle body)` and its `MarkPairings`.
pub struct MarkPins<F> {
    pub pair: fn(&mut F, SubscriptionId, &str),
    pub pairings: fn(&F) -> &MarkPairings<SubscriptionId>,
}

impl<F: DataClient> FeedPins<F> {
    fn open(&self, feeds: &mut F, symbol: &str) -> SubscriptionId {
        (self.spawn)(feeds, symbol, self.intervals[0])
    }

    fn running(&self, feeds: &F) -> usize {
        (self.registry)(feeds).len()
    }

    fn mark(&self) -> &MarkPins<F> {
        self.mark.as_ref().expect("a mark-capable venue")
    }

    /// Two subscriptions get two distinct ids, and `shutdown` joins both.
    pub fn distinct_ids(&self, mut feeds: F, symbols: [&str; 2]) {
        let id1 = self.open(&mut feeds, symbols[0]);
        let id2 = self.open(&mut feeds, symbols[1]);
        assert_ne!(id1, id2, "distinct ids per subscribe");
        feeds.shutdown();
        assert!((self.registry)(&feeds).is_empty());
    }

    /// Unsubscribing one stream stops exactly that one; the other keeps running.
    pub fn unsubscribe_keeps_the_others(&self, mut feeds: F, symbols: [&str; 2]) {
        let id1 = self.open(&mut feeds, symbols[0]);
        let id2 = self.open(&mut feeds, symbols[1]);
        feeds.unsubscribe(id1);
        assert_eq!(self.running(&feeds), 1, "only the unsubscribed stream is removed");
        assert!((self.registry)(&feeds).contains(id2), "the other subscription keeps running");
        feeds.shutdown();
        assert!((self.registry)(&feeds).is_empty());
    }

    /// An id this feed never issued is a no-op.
    pub fn unknown_unsubscribe_is_a_no_op(&self, mut feeds: F, symbol: &str) {
        let id = self.open(&mut feeds, symbol);
        feeds.unsubscribe(SubscriptionId(id.0 + 100)); // never issued
        assert_eq!(self.running(&feeds), 1, "unknown id must not disturb the real subscription");
        feeds.shutdown();
    }

    /// `shutdown` returns only once every feed thread has joined.
    pub fn shutdown_joins_every_feed(&self, mut feeds: F, symbols: &[&str]) {
        for symbol in symbols {
            self.open(&mut feeds, symbol);
        }
        feeds.shutdown(); // must return only once every thread has actually joined
        assert!((self.registry)(&feeds).is_empty());
    }

    /// Two charts (the two intervals) on one perp share ONE mark stream, released only when the
    /// last bars subscription goes.
    pub fn two_intervals_share_one_mark(&self, mut feeds: F, perp: &str) {
        let first = (self.spawn)(&mut feeds, perp, self.intervals[0]);
        (self.mark().pair)(&mut feeds, first, perp);
        let second = (self.spawn)(&mut feeds, perp, self.intervals[1]);
        (self.mark().pair)(&mut feeds, second, perp);
        assert_eq!(self.running(&feeds), 3, "two bars feeds but only ONE mark stream");

        feeds.unsubscribe(first);
        assert_eq!(
            self.running(&feeds),
            2,
            "the mark stream the second chart still needs stays up"
        );
        feeds.unsubscribe(second);
        assert!((self.registry)(&feeds).is_empty(), "the last release stops the mark stream too");
        feeds.shutdown();
    }

    /// A perp bars subscription spawns its companion mark stream; unsubscribing the bars id stops
    /// BOTH and consumes the pairing, while an unrelated subscription keeps running.
    pub fn bars_spawn_and_stop_their_mark(&self, mut feeds: F, [perp, other]: [&str; 2]) {
        let bars_id = self.open(&mut feeds, perp);
        let other_id = self.open(&mut feeds, other);
        (self.mark().pair)(&mut feeds, bars_id, perp);
        assert_eq!(self.running(&feeds), 3, "bars + mark + the unrelated feed");
        assert!((self.mark().pairings)(&feeds).mark_id_of(perp).is_some());

        feeds.unsubscribe(bars_id);
        assert_eq!(self.running(&feeds), 1, "bars AND mark stopped together");
        assert!((self.registry)(&feeds).contains(other_id), "unrelated subscriptions keep running");
        assert!((self.mark().pairings)(&feeds).is_empty(), "the pairing is consumed");
        feeds.shutdown();
    }

    /// A bars subscription on `symbol` pairs NO mark stream (a spot symbol, or the knob off), `why`.
    pub fn pairs_no_mark(&self, mut feeds: F, symbol: &str, why: &str) {
        let bars_id = self.open(&mut feeds, symbol);
        (self.mark().pair)(&mut feeds, bars_id, symbol);
        assert_eq!(self.running(&feeds), 1, "{why}");
        assert!((self.mark().pairings)(&feeds).is_empty());
        feeds.shutdown();
    }
}

/// A feed whose declared row serves no live quotes and no lossless book refuses both.
pub fn refuses_quotes_and_book(mut feeds: impl DataClient, symbol: &str) {
    assert!(matches!(feeds.subscribe_quotes(symbol), Err(LiveDataError::Unsupported(_))));
    assert!(matches!(feeds.subscribe_book(symbol), Err(LiveDataError::Unsupported(_))));
}

/// The br7 subscribe-ack watchdog on `venue`'s REAL pump row: it trips one ms past the declared
/// window with no ack/data (the attributable error the shared driver returns), not at it, and a
/// confirmed subscribe never trips. The caller first pins that its router reads the venue's ack
/// frame as `FeedFrame::Ack`, the confirm this disarms on.
pub fn ack_watchdog(venue: &str) {
    let sub_ack_timeout = crate::pump_spec::market_pump_spec(venue)
        .knobs()
        .ack_timeout
        .unwrap_or_else(|| panic!("{venue} declares the br7 ack watchdog"));
    let timeout_ms = sub_ack_timeout.as_millis() as i64;
    let armed_at = 1_000; // scripted "subscribe sent" wall-clock ms
    let ack = SubscribeAck::new(armed_at, timeout_ms);
    assert!(!ack.overdue(armed_at + timeout_ms), "at the deadline, not yet overdue (strict >)");
    assert!(
        ack.overdue(armed_at + timeout_ms + 1),
        "one ms past the {}s window with no ack/data → attributable error",
        sub_ack_timeout.as_secs()
    );
    let mut acked = SubscribeAck::new(armed_at, timeout_ms);
    acked.confirm();
    assert!(!acked.overdue(armed_at + timeout_ms * 100), "a confirmed subscribe never trips");
}

/// The venue's `mark_streams` row decides — exact `"1"` on, exact `"0"` off — and no row keeps the
/// charter default `charter_on`.
pub fn mark_streams_row_decides<F>(
    fresh: fn() -> F,
    with_row: fn(F, Option<&str>) -> F,
    mark_streams: fn(&F) -> bool,
    charter_on: bool,
) {
    assert_eq!(mark_streams(&fresh()), charter_on, "the charter default");
    assert!(!mark_streams(&with_row(fresh(), Some("0"))));
    assert!(mark_streams(&with_row(fresh(), Some("1"))));
    assert_eq!(mark_streams(&with_row(fresh(), None)), charter_on);
}

/// The default a feed runs and the one `config show` prints are ONE fact: `charter_on` is
/// `venue.<venue>.mark_streams`'s declared default.
pub fn charter_default_is_declared(venue: &str, charter_on: bool) {
    let declared =
        vike_venues::venue_fields::venue_field(venue, "mark_streams").expect("declared").default;
    assert_eq!(charter_on, declared == "1", "declared {declared:?}");
}
