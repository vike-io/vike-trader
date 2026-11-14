//! The tick pump's frame step ([`pump_frame`] over [`route_frame`]): what reaches the tick lane, and
//! what the depth driver is told, per frame kind — and the timed re-seed over the real fold: its REST
//! snapshot fetched on its own thread while the socket keeps being read, then spliced.

use std::cell::{Cell, RefCell};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use vike_bridge_core::depth::run_depth_session;
use vike_bridge_core::scripted::WebsocketSessionMarketStreamScript;
use vike_bridge_core::stream_health::StreamHealth;
use vike_execution::{Ingest, event_channel};

use super::*;

fn wire(stream: &str, data: &str) -> String {
    format!(r#"{{"stream":"{stream}","data":{data}}}"#)
}

const SPOT_DEPTH: &str = r#"{"e":"depthUpdate","E":1700000000000,"s":"BTCUSDT","U":6,"u":9,"b":[["100.00","1.5"]],"a":[]}"#;

/// A quote and a trade go straight onto the tick lane, labelled with the caller's symbol (the `.P`
/// spelling the core dispatches on), and the driver is told nothing happened to the book.
#[test]
fn a_quote_and_a_trade_go_onto_the_lane_and_leave_the_book_alone() {
    let (events, mut rx) = event_channel(8);
    let ticks = TickSender { ingest: events.ingest.clone() };
    let pump = FeedTickSender::new("binance", "BTCUSDT.P", ticks, Arc::new(AtomicBool::new(false)));
    let mut book = L2Book::new(0.01);
    let ticker = wire(
        "btcusdt@bookTicker",
        r#"{"u":400900217,"s":"BTCUSDT","b":"60000.10","B":"1.50","a":"60000.20","A":"2.25"}"#,
    );
    let trade = wire(
        "btcusdt@trade",
        r#"{"e":"trade","E":1700000000000,"s":"BTCUSDT","t":3000000001,"p":"60000.10","q":"0.001","T":1700000000001,"m":true,"M":true}"#,
    );
    assert_eq!(
        pump_frame(route_frame(&ticker, "BTCUSDT.P", &mut book), &pump),
        WebsocketSessionDepthOperation::Ignored
    );
    assert_eq!(
        pump_frame(route_frame(&trade, "BTCUSDT.P", &mut book), &pump),
        WebsocketSessionDepthOperation::Ignored
    );
    let Ok(Ingest::Quote(q)) = rx.try_recv() else { panic!("the quote is on the lane") };
    assert_eq!(
        (q.venue.as_str(), q.symbol.as_str(), q.quote.bid),
        ("binance", "BTCUSDT.P", 60000.10)
    );
    let Ok(Ingest::Trade(t)) = rx.try_recv() else { panic!("the trade is on the lane") };
    assert_eq!((t.symbol.as_str(), t.trade.price), ("BTCUSDT.P", 60000.10));
}

/// A diff that folds is the book the driver publishes (clocked on receive time); a diff on a book the
/// REST seed never reached is a GAP, so the session ends and the next one seeds after the backoff —
/// never a REST re-seed per frame. Neither puts anything on the lane itself: the driver's `on_book`
/// does.
#[test]
fn a_folded_diff_is_published_and_an_unseeded_one_ends_the_session() {
    let (events, mut rx) = event_channel(8);
    let ticks = TickSender { ingest: events.ingest.clone() };
    let pump = FeedTickSender::new("binance", "BTCUSDT", ticks, Arc::new(AtomicBool::new(false)));
    let diff = wire("btcusdt@depth@100ms", SPOT_DEPTH);

    let mut unseeded = L2Book::new(0.01);
    assert_eq!(
        pump_frame(route_frame(&diff, "BTCUSDT", &mut unseeded), &pump),
        WebsocketSessionDepthOperation::Gap
    );

    let mut seeded = L2Book::new(0.01);
    seeded.apply_snapshot(5, &[L2BookLevel::new(99.0, 1.0)], &[L2BookLevel::new(101.0, 1.0)]);
    assert_eq!(
        pump_frame(route_frame(&diff, "BTCUSDT", &mut seeded), &pump),
        WebsocketSessionDepthOperation::Updated(0)
    );
    assert_eq!(seeded.last_sequence, 9, "the diff folded");
    assert!(rx.try_recv().is_err(), "the pump step itself publishes nothing");
}

// ---- the timed re-seed: the REST snapshot fetched on its own thread, spliced on the open socket ----

/// A one-level REST snapshot at `seq`.
fn snapshot(seq: u64, bid: f64, ask: f64) -> L2Book {
    let mut b = L2Book::new(0.01);
    b.apply_snapshot(seq, &[L2BookLevel::new(bid, 1.0)], &[L2BookLevel::new(ask, 1.0)]);
    b
}

/// A spot `@depth@100ms` diff `U..=u` moving one bid level.
fn spot_diff(first: u64, last: u64, bid: &str, qty: &str) -> String {
    wire(
        "btcusdt@depth@100ms",
        &format!(
            r#"{{"e":"depthUpdate","E":1700000000000,"s":"BTCUSDT","U":{first},"u":{last},"b":[["{bid}","{qty}"]],"a":[]}}"#
        ),
    )
}

/// A spot `@bookTicker` frame and a `@trade` frame, as the venue sends them.
fn quote_frame() -> String {
    wire(
        "btcusdt@bookTicker",
        r#"{"u":400900217,"s":"BTCUSDT","b":"60000.10","B":"1.50","a":"60000.20","A":"2.25"}"#,
    )
}
fn trade_frame() -> String {
    wire(
        "btcusdt@trade",
        r#"{"e":"trade","E":1700000000000,"s":"BTCUSDT","t":3000000001,"p":"60000.10","q":"0.001","T":1700000000001,"m":true,"M":true}"#,
    )
}

/// What one re-seeding session did.
struct Reseeded {
    /// Every book published after the re-seed timer fired, in order.
    published: Vec<Arc<L2Book>>,
    /// What the tick lane carried at the `release` frame, before the snapshot answered.
    lane_at_release: Vec<&'static str>,
    /// How the session ended.
    ended: String,
}

/// One session of the pump's own fold over the scripted `frames`, its quotes and trades going onto a
/// real tick lane. Seeded at its start by a snapshot at id 100; at the timed re-seed a `late` frame
/// makes due, the driver fetches `fresh` on its own thread, and that fetch is held shut until a
/// `release` frame (which first records what the tick lane already carries) and lets a `settle`
/// frame wait until it has answered.
fn reseed_session(fresh: Option<L2Book>, frames: &[String]) -> Reseeded {
    let (events, rx) = event_channel(64);
    let rx = RefCell::new(rx);
    let ticks = TickSender { ingest: events.ingest.clone() };
    let pump = FeedTickSender::new("binance", "BTCUSDT", ticks, Arc::new(AtomicBool::new(false)));
    let clock = AtomicI64::new(0);
    let (release, fetched, calls) =
        (AtomicBool::new(false), AtomicBool::new(false), AtomicUsize::new(0));
    let fresh = Mutex::new(fresh);
    let seed = || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Some(snapshot(100, 99.0, 101.0)); // the session's first seed, on the feed thread
        }
        let open =
            vike_log::poll::wait_until(Duration::from_secs(10), || release.load(Ordering::SeqCst));
        assert!(open, "the test never released the fetch");
        let answer = fresh.lock().unwrap().take();
        fetched.store(true, Ordering::SeqCst);
        answer
    };
    let (published, reseeded) = (RefCell::new(Vec::new()), Cell::new(false));
    let lane_at_release = RefCell::new(Vec::new());
    let mut unseeded = L2Book::new(0.01);
    let mut stream = WebsocketSessionMarketStreamScript::from_texts(frames.iter().cloned());
    let ended = run_depth_session(
        &mut stream,
        None,
        None,
        &[],
        &seed,
        &mut |txt: &str, book: &mut Option<Arc<L2Book>>| {
            match txt {
                "late" => {
                    clock.fetch_add(60_000, Ordering::Relaxed); // the re-seed is due next pass
                    return WebsocketSessionDepthOperation::Ignored;
                }
                "release" => {
                    assert!(!fetched.load(Ordering::SeqCst), "the snapshot is still in flight");
                    while let Ok(m) = rx.borrow_mut().try_recv() {
                        lane_at_release.borrow_mut().push(match m {
                            Ingest::Quote(_) => "quote",
                            Ingest::Trade(_) => "trade",
                            _ => "other",
                        });
                    }
                    release.store(true, Ordering::SeqCst);
                    return WebsocketSessionDepthOperation::Ignored;
                }
                "settle" => {
                    let done = vike_log::poll::wait_until(Duration::from_secs(10), || {
                        fetched.load(Ordering::SeqCst)
                    });
                    assert!(done, "the fetch never answered");
                    std::thread::sleep(Duration::from_millis(50)); // its thread has returned
                    return WebsocketSessionDepthOperation::Ignored;
                }
                _ => {}
            }
            let fold = match book {
                Some(b) => Arc::make_mut(b),
                None => &mut unseeded,
            };
            pump_frame(route_frame(txt, "BTCUSDT", fold), &pump)
        },
        &mut |b: &Arc<L2Book>| {
            if reseeded.get() {
                published.borrow_mut().push(Arc::clone(b));
            }
        },
        &mut |_event: vike_model::StreamStatus| {},
        &mut || reseeded.set(true),
        &mut StreamHealth::new(86_400_000),
        &AtomicBool::new(false),
        Duration::from_secs(30),
        Some(Duration::from_secs(30)),
        &|| clock.load(Ordering::Relaxed),
    );
    Reseeded {
        published: published.into_inner(),
        lane_at_release: lane_at_release.into_inner(),
        ended: ended.expect_err("the script runs out or the splice fails").to_string(),
    }
}

