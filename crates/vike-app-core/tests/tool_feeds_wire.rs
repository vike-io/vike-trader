//! **The News and Calendar tools' poll pass, driven end to end over a real loopback datahub
//! double.**
//!
//! `crates/vike-app-core/src/tools/tool_feeds_tests.rs` gates the pure halves (the schedule, the
//! merge, the sentences); this file gates what one pass of the poll thread actually does once
//! something answers. The double is a `std::net::TcpListener` speaking the real frame codec, so a
//! change to the handshake or to the shape of `Response::NewsFeed`, `Response::EconomicCalendar`
//! or `Response::EquityCalendar` reddens here rather than being agreed with by a fake. The
//! properties:
//!
//! 1. **One pass fills the News tool**, the next minute's ask carries the held generation and is
//!    answered `Unchanged` over the SAME connection, and the rows survive it.
//! 2. **The Calendar window asks the economic AND the equity feeds**, for the viewer's local
//!    Monday; a keyed feed the server holds no key for names that key while the others land.
//! 3. **An old server is sent NO tool-feed frame**, and the status names the feature it lacks.
//! 4. **A refusal is the server's sentence and keeps the last good rows**, and a SWITCHED datahub
//!    shows nothing of the old one's.
//! 5. **With no window open, nothing is dialled at all**; with no backend, the status says so.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use vike_app_core::data::catalog_wire::CatalogDial;
use vike_app_core::tools::{FeedInterest, FeedsClock, ToolData, ToolFeedsLink, ToolFeedsPoller};
use vike_datahub_client::FEATURE_TOOL_FEEDS;
use vike_datahub_client::proto::{Request, Response, read_frame, write_frame};
use vike_datahub_client::tool_feeds::{
    CalEvent, DividendRow, EarningsRow, EconomicCalendarReply, EquityCalendarReply, IpoRow,
    NewsItem, NewsReply, ToolFeedOutcome,
};

mod common;

const NOW_MS: i64 = 1_760_000_000_000;

/// What the double's feeds say right now (switchable mid-test).
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Live,
    NotArmed,
    /// The half-open shape: the request arrives, nothing comes back, the socket stays OPEN (no FIN)
    /// until the test switches the mode away, when that connection closes.
    Silent,
}

#[derive(Default)]
struct Counts {
    connections: AtomicUsize,
    news: AtomicUsize,
    economic: AtomicUsize,
    equity: AtomicUsize,
}

/// What the double saw in the requests: the last `if_generation` of each verb and the last week.
#[derive(Default)]
struct Seen {
    news_generation: Option<Option<u64>>,
    equity_week: Option<String>,
}

struct Script {
    mode: Mode,
    generation: u64,
    /// Whether the double holds the Finnhub key (the dividends and IPO feeds need none here).
    finnhub_key: bool,
}

/// A KEY-LESS datahub double: one thread per connection, counting connections and verb frames.
struct FakeHub {
    addr: String,
    counts: Arc<Counts>,
    script: Arc<Mutex<Script>>,
    seen: Arc<Mutex<Seen>>,
}

impl FakeHub {
    fn spawn(features: Vec<String>, mode: Mode, finnhub_key: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr").to_string();
        let counts = Arc::new(Counts::default());
        let script = Arc::new(Mutex::new(Script { mode, generation: 7, finnhub_key }));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (c, s, n) = (Arc::clone(&counts), Arc::clone(&script), Arc::clone(&seen));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                c.connections.fetch_add(1, Ordering::SeqCst);
                let (features, c, s, n) =
                    (features.clone(), Arc::clone(&c), Arc::clone(&s), Arc::clone(&n));
                std::thread::spawn(move || serve(stream, features, &s, &c, &n));
            }
        });
        Self { addr, counts, script, seen }
    }

    fn dial(&self) -> CatalogDial {
        CatalogDial {
            addr: self.addr.clone(),
            key_name: "VIKE_DATAHUB_OBSERVE_KEY".to_string(),
            keys: None,
        }
    }

    fn count(&self, pick: impl Fn(&Counts) -> &AtomicUsize) -> usize {
        pick(&self.counts).load(Ordering::SeqCst)
    }

    fn set_mode(&self, mode: Mode) {
        self.script.lock().unwrap().mode = mode;
    }
}

