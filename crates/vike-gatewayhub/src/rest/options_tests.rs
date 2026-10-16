use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_marketdata::OptionTicker;

use super::*;
use vike_gatewayhub_client::proto::{
    GW_OPTIONS_MAX_INDEXES, GW_OPTIONS_MAX_INSTRUMENTS, GW_OPTIONS_MAX_NAME_BYTES,
};

/// BTC's book: one far-future expiry, five strikes, a call and a put each.
fn btc_book() -> String {
    let mut rows = Vec::new();
    for strike in [80_000, 90_000, 100_000, 110_000, 120_000] {
        for cp in ["C", "P"] {
            rows.push(format!(
                r#"{{"instrument_name":"BTC-25JUN37-{strike}-{cp}","bid_price":0.05,"ask_price":0.06,"mark_price":0.055,"mark_iv":62.5,"open_interest":12.0,"volume":3.0,"underlying_price":104000.0}}"#
            ));
        }
    }
    format!("[{}]", rows.join(","))
}

/// A loopback stand-in for Deribit's book-summary endpoint that COUNTS the requests it answers (a
/// request is one line the venue would have billed), and the URL to hand
/// `DeribitOptionsProvider::with_book_summary_url`.
struct FakeDeribit {
    url: String,
    hits: Arc<AtomicUsize>,
}

fn spawn_fake_deribit() -> FakeDeribit {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr: SocketAddr = listener.local_addr().expect("the assigned port");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let book = btc_book();
    std::thread::Builder::new()
        .name("test-fake-deribit".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let Ok(clone) = stream.try_clone() else { continue };
                let mut reader = BufReader::new(clone);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
                    continue;
                }
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line == "\r\n" || line == "\n" => break,
                        Ok(_) => {}
                    }
                }
                counter.fetch_add(1, Ordering::SeqCst);
                let currency = request_line
                    .split("currency=")
                    .nth(1)
                    .and_then(|rest| rest.split(['&', ' ']).next())
                    .unwrap_or("");
                let rows = if currency == "BTC" { book.as_str() } else { "[]" };
                let body = format!(r#"{{"jsonrpc":"2.0","result":{rows}}}"#);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.flush();
            }
        })
        .expect("spawn the fake venue");
    FakeDeribit { url: format!("http://{addr}/api/v2/public/get_book_summary_by_currency"), hits }
}

fn hub_over(fake: &FakeDeribit) -> OptionsHub {
    OptionsHub::with_provider(DeribitOptionsProvider::new().with_book_summary_url(fake.url.clone()))
}

fn chain_call(strike_window: Option<u32>) -> OptionsCall {
    OptionsCall::Chain {
        underlying: "BTC".to_string(),
        expiry: "2037-06-25".to_string(),
        strike_window,
        r: 0.03,
    }
}

fn expiries_call() -> OptionsCall {
    OptionsCall::Expiries { underlying: "BTC".to_string() }
}

/// **One provider per daemon**: an `Expiries` and a `Chain` call (and a second `Chain`) share ONE
/// book-summary request, because the provider's 5 s cache survives across calls. This is the cost the
/// datahub's refresh pass paid in-process; a provider per request would pay it per call.
#[test]
fn the_persistent_provider_shares_one_book_summary_across_calls() {
    let venue = spawn_fake_deribit();
    let hub = hub_over(&venue);
    hub.call(&expiries_call()).expect("expiries");
    hub.call(&chain_call(None)).expect("the first chain");
    hub.call(&chain_call(Some(2))).expect("the second chain");
    assert_eq!(venue.hits.load(Ordering::SeqCst), 1, "the cache did not survive across calls");
}

/// …and the contrast, so the test above can fail: two hubs (a provider per request) cost two requests.
#[test]
fn a_provider_per_request_would_pay_the_venue_each_time() {
    let venue = spawn_fake_deribit();
    hub_over(&venue).call(&expiries_call()).expect("first");
    hub_over(&venue).call(&expiries_call()).expect("second");
    assert_eq!(venue.hits.load(Ordering::SeqCst), 2);
}

