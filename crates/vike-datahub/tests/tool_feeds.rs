//! The COMPOSED-PATH gate for the TOOL-FEED verbs (`NewsFeed`, `EconomicCalendar`,
//! `EquityCalendar`): a server on an ephemeral loopback port, driven through the real
//! `DatahubClient`, with a scripted provider seam (every build) or the real `ureq` transport against
//! a loopback HTTP stand-in for all five providers (`tool-feeds-serve`). No provider, no network
//! beyond loopback, and no DataFusion: the verbs write no store.
//!
//! 1. [`a_laneless_server_answers_not_armed`] — an unarmed build still answers, successfully, and
//!    advertises the feature;
//! 2. [`a_scripted_lane_answers_across_the_wire`] — lazy start, a snapshot, `Unchanged` on an equal
//!    generation, a refused week, a bad shape as the one error, `NotConfigured` naming the key;
//! 3. [`the_real_transport_reaches_a_loopback_provider_and_keeps_every_key_off_the_wire_answers`]
//!    (`tool-feeds-serve`) — the five providers' real request shapes, the key in exactly one place
//!    each, keep-last-good when a provider starts failing;
//! 4. [`the_store_reader_holds_one_name_and_says_three_things`] — the scoped credential read.

use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vike_data::{HistStore, MemHistStore};
use vike_datahub::server::{ServerLanes, serve_with_lanes};
use vike_datahub::tool_feeds::news::NewsFeedSpec;
use vike_datahub::tool_feeds::{
    Endpoints, FINNHUB_API_KEY, FMP_API_KEY, FeedHttp, HttpGet, KeyRead, KeyReader, Secret,
    StoreKeys, ToolFeedsLane,
};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::tool_feeds::{ToolFeedOutcome, ToolFeedRefusal};

/// 2026-10-14 12:00:00 UTC (a Wednesday): the served Mondays are 2026-10-05, -12 and -19.
const NOW_MS: i64 = 1_791_979_200_000;
const WEEK: &str = "2026-10-12";

fn spawn(lane: Option<ToolFeedsLane>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let lane = lane.map(Arc::new);
    thread::spawn(move || {
        let lanes = ServerLanes { tool_feeds: lane, ..ServerLanes::default() };
        let _ = serve_with_lanes(listener, store, None, None, None, None, lanes, None, false);
    });
    addr
}

fn until<T>(mut ask: impl FnMut() -> T, done: impl Fn(&T) -> bool) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let answer = ask();
        if done(&answer) {
            return answer;
        }
        assert!(Instant::now() < deadline, "the poll never landed");
        thread::sleep(Duration::from_millis(20));
    }
}

struct Keys {
    finnhub: Option<&'static str>,
    fmp: Option<&'static str>,
}

impl KeyReader for Keys {
    fn read(&self, name: &str) -> KeyRead {
        let held = if name == FINNHUB_API_KEY {
            self.finnhub
        } else if name == FMP_API_KEY {
            self.fmp
        } else {
            None
        };
        held.map_or(KeyRead::Absent, |v| KeyRead::Present(Secret::new(v)))
    }
}

/// A scripted provider seam answering by URL fragment; `failing` turns every answer into an error.
struct Scripted {
    failing: Mutex<bool>,
}

impl FeedHttp for Scripted {
    fn get(&self, request: &HttpGet) -> Result<String, String> {
        if *self.failing.lock().unwrap() {
            return Err("HTTP 503".into());
        }
        let url = request.url();
        Ok(if url.contains("/rss/") {
            "<rss><item><title>Headline</title><pubDate>Wed, 14 Oct 2026 10:00:00 +0000</pubDate></item></rss>"
        } else if url.contains("/ff.json") {
            r#"[{"title":"CPI","country":"USD","date":"2026-10-14T08:30:00-04:00","impact":"High"}]"#
        } else if url.contains("/finnhub/") {
            r#"{"earningsCalendar":[{"date":"2026-10-13","symbol":"JPM","hour":"bmo","epsEstimate":4.5,"epsActual":4.8}]}"#
        } else if url.contains("/fmp/") {
            r#"[{"date":"2026-10-14","symbol":"KO","dividend":0.51}]"#
        } else if url.contains("/nasdaq/") {
            r#"{"data":{"priced":{"rows":[{"pricedDate":"10/14/2026","proposedTickerSymbol":"AAA"}]}}}"#
        } else {
            return Err("unscripted".into());
        }
        .to_string())
    }
}