/// `Snapshot` at the double's generation, or `Unchanged` when the caller already holds it.
fn outcome<T>(script: &Script, if_generation: Option<u64>, rows: Vec<T>) -> ToolFeedOutcome<T> {
    match script.mode {
        Mode::NotArmed => ToolFeedOutcome::NotArmed,
        Mode::Live | Mode::Silent if if_generation == Some(script.generation) => {
            ToolFeedOutcome::Unchanged { generation: script.generation }
        }
        Mode::Live | Mode::Silent => ToolFeedOutcome::Snapshot {
            rows,
            fetched_at_ms: NOW_MS - 30_000,
            generation: script.generation,
            sources_ok: 15,
            sources_total: 16,
            cadence_secs: 300,
        },
    }
}

fn serve(
    stream: TcpStream,
    features: Vec<String>,
    script: &Mutex<Script>,
    counts: &Counts,
    seen: &Mutex<Seen>,
) {
    let mut r = stream.try_clone().expect("clone the accepted stream");
    let mut w = stream;
    if !common::accept_keyless(&mut w, features) {
        return;
    }
    while let Ok(req) = read_frame::<_, Request>(&mut r) {
        if script.lock().unwrap().mode == Mode::Silent {
            while script.lock().unwrap().mode == Mode::Silent {
                std::thread::sleep(Duration::from_millis(10));
            }
            return;
        }
        let script = script.lock().unwrap();
        let resp = match req {
            Request::NewsFeed { if_generation } => {
                counts.news.fetch_add(1, Ordering::SeqCst);
                seen.lock().unwrap().news_generation = Some(if_generation);
                let rows = vec![
                    NewsItem {
                        source: "CoinDesk".into(),
                        title: "Bitcoin moves".into(),
                        summary: String::new(),
                        url: "https://example.invalid/a".into(),
                        market: "crypto".into(),
                        tags: vec!["markets".into()],
                        ts_ms: NOW_MS,
                    },
                    NewsItem {
                        source: "CNBC".into(),
                        title: "Stocks move".into(),
                        summary: String::new(),
                        url: "https://example.invalid/b".into(),
                        market: "stocks".into(),
                        tags: Vec::new(),
                        ts_ms: NOW_MS - 1,
                    },
                ];
                Response::NewsFeed(NewsReply { outcome: outcome(&script, if_generation, rows) })
            }
            Request::EconomicCalendar { if_generation } => {
                counts.economic.fetch_add(1, Ordering::SeqCst);
                let rows = vec![CalEvent {
                    ts_ms: NOW_MS,
                    currency: "EUR".into(),
                    title: "ECB rate decision".into(),
                    impact: "High".into(),
                    forecast: String::new(),
                    previous: String::new(),
                    actual: String::new(),
                }];
                Response::EconomicCalendar(EconomicCalendarReply {
                    outcome: outcome(&script, if_generation, rows),
                })
            }
            Request::EquityCalendar { week_start, if_generation } => {
                counts.equity.fetch_add(1, Ordering::SeqCst);
                seen.lock().unwrap().equity_week = Some(week_start);
                let earnings = if script.finnhub_key || script.mode == Mode::NotArmed {
                    outcome(
                        &script,
                        if_generation,
                        vec![EarningsRow {
                            date: "2026-10-14".into(),
                            symbol: "JPM".into(),
                            hour: "Pre-mkt".into(),
                            eps_est: "4.00".into(),
                            eps_act: String::new(),
                            surprise: String::new(),
                        }],
                    )
                } else {
                    ToolFeedOutcome::NotConfigured { key_name: "FINNHUB_API_KEY".into() }
                };
                Response::EquityCalendar(EquityCalendarReply {
                    earnings,
                    dividends: outcome(
                        &script,
                        if_generation,
                        vec![DividendRow {
                            date: "2026-10-14".into(),
                            symbol: "KO".into(),
                            pay_date: String::new(),
                            amount: "0.50 $".into(),
                            yield_pct: String::new(),
                            freq: String::new(),
                        }],
                    ),
                    ipos: outcome(
                        &script,
                        if_generation,
                        vec![IpoRow {
                            date: "2026-10-15".into(),
                            symbol: "NEW".into(),
                            company: "New Co".into(),
                            exchange: "NASDAQ".into(),
                            price: String::new(),
                            shares: String::new(),
                            status: "Upcoming".into(),
                        }],
                    ),
                })
            }
            other => Response::Error(format!("unexpected verb: {other:?}")),
        };
        drop(script);
        if write_frame(&mut w, &resp).is_err() {
            return;
        }
    }
}