/// `strike_window: None` is the FULL chain (what the datahub records before it applies its own
/// window); `Some(n)` windows it. The reply is the provider's own chain, greeks and all.
#[test]
fn the_full_chain_is_all_strikes_and_a_window_narrows_it() {
    let venue = spawn_fake_deribit();
    let hub = hub_over(&venue);
    let OptionsReply::Chain(full) = hub.call(&chain_call(None)).expect("full") else {
        panic!("a Chain call answers a Chain");
    };
    let OptionsReply::Chain(windowed) = hub.call(&chain_call(Some(1))).expect("windowed") else {
        panic!("a Chain call answers a Chain");
    };
    assert_eq!(full.rows.len(), 5, "the full chain holds every strike the venue listed");
    assert!(windowed.rows.len() < full.rows.len(), "a ±1 window must narrow five strikes");
    assert_eq!((full.underlying.as_str(), full.source.as_str()), ("BTC", "deribit"));
}

/// The underlyings are the provider's own, in its own order.
#[test]
fn the_underlyings_are_the_providers_in_its_order() {
    let venue = spawn_fake_deribit();
    let hub = hub_over(&venue);
    let OptionsReply::Underlyings(set) = hub.call(&OptionsCall::Underlyings).expect("set") else {
        panic!("Underlyings answers Underlyings");
    };
    assert_eq!(set, ["BTC", "ETH", "SOL"]);
    assert_eq!(venue.hits.load(Ordering::SeqCst), 0, "listing the set asks the venue nothing");
}

/// A name outside the served set is refused before anything is sent: the client names a term of the
/// URL, and the 0052 cost rule is that it may not.
#[test]
fn an_underlying_the_provider_does_not_serve_is_refused_before_the_venue() {
    let venue = spawn_fake_deribit();
    let hub = hub_over(&venue);
    for call in [
        OptionsCall::Expiries { underlying: "DOGE".to_string() },
        OptionsCall::Chain {
            underlying: "BTC&kind=future".to_string(),
            expiry: "2037-06-25".to_string(),
            strike_window: None,
            r: 0.0,
        },
    ] {
        let text = hub.call(&call).expect_err("not a served underlying");
        assert!(text.contains("served: [BTC, ETH, SOL]"), "{text}");
    }
    assert_eq!(venue.hits.load(Ordering::SeqCst), 0);
}

/// A venue failure is the provider's own words, unchanged — never an empty answer.
#[test]
fn a_provider_failure_is_its_own_words() {
    // Nothing listens at this address: bind, take the port, drop the listener.
    let addr = TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).expect("a free port");
    let hub = OptionsHub::with_provider(
        DeribitOptionsProvider::new().with_book_summary_url(format!("http://{addr}/book")),
    );
    let text = hub.call(&expiries_call()).expect_err("the venue is unreachable");
    assert!(text.starts_with("deribit book summary:"), "{text}");
}

fn instruments(n: usize) -> Vec<Instrument> {
    (0..n)
        .map(|i| {
            Instrument::new(
                "deribit",
                format!("BTC-25JUN37-{i}-C"),
                "BTC",
                "USD",
                vike_model::Contract::CryptoSpot,
            )
        })
        .collect()
}

/// The index fetch asks for `currency=any` and the kind's venue word, and its failure carries the
/// datahub's former prefix and the `CatalogError`'s own `Display`, so the log line is unchanged.
#[test]
fn the_instrument_fetch_asks_for_any_and_keeps_the_former_failure_text() {
    let asked: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&asked);
    let hub =
        OptionsHub::mainnet().with_instruments(Box::new(move |currency: &str, kind: &str| {
            log.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((currency.to_string(), kind.to_string()));
            if kind == "future" {
                Err(CatalogError("HTTP 503".to_string()))
            } else {
                Ok(instruments(2))
            }
        }));
    let ctx = FetchCtx::new();
    assert_eq!(hub.instruments(OptionIndexKind::Option, &ctx).expect("options"), instruments(2));
    assert_eq!(
        hub.instruments(OptionIndexKind::Future, &ctx),
        Err(FetchFailure::Fetch("deribit future instruments: catalog error: HTTP 503".to_string()))
    );
    assert_eq!(
        *asked.lock().unwrap_or_else(PoisonError::into_inner),
        vec![("any".to_string(), "option".to_string()), ("any".to_string(), "future".to_string())]
    );
    ctx.cancel();
    assert!(hub.instruments(OptionIndexKind::Option, &ctx).is_err());
    assert_eq!(
        asked.lock().unwrap_or_else(PoisonError::into_inner).len(),
        2,
        "a cancelled fetch asked"
    );
}

