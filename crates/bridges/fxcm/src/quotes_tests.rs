//! Tests of `crate::quotes`: the judgement of one Offers row, and the desk's polling, caps, idle release and
//! fail-closed rules. The session is SCRIPTED (docs-shaped rows of the shape `fc_offers` writes; nothing here has
//! met a ForexConnect session).

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `asked_ms` for the pure tests: any moment, the stamp is `asked - age`.
const ASKED: i64 = 1_800_000_000_000;

fn offer(name: &str, bid: f64, ask: f64) -> Offer {
    Offer {
        instrument: name.to_string(),
        bid,
        ask,
        digits: 5,
        instrument_type: 1,
        contract_currency: name.split('/').next().unwrap_or_default().to_string(),
        contract_multiplier: 1.0,
        subscription: "T".to_string(),
        trading_status: "O".to_string(),
        bid_tradable: "T".to_string(),
        ask_tradable: "T".to_string(),
        base_unit: 1000,
        quote_age_ms: Some(0),
    }
}

fn aged(mut o: Offer, age_ms: i64) -> Offer {
    o.quote_age_ms = Some(age_ms);
    o
}

fn table() -> Vec<Offer> {
    vec![
        offer("EUR/USD", 1.0998, 1.1002),
        offer("USD/JPY", 150.08, 150.12),
        offer("GBP/USD", 1.3498, 1.3502),
        offer("EUR/JPY", 164.98, 165.02),
    ]
}

// ---- the judgement of one row ------------------------------------------------------------------------

/// The first price: both sides as the venue states them, stamped with the quote's OWN moment.
#[test]
fn a_live_row_is_a_two_sided_tick_stamped_with_its_own_moment() {
    let rows = vec![aged(offer("USD/JPY", 150.08, 150.12), 750)];
    let tick = judge("USDJPY", &rows, ASKED).expect("a live quote");
    assert_eq!(
        tick,
        PriceTick { symbol: "USDJPY".into(), bid: 150.08, ask: 150.12, ts: ASKED - 750 }
    );
}

/// A quote at the limit is a price; one millisecond past it is not (the limit is the conversion rate's: 120 s).
#[test]
fn a_quote_older_than_two_minutes_by_its_own_time_is_no_price() {
    let at_limit = vec![aged(offer("EUR/USD", 1.0998, 1.1002), 120_000)];
    assert!(judge("EURUSD", &at_limit, ASKED).is_ok());
    let past = vec![aged(offer("EUR/USD", 1.0998, 1.1002), 120_001)];
    let why = judge("EURUSD", &past, ASKED).expect_err("a stale quote");
    assert!(why.contains("120 s old") || why.contains("limit is 120 s"), "{why}");
}

/// A row with no time of its own cannot be told from a frozen one: refused, in words.
#[test]
fn a_row_with_no_time_is_no_price() {
    let mut row = offer("EUR/USD", 1.0998, 1.1002);
    row.quote_age_ms = None;
    let why = judge("EURUSD", &[row], ASKED).expect_err("no time");
    assert!(why.contains("no time of its own"), "{why}");
}

/// A little ahead of the server clock is jitter (age 0); more than five seconds ahead is a time frame nobody can
/// trust.
#[test]
fn a_quote_from_the_future_is_clamped_within_jitter_and_refused_beyond_it() {
    let jitter = vec![aged(offer("EUR/USD", 1.0998, 1.1002), -5_000)];
    assert_eq!(
        judge("EURUSD", &jitter, ASKED).expect("jitter").ts,
        ASKED,
        "never newer than the ask"
    );
    let ahead = vec![aged(offer("EUR/USD", 1.0998, 1.1002), -5_001)];
    let why = judge("EURUSD", &ahead, ASKED).expect_err("ahead");
    assert!(why.contains("ahead of the server clock"), "{why}");
}

