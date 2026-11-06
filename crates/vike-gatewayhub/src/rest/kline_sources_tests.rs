use std::sync::atomic::{AtomicUsize, Ordering};

use vike_data::source::{FundingRatePoint, FundingRateSource, KlineSource, SourceError};
use vike_gatewayhub_client::history::{FUNDING_VENUES, KLINE_VENUES};

use super::*;
use crate::registry::{Lane, roster};

/// The statics are the client's rosters, IN ORDER, and the registry's. The datahub's backfill table
/// folds the client's copy of this order and the daemon advertises this one: a reorder here that the
/// client did not follow is an unknown-venue refusal naming the venues in a different order.
#[test]
fn the_registries_are_the_clients_rosters_and_the_registrys_in_order() {
    assert_eq!(
        kline_venues(KLINE_SOURCES),
        KLINE_VENUES,
        "KLINE_SOURCES' order is observable; a reorder is a decision, not an edit"
    );
    assert_eq!(funding_venues(FUNDING_SOURCES), FUNDING_VENUES);
    assert_eq!(kline_venues(KLINE_SOURCES), roster(Lane::Klines));
    assert_eq!(funding_venues(FUNDING_SOURCES), roster(Lane::Funding));
}

/// Every source reports a roster slug and none is listed twice: the table is keyed by `venue()`.
#[test]
fn every_source_is_a_roster_slug_listed_once() {
    let klines = kline_venues(KLINE_SOURCES);
    let funding = funding_venues(FUNDING_SOURCES);
    for list in [&klines, &funding] {
        for (i, venue) in list.iter().enumerate() {
            assert!(
                vike_model::VENUES.contains(&venue.as_str()),
                "`{venue}` is not a vike_model::VENUES slug"
            );
            assert!(!list[..i].contains(venue), "`{venue}` is listed twice");
        }
    }
}

fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.0 / 3.0,
        high: 2.0 / 3.0,
        low: 0.1,
        close: 0.1 + 0.2,
        volume: 7.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A scripted kline source that counts its calls.
struct Scripted {
    venue: &'static str,
    answer: Result<Vec<Bar>, SourceError>,
    calls: AtomicUsize,
}

impl Scripted {
    fn new(venue: &'static str, answer: Result<Vec<Bar>, SourceError>) -> Self {
        Self { venue, answer, calls: AtomicUsize::new(0) }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl KlineSource for Scripted {
    fn venue(&self) -> &str {
        self.venue
    }

    fn fetch(&self, _: &str, _: &str, _: i64, _: i64) -> Result<Vec<Bar>, SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answer.clone()
    }
}

struct ScriptedFunding {
    venue: &'static str,
    answer: Result<Vec<FundingRatePoint>, SourceError>,
    calls: AtomicUsize,
}

impl FundingRateSource for ScriptedFunding {
    fn venue(&self) -> &str {
        self.venue
    }

    fn fetch(&self, _: &str, _: i64, _: i64) -> Result<Vec<FundingRatePoint>, SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answer.clone()
    }
}

/// The venue named is the venue asked, and only that one.
#[test]
fn a_fetch_reaches_the_source_of_its_venue_and_no_other() {
    let first = Scripted::new("binance", Ok(vec![bar(1)]));
    let second = Scripted::new("bybit", Ok(vec![bar(2), bar(3)]));
    let sources: [&dyn KlineSource; 2] = [&first, &second];
    let ctx = FetchCtx::new();
    let got = fetch_klines(&sources, "bybit", "BTCUSDT", "1m", 0, 1, &ctx).expect("bybit's rows");
    assert_eq!(got, vec![bar(2), bar(3)]);
    assert_eq!((first.calls(), second.calls()), (0, 1));
}

