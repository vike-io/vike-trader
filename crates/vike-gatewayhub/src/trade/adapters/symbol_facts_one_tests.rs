//! `SymbolFacts` with a one-symbol source: which asks are read one symbol at a time, what is kept and
//! single-flighted per symbol, that a slow fetch holds up nobody else, and that a failed or half read is a
//! failure (briefly remembered, worded as one) and never "no such symbol". No network: counting closures.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::facts_refusal_is_transient;
use vike_model::{Contract, Instrument, Margining, PerpSpec, SymbolProperties};

use super::{Bucket, InstrumentList, LOOKUP_BURST, MAX_SLOTS, Slot, SymbolFacts, SymbolLister};

fn grid() -> SymbolProperties {
    SymbolProperties {
        tick_size: 0.01,
        step_size: 0.001,
        min_qty: 0.001,
        min_notional: 5.0,
        ..Default::default()
    }
}

fn spot(venue: &str, symbol: &str) -> Instrument {
    Instrument::new(venue, symbol, "BASE", "USDT", Contract::CryptoSpot).with_properties(grid())
}

fn perp(venue: &str, symbol: &str) -> Instrument {
    Instrument::new(
        venue,
        format!("{symbol}.P"),
        "BASE",
        "USDT",
        Contract::Perp(PerpSpec {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_ccy: None,
            funding_interval_ms: None,
            settling_ms: None,
        }),
    )
    .with_properties(grid())
}

/// A one-symbol source that answers every symbol with its own row (bybit's is the `.P` perpetual) and
/// records `(venue, symbol)` for each call.
type CallLog = Arc<Mutex<Vec<(String, String)>>>;

fn lister() -> (SymbolLister, CallLog) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let one: SymbolLister = Arc::new(move |venue, symbol| {
        seen.lock().expect("calls").push((venue.to_string(), symbol.to_string()));
        Ok(vec![if venue == "bybit" { perp(venue, symbol) } else { spot(venue, symbol) }])
    });
    (one, calls)
}

/// A whole-list source of two perpetuals that counts its calls.
fn bulk() -> (InstrumentList, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let list: InstrumentList = Arc::new(move |venue| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(vec![perp(venue, "SOLUSDT"), perp(venue, "ETHUSDT")])
    });
    (list, calls)
}

fn called(calls: &Mutex<Vec<(String, String)>>) -> Vec<(String, String)> {
    calls.lock().expect("calls").clone()
}

#[test]
fn a_spot_ask_reads_the_one_symbol_and_never_the_whole_list() {
    let (list, list_calls) = bulk();
    let (one, calls) = lister();
    let facts = SymbolFacts::new(list).with_symbol_lister(one);
    facts.facts("binance", "solusdt").expect("binance spot");
    facts.facts("aster", "SOLUSDT").expect("aster spot");
    assert_eq!(
        called(&calls),
        [
            ("binance".to_string(), "SOLUSDT".to_string()),
            ("aster".to_string(), "SOLUSDT".to_string())
        ]
    );
    assert_eq!(list_calls.load(Ordering::SeqCst), 0, "no whole list was read");
}

#[test]
fn the_perp_lane_stays_one_list_for_every_symbol() {
    let (list, list_calls) = bulk();
    let (one, calls) = lister();
    let facts = SymbolFacts::new(list).with_symbol_lister(one);
    for symbol in ["SOLUSDT.P", "ETHUSDT.P", "solusdt.p", "ETHUSDT.P"] {
        facts.facts("binance", symbol).unwrap_or_else(|e| panic!("{symbol}: {e}"));
    }
    assert_eq!(list_calls.load(Ordering::SeqCst), 1, "one list served every perpetual");
    assert!(called(&calls).is_empty(), "a perpetual is not a one-symbol read");
}