/// A closed market states no live price, whatever the row's bid and ask still say.
#[test]
fn a_closed_market_is_no_price() {
    let mut row = offer("EUR/USD", 1.0998, 1.1002);
    row.trading_status = "C".to_string();
    let why = judge("EURUSD", &[row], ASKED).expect_err("closed");
    assert!(why.contains("not open") && why.contains("`C`"), "{why}");
}

/// A symbol the table does not list, an instrument the login is not subscribed to, a side that is not tradable
/// and a quote that is crossed or one-sided each give no price, each in its own words.
#[test]
fn a_missing_unsubscribed_untradable_or_malformed_row_is_no_price() {
    let why = judge("AUDNZD", &table(), ASKED).expect_err("missing");
    assert!(why.contains("`AUD/NZD` is not in the venue's Offers table"), "{why}");

    let mut view_only = offer("EUR/USD", 1.0998, 1.1002);
    view_only.subscription = "V".to_string();
    let why = judge("EURUSD", &[view_only], ASKED).expect_err("not subscribed");
    assert!(why.contains("not subscribed for trading"), "{why}");

    let mut untradable = offer("EUR/USD", 1.0998, 1.1002);
    untradable.ask_tradable = "N".to_string();
    assert!(
        judge("EURUSD", &[untradable], ASKED).expect_err("untradable").contains("not available")
    );

    assert!(
        judge("EURUSD", &[offer("EUR/USD", 1.1002, 1.0998)], ASKED)
            .expect_err("crossed")
            .contains("crossed")
    );
    assert!(
        judge("EURUSD", &[offer("EUR/USD", 0.0, 1.1002)], ASKED)
            .expect_err("one sided")
            .contains("two-sided")
    );
}

// ---- the desk ------------------------------------------------------------------------------------------

/// The session, scripted: the table it answers (or its failure), a count of the requests made of it, and whether
/// it exists at all.
struct Scripted {
    answer: Mutex<Result<Vec<Offer>, String>>,
    asked: AtomicUsize,
    ready: Mutex<Result<(), String>>,
}

impl Scripted {
    fn up(rows: Vec<Offer>) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(Ok(rows)),
            asked: AtomicUsize::new(0),
            ready: Mutex::new(Ok(())),
        })
    }

    fn set(&self, answer: Result<Vec<Offer>, String>) {
        *self.answer.lock().unwrap() = answer;
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

impl OffersSource for Scripted {
    fn ready(&self) -> Result<(), String> {
        self.ready.lock().unwrap().clone()
    }

    fn offers(&self) -> Result<Vec<Offer>, String> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.answer.lock().unwrap().clone()
    }
}

/// Where the desk's ticks land.
#[derive(Clone, Default)]
struct Ticks(Arc<Mutex<Vec<PriceTick>>>);

impl Ticks {
    fn listener(&self) -> TickListener {
        let sink = Arc::clone(&self.0);
        Arc::new(move |tick: &PriceTick| sink.lock().unwrap().push(tick.clone()))
    }

    fn all(&self) -> Vec<PriceTick> {
        self.0.lock().unwrap().clone()
    }

    fn symbols(&self) -> Vec<String> {
        self.all().into_iter().map(|t| t.symbol).collect()
    }
}

/// A desk over `session` at a few milliseconds' pace (production's is two seconds).
fn desk(session: &Arc<Scripted>) -> Arc<PriceDesk> {
    let source: Arc<dyn OffersSource> = Arc::clone(session) as Arc<dyn OffersSource>;
    PriceDesk::paced(source, Duration::from_millis(5), Duration::from_millis(20))
}

fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// **The first price**: asking a symbol starts the poller, and the quote reaches the consumer with both sides.
/// Only the WANTED symbols are looked up in the table the read returned, never the whole of it.
#[test]
fn a_wanted_symbol_is_priced_from_the_table_and_the_rest_of_it_is_left_alone() {
    let session = Scripted::up(table());
    let desk = desk(&session);
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    assert_eq!(session.asked(), 0, "building and listening ask the venue nothing");
    listening.want("usdjpy").expect("wanted");
    wait_for("the first tick", || !ticks.all().is_empty());
    let first = ticks.all().remove(0);
    assert_eq!((first.symbol.as_str(), first.bid, first.ask), ("USDJPY", 150.08, 150.12));
    assert_eq!(
        ticks.symbols().iter().filter(|s| *s != "USDJPY").count(),
        0,
        "nothing else is priced"
    );
}

