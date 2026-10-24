//! The REAL REST lane's history verbs through the real server, with the client crate's own
//! `RemoteKlines`, `RemoteFunding` and `RemoteTicks` as the peer: the kline and funding registries,
//! the dukascopy tick lane, and the cancel contract.
//!
//! Drift register rows held here: R6 (the table's order and its refusal text, the `SourceError`
//! variants and their words) and the cancel half of R8 (a `FetchCancel` stops the tick walk between
//! hour files). The venue sources are scripted, so nothing leaves the box; the REAL registries are
//! only asked for their refusals, which are decided before any source is called.

#![cfg(feature = "data")]

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/rest_server.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod rest_server;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_data::source::{FundingRatePoint, FundingRateSource, KlineSource, SourceError};
use vike_gatewayhub::rest::lane::VenueRest;
use vike_gatewayhub_client::history::{
    FUNDING_VENUES, KLINE_VENUES, RemoteFunding, RemoteKlines, RemoteTicks,
};
use vike_marketdata::{Bar, QuoteTick};

use rest_server::RestServer;

type KlineScript = dyn Fn(&str, &str, i64, i64) -> Result<Vec<Bar>, SourceError> + Send + Sync;

/// A scripted kline source: the venue it answers for and what it does.
struct ScriptedKlines {
    venue: &'static str,
    script: Box<KlineScript>,
}

impl KlineSource for ScriptedKlines {
    fn venue(&self) -> &str {
        self.venue
    }

    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        (self.script)(symbol, interval, start_ms, end_ms)
    }
}

type FundingScript =
    dyn Fn(&str, i64, i64) -> Result<Vec<FundingRatePoint>, SourceError> + Send + Sync;

struct ScriptedFunding {
    venue: &'static str,
    script: Box<FundingScript>,
}

impl FundingRateSource for ScriptedFunding {
    fn venue(&self) -> &str {
        self.venue
    }

    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<FundingRatePoint>, SourceError> {
        (self.script)(symbol, start_ms, end_ms)
    }
}

/// The lane holds `&'static` registries (a `static` slice of `&'static dyn` sources), so a suite leaks
/// its scripted ones: a handful of small allocations per test process.
fn leak_klines(sources: Vec<ScriptedKlines>) -> &'static [&'static dyn KlineSource] {
    let refs: Vec<&'static dyn KlineSource> = sources
        .into_iter()
        .map(|s| {
            let leaked: &'static ScriptedKlines = Box::leak(Box::new(s));
            leaked as &'static dyn KlineSource
        })
        .collect();
    Box::leak(refs.into_boxed_slice())
}

fn leak_funding(sources: Vec<ScriptedFunding>) -> &'static [&'static dyn FundingRateSource] {
    let refs: Vec<&'static dyn FundingRateSource> = sources
        .into_iter()
        .map(|s| {
            let leaked: &'static ScriptedFunding = Box::leak(Box::new(s));
            leaked as &'static dyn FundingRateSource
        })
        .collect();
    Box::leak(refs.into_boxed_slice())
}

fn bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let x = i as f64;
            Bar {
                ts: i as i64 * 60_000,
                open: 0.1 * x,
                high: 0.1 * x + 1.0 / 3.0,
                low: 0.1 * x - 1.0 / 7.0,
                close: 0.1 * x + 1.0 / 9.0,
                volume: x / 7.0,
                funding: Some(x * 1e-7),
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect()
}

/// **The refusal text names the REAL registries in the observable order.** The production lane's
/// facts are read off `KLINE_SOURCES` and `FUNDING_SOURCES`, and an unserved venue is refused by the
/// server before any source is called, naming the served set in order — the text the datahub's table
/// printed, and the order the client's rosters carry. No venue is asked: the real sources are in the
/// lane but a refusal never reaches them.
#[test]
fn the_real_registries_refuse_an_unserved_venue_naming_the_roster_in_order() {
    let server = RestServer::start(VenueRest::new(false));

    let err = RemoteKlines::new(server.link(), "kraken")
        .fetch("BTCUSDT", "1m", 0, 1)
        .expect_err("kraken is not a kline venue");
    let SourceError::Refused(text) = err else { panic!("an unserved venue is a Refused: {err:?}") };
    assert!(
        text.contains(&format!("(served: [{}])", KLINE_VENUES.join(", "))),
        "the refusal must print the client's kline roster in order: {text}"
    );
    assert!(text.contains("`kraken`") && text.contains("nothing was asked of any venue"), "{text}");

    let err = RemoteFunding::new(server.link(), "bybit")
        .fetch("BTCUSDT", 0, 1)
        .expect_err("bybit has no funding source");
    let SourceError::Refused(text) = err else { panic!("an unserved venue is a Refused: {err:?}") };
    assert!(text.contains(&format!("(served: [{}])", FUNDING_VENUES.join(", "))), "{text}");
}