struct NullSink;

impl OptionsFeedSink for NullSink {
    fn marks(&self, _: Vec<OptionTicker>) {}
    fn ticker(&self, _: OptionTicker) {}
}

struct CountedHandle(Arc<AtomicUsize>);

impl OptionsFeedHandle for CountedHandle {
    fn shutdown(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

type Opened = Arc<Mutex<Vec<(&'static str, Vec<String>)>>>;

/// A hub whose two feeds record the focus set they were opened over and count their shutdowns.
fn recording_hub() -> (OptionsHub, Opened, Arc<AtomicUsize>) {
    let opened: Opened = Arc::new(Mutex::new(Vec::new()));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let spawn = |kind: &'static str| -> FeedSpawn {
        let opened = Arc::clone(&opened);
        let shutdowns = Arc::clone(&shutdowns);
        Box::new(move |set, _sink| {
            opened.lock().unwrap_or_else(PoisonError::into_inner).push((kind, set));
            Box::new(CountedHandle(Arc::clone(&shutdowns))) as Box<dyn OptionsFeedHandle>
        })
    };
    let hub = OptionsHub::mainnet().with_feeds(spawn("markprice"), spawn("ticker"));
    (hub, opened, shutdowns)
}

/// Each spec reaches ITS feed with its own focus set, unchanged, and the handle stops it.
#[test]
fn each_feed_spec_opens_its_own_feed_over_its_focus_set() {
    let (hub, opened, shutdowns) = recording_hub();
    let sink: Arc<dyn OptionsFeedSink> = Arc::new(NullSink);
    let mut marks = hub
        .open_feed(
            &OptionsFeedSpec::MarkPrice { indexes: vec!["btc_usd".into(), "eth_usd".into()] },
            Arc::clone(&sink),
        )
        .expect("marks");
    let mut ticker = hub
        .open_feed(
            &OptionsFeedSpec::Ticker { instruments: vec!["BTC-25JUN37-100000-C".into()] },
            sink,
        )
        .expect("ticker");
    assert_eq!(
        *opened.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            ("markprice", vec!["btc_usd".to_string(), "eth_usd".to_string()]),
            ("ticker", vec!["BTC-25JUN37-100000-C".to_string()]),
        ]
    );
    marks.shutdown();
    ticker.shutdown();
    assert_eq!(shutdowns.load(Ordering::SeqCst), 2);
}

/// The focus set is capped by the SERVER's constants even when the server did not run the check: an
/// empty set, a blank name and a set over its cap are refused, and no feed is started.
#[test]
fn the_focus_set_is_capped_by_the_servers_constants() {
    let (hub, opened, _) = recording_hub();
    let sink: Arc<dyn OptionsFeedSink> = Arc::new(NullSink);
    let too_many_indexes = (0..=GW_OPTIONS_MAX_INDEXES).map(|i| format!("idx_{i}")).collect();
    let too_many_instruments =
        (0..=GW_OPTIONS_MAX_INSTRUMENTS).map(|i| format!("BTC-25JUN37-{i}-C")).collect();
    for spec in [
        OptionsFeedSpec::MarkPrice { indexes: Vec::new() },
        OptionsFeedSpec::MarkPrice { indexes: too_many_indexes },
        OptionsFeedSpec::Ticker { instruments: vec!["  ".to_string()] },
        OptionsFeedSpec::Ticker { instruments: too_many_instruments },
    ] {
        assert!(hub.open_feed(&spec, Arc::clone(&sink)).is_err(), "{spec:?}");
    }
    assert!(opened.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
    // At the cap exactly, the feed opens.
    let at_cap = (0..GW_OPTIONS_MAX_INSTRUMENTS).map(|i| format!("BTC-25JUN37-{i}-C")).collect();
    assert!(hub.open_feed(&OptionsFeedSpec::Ticker { instruments: at_cap }, sink).is_ok());
}

/// **A focus-set name must be a real one.** A well-formed word that is no index or instrument of a
/// served underlying would be subscribed, refused by Deribit and retried by the bridge feed's
/// reconnect loop for the life of the socket, so it is refused here and no feed is started. The real
/// shapes the datahub names (`btc_usd`, `sol_usdc`, `SOL_USDC-…`) open.
#[test]
fn a_well_formed_but_unreal_name_is_refused_before_a_feed_is_started() {
    let (hub, opened, _) = recording_hub();
    let sink: Arc<dyn OptionsFeedSink> = Arc::new(NullSink);
    let indexes_of = |indexes: &[&str]| OptionsFeedSpec::MarkPrice {
        indexes: indexes.iter().map(|s| (*s).to_string()).collect(),
    };
    let instruments_of = |names: &[&str]| OptionsFeedSpec::Ticker {
        instruments: names.iter().map(|s| (*s).to_string()).collect(),
    };
    for spec in [
        indexes_of(&["xyz_usd"]),
        indexes_of(&["btcusd"]),
        indexes_of(&["_usd"]),
        indexes_of(&["btc_"]),
        indexes_of(&["btc_usd", "junk"]),
        instruments_of(&["BTC-foo"]),
        instruments_of(&["XYZ-25JUN37-100000-C"]),
        instruments_of(&["BTC-31FEB37-100000-C"]),
        instruments_of(&["BTC-25JUN37-100000-X"]),
        instruments_of(&["BTC-25JUN37-100000-C", "ETH"]),
    ] {
        let err = hub.open_feed(&spec, Arc::clone(&sink)).err().unwrap_or_else(|| {
            panic!("{spec:?} was opened");
        });
        assert!(err.contains("nothing was subscribed"), "{err}");
    }
    assert!(opened.lock().unwrap_or_else(PoisonError::into_inner).is_empty(), "a feed was started");
    for spec in [
        indexes_of(&["btc_usd", "eth_usd", "sol_usdc"]),
        indexes_of(&["BTC_USD"]),
        instruments_of(&["SOL_USDC-25JUN37-150-C", "ETH-25JUN37-4000-P"]),
    ] {
        assert!(hub.open_feed(&spec, Arc::clone(&sink)).is_ok(), "{spec:?}");
    }
}

/// The CHARSET and LENGTH of a name are `OptionsFeedSpec::refusal`'s, and the hub re-runs it: a dot, a
/// space, a control byte, a slash and a paragraph never reach a channel string.
#[test]
fn a_name_that_could_write_the_channel_grammar_is_refused_by_the_hub_too() {
    let (hub, opened, _) = recording_hub();
    let sink: Arc<dyn OptionsFeedSink> = Arc::new(NullSink);
    let long = "b".repeat(GW_OPTIONS_MAX_NAME_BYTES + 1);
    for name in
        ["btc_usd.100ms", "btc usd", "btc_usd\n", "btc/usd", "btc_usd,eth_usd", long.as_str()]
    {
        let spec = OptionsFeedSpec::MarkPrice { indexes: vec![name.to_string()] };
        assert!(hub.open_feed(&spec, Arc::clone(&sink)).is_err(), "{name:?}");
        let spec = OptionsFeedSpec::Ticker { instruments: vec![name.to_string()] };
        assert!(hub.open_feed(&spec, Arc::clone(&sink)).is_err(), "{name:?}");
    }
    assert!(opened.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
}