/// A quote is priced while it is live by its OWN time and not a moment past the limit: a row that stops moving
/// while the market still says open is delivered at every poll until its age passes 120 s, then no more (the poll
/// that fetched it does not make it fresh).
#[test]
fn a_quote_is_priced_until_its_own_time_is_two_minutes_old_and_no_longer() {
    let session = Scripted::up(vec![aged(offer("EUR/USD", 1.0998, 1.1002), 119_000)]);
    let desk = desk(&session);
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    listening.want("EURUSD").unwrap();
    wait_for("the renewed ticks", || ticks.all().len() >= 3);
    // the same row, a few seconds on: it now reads 121 s old
    session.set(Ok(vec![aged(offer("EUR/USD", 1.0998, 1.1002), 121_000)]));
    // a request already out when the table changed may still answer with the old one: let it pass
    let polled = session.asked();
    wait_for("the changed table to be read", || session.asked() >= polled + 2);
    let (after, polled) = (ticks.all().len(), session.asked());
    wait_for("three more requests", || session.asked() >= polled + 3);
    assert_eq!(ticks.all().len(), after, "a quote past its limit was priced: {:?}", ticks.all());
}

/// A closed market, a stale quote and a missing symbol give no tick; the price returns when the quote is live.
#[test]
fn no_tick_while_the_market_is_closed_the_quote_is_stale_or_the_symbol_is_missing() {
    let mut closed = offer("EUR/USD", 1.0998, 1.1002);
    closed.trading_status = "C".to_string();
    let session = Scripted::up(vec![closed, aged(offer("GBP/USD", 1.3498, 1.3502), 300_000)]);
    let desk = desk(&session);
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    for symbol in ["EURUSD", "GBPUSD", "AUDNZD"] {
        listening.want(symbol).expect("a want is not a price");
    }
    let polled = session.asked();
    wait_for("several requests", || session.asked() >= polled + 4);
    assert!(
        ticks.all().is_empty(),
        "no price for a closed, stale or missing pair: {:?}",
        ticks.all()
    );

    session.set(Ok(table()));
    wait_for("the reopened prices", || ticks.symbols().len() >= 2);
    let mut priced = ticks.symbols();
    priced.sort();
    priced.dedup();
    assert_eq!(priced, ["EURUSD", "GBPUSD"], "the missing pair stays unpriced");
}

/// **The cap**: the 17th pair is refused in words naming the cap, a pair already wanted answers `Ok`, and a place
/// let go is free at once.
#[test]
fn the_cap_refuses_in_words_and_a_released_place_is_free() {
    let session = Scripted::up(table());
    let desk = desk(&session);
    let listening = desk.listen(Ticks::default().listener());
    let pairs: Vec<String> = ('A'..='Q').map(|c| format!("EUR{c}{c}{c}")).collect();
    assert_eq!(pairs.len(), MAX_PRICED_SYMBOLS + 1);
    for pair in &pairs[..MAX_PRICED_SYMBOLS] {
        listening.want(pair).unwrap_or_else(|e| panic!("{pair}: {e}"));
    }
    listening.want(&pairs[0]).expect("a pair already wanted costs no place");
    let why = listening.want(&pairs[MAX_PRICED_SYMBOLS]).expect_err("past the cap");
    assert!(why.contains("16 pairs"), "{why}");
    listening.unwant(&pairs[3]);
    listening.want(&pairs[MAX_PRICED_SYMBOLS]).expect("the released place is free");
}