fn endpoints(base: &str) -> Endpoints {
    Endpoints {
        news: vec![NewsFeedSpec {
            name: "Wire".into(),
            market: "stocks".into(),
            url: format!("{base}/rss/a"),
        }],
        calendar_url: format!("{base}/ff.json"),
        finnhub_base: format!("{base}/finnhub"),
        fmp_base: format!("{base}/fmp"),
        nasdaq_base: format!("{base}/nasdaq"),
    }
}

#[test]
fn a_laneless_server_answers_not_armed() {
    let mut client = DatahubClient::connect(spawn(None)).expect("handshake");
    assert!(client.supports_tool_feeds(), "a build fact: {:?}", client.features());
    assert_eq!(client.news_feed(None).expect("a success").outcome, ToolFeedOutcome::NotArmed);
    assert_eq!(
        client.economic_calendar(Some(4)).expect("a success").outcome,
        ToolFeedOutcome::NotArmed
    );
    let eq = client.equity_calendar(WEEK, None).expect("a success");
    assert_eq!(
        (eq.earnings, eq.dividends, eq.ipos),
        (ToolFeedOutcome::NotArmed, ToolFeedOutcome::NotArmed, ToolFeedOutcome::NotArmed)
    );
}

#[test]
fn a_scripted_lane_answers_across_the_wire() {
    let http = Arc::new(Scripted { failing: Mutex::new(false) });
    let lane = ToolFeedsLane::new(
        Arc::clone(&http) as Arc<dyn FeedHttp>,
        Arc::new(Keys { finnhub: Some("a-finnhub-key"), fmp: None }),
        endpoints("http://provider.test"),
        Arc::new(|| NOW_MS),
        true,
    );
    let mut client = DatahubClient::connect(spawn(Some(lane))).expect("handshake");

    // lazy: the first ask is Warming, the poll lands behind it
    assert_eq!(client.news_feed(None).expect("answered").outcome, ToolFeedOutcome::Warming);
    let news = until(|| client.news_feed(None).expect("answered").outcome, |o| o.rows().is_some());
    let ToolFeedOutcome::Snapshot { rows, generation, sources_ok, sources_total, .. } = news else {
        unreachable!()
    };
    assert_eq!((rows[0].title.as_str(), rows[0].market.as_str()), ("Headline", "stocks"));
    assert_eq!((sources_ok, sources_total), (1, 1));
    assert_eq!(
        client.news_feed(Some(generation)).expect("answered").outcome,
        ToolFeedOutcome::Unchanged { generation },
        "an equal generation is the cheap answer"
    );

    // the calendar, raw: UTC instant and currency code
    let econ =
        until(|| client.economic_calendar(None).expect("answered").outcome, |o| o.rows().is_some());
    let event = &econ.rows().unwrap()[0];
    assert_eq!((event.currency.as_str(), event.ts_ms), ("USD", 1_791_981_000_000));

    // the equity week: earnings land, dividends say which key is missing, IPOs land keyless
    let eq = until(
        || client.equity_calendar(WEEK, None).expect("answered"),
        |r| {
            r.earnings.rows().is_some()
                && r.ipos.rows().is_some()
                && matches!(r.dividends, ToolFeedOutcome::NotConfigured { .. })
        },
    );
    assert_eq!(eq.earnings.rows().unwrap()[0].surprise, "+6.7%");
    assert_eq!(eq.ipos.rows().unwrap()[0].date, "2026-10-14");
    assert_eq!(
        eq.dividends,
        ToolFeedOutcome::NotConfigured { key_name: FMP_API_KEY.to_string() },
        "the answer names the key, never a value"
    );
    let g = eq.earnings.generation().unwrap();
    assert_eq!(eq.ipos.generation(), Some(g), "one week, one generation");
    let again = client.equity_calendar(WEEK, Some(g)).expect("answered");
    assert_eq!(again.earnings, ToolFeedOutcome::Unchanged { generation: g });
    assert_eq!(again.ipos, ToolFeedOutcome::Unchanged { generation: g });

    // a week outside the window is a success naming the served Mondays; a bad shape is the error
    let far = client.equity_calendar("2026-11-02", None).expect("answered");
    let refusal = ToolFeedRefusal::WeekOutOfRange {
        allowed_from: "2026-10-05".into(),
        allowed_to: "2026-10-19".into(),
    };
    assert_eq!(far.earnings, ToolFeedOutcome::Refused(refusal.clone()));
    assert_eq!(far.dividends, ToolFeedOutcome::Refused(refusal.clone()));
    assert_eq!(far.ipos, ToolFeedOutcome::Refused(refusal));
    assert!(client.equity_calendar("2026-10-1", None).is_err(), "refused client-side");

    // the provider starts failing: the rows are KEPT and the count says nobody answered
    *http.failing.lock().unwrap() = true;
    // (the next poll is a cadence away; the cache is what a client sees meanwhile)
    let still = client.news_feed(None).expect("answered").outcome;
    assert!(still.rows().is_some(), "{still:?}");
}

