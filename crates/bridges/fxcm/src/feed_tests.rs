//! Tests of `crate::feed`: the judgement of one Offers row, and the desk's polling, caps, idle release and
//! fail-closed rules. The session is SCRIPTED (docs-shaped rows of the shape `forex_connect_offers` writes; nothing here has
//! met a ForexConnect session).

use super::*;
use crate::refusals::{FxcmPriceRefusalId, FxcmSymbolRefusal, FxcmSymbolRefusalId};
use crate::test_support::{PanicsFirst, offer};
use std::sync::atomic::{AtomicUsize, Ordering};
use vike_bridge_core::data_client::bus::DataClientBus;
use vike_data::{DataClient, DataClientError, DataClientSink, RecordingSink};
use vike_log::poll::wait_for;

/// `asked_ms` for the pure tests: any moment, the stamp is `asked - age`.
const ASKED: i64 = 1_800_000_000_000;

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
        PriceTick { symbol: "USDJPY".into(), bid: 150.08, ask: 150.12, timestamp: ASKED - 750 }
    );
}

/// The facts' own refusal a price refusal carries ([`FxcmPriceRefusal::Row`]).
fn row_refusal(refusal: &FxcmPriceRefusal) -> FxcmSymbolRefusalId {
    let FxcmPriceRefusal::Row { why } = refusal else { panic!("not the row's refusal: {refusal}") };
    why.id()
}

/// A quote at the limit is a price; one millisecond past it is not (the limit is the conversion rate's: 120 s).
#[test]
fn a_quote_older_than_two_minutes_by_its_own_time_is_no_price() {
    let at_limit = vec![aged(offer("EUR/USD", 1.0998, 1.1002), 120_000)];
    assert!(judge("EURUSD", &at_limit, ASKED).is_ok());
    let past = vec![aged(offer("EUR/USD", 1.0998, 1.1002), 120_001)];
    let why = judge("EURUSD", &past, ASKED).expect_err("a stale quote");
    assert_eq!(row_refusal(&why), FxcmSymbolRefusalId::QuoteStale, "{why}");
}

/// A row with no time of its own cannot be told from a frozen one: refused, in words.
#[test]
fn a_row_with_no_time_is_no_price() {
    let mut row = offer("EUR/USD", 1.0998, 1.1002);
    row.quote_age_ms = None;
    let why = judge("EURUSD", &[row], ASKED).expect_err("no time");
    assert_eq!(row_refusal(&why), FxcmSymbolRefusalId::QuoteUntimed, "{why}");
}

/// A little ahead of the server clock is jitter (age 0); more than five seconds ahead is a time frame nobody can
/// trust.
#[test]
fn a_quote_from_the_future_is_clamped_within_jitter_and_refused_beyond_it() {
    let jitter = vec![aged(offer("EUR/USD", 1.0998, 1.1002), -5_000)];
    assert_eq!(
        judge("EURUSD", &jitter, ASKED).expect("jitter").timestamp,
        ASKED,
        "never newer than the ask"
    );
    let ahead = vec![aged(offer("EUR/USD", 1.0998, 1.1002), -5_001)];
    let why = judge("EURUSD", &ahead, ASKED).expect_err("ahead");
    assert_eq!(row_refusal(&why), FxcmSymbolRefusalId::QuoteAhead, "{why}");
}

/// A closed market states no live price, whatever the row's bid and ask still say.
#[test]
fn a_closed_market_is_no_price() {
    let mut row = offer("EUR/USD", 1.0998, 1.1002);
    row.trading_status = "C".to_string();
    let why = judge("EURUSD", &[row], ASKED).expect_err("closed");
    let closed = FxcmSymbolRefusal::MarketNotOpen {
        instrument: "EUR/USD".to_string(),
        status: "C".to_string(),
    };
    assert_eq!(why, FxcmPriceRefusal::Row { why: closed });
}

