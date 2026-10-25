//! `crate::quotes`' second reader, the RATE reader (`QuoteBus::fresh_tick`, multi-currency, decision 0183), and the
//! account currency the sidecar's `ready` line states (`QuoteBus::account_currency`): what is subscribed and when,
//! that the consumer never sees a rate pair and never loses one to it, the caps, the declines, the age bound, and
//! the replay across sidecars. The bus is driven by hand (no process): a tick is `deliver`ed, the sidecar's command
//! pipe is a recording `Wire`.

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

const NOW: i64 = 1_760_000_000_000;

fn ticks_and_acks() -> Vec<String> {
    vec![FEATURE_TICKS.to_string(), FEATURE_TICK_ACKS.to_string()]
}

fn ticks_only() -> Vec<String> {
    vec![FEATURE_TICKS.to_string()]
}

/// A sidecar's command pipe that records what was written and can be closed.
#[derive(Default)]
struct Wire {
    sent: Mutex<Vec<String>>,
    closed: AtomicBool,
}

impl Wire {
    fn lines(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

impl LinePort for Wire {
    fn send(&self, line: &str) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        self.sent.lock().unwrap().push(line.to_string());
        true
    }
}

fn subscribe(symbols: &[&str]) -> String {
    encode_line(&Command::Subscribe { symbols: symbols.iter().map(|s| (*s).to_string()).collect() })
}

fn unsubscribe(symbols: &[&str]) -> String {
    encode_line(&Command::Unsubscribe {
        symbols: symbols.iter().map(|s| (*s).to_string()).collect(),
    })
}

/// Poll `ready` for up to ten seconds; the failure names what never happened.
fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A bus with a sidecar attached and a rate-age the test chooses.
fn bus_with(features: &[String], rate_age: Duration) -> (Arc<QuoteBus>, Arc<Wire>, u64) {
    let bus = QuoteBus::tuned(Tuning { rate_age, ..Tuning::PRODUCTION });
    let wire = Arc::new(Wire::default());
    let id = bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, features);
    (bus, wire, id)
}

/// A jar that answers every `subscribe` (so a want waits for its answer: use [`plain_bus`] when a test wants a
/// consumer's pairs without scripting those answers).
fn bus() -> (Arc<QuoteBus>, Arc<Wire>, u64) {
    bus_with(&ticks_and_acks(), Duration::from_millis(MAX_TICK_AGE_MS as u64))
}

/// A jar of the release before the answers: it ticks and does not answer a `subscribe`, so nothing waits.
fn plain_bus() -> (Arc<QuoteBus>, Arc<Wire>, u64) {
    bus_with(&ticks_only(), Duration::from_millis(MAX_TICK_AGE_MS as u64))
}

/// A tick the bus accepts: its own time is the clock's.
fn tick(bus: &QuoteBus, symbol: &str, bid: f64, ask: f64) {
    bus.deliver(symbol, bid, ask, NOW, NOW);
}

/// Ask for `symbol`'s tick on a thread, deliver one once the `subscribe` is on the wire, and return what the
/// thread got.
fn ask_and_answer(
    bus: &Arc<QuoteBus>,
    wire: &Arc<Wire>,
    symbol: &'static str,
    bid: f64,
    ask: f64,
) -> Result<PriceTick, String> {
    let asker = {
        let bus = Arc::clone(bus);
        std::thread::spawn(move || bus.fresh_tick(symbol, Duration::from_secs(10)))
    };
    wait_for("the subscribe", || wire.lines().contains(&subscribe(&[symbol])));
    // The jar answers a subscribe before it ticks.
    bus.subscription(symbol, true, "");
    tick(bus, symbol, bid, ask);
    asker.join().expect("the asker did not panic")
}

// ---- the refusals in words ------------------------------------------------------------------------

#[test]
fn no_sidecar_and_an_old_jar_are_refused_in_words_and_subscribe_nothing() {
    let bus = QuoteBus::new();
    let why = bus.fresh_tick("USDJPY", Duration::from_millis(10)).expect_err("nothing is attached");
    assert!(why.contains("no JForex sidecar is running") && why.contains("USDJPY"), "{why}");

    let wire = Arc::new(Wire::default());
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &[]);
    let why = bus.fresh_tick("USDJPY", Duration::from_millis(10)).expect_err("an old jar");
    assert!(why.contains("predates the price lane"), "{why}");
    assert!(wire.lines().is_empty(), "an old jar is never written to: {:?}", wire.lines());

    for bad in ["USD/JPY", "USDJPYX", ""] {
        let why = bus.fresh_tick(bad, Duration::from_millis(10)).expect_err(bad);
        assert!(why.contains("six-letter"), "{bad}: {why}");
    }
}