/// **Quotes and trades go out while the snapshot is in flight, and the splice equals a fresh
/// snapshot plus the later diffs** (owner ruling 2026-11-11, variant B). At the timed re-seed the
/// fresh REST snapshot (id 200) is fetched on its own thread and held shut: meanwhile a quote and a
/// trade reach the tick lane at once, and two diffs fold onto the current book (still published)
/// and are held. When the snapshot answers, the held diffs are spliced onto it by binance's own rule
/// — the one wholly inside it (`u` 180) dropped, the one straddling it (`U` 181) applied — so the
/// spliced book goes out at once, the live diff after it chains, and the bare snapshot is never
/// published.
#[test]
fn a_timed_reseed_fetches_off_thread_and_splices_the_held_diffs() {
    let frames = [
        spot_diff(101, 105, "99.00", "2"), // folds onto the session's first snapshot
        "late".to_string(),
        quote_frame(),
        trade_frame(),
        spot_diff(106, 180, "98.50", "5"), // held; already inside the fresh snapshot
        spot_diff(181, 205, "98.00", "9"), // held; straddles the snapshot's id 200
        "release".to_string(),
        "settle".to_string(),
        spot_diff(206, 210, "97.50", "3"), // live, onto the spliced book
    ];
    let run = reseed_session(Some(snapshot(200, 98.0, 102.0)), &frames);
    assert_eq!(
        run.lane_at_release,
        ["quote", "trade"],
        "both went out before the snapshot answered"
    );
    let mut spliced = snapshot(200, 98.0, 102.0);
    spliced.apply_delta(205, &[L2BookLevel::new(98.0, 9.0)], &[]);
    let mut then = spliced.clone();
    then.apply_delta(210, &[L2BookLevel::new(97.5, 3.0)], &[]);
    let seen: Vec<u64> = run.published.iter().map(|b| b.last_sequence).collect();
    assert_eq!(
        seen,
        [180, 205, 205, 210],
        "the current book during the fetch, then the spliced one"
    );
    assert_eq!(
        run.published[2].top_n(10),
        spliced.top_n(10),
        "snapshot + the held diff that lines up"
    );
    assert_eq!(run.published[3].top_n(10), then.top_n(10), "…and the live diff after it");
    assert!(run.ended.contains("script exhausted"), "the session went on: {}", run.ended);
}

