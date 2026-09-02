//! Live demo MARKET-DATA smoke — the one thing the offline suites cannot prove: that IG's
//! Lightstreamer server accepts THIS hand-rolled TLCP client. The codec's fixtures
//! (`vike_ig::lightstreamer`'s spec-worked-example test) pin the grammar against the published
//! TLCP 2.1.0 specification, and the normalizers' fixtures pin the field folds — but "the spec says
//! this" and "IG's server agrees" are different claims, and only a live session settles the second.
//!
//!     cargo test -p vike-ig --test ig_market_feed_smoke -- --ignored --nocapture
//!
//! READ-ONLY — a market-data subscription places no order and touches no account state. Double-
//! gated like every other `*_smoke.rs`: network + `IG_DEMO_*` creds in the credential store,
//! self-skipping (a printed note, then an early `return`) when creds are absent or the demo login
//! fails, so a cred-less rig sees a clean skip and never a red.
//!
//! ⚠ **A quote assertion would be market-hours-dependent, so there isn't one.** FX streams around
//! the clock on weekdays but IG's demo gateway still closes at the weekend, and a test that goes
//! red on a Sunday is a test people learn to ignore. What this asserts instead is the part that is
//! ALWAYS true once the protocol is right: the session comes up and IG answers the subscription.
//! Any quote that does arrive is validated (finite, bid <= offer) and printed.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_bridge_core::credentials::{load_workspace_dotenv_from, Environment};
use vike_data::{DataClient, LiveDataSink};
use vike_ig::load_ig_config_from;
use vike_model::{Bar, QuoteTick};

/// EUR/USD mini — present on the demo gateway.
const EPIC: &str = "CS.D.EURUSD.MINI.IP";
/// How long to hold the subscription open waiting for the first frame.
const WINDOW: Duration = Duration::from_secs(25);

fn cfg() -> Option<vike_ig::IgConfig> {
    load_ig_config_from(
        Environment::Demo,
        &load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref()),
    )
}

/// Counting sink: records what the feed emitted, and validates each quote as it lands.
#[derive(Default)]
struct CountingSink {
    quotes: AtomicUsize,
    bars: AtomicUsize,
    statuses: Mutex<Vec<String>>,
    bad: Mutex<Vec<String>>,
    first_quote: Mutex<Option<QuoteTick>>,
}

impl LiveDataSink for CountingSink {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _bars: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {
        self.bars.fetch_add(1, Ordering::Relaxed);
    }
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, b: Bar) {
        self.bars.fetch_add(1, Ordering::Relaxed);
        if !(b.open.is_finite() && b.high.is_finite() && b.low.is_finite() && b.close.is_finite()) {
            self.bad.lock().unwrap().push(format!("non-finite bar: {b:?}"));
        }
        if b.high < b.low {
            self.bad.lock().unwrap().push(format!("bar high < low: {b:?}"));
        }
    }
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, venue: &str, symbol: &str, q: QuoteTick) {
        self.quotes.fetch_add(1, Ordering::Relaxed);
        if venue != "ig" || symbol != EPIC {
            self.bad.lock().unwrap().push(format!("misrouted quote: {venue}/{symbol}"));
        }
        if !q.bid.is_finite() || !q.ask.is_finite() || q.bid <= 0.0 || q.ask <= 0.0 {
            self.bad.lock().unwrap().push(format!("implausible quote: {q:?}"));
        } else if q.bid > q.ask {
            self.bad.lock().unwrap().push(format!("crossed quote (bid > offer): {q:?}"));
        }
        self.first_quote.lock().unwrap().get_or_insert(q);
    }
    fn trade(&self, _v: &str, _s: &str, _t: vike_model::TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<vike_model::L2Book>) {}
    fn stream_status(&self, _v: &str, _s: &str, stream: &str, status: vike_data::StreamStatus) {
        self.statuses.lock().unwrap().push(format!("{stream}: {status:?}"));
    }
}

