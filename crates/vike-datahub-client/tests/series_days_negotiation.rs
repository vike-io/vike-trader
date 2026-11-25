//! `Request::SeriesDays`' tick kinds, NEGOTIATED by `series_days_kinds`
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`): the field is
//! additive, a bar read's frame is the bytes it was, and a client sends a tick `kind` only to a server that reads it,
//! because an older one would skip the unknown field and answer the BAR days of the series.

use vike_data::{BarDay, TimestampRange};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{
    FEATURE_SERIES_DAYS, FEATURE_SERIES_DAYS_KINDS, Request, Response,
};
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_model::StoreKindId;

/// A bar read's frame as every client wrote it before the tick kinds (no `kind`, its `interval` always there).
const OLD_FRAME: &str = r#"{"SeriesDays":{"venue":"binance","symbol":"BTCUSDT","interval":"1h","start":0,"end":86399999}}"#;

/// **An old frame without `kind` is the bytes it was and reads as a bar read**: it parses with no kind and its
/// interval, and the request a client builds for bars today serializes back to it byte for byte.
#[test]
fn a_bar_frame_without_kind_is_the_bytes_it_was_and_reads_as_bars() {
    let read: Request = serde_json::from_str(OLD_FRAME).expect("an old frame parses");
    let Request::SeriesDays { kind, interval, source, .. } = &read else {
        panic!("not SeriesDays: {read:?}")
    };
    assert_eq!((kind, interval.as_deref(), source), (&None, Some("1h"), &None), "a bar read");
    assert_eq!(serde_json::to_string(&read).expect("serializes"), OLD_FRAME, "byte for byte");
    let today = Request::SeriesDays {
        venue: "binance".into(),
        source: None,
        symbol: "BTCUSDT".into(),
        interval: Some("1h".into()),
        start: Some(0),
        end: Some(86_399_999),
        kind: None,
    };
    assert_eq!(serde_json::to_string(&today).expect("serializes"), OLD_FRAME);
}

/// What a fake peer answers every `SeriesDays` with: one stamped day.
fn one_day(request: &Request) -> Option<Response> {
    matches!(request, Request::SeriesDays { .. })
        .then(|| Response::SeriesDays(Some(vec![BarDay { day: 3, stamp: 7, provisional: false }])))
}

/// The `SeriesDays` requests a client sends a peer advertising `features`, asked for one bar read and one trade read.
fn sent_to(features: &[&str]) -> (Vec<Request>, Option<Vec<BarDay>>, Option<Vec<BarDay>>) {
    let features = features.iter().map(|f| f.to_string()).collect();
    let (addr, rx, handle) = spawn_fake_peer(features, one_day);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let range = TimestampRange::of(0, 86_399_999);
    let bars = client
        .series_days(StoreKindId::Bar, "binance", None, "BTCUSDT", Some("1h"), range)
        .expect("a bar read");
    let trades = client
        .series_days(StoreKindId::Trade, "binance", None, "BTCUSDT", None, range)
        .expect("a trade read");
    drop(client);
    let sent = rx.recv().expect("the peer's record");
    handle.join().expect("the peer ends");
    (sent, bars, trades)
}

/// **A new client against a server without `series_days_kinds` sends no tick `kind`**: the bar read crosses with no
/// `kind`, the trade read sends nothing at all and answers `None` (the cache then keeps nothing).
#[test]
fn a_server_without_the_kinds_is_sent_no_tick_kind() {
    let (sent, bars, trades) = sent_to(&[FEATURE_SERIES_DAYS]);
    assert!(bars.is_some(), "the bar read is answered");
    assert_eq!(trades, None, "no tick read is asked of a server that would answer bar days");
    assert_eq!(sent.len(), 1, "one frame, the bar read's: {sent:?}");
    assert!(
        matches!(&sent[0], Request::SeriesDays { kind: None, interval: Some(i), .. } if i == "1h"),
        "{sent:?}"
    );
}

/// A server advertising the kinds is sent the trade read with its `kind`, no interval and no source; the bar read
/// still crosses with no `kind`.
#[test]
fn a_server_with_the_kinds_is_sent_the_tick_kind() {
    let (sent, bars, trades) = sent_to(&[FEATURE_SERIES_DAYS, FEATURE_SERIES_DAYS_KINDS]);
    assert!(bars.is_some() && trades.is_some(), "both reads are answered");
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(matches!(&sent[0], Request::SeriesDays { kind: None, .. }), "{sent:?}");
    assert!(
        matches!(
            &sent[1],
            Request::SeriesDays {
                kind: Some(StoreKindId::Trade),
                interval: None,
                source: None,
                ..
            }
        ),
        "{sent:?}"
    );
}

/// A server without `series_days` at all is sent nothing, whatever the kind.
#[test]
fn a_server_without_the_verb_is_sent_nothing() {
    let (sent, bars, trades) = sent_to(&[]);
    assert_eq!((bars, trades), (None, None));
    assert!(sent.is_empty(), "{sent:?}");
}
