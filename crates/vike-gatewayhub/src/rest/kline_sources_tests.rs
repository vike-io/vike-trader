use std::sync::atomic::{AtomicUsize, Ordering};

use vike_data::source::{FundingRateSource, KlineSource, SourceError};
use vike_gatewayhub_client::history::{FUNDING_VENUES, KLINE_VENUES};
use vike_marketdata::FundingRatePoint;

use vike_secrets::venue_setting::seeded_venue_settings;

use super::*;

/// The registries BUILT from the release's address rows: what a fresh box serves.
fn built() -> (Vec<String>, Vec<String>) {
    let seeded = seeded_venue_settings();
    (kline_venues(&kline_sources(&seeded)), funding_venues(&funding_sources(&seeded)))
}

/// The registries are the client's rosters, IN ORDER (which the registry's rows are). The datahub's
/// backfill table folds the client's copy of this order and the daemon advertises this one: a reorder
/// here that the client did not follow is an unknown-venue refusal naming the venues in a different order.
#[test]
fn the_registries_are_the_clients_rosters_in_order() {
    let (klines, funding) = built();
    assert_eq!(
        klines, KLINE_VENUES,
        "KLINE_SOURCES' order is observable; a reorder is a decision, not an edit"
    );
    assert_eq!(funding, FUNDING_VENUES);
}

/// Each row's venue word is the venue its built source answers for, so the address rows a row is
/// built on are its own venue's (and a row refused at the build keeps the right name in its place).
#[test]
fn every_row_builds_from_the_seed_as_the_venue_it_names() {
    let seeded = seeded_venue_settings();
    for &(venue, build) in KLINE_SOURCES {
        let source = build(&venue_urls(&seeded, venue)).unwrap_or_else(|e| panic!("{venue}: {e}"));
        assert_eq!(source.venue(), venue);
    }
    for &(venue, build) in FUNDING_SOURCES {
        let source = build(&venue_urls(&seeded, venue)).unwrap_or_else(|e| panic!("{venue}: {e}"));
        assert_eq!(source.venue(), venue);
    }
}

/// A row refused at the build stays advertised in its place and refuses every fetch as a `Fetch`
/// (never a `Refused`, which the datahub keeps as "cannot be expressed"), naming the row.
#[test]
fn a_row_refused_at_the_build_keeps_its_place_and_refuses_naming_the_row() {
    use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType};
    let words = "binance: no address row for live history rest";
    let why = UrlRefusal::Missing {
        venue: "binance".into(),
        ty: UrlType::Live,
        channel: UrlChannel::History,
        protocol: UrlProtocol::Rest,
        product: String::new(),
    };
    assert!(why.to_string().starts_with(words), "{why}");
    let unbuilt: Box<dyn KlineSource> = Box::new(Unbuilt::logged("klines", "binance", &why));
    let sources = [unbuilt];
    assert_eq!(kline_venues(&sources), ["binance"]);
    let ctx = FetchContext::new();
    let Err(SourceError::Fetch(text)) = fetch_klines(&sources, "binance", "S", "1m", 0, 1, &ctx)
    else {
        panic!("an unbuilt row must refuse as a Fetch");
    };
    assert!(text.text().contains("NOT RUNNING") && text.text().contains(words), "{text}");
}

/// Every source reports a roster slug and none is listed twice: the table is keyed by `venue()`.
#[test]
fn every_source_is_a_roster_slug_listed_once() {
    let (klines, funding) = built();
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
        timestamp: ts,
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
    let ctx = FetchContext::new();
    let got = fetch_klines(&sources, "bybit", "BTCUSDT", "1m", 0, 1, &ctx).expect("bybit's rows");
    assert_eq!(got, vec![bar(2), bar(3)]);
    assert_eq!((first.calls(), second.calls()), (0, 1));
}