/// The scripted registry answers for ITS venue only; rows cross bit-exactly; and each
/// `SourceError` variant crosses with its inner words unchanged.
#[test]
fn a_fetch_reaches_the_source_of_its_venue_and_keeps_rows_and_error_words() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = |name: &'static str, calls: &Arc<Mutex<Vec<String>>>| {
        let calls = Arc::clone(calls);
        move |symbol: &str, interval: &str, start: i64, end: i64| {
            calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("{name} {symbol} {interval} {start} {end}"));
        }
    };
    let (note_a, note_b, note_c) = (log("a", &calls), log("b", &calls), log("c", &calls));
    let registry = leak_klines(vec![
        ScriptedKlines {
            venue: "binance",
            script: Box::new(move |s: &str, i: &str, a: i64, b: i64| {
                note_a(s, i, a, b);
                Ok(bars(3))
            }),
        },
        ScriptedKlines {
            venue: "bybit",
            script: Box::new(move |s: &str, i: &str, a: i64, b: i64| {
                note_b(s, i, a, b);
                Err(SourceError::Fetch("venue fetch: HTTP 429 — {\"retCode\": 10006}".into()))
            }),
        },
        ScriptedKlines {
            venue: "okx",
            script: Box::new(move |s: &str, i: &str, a: i64, b: i64| {
                note_c(s, i, a, b);
                Err(SourceError::Refused("okx: unsupported interval \"7m\"".into()))
            }),
        },
    ]);
    let server = RestServer::start(VenueRest::new(false).with_klines(registry));

    let got = RemoteKlines::new(server.link(), "binance")
        .fetch("BTCUSDT", "1m", 10, 20)
        .expect("binance's rows");
    assert_eq!(got.len(), 3);
    for (g, w) in got.iter().zip(bars(3)) {
        assert_eq!(
            (g.ts, g.open.to_bits(), g.high.to_bits(), g.low.to_bits(), g.close.to_bits()),
            (w.ts, w.open.to_bits(), w.high.to_bits(), w.low.to_bits(), w.close.to_bits()),
            "a bar drifted over the hop"
        );
        assert_eq!(g.volume.to_bits(), w.volume.to_bits());
        assert_eq!(g.funding.map(f64::to_bits), w.funding.map(f64::to_bits));
    }
    assert_eq!(
        RemoteKlines::new(server.link(), "bybit").fetch("BTCUSDT", "1m", 10, 20),
        Err(SourceError::Fetch("venue fetch: HTTP 429 — {\"retCode\": 10006}".into())),
        "a Fetch keeps its variant AND its words"
    );
    assert_eq!(
        RemoteKlines::new(server.link(), "okx").fetch("BTC-USDT", "7m", 10, 20),
        Err(SourceError::Refused("okx: unsupported interval \"7m\"".into())),
        "a Refused keeps its variant AND its words"
    );
    // Exactly one call per fetch, to the source of that fetch's venue.
    assert_eq!(
        *calls.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            "a BTCUSDT 1m 10 20".to_string(),
            "b BTCUSDT 1m 10 20".to_string(),
            "c BTC-USDT 7m 10 20".to_string(),
        ]
    );
}

/// Funding rows cross bit-exactly (premium included) and are dispatched by venue.
#[test]
fn funding_is_dispatched_by_venue_and_crosses_bit_exact() {
    let points = vec![
        FundingRatePoint { ts_ms: 1, rate: 0.000_1 + 0.000_2, premium: Some(1.0 / 3.0) },
        FundingRatePoint { ts_ms: 2, rate: -0.000_05, premium: None },
    ];
    let served = points.clone();
    let registry = leak_funding(vec![ScriptedFunding {
        venue: "binance",
        script: Box::new(move |_: &str, _: i64, _: i64| Ok(served.clone())),
    }]);
    let server = RestServer::start(VenueRest::new(false).with_funding(registry));
    let got = RemoteFunding::new(server.link(), "binance").fetch("BTCUSDT", 0, 9).expect("rows");
    assert_eq!(got.len(), points.len());
    for (g, w) in got.iter().zip(&points) {
        assert_eq!((g.ts_ms, g.rate.to_bits(), g.premium), (w.ts_ms, w.rate.to_bits(), w.premium));
    }
}