/// **Idle release**: when nothing is wanted any more the poller ends and the venue is not asked again; a later
/// want starts it afresh.
#[test]
fn letting_go_of_the_last_symbol_stops_the_requests() {
    let session = Scripted::up(table());
    let desk = desk(&session);
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    listening.want("EURUSD").unwrap();
    wait_for("the poller to run", || session.asked() >= 2);
    listening.unwant("EURUSD");
    wait_for("the poller to end", || desk.state().poller.is_none());
    let settled = session.asked();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(session.asked(), settled, "the venue is asked nothing while nothing is wanted");
    listening.want("GBPUSD").unwrap();
    wait_for("the poller to start again", || session.asked() > settled);
    drop(listening);
    wait_for("the poller to end with its consumer", || desk.state().poller.is_none());
}

/// The lane's load is bounded by its pace, whatever the number of symbols: one request per pace at most.
#[test]
fn requests_are_paced_whatever_the_number_of_symbols() {
    let session = Scripted::up(table());
    let source: Arc<dyn OffersSource> = Arc::clone(&session) as Arc<dyn OffersSource>;
    let desk = PriceDesk::paced(source, Duration::from_millis(100), Duration::from_millis(100));
    let listening = desk.listen(Ticks::default().listener());
    for symbol in ["EURUSD", "GBPUSD", "USDJPY", "EURJPY"] {
        listening.want(symbol).unwrap();
    }
    std::thread::sleep(Duration::from_millis(250));
    let asked = session.asked();
    assert!((1..=4).contains(&asked), "four symbols, ~250 ms at a 100 ms pace: {asked} requests");
}

/// **No session, no price**: a want says why, in words, and nothing is asked; a failing read is backed off, said,
/// and recovers when the session answers.
#[test]
fn no_session_refuses_in_words_and_a_failing_read_is_backed_off_then_recovers() {
    let session = Scripted::up(table());
    *session.ready.lock().unwrap() = Err("fxcm: no reconcile session is running".to_string());
    let desk = desk(&session);
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    assert!(listening.want("EURUSD").expect_err("no session").contains("no reconcile session"));
    assert_eq!(session.asked(), 0);
    let why = listening.want("EUR/USD").expect_err("a slash is not the engine's spelling");
    assert!(why.contains("six-letter"), "{why}");

    *session.ready.lock().unwrap() = Ok(());
    session.set(Err("the shim this process opened exports no `fc_offers`".to_string()));
    listening.want("EURUSD").unwrap();
    wait_for("two failed reads", || session.asked() >= 2);
    assert!(ticks.all().is_empty());
    session.set(Ok(table()));
    wait_for("the recovered price", || !ticks.all().is_empty());
}

/// A newer consumer replaces the older, which refuses from then on; dropping the older touches nothing of the
/// newer's.
#[test]
fn a_newer_consumer_replaces_the_older_which_then_refuses() {
    let session = Scripted::up(table());
    let desk = desk(&session);
    let (older_ticks, newer_ticks) = (Ticks::default(), Ticks::default());
    let older = desk.listen(older_ticks.listener());
    older.want("EURUSD").unwrap();
    let newer = desk.listen(newer_ticks.listener());
    assert!(older.ensure_current().expect_err("replaced").contains("replaced by a newer one"));
    assert!(older.want("GBPUSD").is_err());
    newer.want("GBPUSD").unwrap();
    wait_for("the newer consumer's price", || !newer_ticks.all().is_empty());
    drop(older);
    assert!(newer.ensure_current().is_ok(), "dropping the older one left the newer one alone");
    assert!(older_ticks.all().is_empty() || older_ticks.symbols().iter().all(|s| s == "EURUSD"));
    assert_eq!(newer_ticks.symbols().iter().filter(|s| *s != "GBPUSD").count(), 0);
}

/// A consumer that panics is detached with its wants; the poller's thread survives and a new consumer is served.
#[test]
fn a_panicking_consumer_is_detached_and_the_next_one_is_served() {
    let session = Scripted::up(table());
    let desk = desk(&session);
    let bad = desk.listen(Arc::new(|_: &PriceTick| panic!("a consumer fault")));
    bad.want("EURUSD").unwrap();
    wait_for("the fault to detach the consumer", || bad.ensure_current().is_err());
    let ticks = Ticks::default();
    let good = desk.listen(ticks.listener());
    good.want("EURUSD").unwrap();
    wait_for("the next consumer's price", || !ticks.all().is_empty());
}