#[test]
fn no_tick_in_time_is_refused_in_words_and_the_pair_stays_held() {
    let (bus, wire, _) = bus();
    let why = bus.fresh_tick("USDJPY", Duration::from_millis(60)).expect_err("a quiet market");
    assert!(why.contains("no tick of `USDJPY` arrived") && why.contains("closed market"), "{why}");
    assert_eq!(wire.lines(), [subscribe(&["USDJPY"])]);
    // It stays held (the next ask finds the same subscription): the tick that arrives later answers it, and
    // nothing is subscribed twice.
    tick(&bus, "USDJPY", 150.09, 150.11);
    let got = bus.fresh_tick("USDJPY", Duration::from_millis(60)).expect("now there is one");
    assert_eq!((got.symbol.as_str(), got.bid, got.ask, got.ts), ("USDJPY", 150.09, 150.11, NOW));
    assert_eq!(wire.lines(), [subscribe(&["USDJPY"])]);
}

// ---- the tick is the bus's own judgement of one -----------------------------------------------------

#[test]
fn the_first_ask_subscribes_waits_and_returns_the_tick_and_the_next_is_answered_from_the_bus() {
    let (bus, wire, _) = bus();
    let got = ask_and_answer(&bus, &wire, "USDJPY", 150.09, 150.11).expect("a tick");
    assert_eq!(got, PriceTick { symbol: "USDJPY".into(), bid: 150.09, ask: 150.11, ts: NOW });
    assert_eq!(wire.lines(), [subscribe(&["USDJPY"])], "exactly one subscribe");

    let again = bus.fresh_tick("usdjpy", Duration::from_millis(10)).expect("kept, young");
    assert_eq!(again, got);
    assert_eq!(wire.lines().len(), 1, "a young tick costs the sidecar nothing");

    // a newer tick replaces the kept one
    bus.deliver("USDJPY", 150.19, 150.21, NOW + 5, NOW + 5);
    assert_eq!(bus.fresh_tick("USDJPY", Duration::ZERO).expect("newer").bid, 150.19);
}

#[test]
fn a_tick_the_bus_would_not_quote_is_never_a_rate() {
    let (bus, _wire, _) = bus();
    bus.fresh_tick("USDJPY", Duration::from_millis(1)).expect_err("subscribes it, no tick yet");
    for (bid, ask, time, now) in [
        (0.0, 150.1, NOW, NOW),                         // one-sided
        (150.2, 150.1, NOW, NOW),                       // crossed
        (150.0, 150.1, 0, NOW),                         // no time of its own
        (150.0, 150.1, NOW - MAX_TICK_AGE_MS - 1, NOW), // stale at arrival
        (150.0, 150.1, NOW + MAX_TICK_AGE_MS + 1, NOW), // from the future
        (f64::NAN, 150.1, NOW, NOW),
    ] {
        bus.deliver("USDJPY", bid, ask, time, now);
        let why = bus.fresh_tick("USDJPY", Duration::from_millis(1)).expect_err("not a quote");
        assert!(why.contains("no tick of `USDJPY`"), "{bid} {ask} {time}: {why}");
    }
}

/// A kept tick stops being a rate as time passes, so a tick that was fine does not stay one for ever when the feed
/// goes quiet (the market closes, the sidecar's subscription is lost).
#[test]
fn a_tick_older_than_the_rate_age_is_not_returned() {
    // 600 ms of room for the "young" read on a loaded runner; the sleep is past the bound, and a longer one only
    // makes the refusal surer
    let (bus, _wire, _) = bus_with(&ticks_and_acks(), Duration::from_millis(600));
    bus.fresh_tick("USDJPY", Duration::from_millis(1)).expect_err("subscribed");
    tick(&bus, "USDJPY", 150.09, 150.11);
    assert!(bus.fresh_tick("USDJPY", Duration::ZERO).is_ok(), "young");
    std::thread::sleep(Duration::from_millis(800));
    let why = bus.fresh_tick("USDJPY", Duration::from_millis(5)).expect_err("old");
    assert!(why.contains("no tick of `USDJPY`"), "{why}");
}