/// `SourceError::{Fetch, Refused}` keep their variant AND their inner text byte for byte: the
/// datahub stores a `Refused` as "cannot be expressed" and retries a `Fetch`.
#[test]
fn source_errors_keep_their_variant_and_their_words() {
    let words = "venue fetch: HTTP 418 — {\"msg\": \"I'm a teapot\", \"retry\": 3}";
    let failing = Scripted::new("binance", Err(SourceError::Fetch(words.into())));
    let refusing = Scripted::new("bybit", Err(SourceError::Refused(words.into())));
    let sources: [&dyn KlineSource; 2] = [&failing, &refusing];
    let ctx = FetchContext::new();
    assert_eq!(
        fetch_klines(&sources, "binance", "S", "1m", 0, 1, &ctx),
        Err(SourceError::Fetch(words.into()))
    );
    assert_eq!(
        fetch_klines(&sources, "bybit", "S", "1m", 0, 1, &ctx),
        Err(SourceError::Refused(words.into()))
    );
}

/// A venue the registry does not carry is a refusal naming the registry in its order, and no source
/// is asked.
#[test]
fn an_unknown_venue_is_refused_naming_the_registry_in_order() {
    let only = Scripted::new("binance", Ok(Vec::new()));
    let other = Scripted::new("bybit", Ok(Vec::new()));
    let sources: [&dyn KlineSource; 2] = [&only, &other];
    let ctx = FetchContext::new();
    let Err(SourceError::Refused(text)) = fetch_klines(&sources, "kraken", "S", "1m", 0, 1, &ctx)
    else {
        panic!("an unknown venue must be a Refused");
    };
    assert!(
        text.text().contains("`kraken`") && text.text().contains("(served: [binance, bybit])"),
        "{text}"
    );
    assert!(text.text().contains("nothing was asked of any venue"), "{text}");
    assert_eq!((only.calls(), other.calls()), (0, 0));
}

/// A fetch cancelled while it waited never reaches the venue: the one point a one-batch source can
/// be stopped at.
#[test]
fn a_cancelled_fetch_never_reaches_the_venue() {
    let source = Scripted::new("binance", Ok(vec![bar(1)]));
    let sources: [&dyn KlineSource; 1] = [&source];
    let ctx = FetchContext::new();
    ctx.cancel();
    let out = fetch_klines(&sources, "binance", "S", "1m", 0, 1, &ctx);
    assert!(matches!(out, Err(SourceError::Fetch(_))), "{out:?}");
    assert_eq!(source.calls(), 0);
}

/// Funding rows cross as the wire mirror, bit for bit, premium and mark included.
#[test]
fn funding_points_cross_bit_exact_and_errors_keep_their_words() {
    let points = vec![
        FundingRatePoint {
            timestamp_ms: 1,
            rate: 0.000_1 + 0.000_2,
            premium: Some(1.0 / 3.0),
            mark: Some(2.0 / 3.0),
        },
        FundingRatePoint { timestamp_ms: 2, rate: -0.000_05, premium: None, mark: None },
    ];
    let good = ScriptedFunding {
        venue: "binance",
        answer: Ok(points.clone()),
        calls: AtomicUsize::new(0),
    };
    let bad = ScriptedFunding {
        venue: "hyperliquid",
        answer: Err(SourceError::Refused("funding history needs a perp".into())),
        calls: AtomicUsize::new(0),
    };
    let sources: [&dyn FundingRateSource; 2] = [&good, &bad];
    let ctx = FetchContext::new();
    let wire = fetch_funding(&sources, "binance", "BTCUSDT", 0, 9, &ctx).expect("funding");
    assert_eq!(wire.len(), points.len());
    for (w, p) in wire.iter().zip(&points) {
        assert_eq!(
            (w.timestamp_ms, w.rate.to_bits(), w.premium, w.mark.map(f64::to_bits)),
            (p.timestamp_ms, p.rate.to_bits(), p.premium, p.mark.map(f64::to_bits))
        );
    }
    assert_eq!(
        fetch_funding(&sources, "hyperliquid", "BTC", 0, 9, &ctx),
        Err(SourceError::Refused("funding history needs a perp".into()))
    );
    assert_eq!(bad.calls.load(Ordering::SeqCst), 1);
}