/// The tool's shared data with a live link and one window showing `interest`.
fn open_window(interest: FeedInterest) -> Mutex<ToolData> {
    let (tx, _rx) = std::sync::mpsc::channel();
    let td = ToolData { feeds_link: ToolFeedsLink::new(tx), ..ToolData::default() };
    td.feeds_link.want(interest);
    Mutex::new(td)
}

fn full_features() -> Vec<String> {
    vec![FEATURE_TOOL_FEEDS.to_string()]
}

/// A clock `after` past `t0`, on Wednesday 2026-10-14 (its Monday is 2026-10-12).
fn clock(t0: Instant, after: Duration) -> FeedsClock {
    FeedsClock {
        now: t0 + after,
        now_ms: NOW_MS,
        today: NaiveDate::from_ymd_opt(2026, 10, 14).unwrap(),
    }
}

#[test]
fn one_pass_fills_the_news_and_the_next_minute_is_answered_unchanged_over_the_same_socket() {
    let hub = FakeHub::spawn(full_features(), Mode::Live, true);
    let shared = open_window(FeedInterest::News);
    let mut poller = ToolFeedsPoller::default();
    let t0 = Instant::now();

    assert!(poller.run_once(&shared, Some(&hub.dial()), &clock(t0, Duration::ZERO)));
    {
        let td = shared.lock().unwrap();
        assert_eq!(td.news.len(), 2);
        assert!(
            td.news_status.contains("2 headlines · LIVE · 15 of 16 sources"),
            "{}",
            td.news_status
        );
        assert!(td.news_status.contains(&hub.addr), "{}", td.news_status);
    }
    assert_eq!(hub.count(|c| &c.news), 1);
    assert_eq!(hub.count(|c| &c.economic), 0, "a News window asks for no calendar");

    assert!(
        !poller.run_once(&shared, Some(&hub.dial()), &clock(t0, Duration::from_secs(10))),
        "inside its minute nothing is due"
    );
    assert_eq!(hub.count(|c| &c.news), 1);

    shared.lock().unwrap().feeds_link.want(FeedInterest::News);
    assert!(poller.run_once(&shared, Some(&hub.dial()), &clock(t0, Duration::from_secs(61))));
    assert_eq!(hub.count(|c| &c.news), 2);
    assert_eq!(
        hub.seen.lock().unwrap().news_generation,
        Some(Some(7)),
        "the held generation rode along"
    );
    let td = shared.lock().unwrap();
    assert_eq!(td.news.len(), 2, "Unchanged carried no rows and lost none");
    assert!(td.news_status.contains("LIVE"), "{}", td.news_status);
    assert_eq!(hub.count(|c| &c.connections), 1, "the connection is HELD across passes");
}

#[test]
fn the_refresh_pill_asks_again_inside_the_minute() {
    let hub = FakeHub::spawn(full_features(), Mode::Live, true);
    let shared = open_window(FeedInterest::News);
    let mut poller = ToolFeedsPoller::default();
    let t0 = Instant::now();
    assert!(poller.run_once(&shared, Some(&hub.dial()), &clock(t0, Duration::ZERO)));
    shared.lock().unwrap().feeds_link.refresh();
    assert!(poller.run_once(&shared, Some(&hub.dial()), &clock(t0, Duration::from_secs(2))));
    assert_eq!(hub.count(|c| &c.news), 2);
}