/// **The bound is on the tick's OWN stamp, not on the time since this box took it**: a tick that was already 1500 ms
/// old when it arrived is good for 500 ms more under a 2000 ms bound, where counting only the time since arrival
/// would have kept it a rate for 2000 ms more (twice the bound in all: ten and twenty seconds in production). The
/// numbers are sized for a loaded runner: the tick must still be a rate when it is asked for straight after it
/// arrived (500 ms of room for a stalled thread), and refused after a 700 ms sleep that is far under the bound by
/// itself (a longer stall only makes the refusal surer).
#[test]
fn a_kept_tick_is_judged_on_its_own_stamp_so_it_is_never_older_than_the_bound() {
    let (bus, _wire, _) = bus_with(&ticks_and_acks(), Duration::from_millis(2_000));
    bus.fresh_tick("USDJPY", Duration::from_millis(1)).expect_err("subscribed, no tick yet");
    bus.deliver("USDJPY", 150.09, 150.11, NOW - 1_500, NOW);
    assert!(
        bus.fresh_tick("USDJPY", Duration::ZERO).is_ok(),
        "1500 ms old on arrival, taken just now"
    );
    std::thread::sleep(Duration::from_millis(700));
    let why =
        bus.fresh_tick("USDJPY", Duration::from_millis(5)).expect_err("2200 ms old by its stamp");
    assert!(why.contains("no tick of `USDJPY`"), "{why}");
    // a tick stamped a little AHEAD of the clock is as young as it gets, never negative
    bus.deliver("USDJPY", 150.09, 150.11, NOW + 5, NOW);
    assert!(bus.fresh_tick("USDJPY", Duration::ZERO).is_ok());
}

// ---- the consumer and the rate reader never take each other's place ---------------------------------

#[test]
fn a_rate_pair_is_never_forwarded_to_the_consumer_and_its_own_pairs_still_are() {
    let (bus, wire, _) = plain_bus();
    let got: Arc<Mutex<Vec<PriceTick>>> = Arc::default();
    let sink = Arc::clone(&got);
    let listening = bus.listen(Arc::new(move |t: &PriceTick| sink.lock().unwrap().push(t.clone())));
    listening.want("EURUSD").expect("wanted");
    ask_and_answer(&bus, &wire, "USDJPY", 150.09, 150.11).expect("a rate tick");
    bus.subscription("EURUSD", true, "");
    tick(&bus, "EURUSD", 1.1, 1.1002);
    wait_for("the consumer's tick", || !got.lock().unwrap().is_empty());
    std::thread::sleep(Duration::from_millis(50));
    let symbols: Vec<String> = got.lock().unwrap().iter().map(|t| t.symbol.clone()).collect();
    assert_eq!(symbols, ["EURUSD"], "the consumer saw its own pair and nothing of the rate's");
}

/// The consumer's `unwant`, a replaced consumer and a dropped one stop THEIR pairs at the sidecar, never the pair
/// the rate reader holds.
#[test]
fn a_pair_the_rate_reader_holds_survives_the_consumer_letting_go() {
    let (bus, wire, _) = plain_bus();
    let listening = bus.listen(Arc::new(|_: &PriceTick| {}));
    ask_and_answer(&bus, &wire, "USDJPY", 150.09, 150.11).expect("a rate tick");

    // the consumer takes the same pair (shared: nothing new is written) and lets it go
    listening.want("USDJPY").expect("shared with the rate reader");
    assert_eq!(wire.lines(), [subscribe(&["USDJPY"])], "the sidecar already prices it");
    listening.unwant("USDJPY");
    assert_eq!(wire.lines().len(), 1, "no unsubscribe: the rate reader still holds it");

    // a pair only the consumer holds IS stopped by the same calls; a replaced consumer stops its own, not the rate's
    listening.want("EURUSD").expect("wanted");
    let newer = bus.listen(Arc::new(|_: &PriceTick| {}));
    assert_eq!(wire.lines().last(), Some(&unsubscribe(&["EURUSD"])), "{:?}", wire.lines());
    assert!(!wire.lines().contains(&unsubscribe(&["USDJPY"])));
    newer.want("GBPUSD").expect("wanted");
    drop(newer); // the stream closing stops what it asked
    assert_eq!(wire.lines().last(), Some(&unsubscribe(&["GBPUSD"])));
    assert!(!wire.lines().iter().any(|l| l.contains("unsubscribe") && l.contains("USDJPY")));

    // and the rate pair is still answered
    tick(&bus, "USDJPY", 150.29, 150.31);
    assert_eq!(bus.fresh_tick("USDJPY", Duration::ZERO).expect("still held").bid, 150.29);
}