#[test]
fn bybit_is_asked_for_the_bare_symbol_whatever_the_spelling() {
    let (list, list_calls) = bulk();
    let (one, calls) = lister();
    let facts = SymbolFacts::new(list).with_symbol_lister(one);
    for symbol in ["ethusdt", "ETHUSDT", "ETHUSDT.P"] {
        facts.facts("bybit", symbol).unwrap_or_else(|e| panic!("{symbol}: {e}"));
    }
    assert_eq!(
        called(&calls),
        [("bybit".to_string(), "ETHUSDT".to_string())],
        "one slot, one read"
    );
    assert_eq!(list_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_venue_that_cannot_filter_keeps_reading_its_list() {
    let (list, list_calls) = bulk();
    let (one, calls) = lister();
    let facts = SymbolFacts::new(list).with_symbol_lister(one);
    facts.facts("deribit", "SOLUSDT.P").expect("deribit's row comes from the list");
    assert_eq!(list_calls.load(Ordering::SeqCst), 1);
    assert!(called(&calls).is_empty());
}

#[test]
fn a_symbol_is_kept_for_the_keep_time_and_read_again_after_it() {
    let (list, _) = bulk();
    let (one, calls) = lister();
    let facts = SymbolFacts::with_ttl(list, Duration::from_secs(3600)).with_symbol_lister(one);
    for _ in 0..5 {
        facts.facts("binance", "SOLUSDT").expect("kept");
    }
    facts.facts("binance", "ETHUSDT").expect("another symbol is its own read");
    assert_eq!(called(&calls).len(), 2);

    let (list, _) = bulk();
    let (one, calls) = lister();
    let stale = SymbolFacts::with_ttl(list, Duration::ZERO).with_symbol_lister(one);
    stale.facts("binance", "SOLUSDT").expect("first");
    stale.facts("binance", "SOLUSDT").expect("second");
    assert_eq!(called(&calls).len(), 2, "a row past its keep time is read again");
}

/// **A slow read holds up nobody else**: while one symbol's fetch is in flight, an ask for another symbol
/// of the SAME venue, and for another venue, is answered at once. (The venue's whole-list slot is another
/// slot again, so a `.P` ask does not wait either.)
#[test]
fn a_slow_fetch_holds_no_lock_an_unrelated_ask_needs() {
    let (list, _) = bulk();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let started_tx = Mutex::new(started_tx);
    let release_rx = Mutex::new(release_rx);
    let one: SymbolLister = Arc::new(move |venue, symbol| {
        if symbol == "SLOWUSDT" {
            started_tx.lock().expect("started").send(()).expect("signal");
            release_rx.lock().expect("release").recv().expect("released");
        }
        Ok(vec![if venue == "bybit" { perp(venue, symbol) } else { spot(venue, symbol) }])
    });
    let facts = Arc::new(SymbolFacts::new(list).with_symbol_lister(one));

    let slow = {
        let facts = Arc::clone(&facts);
        std::thread::spawn(move || facts.facts("binance", "SLOWUSDT").is_ok())
    };
    started_rx.recv_timeout(Duration::from_secs(10)).expect("the slow fetch is in flight");

    let (done_tx, done_rx) = mpsc::channel();
    let others = {
        let facts = Arc::clone(&facts);
        std::thread::spawn(move || {
            let same_venue = facts.facts("binance", "FASTUSDT").is_ok();
            let other_venue = facts.facts("bybit", "FASTUSDT").is_ok();
            let perp_lane = facts.facts("binance", "SOLUSDT.P").is_ok();
            done_tx.send((same_venue, other_venue, perp_lane)).expect("report");
        })
    };
    let answered = done_rx.recv_timeout(Duration::from_secs(10));
    release_tx.send(()).expect("release the slow fetch");
    assert_eq!(
        answered.expect("unrelated asks must not wait for the slow fetch"),
        (true, true, true)
    );
    assert!(slow.join().expect("slow asker"));
    others.join().expect("other asker");
}

#[test]
fn concurrent_askers_of_one_symbol_share_one_read() {
    let (list, _) = bulk();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let one: SymbolLister = Arc::new(move |venue, symbol| {
        seen.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        Ok(vec![spot(venue, symbol)])
    });
    let facts = Arc::new(SymbolFacts::new(list).with_symbol_lister(one));
    let askers: Vec<_> = (0..4)
        .map(|_| {
            let facts = Arc::clone(&facts);
            std::thread::spawn(move || facts.facts("binance", "SOLUSDT").is_ok())
        })
        .collect();
    assert!(askers.into_iter().all(|a| a.join().expect("asker")));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// "No such symbol" is the venue's answer (a verdict, remembered briefly), not a failure.
#[test]
fn an_empty_answer_is_no_such_instrument_and_is_remembered_only_briefly() {
    let (list, _) = bulk();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let one: SymbolLister = Arc::new(move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    });
    let facts = SymbolFacts::new(list).with_symbol_lister(Arc::clone(&one));
    for _ in 0..5 {
        let err = facts.facts("binance", "NOPEUSDT").expect_err("unknown");
        assert!(
            err.starts_with("binance: no instrument NOPEUSDT in the venue's instrument list"),
            "{err}"
        );
        assert!(!facts_refusal_is_transient(&err), "a verdict about the symbol: {err}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "five asks, one read");

    let (list, _) = bulk();
    let eager = SymbolFacts::with_ttls(list, Duration::from_secs(3600), Duration::ZERO)
        .with_symbol_lister(one);
    eager.facts("binance", "NOPEUSDT").expect_err("unknown");
    eager.facts("binance", "NOPEUSDT").expect_err("unknown again");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "the miss is not kept as a listing for ten minutes"
    );
}

/// **A failed or half read is a failure**: worded as one (transient: the node does not remember it as a
/// verdict on the symbol), remembered for the SHORT failure time, and never kept as the venue's answer.
#[test]
fn a_failed_or_half_read_is_a_transient_failure_never_no_such_symbol() {
    let (list, _) = bulk();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let one: SymbolLister = Arc::new(move |venue, symbol| {
        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
            // what a two-read lookup reports when the second read failed after the first came back empty
            Err("inverse: HTTP 500".to_string())
        } else {
            Ok(vec![perp(venue, symbol)])
        }
    });
    let facts = SymbolFacts::new(list).with_symbol_lister(Arc::clone(&one));
    for _ in 0..5 {
        let err = facts.facts("bybit", "BTCUSD").expect_err("half read");
        assert_eq!(
            err,
            "bybit: the venue's instrument list could not be fetched: inverse: HTTP 500"
        );
        assert!(facts_refusal_is_transient(&err), "{err}");
        assert!(!err.contains("no instrument"), "{err}");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a failing venue is not asked again inside the failure memory"
    );

    // once the failure memory has passed, the next ask reads again and the failure is gone
    calls.store(0, Ordering::SeqCst);
    let (list, _) = bulk();
    let forgetful = SymbolFacts::with_ttls(list, Duration::from_secs(3600), Duration::ZERO)
        .with_symbol_lister(one);
    forgetful.facts("bybit", "BTCUSD").expect_err("first read fails");
    forgetful.facts("bybit", "BTCUSD").expect("the failure was not kept as an answer");
}

/// A flood of distinct symbols cannot grow the slot map without bound once the old ones have expired.
#[test]
fn the_slot_map_is_bounded_by_dropping_expired_slots() {
    let (list, _) = bulk();
    let (one, _) = lister();
    let facts = SymbolFacts::with_ttls(list, Duration::ZERO, Duration::ZERO)
        .with_symbol_lister(one)
        .with_lookup_rate(u32::MAX, Duration::ZERO);
    let mut asked = HashSet::new();
    for n in 0..MAX_SLOTS + 100 {
        let symbol = format!("S{n}USDT");
        facts.facts("binance", &symbol).expect("read");
        asked.insert(symbol);
    }
    let held = facts.slots.lock().expect("slots").len();
    assert!(held <= MAX_SLOTS, "{held} slots held after {} distinct symbols", asked.len());
}

// ---- the rate bound (v0.1.71 review, finding 5) ----

/// The bucket on its own, on a clock the test moves: a burst, nothing back before a refill, one token per
/// refill, and never more than the burst however long it has been idle.
#[test]
fn the_token_bucket_bursts_then_refills_one_per_interval_and_never_over_the_burst() {
    let t0 = Instant::now();
    let refill = Duration::from_secs(1);
    let mut bucket = Bucket { tokens: 3.0, at: t0 };
    assert!((0..3).all(|_| bucket.take(t0, 3, refill)), "the burst");
    assert!(!bucket.take(t0, 3, refill), "empty");
    assert!(!bucket.take(t0 + Duration::from_millis(900), 3, refill), "not yet");
    assert!(bucket.take(t0 + Duration::from_millis(1000), 3, refill), "one token back");
    assert!(!bucket.take(t0 + Duration::from_millis(1000), 3, refill), "and only one");
    let idle = t0 + Duration::from_secs(3600);
    assert!((0..3).all(|_| bucket.take(idle, 3, refill)), "an idle hour refills to the burst");
    assert!(!bucket.take(idle, 3, refill), "...and not past it");
}

/// **A burst of distinct symbols makes at most the burst's worth of upstream requests**, the rest are
/// answered "busy" (transient: the node remembers nothing) with NO network call, and another venue has
/// its own tokens.
#[test]
fn a_burst_of_distinct_symbols_is_cut_off_at_the_venues_burst() {
    let (list, _) = bulk();
    let (one, calls) = lister();
    let facts = SymbolFacts::new(list).with_symbol_lister(one);
    let mut refused = 0;
    for n in 0..LOOKUP_BURST + 5 {
        if let Err(why) = facts.facts("binance", &format!("S{n}USDT")) {
            assert!(facts_refusal_is_transient(&why), "a not-now, never a verdict: {why}");
            refused += 1;
        }
    }
    assert_eq!(refused, 5);
    assert_eq!(
        called(&calls).len(),
        LOOKUP_BURST as usize,
        "the refused five made no upstream call"
    );
    facts.facts("bybit", "ETHUSDT").expect("bybit has its own bucket");
    facts.facts("binance", "S0USDT").expect("a symbol already kept costs no token");
    assert_eq!(called(&calls).len(), LOOKUP_BURST as usize + 1);
}

/// A refused token is "not now": it is not remembered as the symbol's failure, so the next ask after a
/// refill reads upstream instead of being served the refusal for the failure memory's 30 s.
#[test]
fn a_refused_token_is_not_remembered_as_a_failure() {
    let (list, _) = bulk();
    let (one, calls) = lister();
    let facts = SymbolFacts::new(list)
        .with_symbol_lister(one)
        .with_lookup_rate(1, Duration::from_millis(20));
    facts.facts("binance", "AUSDT").expect("the one token");
    let busy = facts.facts("binance", "BUSDT").expect_err("no token left");
    assert!(facts_refusal_is_transient(&busy), "{busy}");
    {
        let slots = facts.slots.lock().expect("slots");
        let slot = slots.get("binance\u{1f}BUSDT").expect("the refused ask's slot");
        let state = slot.lock().expect("slot");
        assert!(state.failed.is_none() && state.listing.is_none(), "nothing was remembered");
    }
    std::thread::sleep(Duration::from_millis(100));
    facts.facts("binance", "BUSDT").expect("a token came back, and the symbol is read");
    assert_eq!(called(&calls).len(), 2);
}

/// **`MAX_SLOTS` is a hard cap**: with every slot still fresh, a new symbol evicts the OLDEST finished
/// slot (it is read again when asked), the newest stay kept.
#[test]
fn a_full_table_of_fresh_slots_evicts_the_oldest_finished_one() {
    let (list, _) = bulk();
    let (one, calls) = lister();
    let facts = SymbolFacts::with_ttl(list, Duration::from_secs(3600))
        .with_symbol_lister(one)
        .with_lookup_rate(u32::MAX, Duration::ZERO);
    let total = MAX_SLOTS + 100;
    for n in 0..total {
        facts.facts("binance", &format!("S{n}USDT")).expect("read");
        assert!(facts.slots.lock().expect("slots").len() <= MAX_SLOTS, "after {n} symbols");
    }
    assert_eq!(called(&calls).len(), total);
    facts.facts("binance", &format!("S{}USDT", total - 1)).expect("the newest is kept");
    assert_eq!(called(&calls).len(), total, "no new read for the newest");
    facts.facts("binance", "S0USDT").expect("the oldest was evicted, so it is read again");
    assert_eq!(called(&calls).len(), total + 1);
}

/// A slot whose read is in flight (its lock is held) is never the one dropped; and when EVERY slot is
/// mid-read the ask is answered "busy" with no upstream call.
#[test]
fn an_in_flight_slot_is_never_evicted_and_a_table_of_them_answers_busy() {
    let (list, _) = bulk();
    let (one, _) = lister();
    let facts = SymbolFacts::with_ttl(list, Duration::from_secs(3600))
        .with_symbol_lister(one)
        .with_lookup_rate(u32::MAX, Duration::ZERO);
    // the oldest slot is held, as a read in flight holds it
    let held_key = "binance\u{1f}HELDUSDT";
    let held: Slot = Arc::default();
    facts.slots.lock().expect("slots").insert(held_key.to_string(), Arc::clone(&held));
    let _in_flight = held.lock().expect("held");
    for n in 0..MAX_SLOTS - 1 {
        facts.facts("binance", &format!("S{n}USDT")).expect("read");
    }
    facts.facts("binance", "NEWUSDT").expect("a finished slot made room");
    assert!(facts.slots.lock().expect("slots").contains_key(held_key), "the in-flight slot stays");

    // now every slot is mid-read
    let (list, _) = bulk();
    let (one, calls) = lister();
    let full = SymbolFacts::new(list).with_symbol_lister(one);
    let slots: Vec<Slot> = (0..MAX_SLOTS).map(|_| Slot::default()).collect();
    for (n, slot) in slots.iter().enumerate() {
        full.slots.lock().expect("slots").insert(format!("binance\u{1f}H{n}"), Arc::clone(slot));
    }
    let _guards: Vec<_> = slots.iter().map(|s| s.lock().expect("slot")).collect();
    let busy = full.facts("binance", "LATEUSDT").expect_err("no slot can be spared");
    assert!(facts_refusal_is_transient(&busy), "{busy}");
    assert!(called(&calls).is_empty(), "and no upstream call was made");
}
