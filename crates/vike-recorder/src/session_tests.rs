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
    assert!(c.take_calls().iter().all(|(sym, _)| sym == "ARRIVE"), "KEEP was not re-subscribed");
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
        vec![("C".into(), Stream::Quotes), ("C".into(), Stream::Book), ("C".into(), Stream::Depth)]
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