// ---- the caps --------------------------------------------------------------------------------------

/// The rate reader's pairs are a cache of at most four: a fifth asked pair gives the least recently ASKED one back
/// (an `unsubscribe`, then the `subscribe`), where it used to be refused until the process restarted.
#[test]
fn a_fifth_conversion_pair_gives_the_least_recently_asked_one_back() {
    let (bus, wire, _) = plain_bus();
    for pair in ["USDJPY", "GBPUSD", "USDCHF", "USDCAD"] {
        bus.fresh_tick(pair, Duration::from_millis(1)).expect_err("subscribed, no tick yet");
    }
    // asking USDJPY again makes GBPUSD the oldest
    bus.fresh_tick("USDJPY", Duration::from_millis(1)).expect_err("still no tick; kept");
    bus.fresh_tick("AUDUSD", Duration::from_millis(1))
        .expect_err("a fifth: subscribed, no tick yet");
    assert_eq!(
        wire.lines(),
        [
            subscribe(&["USDJPY"]),
            subscribe(&["GBPUSD"]),
            subscribe(&["USDCHF"]),
            subscribe(&["USDCAD"]),
            unsubscribe(&["GBPUSD"]),
            subscribe(&["AUDUSD"]),
        ]
    );
    let held: Vec<String> = bus.state().rated.keys().cloned().collect();
    assert_eq!(held, ["AUDUSD", "USDCAD", "USDCHF", "USDJPY"]);
}

/// A pair somebody is waiting on is not given back: with four lookups each waiting for its tick, a fifth is refused
/// in words ("ask again"), and nothing is unsubscribed from under the four.
#[test]
fn a_conversion_pair_with_a_lookup_waiting_on_it_is_not_given_back() {
    let (bus, wire, _) = plain_bus();
    let pairs = ["USDJPY", "GBPUSD", "USDCHF", "USDCAD"];
    let askers: Vec<_> = pairs
        .iter()
        .map(|pair| {
            let bus = Arc::clone(&bus);
            let pair = *pair;
            std::thread::spawn(move || bus.fresh_tick(pair, Duration::from_secs(10)))
        })
        .collect();
    wait_for("the four subscribes", || wire.lines().len() == 4);
    let why =
        bus.fresh_tick("AUDUSD", Duration::from_millis(1)).expect_err("a fifth, all waited on");
    assert!(why.contains("4 conversion pairs are being priced right now"), "{why}");
    assert_eq!(wire.lines().len(), 4, "no unsubscribe, no fifth subscribe: {:?}", wire.lines());
    for pair in pairs {
        tick(&bus, pair, 1.5, 1.6);
    }
    for asker in askers {
        asker.join().unwrap().expect("a tick");
    }
    // once nobody waits, the fifth gets its place
    bus.fresh_tick("AUDUSD", Duration::from_millis(1)).expect_err("subscribed, no tick yet");
    assert_eq!(wire.lines().len(), 6, "{:?}", wire.lines());
}

/// A conversion pair nobody has asked a rate of for the idle time is given back to the sidecar the next time the
/// rate reader needs anything (no thread of its own: the sweep is the next ask).
#[test]
fn an_idle_conversion_pair_is_unsubscribed_at_the_next_ask() {
    let bus =
        QuoteBus::tuned(Tuning { rate_idle: Duration::from_millis(40), ..Tuning::PRODUCTION });
    let wire = Arc::new(Wire::default());
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &ticks_only());
    bus.fresh_tick("USDJPY", Duration::from_millis(1)).expect_err("subscribed, no tick yet");
    tick(&bus, "USDJPY", 150.09, 150.11);
    assert!(bus.fresh_tick("USDJPY", Duration::ZERO).is_ok(), "an ask keeps it");
    std::thread::sleep(Duration::from_millis(80));
    bus.fresh_tick("GBPUSD", Duration::from_millis(1)).expect_err("subscribed, no tick yet");
    assert_eq!(
        wire.lines(),
        [subscribe(&["USDJPY"]), unsubscribe(&["USDJPY"]), subscribe(&["GBPUSD"])]
    );
    assert!(!bus.state().last.contains_key("USDJPY"), "its kept tick went with it");
    let held: Vec<String> = bus.state().rated.keys().cloned().collect();
    assert_eq!(held, ["GBPUSD"]);
}