/// **A splice that cannot line up the ids ends the session** for the full reconnect: the fresh
/// snapshot (id 102) is older than the first held diff's first id (106), and that snapshot is never
/// published.
#[test]
fn a_timed_reseed_whose_ids_do_not_line_up_ends_the_session() {
    let frames = [
        spot_diff(101, 105, "99.00", "2"),
        "late".to_string(),
        spot_diff(106, 140, "98.00", "9"),
        "release".to_string(),
        "settle".to_string(),
        spot_diff(141, 150, "97.50", "3"),
    ];
    let run = reseed_session(Some(snapshot(102, 98.0, 102.0)), &frames);
    assert!(run.ended.contains("do not line up"), "a misaligned splice is a fault: {}", run.ended);
    assert!(
        run.published.iter().all(|b| b.last_sequence != 102),
        "the unspliced snapshot never went out"
    );
}

/// **A re-seed whose snapshot fetch fails ends the session** for the full reconnect (the driver's
/// fault path), after the current book kept serving while the fetch ran.
#[test]
fn a_timed_reseed_whose_fetch_fails_ends_the_session() {
    let frames = [
        spot_diff(101, 105, "99.00", "2"),
        "late".to_string(),
        spot_diff(106, 140, "98.00", "9"),
        "release".to_string(),
        "settle".to_string(),
        spot_diff(141, 150, "97.50", "3"),
    ];
    let run = reseed_session(None, &frames);
    assert!(run.ended.contains("snapshot fetch failed"), "{}", run.ended);
    let seen: Vec<u64> = run.published.iter().map(|b| b.last_sequence).collect();
    assert_eq!(seen, [140], "the current book served while the fetch ran");
}