/// Subscribe to live L1 quotes and prove the TLCP session comes up against the real server.
#[test]
#[ignore = "live demo; needs IG_DEMO_* creds — read-only (no order), run manually (see module doc)"]
fn ig_quotes_feed_smoke() {
    let Some(c) = cfg() else {
        eprintln!("skip: no IG_DEMO creds");
        return;
    };
    // A login failure here is a rotated password / gateway outage — a skip, not a red. Proven up
    // front so the feed thread's own failure below can be read as a PROTOCOL failure.
    if vike_ig::IgSession::login(&c).is_err() {
        eprintln!("skip: IG demo login failed (creds present but session not established)");
        return;
    }

    let sink = Arc::new(CountingSink::default());
    let woke = Arc::new(AtomicBool::new(false));
    let woke_c = Arc::clone(&woke);
    let mut feeds = vike_ig::Feeds::new(
        Arc::clone(&sink) as Arc<dyn LiveDataSink>,
        move || {
            woke_c.store(true, Ordering::Relaxed);
        },
        c,
    );

    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");
    let started = Instant::now();
    while started.elapsed() < WINDOW && sink.quotes.load(Ordering::Relaxed) == 0 {
        std::thread::sleep(Duration::from_millis(200));
    }
    let status = feeds.status.lock().unwrap().clone();
    let quotes = sink.quotes.load(Ordering::Relaxed);
    println!("status after {:?}: {status}", started.elapsed());
    println!("quotes: {quotes}");
    println!("stream_status events: {:?}", sink.statuses.lock().unwrap());
    if let Some(q) = sink.first_quote.lock().unwrap().as_ref() {
        println!("first quote: bid={} offer={} ts={}", q.bid, q.ask, q.ts);
    }

    feeds.unsubscribe(id);
    feeds.shutdown();

    let bad = sink.bad.lock().unwrap().clone();
    assert!(bad.is_empty(), "sink validation failures: {bad:?}");

    // The protocol claim, market-hours-independent: the session came up and IG did not refuse it.
    // A TLCP/auth/subscription failure leaves the status line naming the fault (the feed's
    // `on_session_error` writes it) instead of the live/up text the handshake writes.
    assert!(
        !status.contains("error") && !status.contains("failed"),
        "IG market feed never established a session — status: {status}"
    );
    if quotes > 0 {
        assert!(woke.load(Ordering::Relaxed), "a delivered quote must have woken the UI");
    } else {
        eprintln!(
            "note: session up but no quote inside {WINDOW:?} — expected OUTSIDE market hours \
             (the handshake assertion above is the protocol proof)"
        );
    }
}

/// The candle lane's refusal law is offline-checkable and market-hours-independent: IG's
/// Lightstreamer serves four chart scales, and any other interval must be refused up front rather
/// than opening a socket that can never deliver.
#[test]
#[ignore = "live demo; needs IG_DEMO_* creds — read-only, run manually (see module doc)"]
fn ig_bars_refuse_non_streaming_intervals() {
    let Some(c) = cfg() else {
        eprintln!("skip: no IG_DEMO creds");
        return;
    };
    let sink = Arc::new(CountingSink::default());
    let mut feeds = vike_ig::Feeds::new(Arc::clone(&sink) as Arc<dyn LiveDataSink>, || {}, c);
    for unsupported in ["15m", "4h", "1d"] {
        let err = feeds
            .subscribe_bars(EPIC, unsupported)
            .expect_err("a REST-only resolution must be refused, not silently opened");
        println!("{unsupported} -> {err}");
    }
    // ...and the venue serves no tape or ladder at all — caps-driven refusals, no socket opened.
    assert!(feeds.subscribe_trades(EPIC).is_err(), "IG has no public trade tape");
    assert!(feeds.subscribe_book(EPIC).is_err(), "IG streams L1 only, no L2 ladder");
    assert!(feeds.subscribe_depth(EPIC).is_err(), "IG streams L1 only, no depth ladder");
    feeds.shutdown();
}