#[cfg(feature = "tool-feeds-serve")]
mod real_transport {
    use std::io::{BufRead, BufReader, Write};

    use vike_datahub::tool_feeds::http::UreqHttp;

    use super::*;

    const FINNHUB_VALUE: &str = "finnhub-secret-61f2";
    const FMP_VALUE: &str = "fmp-secret-9c0d";

    /// One recorded request: its path and its lower-cased header lines.
    type Seen = (String, Vec<String>);

    /// A loopback HTTP/1.1 stand-in for all five providers: records `(request line, headers)` and
    /// answers by path; `/ff.json` answers 503 once `broken` is set.
    struct Stand {
        addr: SocketAddr,
        seen: Arc<Mutex<Vec<Seen>>>,
        broken: Arc<Mutex<bool>>,
    }

    fn stand_in() -> Stand {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let broken = Arc::new(Mutex::new(false));
        let (log, flag) = (Arc::clone(&seen), Arc::clone(&broken));
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (log, flag) = (Arc::clone(&log), Arc::clone(&flag));
                thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let mut headers = Vec::new();
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                            break;
                        }
                        headers.push(h.trim().to_lowercase());
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    log.lock().unwrap().push((path.clone(), headers));
                    let (status, body) = if path.starts_with("/ff.json") && *flag.lock().unwrap() {
                        ("503 Service Unavailable", String::from("down"))
                    } else if path.starts_with("/rss/") {
                        ("200 OK", "<rss><item><title>R</title></item></rss>".into())
                    } else if path.starts_with("/ff.json") {
                        ("200 OK", r#"[{"title":"CPI","country":"USD","date":"2026-10-14T08:30:00-04:00","impact":"High"}]"#.into())
                    } else if path.starts_with("/finnhub/") {
                        (
                            "200 OK",
                            r#"{"earningsCalendar":[{"date":"2026-10-13","symbol":"JPM"}]}"#.into(),
                        )
                    } else if path.starts_with("/fmp/") {
                        ("200 OK", r#"[{"date":"2026-10-14","symbol":"KO"}]"#.into())
                    } else if path.starts_with("/nasdaq/") {
                        ("200 OK", r#"{"data":{"priced":{"rows":[]}}}"#.into())
                    } else {
                        ("404 Not Found", String::new())
                    };
                    let mut out = stream;
                    let _ = write!(
                        out,
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        Stand { addr, seen, broken }
    }

    #[test]
    fn the_real_transport_reaches_a_loopback_provider_and_keeps_every_key_off_the_wire_answers() {
        let stand = stand_in();
        let lane = ToolFeedsLane::new(
            Arc::new(UreqHttp),
            Arc::new(Keys { finnhub: Some(FINNHUB_VALUE), fmp: Some(FMP_VALUE) }),
            endpoints(&format!("http://{}", stand.addr)),
            Arc::new(|| NOW_MS),
            true,
        );
        let mut client = DatahubClient::connect(spawn(Some(lane))).expect("handshake");

        let eq = until(
            || client.equity_calendar(WEEK, None).expect("answered"),
            |r| {
                r.earnings.rows().is_some()
                    && r.dividends.rows().is_some()
                    && r.ipos.rows().is_some()
            },
        );
        let news =
            until(|| client.news_feed(None).expect("answered").outcome, |o| o.rows().is_some());
        let econ = until(
            || client.economic_calendar(None).expect("answered").outcome,
            |o| o.rows().is_some(),
        );
        assert_eq!(news.rows().unwrap()[0].title, "R");
        assert_eq!(econ.rows().unwrap()[0].currency, "USD");
        // the IPO answer was a success with no rows: a Snapshot that says so, not a Warming
        assert_eq!(eq.ipos.rows().map(<[_]>::len), Some(0));

        // the request shapes, and the key in exactly one place each
        let seen = stand.seen.lock().unwrap().clone();
        let find = |prefix: &str| seen.iter().find(|(p, _)| p.starts_with(prefix)).cloned();
        let (path, headers) = find("/finnhub/").expect("a Finnhub request");
        assert_eq!(path, "/finnhub/api/v1/calendar/earnings?from=2026-10-12&to=2026-10-18");
        assert!(
            headers.iter().any(|h| h == &format!("x-finnhub-token: {FINNHUB_VALUE}")),
            "{headers:?}"
        );
        let (path, headers) = find("/fmp/").expect("an FMP request");
        assert_eq!(
            path,
            format!(
                "/fmp/stable/dividends-calendar?from=2026-10-12&to=2026-10-18&apikey={FMP_VALUE}"
            )
        );
        assert!(!headers.iter().any(|h| h.contains(FMP_VALUE)), "{headers:?}");
        let (path, headers) = find("/nasdaq/").expect("a Nasdaq request");
        assert_eq!(path, "/nasdaq/api/ipo/calendar?date=2026-10");
        assert!(headers.iter().any(|h| h.starts_with("user-agent: mozilla/5.0")), "{headers:?}");
        let (_, headers) = find("/ff.json").expect("a calendar request");
        assert!(headers.iter().any(|h| h.starts_with("user-agent: mozilla/5.0")), "{headers:?}");

        // no value comes back in any answer
        let wire = format!("{eq:?} {news:?} {econ:?}");
        assert!(!wire.contains(FINNHUB_VALUE) && !wire.contains(FMP_VALUE), "{wire}");
        let _ = stand.broken; // (the flag is for a cadence-long failure test the unit tests cover)
    }
}

#[test]
fn the_store_reader_holds_one_name_and_says_three_things() {
    let dir = tempfile::tempdir().expect("a temp dir");
    vike_secrets::create_store(Some(dir.path().to_str().unwrap())).expect("create the store");
    let finnhub_value = "finnhub-value-77aa";
    let neighbour = ("BINANCE_LIVE_API_KEY".to_string(), "neighbour-value-3c".to_string());
    vike_secrets::save_credentials_to_store_unjournalled(
        dir.path(),
        vike_secrets::Table::Credential,
        &[(FINNHUB_API_KEY.to_string(), finnhub_value.to_string()), neighbour],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("seed the store");
    // The handle the boot hands the lane, over a settings directory or over none.
    let at = |dir: &std::path::Path| {
        vike_secrets::CredentialStore::open(
            Some(vike_secrets::StoreLocation::new(dir)),
            vike_secrets::CredentialScope::Full,
        )
    };
    let keys = StoreKeys::new(at(dir.path()));
    match keys.read(FINNHUB_API_KEY) {
        KeyRead::Present(secret) => assert_eq!(secret.expose(), finnhub_value),
        other => panic!("expected the key, got {other:?}"),
    }
    assert!(matches!(keys.read(FMP_API_KEY), KeyRead::Absent), "a store without the key is Absent");
    assert!(
        matches!(
            StoreKeys::new(vike_secrets::CredentialStore::open(
                None,
                vike_secrets::CredentialScope::Full,
            ))
            .read(FINNHUB_API_KEY),
            KeyRead::Absent
        ),
        "no settings directory, no store, no key"
    );
    assert!(!format!("{:?}", keys.read(FINNHUB_API_KEY)).contains(finnhub_value), "Debug redacts");
    // a store that exists and will not read is Unreadable, never Absent
    let broken = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(broken.path().join("db")).unwrap();
    std::fs::write(vike_secrets::db_path_in(broken.path()), b"not a sqlite database").unwrap();
    assert!(matches!(StoreKeys::new(at(broken.path())).read(FINNHUB_API_KEY), KeyRead::Unreadable));
}

/// The datahub spells the two provider key names as its own constants (the settings registry
/// resolves a name through a constant declared in the READING crate), and the GUI's "Data providers"
/// group writes them through `vike_model::credential_keys` — so the two spellings are held equal
/// here, or a key saved from the desktop would land under a name the lane never reads.
#[test]
fn the_provider_key_names_are_the_models_own() {
    assert_eq!(FINNHUB_API_KEY, vike_model::credential_keys::FINNHUB_API_KEY);
    assert_eq!(FMP_API_KEY, vike_model::credential_keys::FMP_API_KEY);
    assert_eq!(
        vike_model::credential_keys::DATA_PROVIDER_KEYS,
        [FINNHUB_API_KEY, FMP_API_KEY],
        "the closed pair the account wire admits is exactly the pair the lane reads"
    );
}