/// `SourceError::{Fetch, Refused}` keep their variant AND their inner text byte for byte: the
/// datahub stores a `Refused` as "cannot be expressed" and retries a `Fetch`.
#[test]
fn source_errors_keep_their_variant_and_their_words() {
    let words = "venue fetch: HTTP 418 — {\"msg\": \"I'm a teapot\", \"retry\": 3}";
    let failing = Scripted::new("binance", Err(SourceError::Fetch(words.to_string())));
    let refusing = Scripted::new("bybit", Err(SourceError::Refused(words.to_string())));
    let sources: [&dyn KlineSource; 2] = [&failing, &refusing];
    let ctx = FetchCtx::new();
    assert_eq!(
        fetch_klines(&sources, "binance", "S", "1m", 0, 1, &ctx),
        Err(FetchFailure::Fetch(words.to_string()))
    );
    assert_eq!(
        fetch_klines(&sources, "bybit", "S", "1m", 0, 1, &ctx),
        Err(FetchFailure::Refused(words.to_string()))
    );
}

/// A venue the registry does not carry is a refusal naming the registry in its order, and no source
/// is asked.
#[test]
fn an_unknown_venue_is_refused_naming_the_registry_in_order() {
    let only = Scripted::new("binance", Ok(Vec::new()));
    let other = Scripted::new("bybit", Ok(Vec::new()));
    let sources: [&dyn KlineSource; 2] = [&only, &other];
    let ctx = FetchCtx::new();
    let Err(FetchFailure::Refused(text)) = fetch_klines(&sources, "kraken", "S", "1m", 0, 1, &ctx)
    else {
        panic!("an unknown venue must be a Refused");
    };
    assert!(text.contains("`kraken`") && text.contains("(served: [binance, bybit])"), "{text}");
    assert!(text.contains("nothing was asked of any venue"), "{text}");
    assert_eq!((only.calls(), other.calls()), (0, 0));
}

/// A fetch cancelled while it waited never reaches the venue: the one point a one-batch source can
/// be stopped at.
#[test]
fn a_cancelled_fetch_never_reaches_the_venue() {
    let source = Scripted::new("binance", Ok(vec![bar(1)]));
    let sources: [&dyn KlineSource; 1] = [&source];
    let ctx = FetchCtx::new();
    ctx.cancel();
    let out = fetch_klines(&sources, "binance", "S", "1m", 0, 1, &ctx);
    assert!(matches!(out, Err(FetchFailure::Fetch(_))), "{out:?}");
    assert_eq!(source.calls(), 0);
}

/// Funding rows cross as the wire mirror, bit for bit, premium included.
#[test]
fn funding_points_cross_bit_exact_and_errors_keep_their_words() {
    let points = vec![
        FundingRatePoint { ts_ms: 1, rate: 0.000_1 + 0.000_2, premium: Some(1.0 / 3.0) },
        FundingRatePoint { ts_ms: 2, rate: -0.000_05, premium: None },
    ];
    let good = ScriptedFunding {
        venue: "binance",
        answer: Ok(points.clone()),
        calls: AtomicUsize::new(0),
    };
    let bad = ScriptedFunding {
        venue: "hyperliquid",
        answer: Err(SourceError::Refused("funding history needs a perp".to_string())),
        calls: AtomicUsize::new(0),
    };
    let sources: [&dyn FundingRateSource; 2] = [&good, &bad];
    let ctx = FetchCtx::new();
    let wire = fetch_funding(&sources, "binance", "BTCUSDT", 0, 9, &ctx).expect("funding");
    assert_eq!(wire.len(), points.len());
    for (w, p) in wire.iter().zip(&points) {
        assert_eq!((w.ts_ms, w.rate.to_bits(), w.premium), (p.ts_ms, p.rate.to_bits(), p.premium));
    }
    assert_eq!(
        fetch_funding(&sources, "hyperliquid", "BTC", 0, 9, &ctx),
        Err(FetchFailure::Refused("funding history needs a perp".to_string()))
    );
    assert_eq!(bad.calls.load(Ordering::SeqCst), 1);
}