/// **The consumer keeps all eight of its own places whatever the rate reader holds**: its `want` finds the idle
/// conversion pairs unsubscribed (oldest first) to make room, and a pair it shares with the rate reader is not one of
/// them. Where this used to refuse the fifth want while four rate pairs were held.
#[test]
fn the_consumer_can_still_have_all_eight_of_its_own_places() {
    let (bus, wire, _) = plain_bus();
    for pair in ["USDJPY", "GBPUSD", "USDCHF", "USDCAD"] {
        bus.fresh_tick(pair, Duration::from_millis(1)).expect_err("subscribed, no tick yet");
    }
    let listening = bus.listen(Arc::new(|_: &PriceTick| {}));
    let own = ["EURUSD", "EURGBP", "EURJPY", "NZDUSD", "AUDJPY", "GBPJPY", "CHFJPY", "AUDUSD"];
    for pair in own {
        listening.want(pair).expect(pair);
    }
    assert_eq!(bus.state().wanted.len(), 8);
    assert!(
        bus.state().rated.is_empty(),
        "every conversion pair yielded: {:?}",
        bus.state().rated.keys()
    );
    // the four oldest rate pairs were unsubscribed, in the order they were asked, each BEFORE the want that needed it
    let lines = wire.lines();
    let at = |line: String| lines.iter().position(|l| *l == line).expect("on the wire");
    for (rate, wanted) in
        [("USDJPY", "AUDJPY"), ("GBPUSD", "GBPJPY"), ("USDCHF", "CHFJPY"), ("USDCAD", "AUDUSD")]
    {
        assert!(
            at(unsubscribe(&[rate])) < at(subscribe(&[wanted])),
            "{rate} before {wanted}: {lines:?}"
        );
    }
    // the ninth is refused as ever
    let why = listening.want("USDJPY").expect_err("nine");
    assert!(why.contains("at most 8 instruments"), "{why}");
}

/// A pair the consumer also holds is never the one given back for the consumer's own want: it is not the rate
/// reader's alone.
#[test]
fn a_pair_shared_with_the_consumer_is_not_given_back_for_the_consumers_want() {
    let (bus, wire, _) = plain_bus();
    let listening = bus.listen(Arc::new(|_: &PriceTick| {}));
    listening.want("USDJPY").expect("wanted");
    bus.fresh_tick("USDJPY", Duration::from_millis(1)).expect_err("shared, no tick yet");
    for pair in ["EURGBP", "EURJPY", "NZDUSD", "AUDJPY", "GBPJPY", "CHFJPY", "AUDUSD"] {
        listening.want(pair).expect(pair);
    }
    assert_eq!(bus.state().wanted.len(), 8);
    assert!(!wire.lines().iter().any(|l| *l == unsubscribe(&["USDJPY"])));
    assert!(bus.state().rated.contains_key("USDJPY"), "still the rate reader's too");
}

#[test]
fn the_eighth_place_is_refused_to_the_rate_reader_too() {
    let (bus, _wire, _) = plain_bus();
    let listening = bus.listen(Arc::new(|_: &PriceTick| {}));
    for pair in ["EURUSD", "EURGBP", "EURJPY", "NZDUSD", "AUDJPY", "GBPJPY", "CHFJPY", "AUDUSD"] {
        listening.want(pair).expect(pair);
    }
    let why = bus.fresh_tick("USDJPY", Duration::from_millis(1)).expect_err("all eight taken");
    assert!(why.contains("at most 8 instruments"), "{why}");
    // a pair the consumer already holds is shared, not counted twice
    bus.fresh_tick("EURUSD", Duration::from_millis(1)).expect_err("shared, no tick yet");
}

// ---- declines, a closed pipe, a vanished sidecar ----------------------------------------------------

#[test]
fn a_decline_is_the_refusal_and_frees_the_place() {
    let (bus, wire, _) = bus();
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || bus.fresh_tick("USDJPY", Duration::from_secs(10)))
    };
    wait_for("the subscribe", || !wire.lines().is_empty());
    bus.subscription("USDJPY", false, "unknown instrument");
    let why = asker.join().unwrap().expect_err("declined");
    assert!(
        why.contains("declined to price `USDJPY`") && why.contains("unknown instrument"),
        "{why}"
    );
    assert!(bus.state().rated.is_empty(), "the place it took is free");
    assert!(bus.state().last.is_empty());
}