/// A symbol the table does not list, an instrument the login is not subscribed to, a side that is not tradable
/// and a quote that is crossed or one-sided each give no price, each in its own words.
#[test]
fn a_missing_unsubscribed_untradable_or_malformed_row_is_no_price() {
    let why = judge("AUDNZD", &table(), ASKED).expect_err("missing");
    assert_eq!(why, FxcmPriceRefusal::NotListed { symbol: "AUDNZD".to_string() });
    let why = judge("EUR/USD", &table(), ASKED).expect_err("the venue's spelling is no user name");
    assert_eq!(why, FxcmPriceRefusal::NotListed { symbol: "EUR/USD".to_string() });

    let mut view_only = offer("EUR/USD", 1.0998, 1.1002);
    view_only.subscription = "V".to_string();
    let why = judge("EURUSD", &[view_only], ASKED).expect_err("not subscribed");
    assert_eq!(why.id(), FxcmPriceRefusalId::NotSubscribed, "{why}");

    let mut untradable = offer("EUR/USD", 1.0998, 1.1002);
    untradable.ask_tradable = "N".to_string();
    let why = judge("EURUSD", &[untradable], ASKED).expect_err("untradable");
    assert_eq!(row_refusal(&why), FxcmSymbolRefusalId::SideNotTradable);

    let why = judge("EURUSD", &[offer("EUR/USD", 1.1002, 1.0998)], ASKED).expect_err("crossed");
    assert_eq!(row_refusal(&why), FxcmSymbolRefusalId::Crossed);
    let why = judge("EURUSD", &[offer("EUR/USD", 0.0, 1.1002)], ASKED).expect_err("one sided");
    assert_eq!(row_refusal(&why), FxcmSymbolRefusalId::NotTwoSided);
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

/// A desk over `session` at a few milliseconds' pace (production's is two seconds), the kept list held before its first
/// poll (its one-time read would otherwise land inside a timed wait).
fn desk(session: &Arc<Scripted>) -> Arc<PriceDesk> {
    crate::instrument::names::kept_list_for_tests();
    let source: Arc<dyn OffersSource> = Arc::clone(session) as Arc<dyn OffersSource>;
    PriceDesk::paced(source, Duration::from_millis(5), Duration::from_millis(20))
}

/// How long a wait in this file allows before `vike_log::poll::wait_for` fails it.
const WAIT_FOR: Duration = Duration::from_secs(10);

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
    wait_for(WAIT_FOR, "the first tick", || !ticks.all().is_empty());
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
    wait_for(WAIT_FOR, "the renewed ticks", || ticks.all().len() >= 3);
    // the same row, a few seconds on: it now reads 121 s old
    session.set(Ok(vec![aged(offer("EUR/USD", 1.0998, 1.1002), 121_000)]));
    // a request already out when the table changed may still answer with the old one: let it pass
    let polled = session.asked();
    wait_for(WAIT_FOR, "the changed table to be read", || session.asked() >= polled + 2);
    let (after, polled) = (ticks.all().len(), session.asked());
    wait_for(WAIT_FOR, "three more requests", || session.asked() >= polled + 3);
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
    wait_for(WAIT_FOR, "several requests", || session.asked() >= polled + 4);
    assert!(
        ticks.all().is_empty(),
        "no price for a closed, stale or missing pair: {:?}",
        ticks.all()
    );

    session.set(Ok(table()));
    wait_for(WAIT_FOR, "the reopened prices", || ticks.symbols().len() >= 2);
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
    let cap = FXCM.max_streams;
    assert_eq!(pairs.len(), cap + 1);
    for pair in &pairs[..cap] {
        listening.want(pair).unwrap_or_else(|e| panic!("{pair}: {e}"));
    }
    listening.want(&pairs[0]).expect("a pair already wanted costs no place");
    let why = listening.want(&pairs[cap]).expect_err("past the cap");
    assert_eq!(why, FxcmPriceRefusal::AtCap { cap: 16, symbol: pairs[cap].clone() });
    listening.unwant(&pairs[3]);
    listening.want(&pairs[cap]).expect("the released place is free");
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
    wait_for(WAIT_FOR, "the poller to run", || session.asked() >= 2);
    listening.unwant("EURUSD");
    wait_for(WAIT_FOR, "the poller to end", || desk.state().poller.is_none());
    let settled = session.asked();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(session.asked(), settled, "the venue is asked nothing while nothing is wanted");
    listening.want("GBPUSD").unwrap();
    wait_for(WAIT_FOR, "the poller to start again", || session.asked() > settled);
    drop(listening);
    wait_for(WAIT_FOR, "the poller to end with its consumer", || desk.state().poller.is_none());
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
    assert_eq!(
        listening.want("EURUSD").expect_err("no session"),
        FxcmPriceRefusal::SourceNotReady {
            why: "fxcm: no reconcile session is running".to_string()
        }
    );
    assert_eq!(session.asked(), 0);
    // a name the engine could not have spelled (a space, an operator) is refused before anything is asked
    let why = listening.want("EUR USD").expect_err("not an instrument name");
    assert_eq!(why, FxcmPriceRefusal::NotAUserName { symbol: "EUR USD".to_string() });
    assert_eq!(session.asked(), 0);

    *session.ready.lock().unwrap() = Ok(());
    session.set(Err("the shim this process opened exports no `forex_connect_offers`".to_string()));
    listening.want("EURUSD").unwrap();
    wait_for(WAIT_FOR, "two failed reads", || session.asked() >= 2);
    assert!(ticks.all().is_empty());
    session.set(Ok(table()));
    wait_for(WAIT_FOR, "the recovered price", || !ticks.all().is_empty());
}