#[test]
fn the_calendar_asks_both_feeds_for_the_local_monday_and_a_missing_key_names_itself() {
    let hub = FakeHub::spawn(full_features(), Mode::Live, false);
    let shared = open_window(FeedInterest::Calendar);
    let mut poller = ToolFeedsPoller::default();
    let t0 = Instant::now();

    assert!(poller.run_once(&shared, Some(&hub.dial()), &clock(t0, Duration::ZERO)));
    assert_eq!(hub.count(|c| &c.economic), 1);
    assert_eq!(hub.count(|c| &c.equity), 1);
    assert_eq!(hub.seen.lock().unwrap().equity_week.as_deref(), Some("2026-10-12"));
    let td = shared.lock().unwrap();
    assert_eq!(td.calendar.len(), 1);
    assert_eq!(td.calendar[0].country_name, "European Union");
    assert_eq!(td.cal_range, "Oct 12 — Oct 18, 2026");
    assert!(td.cal_status.contains("LIVE"), "{}", td.cal_status);
    assert!(td.cal_earnings.is_empty());
    assert!(
        td.cal_earnings_status.contains("vike-cli secrets set FINNHUB_API_KEY"),
        "{}",
        td.cal_earnings_status
    );
    assert_eq!(td.cal_dividends.len(), 1, "the other keyed feed and the keyless one still land");
    assert_eq!(td.cal_ipos.len(), 1);
    assert_eq!(td.cal_equity.get("2026-10-14"), Some(&(0, 1, 0)));
    assert_eq!(td.cal_equity.get("2026-10-15"), Some(&(0, 0, 1)));
    assert_eq!(hub.count(|c| &c.news), 0, "a Calendar window asks for no news");
}

#[test]
fn an_old_server_is_sent_no_tool_feed_frame_and_says_so() {
    let hub = FakeHub::spawn(vec!["venue_catalog".to_string()], Mode::Live, true);
    let shared = open_window(FeedInterest::News);
    shared.lock().unwrap().feeds_link.want(FeedInterest::Calendar);
    let mut poller = ToolFeedsPoller::default();
    assert!(poller.run_once(&shared, Some(&hub.dial()), &clock(Instant::now(), Duration::ZERO)));
    let td = shared.lock().unwrap();
    assert!(td.news_status.contains(FEATURE_TOOL_FEEDS), "{}", td.news_status);
    assert!(td.news_status.contains("nothing was sent"), "{}", td.news_status);
    assert!(
        td.cal_status.contains(FEATURE_TOOL_FEEDS)
            && td.cal_ipo_status.contains("nothing was sent")
    );
    assert_eq!(hub.count(|c| &c.news) + hub.count(|c| &c.economic) + hub.count(|c| &c.equity), 0);
    assert!(td.news.is_empty() && td.calendar.is_empty(), "and nothing was invented");
}

#[test]
fn a_refusal_is_the_servers_sentence_and_keeps_the_last_good_rows() {
    let hub = FakeHub::spawn(full_features(), Mode::Live, true);
    let shared = open_window(FeedInterest::News);
    let mut poller = ToolFeedsPoller::default();
    let t0 = Instant::now();
    assert!(poller.run_once(&shared, Some(&hub.dial()), &clock(t0, Duration::ZERO)));
    assert_eq!(shared.lock().unwrap().news.len(), 2);

    // The SAME server's operator rebuilds it without the lane.
    hub.set_mode(Mode::NotArmed);
    shared.lock().unwrap().feeds_link.refresh();
    assert!(poller.run_once(&shared, Some(&hub.dial()), &clock(t0, Duration::from_secs(5))));
    let td = shared.lock().unwrap();
    assert_eq!(td.news_status, ToolFeedOutcome::<NewsItem>::NotArmed.describe("news"));
    assert_eq!(td.news.len(), 2, "a refusal never blanks the window");
}