fn quote(ts: i64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid: 1.0 + ts as f64 / 3.0e9,
        ask: 1.0 + ts as f64 / 3.0e9 + 1e-4,
        bid_size: 1.5,
        ask_size: 2.5,
        symbol: "EURUSD".to_string(),
    }
}

const HOUR_MS: i64 = 3_600_000;

/// The dukascopy tick lane answers per hour file, and the concatenation over the wire is the whole
/// window in order — the same ticks the bridge's whole-window fetch returns.
#[test]
fn the_tick_lane_walks_hour_files_and_the_wire_carries_their_concatenation() {
    let asked = Arc::new(Mutex::new(Vec::<(i64, i64)>::new()));
    let log = Arc::clone(&asked);
    let fetch: vike_gatewayhub::rest::ticks::QuoteFetch =
        Arc::new(move |_symbol: &str, from: i64, to: i64| {
            log.lock().unwrap_or_else(PoisonError::into_inner).push((from, to));
            // One tick at the start of the slice's hour and one ten minutes in.
            let hour = from.div_euclid(HOUR_MS) * HOUR_MS;
            Ok([hour + 7, hour + 600_007]
                .into_iter()
                .filter(|ts| from <= *ts && *ts <= to)
                .map(quote)
                .collect())
        });
    let server = RestServer::start(VenueRest::new(false).with_ticks(fetch));
    let got = RemoteTicks::new(server.link())
        .fetch_quotes_range("EURUSD", 0, 3 * HOUR_MS - 1, None)
        .expect("three hours of ticks");
    let want: Vec<QuoteTick> =
        (0..3).flat_map(|h| [h * HOUR_MS + 7, h * HOUR_MS + 600_007]).map(quote).collect();
    assert_eq!(got, want);
    assert_eq!(
        *asked.lock().unwrap_or_else(PoisonError::into_inner),
        vec![(0, HOUR_MS - 1), (HOUR_MS, 2 * HOUR_MS - 1), (2 * HOUR_MS, 3 * HOUR_MS - 1)],
        "each call names one hour file"
    );
}

/// A bridge failure on one hour fails the whole fetch in the bridge's own words.
#[test]
fn a_failed_hour_fails_the_tick_fetch_in_the_bridges_words() {
    let fetch: vike_gatewayhub::rest::ticks::QuoteFetch = Arc::new(|_: &str, from: i64, _: i64| {
        if from >= HOUR_MS {
            Err("network: connection reset by peer".to_string())
        } else {
            Ok(vec![quote(from + 7)])
        }
    });
    let server = RestServer::start(VenueRest::new(false).with_ticks(fetch));
    let err = RemoteTicks::new(server.link())
        .fetch_quotes_range("EURUSD", 0, 2 * HOUR_MS, None)
        .expect_err("the second hour failed");
    assert!(err.contains("network: connection reset by peer"), "{err}");
}

/// **A cancel lands between hour files.** The client's stop probe fires while the lane is inside the
/// second hour; the client sends `FetchCancel` and drops the connection; the lane finishes THAT hour
/// (a download cannot be interrupted) and asks for no further one.
#[test]
fn a_cancel_stops_the_tick_walk_at_the_next_hour_boundary() {
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    let fetch: vike_gatewayhub::rest::ticks::QuoteFetch =
        Arc::new(move |_: &str, from: i64, _: i64| {
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(150));
            Ok(vec![quote(from + 7)])
        });
    let server = RestServer::start(VenueRest::new(false).with_ticks(fetch));
    let probe_count = Arc::clone(&asked);
    let probe = move || probe_count.load(Ordering::SeqCst) >= 2;
    let err = RemoteTicks::new(server.link())
        .fetch_quotes_range("EURUSD", 0, 8 * HOUR_MS - 1, Some(&probe))
        .expect_err("cancelled by the client's probe");
    assert!(err.contains("cancel"), "{err}");

    // The lane notices at its next boundary and stops: far fewer than the eight hours.
    let settle = Instant::now() + Duration::from_millis(900);
    while Instant::now() < settle {
        std::thread::sleep(Duration::from_millis(20));
    }
    let total = asked.load(Ordering::SeqCst);
    assert!((2..=3).contains(&total), "the walk went on after the cancel: {total} hours asked");
}