/// A newer consumer replaces the older, which refuses while the newer one stands; dropping the older touches nothing
/// of the newer's.
#[test]
fn a_newer_consumer_replaces_the_older_which_then_refuses() {
    let session = Scripted::up(table());
    let desk = desk(&session);
    let (older_ticks, newer_ticks) = (Ticks::default(), Ticks::default());
    let older = desk.listen(older_ticks.listener());
    older.want("EURUSD").unwrap();
    let newer = desk.listen(newer_ticks.listener());
    assert_eq!(older.ensure_current(), Err(FxcmPriceRefusal::NotCurrent), "replaced");
    assert!(older.want("GBPUSD").is_err());
    newer.want("GBPUSD").unwrap();
    wait_for(WAIT_FOR, "the newer consumer's price", || !newer_ticks.all().is_empty());
    drop(older);
    assert!(desk.state().displaced.is_empty(), "a hold let go is nobody's to bring back");
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
    wait_for(WAIT_FOR, "the fault to detach the consumer", || bad.ensure_current().is_err());
    let ticks = Ticks::default();
    let good = desk.listen(ticks.listener());
    good.want("EURUSD").unwrap();
    wait_for(WAIT_FOR, "the next consumer's price", || !ticks.all().is_empty());
    drop(good);
    assert!(bad.ensure_current().is_err(), "a free desk is not a faulted consumer's to take");
    assert!(bad.want("EURUSD").is_err());
}

/// **Decision 0193 verdict 4: a replaced consumer comes back, and only to a free desk.** Its wants went with the
/// takeover and stay stopped, and the desk keeps one consumer: a third one takes it from the returned one.
#[test]
fn a_replaced_consumer_comes_back_only_once_the_desk_is_free() {
    let session = Scripted::up(table());
    let desk = desk(&session);
    let older_ticks = Ticks::default();
    let older = desk.listen(older_ticks.listener());
    older.want("EURUSD").unwrap();
    wait_for(WAIT_FOR, "the older consumer's price", || !older_ticks.all().is_empty());
    let newer = desk.listen(Ticks::default().listener());
    assert_eq!(older.ensure_current(), Err(FxcmPriceRefusal::NotCurrent), "replaced");
    assert!(older.want("GBPUSD").is_err(), "the newer one still holds the desk");

    drop(newer);
    let before = older_ticks.all().len();
    older.want("GBPUSD").expect("the desk is free");
    wait_for(WAIT_FOR, "the returned consumer's price", || older_ticks.all().len() > before);
    assert!(
        older_ticks.symbols()[before..].iter().all(|s| s == "GBPUSD"),
        "what it held stays stopped"
    );

    let third_ticks = Ticks::default();
    let third = desk.listen(third_ticks.listener());
    assert!(older.ensure_current().is_err(), "one consumer at a time");
    third.want("USDJPY").unwrap();
    wait_for(WAIT_FOR, "the third consumer's price", || !third_ticks.all().is_empty());
    assert_eq!(third_ticks.symbols().iter().filter(|s| *s != "USDJPY").count(), 0);
}

// ---- the review's findings (F4, F5, F6, F7) ---------------------------------------------------------------

