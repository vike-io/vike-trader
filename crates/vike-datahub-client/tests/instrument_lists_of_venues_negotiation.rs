//! `DatahubClient::instrument_lists_of_venues`'s NEGOTIATION (decision 0112): a datahub that advertises
//! `FEATURE_INSTRUMENT_LISTS_OF_VENUES` is sent `Request::InstrumentListsOfVenues`; one that keeps lists but predates
//! the verb is asked the WHOLE lists and they are filtered here by the same filter
//! (`InstrumentListsReply::of_venues`), so the answer is the same; one that keeps no list is refused locally, nothing
//! sent.

use vike_catalog::VenueInstrumentList;
use vike_datahub_client::DatahubClient;
use vike_datahub_client::catalog::InstrumentListsReply;
use vike_datahub_client::proto::{
    FEATURE_INSTRUMENT_LISTS, FEATURE_INSTRUMENT_LISTS_OF_VENUES, Request, Response,
};
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_model::{Contract, Instrument, StockSplit};

fn list(venue: &str, symbols: &[&str]) -> VenueInstrumentList {
    VenueInstrumentList {
        venue: venue.to_string(),
        refreshed_ms: Some(1),
        truncated: false,
        last_error: None,
        last_try_ms: Some(1),
        instruments: symbols
            .iter()
            .map(|s| Instrument::new(venue, *s, "BTC", "USDT", Contract::CryptoSpot))
            .collect(),
        added_by_operator: Vec::new(),
    }
}

/// Two venues' lists and a split each: the whole answer a datahub keeps.
fn whole() -> InstrumentListsReply {
    InstrumentListsReply {
        venues: vec![list("binance", &["BTCUSDT", "ETHUSDT"]), list("okx", &["BTC-USDT"])],
        operator: Vec::new(),
        splits: vec![
            StockSplit::new("binance", "BTCUSDT", "2020-08-31", 4.0).expect("a split"),
            StockSplit::new("okx", "BTC-USDT", "2020-08-31", 2.0).expect("a split"),
        ],
    }
}

fn binance() -> Vec<String> {
    vec!["binance".to_string()]
}

/// A datahub too old to filter is asked the whole lists, and the answer is the named venue's parts alone, the whole
/// answer's own bytes.
#[test]
fn an_older_datahub_is_asked_the_whole_lists_and_they_are_filtered_here() {
    let (addr, rx, handle) = spawn_fake_peer(vec![FEATURE_INSTRUMENT_LISTS.to_string()], |r| {
        matches!(r, Request::InstrumentLists).then(|| Response::InstrumentLists(whole()))
    });
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let got = client.instrument_lists_of_venues(&binance()).expect("the lists");
    assert_eq!(json(&got.venues), json(&whole().venues[..1]));
    assert_eq!(json(&got.splits), json(&whole().splits[..1]));
    assert!(got.operator.is_empty());
    drop(client);
    assert!(matches!(rx.recv().expect("sent")[..], [Request::InstrumentLists]));
    handle.join().expect("joins");
}

/// A datahub that filters is sent the venues, and its answer is passed on as it came.
#[test]
fn a_filtering_datahub_is_sent_the_venues() {
    let features =
        vec![FEATURE_INSTRUMENT_LISTS.to_string(), FEATURE_INSTRUMENT_LISTS_OF_VENUES.to_string()];
    let (addr, rx, handle) = spawn_fake_peer(features, |r| match r {
        Request::InstrumentListsOfVenues { venues } => {
            Some(Response::InstrumentLists(whole().of_venues(venues)))
        }
        _ => None,
    });
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let got = client.instrument_lists_of_venues(&binance()).expect("the lists");
    assert_eq!(got, whole().of_venues(&binance()));
    drop(client);
    let sent = rx.recv().expect("sent");
    assert!(
        matches!(&sent[..], [Request::InstrumentListsOfVenues { venues }] if *venues == binance()),
        "{sent:?}"
    );
    handle.join().expect("joins");
}

/// A datahub that keeps no list is refused by name, nothing sent.
#[test]
fn a_datahub_without_lists_is_refused_locally() {
    let (addr, rx, handle) = spawn_fake_peer(Vec::new(), |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let err = client.instrument_lists_of_venues(&binance()).expect_err("refused");
    assert!(err.contains(FEATURE_INSTRUMENT_LISTS), "{err}");
    drop(client);
    assert!(rx.recv().expect("sent").is_empty(), "nothing was sent");
    handle.join().expect("joins");
}

/// The JSON bytes of a value, what the wire carries.
fn json<T: serde::Serialize + ?Sized>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("json")
}
