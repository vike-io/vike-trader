//! The stamped kept rows' NEGOTIATION (decision 0112; the owner's "yes both" of 2026-11-19): a datahub that does not
//! advertise `FEATURE_KEPT_ROWS_STAMPED` (one from before the verb) is never sent `Request::KeptRowsOfVenues` —
//! `DatahubClient::kept_rows_of_venues` refuses locally, and `HistoryStoreDatahub` holds no encoded form and reads the
//! JSON verb, as before. The stamped path against a real datahub, and an older reader against a newer datahub (the
//! JSON verb still answered beside it), are `crates/vike-datahub/tests/default_build/kept_rows_stamped.rs`'s.

use std::collections::BTreeMap;

use vike_catalog::VenueInstrumentList;
use vike_data::HistoryStore;
use vike_datahub_client::catalog::InstrumentListsReply;
use vike_datahub_client::proto::{
    FEATURE_INSTRUMENT_LISTS, FEATURE_INSTRUMENT_LISTS_OF_VENUES, FEATURE_KEPT_ROWS_STAMPED,
    Request, Response,
};
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};
use vike_model::{Contract, Instrument, StockSplit};

fn binance() -> Vec<String> {
    vec!["binance".to_string()]
}

/// One venue's list and a split: what the older datahub keeps.
fn whole() -> InstrumentListsReply {
    InstrumentListsReply {
        venues: vec![VenueInstrumentList {
            venue: "binance".to_string(),
            refreshed_ms: Some(1),
            truncated: false,
            last_error: None,
            last_try_ms: Some(1),
            instruments: vec![Instrument::new(
                "binance",
                "BTCUSDT",
                "BTC",
                "USDT",
                Contract::CryptoSpot,
            )],
            added_by_operator: Vec::new(),
        }],
        operator: Vec::new(),
        splits: vec![StockSplit::new("binance", "BTCUSDT", "2020-08-31", 4.0).expect("a split")],
    }
}

/// A datahub from before the verb is refused locally, nothing sent.
#[test]
fn the_stamped_verb_is_refused_locally_by_an_older_datahub() {
    let (addr, rx, handle) = spawn_fake_peer(vec![FEATURE_INSTRUMENT_LISTS.to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(!client.serves_kept_rows_stamped());
    let err = client.kept_rows_of_venues(&binance(), &BTreeMap::new()).expect_err("refused");
    assert!(err.contains(FEATURE_KEPT_ROWS_STAMPED), "{err}");
    drop(client);
    assert!(rx.recv().expect("sent").is_empty(), "nothing was sent");
    handle.join().expect("joins");
}

/// A new backtesthub against an older datahub: no encoded form, so the relay writes the typed answer, and the typed
/// read is the JSON verb's, the one frame sent.
#[test]
fn a_store_over_an_older_datahub_reads_the_json_verb_and_holds_nothing_encoded() {
    let features =
        vec![FEATURE_INSTRUMENT_LISTS.to_string(), FEATURE_INSTRUMENT_LISTS_OF_VENUES.to_string()];
    let (addr, rx, handle) = spawn_fake_peer(features, |r| match r {
        Request::InstrumentListsOfVenues { venues } => {
            Some(Response::InstrumentLists(whole().of_venues(venues)))
        }
        _ => None,
    });
    let remote = HistoryStoreDatahub::new(addr.to_string());
    assert!(remote.kept_instruments_and_splits_encoded_of(&binance()).is_none(), "nothing encoded");
    let (rows, splits) = remote.kept_instruments_and_splits_of(&binance());
    assert_eq!(rows.expect("rows"), Some(whole().venues[0].instruments.clone()));
    assert_eq!(splits.expect("splits"), Some(whole().splits));
    drop(remote);
    let sent = rx.recv().expect("sent");
    assert!(matches!(&sent[..], [Request::InstrumentListsOfVenues { .. }]), "{sent:?}");
    handle.join().expect("joins");
}