/// A desk over `session` with every clock chosen, the kept list held as [`desk`] holds it.
fn tuned_desk(session: &Arc<Scripted>, pace: Pace) -> Arc<PriceDesk> {
    crate::instrument::names::kept_list_for_tests();
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

/// The lane prices every class the SDK names (the same two tradable sides of the same row), found by its USER name
/// (decision 0290: a metal `XAUUSD`, a CFD `<base>.CFD`, the row's case not the name's); a type the SDK does not name has
/// no user name and no price. (F5 once refused the metals and crypto CFDs here: their lot, contract size and money are
/// now the facts' to state, `account::quote_rate::facts_from_offers`.)
#[test]
fn a_row_of_any_class_the_sdk_names_is_a_price_and_an_unnamed_type_is_not() {
    let row_of = |name: &str, kind: i32| {
        let mut row = offer(name, 2000.0, 2000.5);
        row.instrument_type = kind;
        if kind != 1 {
            row.contract_currency = "USD".to_string(); // a CFD is quoted in its ContractCurrency
        }
        row
    };
    for (name, kind, symbol) in [
        ("XAU/USD", 5, "XAUUSD"),
        ("BTC/USD", 9, "BTCUSD.CFD"),
        ("US30", 2, "US30.CFD"),
        ("USOil", 3, "USOIL.CFD"),
        ("NVDA.ext", 8, "NVDA.EXT.CFD"),
        ("EUR/USD", 1, "EURUSD"),
    ] {
        let tick = judge(symbol, &[row_of(name, kind)], ASKED).expect(name);
        assert_eq!((tick.symbol.as_str(), tick.bid, tick.ask), (symbol, 2000.0, 2000.5));
    }
    for kind in [0, 10, 99] {
        let why = judge("XAUUSD", &[row_of("XAU/USD", kind)], ASKED).expect_err("an unnamed type");
        let missing = FxcmPriceRefusal::NotListed { symbol: "XAUUSD".to_string() };
        assert_eq!(why, missing, "type {kind}");
    }
}

/// F5: an INDICATIVE side (`I`), an empty flag or an unknown one is not a tradable quote.
#[test]
fn an_indicative_or_unflagged_side_is_no_price() {
    for flag in ["I", "", "X"] {
        let mut bid_side = offer("EUR/USD", 1.0998, 1.1002);
        bid_side.bid_tradable = flag.to_string();
        let why = judge("EURUSD", &[bid_side], ASKED).expect_err(flag);
        assert_eq!(row_refusal(&why), FxcmSymbolRefusalId::SideNotTradable, "bid {flag:?}");
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
    wait_for(WAIT_FOR, "the first ticks", || ticks.all().len() >= 3);
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
    wait_for(WAIT_FOR, "the price to return", || ticks.all().len() > frozen_at);
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
    wait_for(WAIT_FOR, "the first ticks", || ticks.all().len() >= 2);
    // closed for longer than the limit, then reopened at the very same prices
    session.set(Ok(vec![closed_row]));
    std::thread::sleep(Duration::from_millis(350));
    let before = ticks.all().len();
    session.set(Ok(open));
    wait_for(WAIT_FOR, "the reopened price", || ticks.all().len() > before + 2);
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
    wait_for(WAIT_FOR, "the first read", || session.asked() >= 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(session.asked(), 1, "a closed market is asked half a second apart, not every 5 ms");
    assert!(desk.state().refused.get("EURUSD").is_some_and(|said| said.closed));

    let before = session.asked();
    listening.want("GBPUSD").unwrap();
    wait_for(WAIT_FOR, "the new pair's read", || session.asked() > before);
    // the market opens: the next slow poll prices both
    session.set(Ok(table()));
    wait_for(WAIT_FOR, "the reopened prices", || ticks.symbols().len() >= 2);
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
    assert!(matches!(desk.step(3), PricePollStep::Done));
    drop(PollerEnd(Arc::downgrade(&desk), 3));
    assert_eq!(desk.state().poller, Some(7), "poller 7's claim stands");
    assert!(matches!(desk.step(7), PricePollStep::Done));
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
    assert!(matches!(desk.step(1), PricePollStep::Done));
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
    wait_for(WAIT_FOR, "poller 2 to end", || desk.state().poller.is_none());
}

/// F-4 (the cold re-review): production order flow never re-sends a want (the tradehub's watcher de-duplicates a
/// repeat subscription), so a poller killed by a panic in the session read must restart ITSELF: every wanted pair
/// would otherwise stay silent until a restart. No repeat `want` here.
#[test]
fn a_poller_killed_by_a_panic_restarts_itself_without_any_new_want() {
    let source = PanicsFirst::first(1, Scripted::up(table()));
    let desk = PriceDesk::tuned(Arc::clone(&source) as Arc<dyn OffersSource>, quick(5, 120_000));
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    listening.want("EURUSD").unwrap();
    wait_for(WAIT_FOR, "the successor's price", || !ticks.all().is_empty());
    assert!(source.spent(), "the first read did panic");
    drop(listening);
}

/// F4/F6: the self-restart is THROTTLED (a deterministic panic is not a tight respawn loop): a poller that dies
/// again inside `restart_every` stays down with its wants in place, and the next `want` of a pair ALREADY wanted
/// (no new place) starts a new one.
#[test]
fn a_poller_that_dies_again_inside_the_throttle_waits_for_a_repeat_want_of_a_wanted_pair() {
    let source = PanicsFirst::first(2, Scripted::up(table()));
    let desk = PriceDesk::tuned(Arc::clone(&source) as Arc<dyn OffersSource>, quick(5, 120_000));
    let ticks = Ticks::default();
    let listening = desk.listen(ticks.listener());
    listening.want("EURUSD").unwrap();
    // read 1 panics (restarted at once), read 2 panics in the successor (inside the 10 s throttle: not restarted)
    wait_for(WAIT_FOR, "both reads to panic and the poller to stay down", || {
        source.spent() && desk.state().poller.is_none()
    });
    assert!(desk.state().wanted.contains("EURUSD"), "its wants outlive it");
    assert!(ticks.all().is_empty());
    // the same pair asked again: no new place, but a poller
    listening.want("EURUSD").unwrap();
    wait_for(WAIT_FOR, "the restarted poller's price", || !ticks.all().is_empty());
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

// ---- the stream's holders (decision 0193: the strategy lane and the recorder behind one client) ----------------

/// The stream the gatewayhub serves on `desk`, delivering into a recording sink.
fn stream_on(desk: &Arc<PriceDesk>) -> (DataClientBus<PriceDesk>, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    (DataClientBus::on(desk, Arc::clone(&sink) as Arc<dyn DataClientSink>), sink)
}

fn quotes_of(sink: &RecordingSink, symbol: &str) -> usize {
    sink.quotes().iter().filter(|q| q.1 == symbol).count()
}

/// **The reading `crates/vike-datahub/src/feeds/venues.rs`'s `EXPLICIT_SHARING` row of fxcm rests on** (decisions
/// 0092 and 0193): `unsubscribe` takes the one pair out of the desk's wanted set. The consumer place and the poller
/// are untouched, the pair left behind keeps its quotes, and the stream carries quotes alone, so no other key of the
/// same symbol exists for a release to disturb.
#[test]
fn releasing_one_quote_key_stops_exactly_that_pair_and_no_other() {
    let session = Scripted::up(table());
    let desk = desk(&session);
    let (mut stream, sink) = stream_on(&desk);
    let eurusd = stream.subscribe_quotes("EURUSD").unwrap();
    stream.subscribe_quotes("USDJPY").unwrap();
    wait_for(WAIT_FOR, "both pairs' quotes", || {
        quotes_of(&sink, "EURUSD") > 0 && quotes_of(&sink, "USDJPY") > 0
    });

    stream.unsubscribe(eurusd);

    {
        let st = desk.state();
        assert_eq!(st.wanted.iter().map(String::as_str).collect::<Vec<_>>(), ["USDJPY"]);
        assert_eq!(st.next_token, 1, "a release makes no second consumer");
        assert_eq!((st.poller, st.next_poller), (Some(1), 1), "and starts no second poller");
    }
    // The read out when the pair was released has ended once the one after it has begun.
    let polled = session.asked();
    wait_for(WAIT_FOR, "a read begun after the release", || session.asked() >= polled + 2);
    let (released, kept) = (quotes_of(&sink, "EURUSD"), quotes_of(&sink, "USDJPY"));
    wait_for(WAIT_FOR, "the pair left behind to go on", || quotes_of(&sink, "USDJPY") > kept);
    assert_eq!(quotes_of(&sink, "EURUSD"), released, "the released pair is not delivered");

    for other in [
        stream.subscribe_bars("EURUSD", "1m"),
        stream.subscribe_trades("EURUSD"),
        stream.subscribe_book("EURUSD"),
        stream.subscribe_depth("EURUSD"),
    ] {
        assert!(matches!(other, Err(DataClientError::Unsupported(_))), "{other:?}");
    }
}

/// **A session that goes away under a held key (the account turned off, the reconcile login lost) is silence said in
/// words.** The key stands, no quote is made up, the retry is held to the failure's pace, the sentence is not repeated
/// by each retry, a NEW pair is refused in the same words, and the price returns with the session with nothing asked
/// again: no second consumer, no second label, because nothing above the desk was told anything (the stream discloses
/// no link state).
#[test]
fn a_session_that_goes_away_under_a_held_key_is_silence_said_once_and_the_key_stands() {
    const BACKOFF_MS: u128 = 50;
    let session = Scripted::up(table());
    let desk = PriceDesk::paced(
        Arc::clone(&session) as Arc<dyn OffersSource>,
        Duration::from_millis(5),
        Duration::from_millis(50),
    );
    let (mut stream, sink) = stream_on(&desk);
    stream.subscribe_quotes("EURUSD").unwrap();
    wait_for(WAIT_FOR, "the first quote", || quotes_of(&sink, "EURUSD") > 0);

    let gone = "fxcm: no reconcile session is running for the default account";
    *session.ready.lock().unwrap() = Err(gone.to_string());
    session.set(Err(gone.to_string()));
    wait_for(WAIT_FOR, "the failure to be said", || desk.state().failure_said.is_some());
    let said_at = desk.state().failure_said.as_ref().map(|(at, _)| *at);
    let quotes = quotes_of(&sink, "EURUSD");

    let (started, reads) = (Instant::now(), session.asked());
    wait_for(WAIT_FOR, "three retries", || session.asked() >= reads + 3);
    let retries = u128::try_from(session.asked() - reads).expect("a small count");
    assert!(retries <= started.elapsed().as_millis() / BACKOFF_MS + 1, "{retries} retries");
    assert_eq!(quotes_of(&sink, "EURUSD"), quotes, "silence: nothing is made up");
    assert_eq!(
        desk.state().failure_said.as_ref().map(|(at, _)| *at),
        said_at,
        "a retry does not say it again"
    );
    assert!(desk.state().wanted.contains("EURUSD"), "the held key stands");

    let why = stream.subscribe_quotes("GBPUSD").expect_err("no session").to_string();
    assert!(why.contains(gone), "{why}");

    *session.ready.lock().unwrap() = Ok(());
    session.set(Ok(table()));
    wait_for(WAIT_FOR, "the price's return", || quotes_of(&sink, "EURUSD") > quotes);
    assert_eq!(desk.state().next_token, 1, "the same consumer");
    assert_eq!(sink.data_delays().len(), 1, "labelled once: nothing above the desk saw a gap");
}

/// A session behind the REAL registry ([`crate::account::reads`]): how often it was read, and its one row's bid.
struct Registered {
    bid: f64,
    reads: AtomicUsize,
}

impl Registered {
    fn with_bid(bid: f64) -> Arc<Self> {
        Arc::new(Self { bid, reads: AtomicUsize::new(0) })
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl crate::account::reads::SessionReads for Registered {
    fn system_properties(&self) -> crate::account::quote_rate::PropertiesRead {
        crate::account::quote_rate::PropertiesRead::Json("{}".to_string())
    }

    fn offers(&self) -> Result<String, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(serde_json::json!([{
            "instrument": "EUR/USD", "bid": self.bid, "ask": self.bid + 0.0004, "digits": 5,
            "instrument_type": 1, "contract_currency": "EUR", "contract_multiplier": 1.0,
            "subscription": "T", "trading_status": "O", "bid_tradable": "T", "ask_tradable": "T",
            "base_unit": 1000, "quote_age_ms": 0
        }])
        .to_string())
    }
}

/// **The retries cost the venue nothing**, over the production source and the real registry: with the session
/// unregistered the poll is a lookup that finds nothing, so the session that went away is asked nothing more, a new
/// pair is refused in the operator's words, and the next login is found by the next poll with no second consumer.
#[test]
fn the_real_source_asks_a_session_that_went_away_nothing_and_finds_the_next_login_by_itself() {
    let first = Registered::with_bid(1.0998);
    let entry = crate::account::reads::register(
        PRICED_ACCOUNT,
        Arc::clone(&first) as Arc<dyn crate::account::reads::SessionReads>,
    );
    let desk = PriceDesk::paced(
        Arc::new(OffersSourceSession),
        Duration::from_millis(5),
        Duration::from_millis(20),
    );
    let (mut stream, sink) = stream_on(&desk);
    stream.subscribe_quotes("EURUSD").expect("a session is registered");
    wait_for(WAIT_FOR, "the first quote", || !sink.quotes().is_empty());

    crate::account::reads::unregister(PRICED_ACCOUNT, &entry);
    let why = stream.subscribe_quotes("GBPUSD").expect_err("no session").to_string();
    assert!(why.contains(&FxcmPriceRefusal::NoSession.to_string()), "{why}");
    wait_for(WAIT_FOR, "the failure to be said", || desk.state().failure_said.is_some());
    let (read, quotes) = (first.reads(), sink.quotes().len());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(first.reads(), read, "a session that went away is asked nothing");
    assert_eq!(sink.quotes().len(), quotes, "silence");
    assert!(desk.state().wanted.contains("EURUSD"), "the held key stands");

    let second = Registered::with_bid(1.0999);
    let entry = crate::account::reads::register(
        PRICED_ACCOUNT,
        Arc::clone(&second) as Arc<dyn crate::account::reads::SessionReads>,
    );
    wait_for(WAIT_FOR, "the next login's quote", || {
        sink.quotes().last().is_some_and(|q| (q.2.bid - 1.0999).abs() < 1e-9)
    });
    assert_eq!(first.reads(), read, "and it stays unasked");
    assert_eq!(desk.state().next_token, 1, "the same consumer");
    crate::account::reads::unregister(PRICED_ACCOUNT, &entry);
}

/// **Every row of the measured Offers table (497 rows) is priced under a key the store accepts**: its USER name
/// (decision 0290: `EURUSD`, `DOGEUSD.CFD`, `NVDA.EXT.CFD`), which the lane's own spelling keeps as it is, and that
/// name is the tick's and so the series' (`kind=quote/venue=fxcm/symbol=<name>`).
#[test]
fn every_measured_name_is_a_canonical_key_the_store_accepts() {
    let mut names = 0;
    for table in [
        include_str!("../tests/fixtures/eur_demo_offers.json"),
        include_str!("../tests/fixtures/offers_classes.json"),
    ] {
        let fixture: serde_json::Value = serde_json::from_str(table).expect("the fixture is JSON");
        let offers = crate::account::quote_rate::parse_offers(&fixture["offers"].to_string())
            .expect("the shim's shape");
        for row in offers {
            let user = crate::instrument::names::user_name_of_offer(&row)
                .unwrap_or_else(|| panic!("{}: no user name", row.instrument));
            let key = canonical(&user).unwrap_or_else(|why| panic!("{why}"));
            assert_eq!(key, user, "the lane keys a user name as it is");
            vike_paths::store_path::refuse_a_path_hostile_symbol(&key)
                .unwrap_or_else(|why| panic!("{}: {why}", row.instrument));
            names += 1;
        }
    }
    assert_eq!(names, 497, "the whole table was read");
}

/// **A quote is asked and keyed by its USER name** (decision 0290): a coin CFD the venue spells `DOGE/USD` is priced
/// under `DOGEUSD.CFD`, however the asker cased it, and the venue's own spelling is refused in words. The tick never
/// carries a slash, so the store is never handed one.
#[test]
fn a_quote_is_keyed_by_its_user_name_and_the_venues_spelling_is_refused() {
    let mut doge = offer("DOGE/USD", 0.1, 0.2);
    doge.instrument_type = 9;
    doge.contract_currency = "USD".to_string();
    let session = Scripted::up(vec![doge]);
    let desk = desk(&session);
    let (mut stream, sink) = stream_on(&desk);
    let why = stream.subscribe_quotes("DOGE/USD").expect_err("no user name").to_string();
    let refused = FxcmPriceRefusal::NotAUserName { symbol: "DOGE/USD".to_string() };
    assert!(why.contains(&refused.to_string()), "{why}");
    stream.subscribe_quotes("dogeusd.cfd").expect("wanted under its user name");
    wait_for(WAIT_FOR, "the quote", || !sink.quotes().is_empty());
    assert_eq!(sink.quotes()[0].1, "DOGEUSD.CFD");
}