// ---- the review's findings (F4, F5, F6, F7) ---------------------------------------------------------------

/// A desk over `session` with every clock chosen.
fn tuned_desk(session: &Arc<Scripted>, pace: Pace) -> Arc<PriceDesk> {
    PriceDesk::tuned(Arc::clone(session) as Arc<dyn OffersSource>, pace)
}

fn quick(closed_every_ms: u64, frozen_after_ms: u64) -> Pace {
    Pace {
        poll_every: Duration::from_millis(5),
        failure_backoff: Duration::from_millis(20),
        closed_every: Duration::from_millis(closed_every_ms),
        frozen_after: Duration::from_millis(frozen_after_ms),
        restart_every: Duration::from_secs(10),
    }
}

/// F5: the lane prices a regular forex pair and nothing else, even if the table lists the row as subscribed and
/// open: a metal or a crypto CFD has a lot, a contract size and a money nobody has measured.
#[test]
fn a_directly_subscribed_metal_or_crypto_row_is_no_price() {
    for (name, kind) in [("XAU/USD", 5), ("BTC/USD", 7), ("EUR/USD", 3)] {
        let mut row = offer(name, 2000.0, 2000.5);
        row.instrument_type = kind;
        let symbol = name.replace('/', "");
        let why = judge(&symbol, &[row], ASKED).expect_err(name);
        assert!(why.contains("not a regular forex instrument"), "{name}: {why}");
    }
}

/// F5: an INDICATIVE side (`I`), an empty flag or an unknown one is not a tradable quote.
#[test]
fn an_indicative_or_unflagged_side_is_no_price() {
    for flag in ["I", "", "X"] {
        let mut bid_side = offer("EUR/USD", 1.0998, 1.1002);
        bid_side.bid_tradable = flag.to_string();
        let why = judge("EURUSD", &[bid_side], ASKED).expect_err(flag);
        assert!(why.contains("only `T` is tradable"), "bid {flag:?}: {why}");
        let mut ask_side = offer("EUR/USD", 1.0998, 1.1002);
        ask_side.ask_tradable = flag.to_string();
        assert!(judge("EURUSD", &[ask_side], ASKED).is_err(), "ask {flag:?}");
    }
}

/// F4: the row's age rests on two UNMEASURED clocks. A quote whose bid and ask are bit-identical at every poll for
/// longer than the limit while the market reads open is refused as frozen, whatever its age says (the scripted
/// age is 0 throughout: only the backstop can catch it), and the price returns as soon as either side moves.
#[test]
fn a_quote_that_stands_still_past_the_limit_is_refused_as_frozen_until_it_moves() {
    let session = Scripted::up(vec![offer("EUR/USD", 1.0998, 1.1002)]);
    let desk = tuned_desk(&session, quick(5, 150));
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    listening.want("EURUSD").unwrap();
    wait_for("the first ticks", || ticks.all().len() >= 3);
    // past the limit the ticks stop: let the freeze set in, then watch that nothing more arrives
    std::thread::sleep(Duration::from_millis(400));
    let frozen_at = ticks.all().len();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(ticks.all().len(), frozen_at, "a frozen quote was still delivered");
    assert!(frozen_at < 80, "the freeze took far longer than its limit: {frozen_at} ticks");
    assert!(
        desk.state().refused.contains_key("EURUSD"),
        "the freeze is said (it is a refusal like any other)"
    );
    // either side moving restarts the clock and the price
    session.set(Ok(vec![offer("EUR/USD", 1.0998, 1.1003)]));
    wait_for("the price to return", || ticks.all().len() > frozen_at);
    assert!(desk.state().refused.is_empty(), "priced again, so nothing is refused");
}