/// **A switched backend shows nothing of the old one.** The moment the dial names another
/// datahub, the old server's rows and statuses are gone, before the new one answers and whatever
/// it answers.
#[test]
fn a_switched_datahub_never_shows_the_old_ones_rows() {
    let old = FakeHub::spawn(full_features(), Mode::Live, true);
    let shared = open_window(FeedInterest::News);
    let mut poller = ToolFeedsPoller::default();
    let t0 = Instant::now();
    assert!(poller.run_once(&shared, Some(&old.dial()), &clock(t0, Duration::ZERO)));
    assert!(!shared.lock().unwrap().news.is_empty());

    let new = FakeHub::spawn(full_features(), Mode::NotArmed, true);
    shared.lock().unwrap().feeds_link.want(FeedInterest::News);
    assert!(poller.run_once(&shared, Some(&new.dial()), &clock(t0, Duration::from_secs(1))));
    let td = shared.lock().unwrap();
    assert!(td.news.is_empty(), "the old server's headlines are not last good here");
    assert!(!td.news_status.contains(&old.addr), "nor its status: {}", td.news_status);
    assert_eq!(new.count(|c| &c.news), 1, "the new server is asked at once, from scratch");
    assert_eq!(
        new.seen.lock().unwrap().news_generation,
        Some(None),
        "with none of the old generation"
    );
}

/// **The half-open link.** The held connection's peer stops answering mid-pass without closing: the
/// pass ends within the (test-sized) reply bound with a status naming the timeout, the rows the last
/// answer carried stay on screen, and the next pass dials a NEW connection.
#[test]
fn a_silent_datahub_ends_the_pass_keeps_last_good_and_the_next_pass_redials() {
    let hub = FakeHub::spawn(full_features(), Mode::Live, true);
    let shared = open_window(FeedInterest::News);
    let mut poller = ToolFeedsPoller::with_reply_timeout(Duration::from_millis(200));
    let t0 = Instant::now();
    assert!(poller.run_once(&shared, Some(&hub.dial()), &clock(t0, Duration::ZERO)));
    assert_eq!(shared.lock().unwrap().news.len(), 2);
    assert_eq!(hub.count(|c| &c.connections), 1);

    hub.set_mode(Mode::Silent);
    shared.lock().unwrap().feeds_link.refresh();
    let started = Instant::now();
    assert!(poller.run_once(&shared, Some(&hub.dial()), &clock(t0, Duration::from_secs(1))));
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_secs(10),
        "the pass must not park on a silent peer: {waited:?}"
    );
    {
        let td = shared.lock().unwrap();
        assert!(td.news_status.contains("no reply byte for 200ms"), "names it: {}", td.news_status);
        assert!(td.news_status.contains(&hub.addr), "and the datahub: {}", td.news_status);
        assert!(!td.news_status.contains("LIVE"), "{}", td.news_status);
        assert_eq!(td.news.len(), 2, "a dead link never blanks the last good rows");
    }

    hub.set_mode(Mode::Live);
    shared.lock().unwrap().feeds_link.refresh();
    assert!(poller.run_once(&shared, Some(&hub.dial()), &clock(t0, Duration::from_secs(2))));
    assert_eq!(hub.count(|c| &c.connections), 2, "the dead connection was dropped and re-dialled");
    let status = shared.lock().unwrap().news_status.clone();
    assert!(status.contains("LIVE"), "answered again over the new connection: {status}");
}

#[test]
fn with_no_window_open_nothing_is_dialled() {
    let hub = FakeHub::spawn(full_features(), Mode::Live, true);
    let (tx, _rx) = std::sync::mpsc::channel();
    let shared = Mutex::new(ToolData { feeds_link: ToolFeedsLink::new(tx), ..ToolData::default() });
    let mut poller = ToolFeedsPoller::default();
    assert!(!poller.run_once(&shared, Some(&hub.dial()), &clock(Instant::now(), Duration::ZERO)));
    assert_eq!(hub.count(|c| &c.connections), 0);
}

#[test]
fn with_no_backend_the_status_says_so_and_nothing_is_asked() {
    let shared = open_window(FeedInterest::News);
    let mut poller = ToolFeedsPoller::default();
    assert!(poller.run_once(&shared, None, &clock(Instant::now(), Duration::ZERO)));
    let status = shared.lock().unwrap().news_status.clone();
    assert!(status.contains("config.datahub_addr"), "{status}");
    assert!(!status.contains("LIVE"), "{status}");
}