#[test]
fn a_closed_pipe_is_a_refusal_and_holds_nothing() {
    let (bus, wire, _) = bus();
    wire.closed.store(true, Ordering::SeqCst);
    let why = bus.fresh_tick("USDJPY", Duration::from_millis(10)).expect_err("the pipe is gone");
    assert!(why.contains("sidecar is gone"), "{why}");
    assert!(bus.state().rated.is_empty());
    assert!(bus.state().sidecar.is_none());
}

#[test]
fn a_sidecar_that_detaches_while_a_tick_is_awaited_ends_the_wait_in_words() {
    let (bus, wire, id) = bus();
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || bus.fresh_tick("USDJPY", Duration::from_secs(10)))
    };
    wait_for("the subscribe", || !wire.lines().is_empty());
    bus.detach(id);
    let why = asker.join().unwrap().expect_err("gone");
    assert!(why.contains("went away"), "{why}");
}

/// A pair the rate reader holds is held across a sidecar, exactly as a want is: the next one is told.
#[test]
fn the_rate_pairs_are_replayed_to_the_next_sidecar() {
    let (bus, wire, id) = bus();
    bus.fresh_tick("USDJPY", Duration::from_millis(1)).expect_err("subscribed");
    bus.detach(id);
    let next = Arc::new(Wire::default());
    bus.attach(Arc::clone(&next) as Arc<dyn LinePort>, &ticks_and_acks());
    assert_eq!(next.lines(), [subscribe(&["USDJPY"])]);
    assert_eq!(wire.lines().len(), 1);
    // an old jar gets no replay (it would log an unknown command and never answer it)
    let old = Arc::new(Wire::default());
    bus.attach(Arc::clone(&old) as Arc<dyn LinePort>, &[]);
    assert!(old.lines().is_empty());
}

#[test]
fn a_jar_that_does_not_answer_a_subscribe_is_waited_on_for_the_tick_alone() {
    let (bus, wire, _) = bus_with(&ticks_only(), Duration::from_secs(10));
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || bus.fresh_tick("USDJPY", Duration::from_secs(10)))
    };
    wait_for("the subscribe", || !wire.lines().is_empty());
    assert!(bus.state().awaiting.is_empty(), "no answer is expected from this jar");
    tick(&bus, "USDJPY", 150.09, 150.11);
    assert_eq!(asker.join().unwrap().expect("a tick").bid, 150.09);
}

// ---- the account's currency -------------------------------------------------------------------------

#[test]
fn the_account_currency_is_what_the_attached_sidecar_stated_and_nothing_else() {
    let bus = QuoteBus::new();
    assert_eq!(bus.account_currency(), None, "no sidecar");
    assert!(!bus.sidecar_attached());
    let wire = Arc::new(Wire::default());
    let id = bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &ticks_and_acks());
    assert!(bus.sidecar_attached(), "attached, though it states no currency");
    assert_eq!(bus.account_currency(), None, "a jar that stated none is UNKNOWN, never USD");

    bus.set_account_currency(id, Some(" eur "));
    assert_eq!(bus.account_currency().as_deref(), Some("EUR"));
    // a statement about a sidecar that is not the attached one is no statement
    bus.set_account_currency(id + 100, Some("GBP"));
    assert_eq!(bus.account_currency().as_deref(), Some("EUR"));
    // a restated absence clears it
    bus.set_account_currency(id, None);
    assert_eq!(bus.account_currency(), None);

    // a word that is no currency code leaves it unknown, and says so
    for bad in ["", "  ", "US", "USDX", "us1", "€€€", "U$D"] {
        bus.set_account_currency(id, Some("USD"));
        bus.set_account_currency(id, Some(bad));
        assert_eq!(bus.account_currency(), None, "{bad:?}");
    }

    // it dies with the sidecar, and the next one states its own
    bus.set_account_currency(id, Some("USD"));
    bus.detach(id);
    assert_eq!(bus.account_currency(), None);
    let next = bus.attach(Arc::new(Wire::default()) as Arc<dyn LinePort>, &ticks_and_acks());
    assert_eq!(bus.account_currency(), None, "a new sidecar has stated nothing yet");
    bus.set_account_currency(next, Some("CHF"));
    assert_eq!(bus.account_currency().as_deref(), Some("CHF"));
}