/// F4: a closed market restarts the frozen clock: a quote that reopens at the price it closed at is a new quote,
/// not a frozen one.
#[test]
fn a_reopened_market_at_the_same_prices_is_not_a_frozen_quote() {
    let open = vec![offer("EUR/USD", 1.0998, 1.1002)];
    let mut closed_row = offer("EUR/USD", 1.0998, 1.1002);
    closed_row.trading_status = "C".to_string();
    let session = Scripted::up(open.clone());
    let desk = tuned_desk(&session, quick(5, 200));
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    listening.want("EURUSD").unwrap();
    wait_for("the first ticks", || ticks.all().len() >= 2);
    // closed for longer than the limit, then reopened at the very same prices
    session.set(Ok(vec![closed_row]));
    std::thread::sleep(Duration::from_millis(350));
    let before = ticks.all().len();
    session.set(Ok(open));
    wait_for("the reopened price", || ticks.all().len() > before + 2);
}

/// F7: a table that shows every wanted pair's market closed is asked rarely (a weekend is not a poll every two
/// seconds), the closing is said once (no line per poll), a newly wanted pair ends the wait at once, and the price
/// returns with the market.
#[test]
fn a_closed_market_slows_the_poll_and_a_new_pair_ends_the_slow_wait() {
    let closed = |name: &str| {
        let mut row = offer(name, 1.0, 1.1);
        row.trading_status = "C".to_string();
        row
    };
    let session = Scripted::up(vec![closed("EUR/USD"), closed("GBP/USD")]);
    let desk = tuned_desk(&session, quick(500, 120_000));
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    listening.want("EURUSD").unwrap();
    wait_for("the first read", || session.asked() >= 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(session.asked(), 1, "a closed market is asked half a second apart, not every 5 ms");
    assert!(desk.state().refused.get("EURUSD").is_some_and(|said| said.closed));

    let before = session.asked();
    listening.want("GBPUSD").unwrap();
    wait_for("the new pair's read", || session.asked() > before);
    // the market opens: the next slow poll prices both
    session.set(Ok(table()));
    wait_for("the reopened prices", || ticks.symbols().len() >= 2);
    assert!(desk.state().refused.is_empty());
}

/// F6: a poller's exit cannot clear a SUCCESSOR's claim. The old flag was cleared once by the poller's last step and
/// again by its drop, so a `want` between the two started a second poller (double requests, duplicate ticks).
#[test]
fn a_stale_pollers_exit_cannot_clear_a_live_pollers_claim() {
    let session = Scripted::up(table());
    let desk = desk(&session);
    desk.state().poller = Some(7);
    // poller 3 (long gone) takes a last step with nothing wanted, and its guard drops
    assert!(matches!(desk.step(3), Step::Done));
    drop(PollerEnd(Arc::downgrade(&desk), 3));
    assert_eq!(desk.state().poller, Some(7), "poller 7's claim stands");
    assert!(matches!(desk.step(7), Step::Done));
    assert_eq!(desk.state().poller, None, "a poller clears its own claim at its last step");
}

/// F6, DETERMINISTIC (the old storm test could pass with the bug): replay the exact interleaving that started a second
/// poller. Poller 1 decides it is done (its claim cleared in the same hold of the lock); a `want` then starts poller 2;
/// poller 1's guard drops LATE. The guard must leave poller 2's claim alone, and the next `want` must not start a
/// third (poller identities are counted, never reused).
#[test]
fn the_interleaving_that_started_a_second_poller_cannot_happen() {
    let session = Scripted::up(table());
    let desk = desk(&session);
    let listening = desk.listen(Ticks::default().listener());
    // poller 1 is a claim only (no thread): it takes its last step with nothing wanted
    desk.state().poller = Some(1);
    desk.state().next_poller = 1;
    assert!(matches!(desk.step(1), Step::Done));
    assert_eq!(desk.state().poller, None);
    // a want arrives between poller 1's last step and its exit: poller 2 starts
    listening.want("EURUSD").unwrap();
    assert_eq!(desk.state().poller, Some(2));
    // poller 1's guard drops late
    drop(PollerEnd(Arc::downgrade(&desk), 1));
    assert_eq!(desk.state().poller, Some(2), "the late guard did not clear poller 2's claim");
    // so another want finds a poller and starts none
    listening.want("GBPUSD").unwrap();
    listening.want("EURUSD").unwrap();
    assert_eq!(desk.state().next_poller, 2, "no third poller was ever started");
    drop(listening);
    wait_for("poller 2 to end", || desk.state().poller.is_none());
}

/// A session whose first `left` reads panic (a bug in the read), then answers as the scripted one does.
struct Panics {
    inner: Arc<Scripted>,
    left: AtomicUsize,
}

impl Panics {
    fn first(n: usize, inner: Arc<Scripted>) -> Arc<Self> {
        Arc::new(Self { inner, left: AtomicUsize::new(n) })
    }

    fn spent(&self) -> bool {
        self.left.load(Ordering::SeqCst) == 0
    }
}

impl OffersSource for Panics {
    fn ready(&self) -> Result<(), String> {
        self.inner.ready()
    }

    fn offers(&self) -> Result<Vec<Offer>, String> {
        assert!(
            self.left
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_err(),
            "a bug in the session read"
        );
        self.inner.offers()
    }
}

/// F-4 (the cold re-review): production order flow never re-sends a want (the tradehub's watcher de-duplicates a
/// repeat subscription), so a poller killed by a panic in the session read must restart ITSELF: every wanted pair
/// would otherwise stay silent until a restart. No repeat `want` here.
#[test]
fn a_poller_killed_by_a_panic_restarts_itself_without_any_new_want() {
    let source = Panics::first(1, Scripted::up(table()));
    let desk = PriceDesk::tuned(Arc::clone(&source) as Arc<dyn OffersSource>, quick(5, 120_000));
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    listening.want("EURUSD").unwrap();
    wait_for("the successor's price", || !ticks.all().is_empty());
    assert!(source.spent(), "the first read did panic");
    drop(listening);
}

/// F4/F6: the self-restart is THROTTLED (a deterministic panic is not a tight respawn loop): a poller that dies
/// again inside `restart_every` stays down with its wants in place, and the next `want` of a pair ALREADY wanted
/// (no new place) starts a new one.
#[test]
fn a_poller_that_dies_again_inside_the_throttle_waits_for_a_repeat_want_of_a_wanted_pair() {
    let source = Panics::first(2, Scripted::up(table()));
    let desk = PriceDesk::tuned(Arc::clone(&source) as Arc<dyn OffersSource>, quick(5, 120_000));
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    listening.want("EURUSD").unwrap();
    // read 1 panics (restarted at once), read 2 panics in the successor (inside the 10 s throttle: not restarted)
    wait_for("both reads to panic and the poller to stay down", || {
        source.spent() && desk.state().poller.is_none()
    });
    assert!(desk.state().wanted.contains("EURUSD"), "its wants outlive it");
    assert!(ticks.all().is_empty());
    // the same pair asked again: no new place, but a poller
    listening.want("EURUSD").unwrap();
    wait_for("the restarted poller's price", || !ticks.all().is_empty());
}

/// F-5: the poller re-checks its deadline under the lock the wait releases. A `want` that moved `next_poll` after the
/// step read it (and notified before the poller was waiting) must not be slept past: `park` on a STALE deadline
/// returns at once. (On the old `park(Duration)` this slept the full stale thirty seconds.)
#[test]
fn a_poller_does_not_sleep_past_a_deadline_a_want_moved() {
    let session = Scripted::up(table());
    let desk = desk(&session);
    let stale = Instant::now() + Duration::from_secs(30);
    desk.state().next_poll = Some(Instant::now() + Duration::from_millis(100));
    let started = Instant::now();
    desk.park(stale);
    assert!(started.elapsed() < Duration::from_secs(5), "slept on a deadline that had moved");
}
