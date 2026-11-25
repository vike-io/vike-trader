use super::*;

use vike_data::HistoryStoreMemory;
use vike_marketdata::test_support::flat_bar_unit_volume;

/// The bytes `step` would write, or a panic naming the value it answered instead.
fn body(step: Step) -> Vec<u8> {
    match step {
        Step::Bytes(body) => body,
        Step::Reply(response) => panic!("answered as a value: {response:?}"),
        _ => panic!("not a reply"),
    }
}

/// What the reply is with no bytes held: the listing's value, written now in msgpack (the cache off).
fn as_value(store: &dyn HistoryStore) -> Vec<u8> {
    let listing = store.list_series().expect("a listing");
    encode_reply(BodyCodec::Msgpack, &Response::SeriesList(listing)).expect("msgpack")[4..].to_vec()
}

/// **A JSON request (an older client) is answered the value**, written by the connection as it always was, and
/// holds nothing; a msgpack request in LZ4 is the LZ4 form of the same held bytes.
#[test]
fn a_json_ask_is_answered_the_value_and_lz4_is_the_held_bytes_compressed() {
    let listed = Listed::new();
    let store = HistoryStoreMemory::new();
    let bar = [flat_bar_unit_volume(1_700_000_000_000, 10.0)];
    for i in 0..200 {
        store.append_bars("binance", &format!("SYM{i:03}"), "1m", &bar, None).expect("a series");
    }
    match listed.step(&store, BodyCodec::Json) {
        Step::Reply(response) => assert!(matches!(*response, Response::SeriesList(_))),
        _ => panic!("a JSON ask is a value"),
    }
    assert_eq!(listed.written.load(Ordering::Relaxed), 0, "and holds nothing");
    let listing = store.list_series().expect("a listing");
    let lz4 = body(listed.step(&store, BodyCodec::MsgpackLz4));
    let want = encode_reply(BodyCodec::MsgpackLz4, &Response::SeriesList(listing)).expect("lz4");
    assert_eq!(lz4, want[4..], "the held bytes in the asked form");
}

/// **A listing asked again is answered with the bytes held, a listing that moved is written again**, and every answer
/// is byte for byte the value's: a hit on repeat writes nothing, a new series (any writer: the listing is the key) is
/// written once, and a removed one too.
#[test]
fn the_held_bytes_answer_an_equal_listing_and_a_moved_one_is_written_again() {
    let listed = Listed::new();
    let store = HistoryStoreMemory::new();
    let bar = [flat_bar_unit_volume(1_700_000_000_000, 10.0)];
    store.append_bars("binance", "BTCUSDT", "1m", &bar, None).expect("a series");

    let first = body(listed.step(&store, BodyCodec::Msgpack));
    assert_eq!(first, as_value(&store), "the value's bytes");
    assert_eq!(listed.written.load(Ordering::Relaxed), 1);
    assert_eq!(body(listed.step(&store, BodyCodec::Msgpack)), first, "asked again: the held bytes");
    assert_eq!(listed.written.load(Ordering::Relaxed), 1, "a hit writes nothing");

    store.append_bars("okx", "BTC-USDT", "1m", &bar, None).expect("a second series");
    let second = body(listed.step(&store, BodyCodec::Msgpack));
    assert_ne!(second, first);
    assert_eq!(second, as_value(&store), "a moved listing: its value's bytes");
    assert_eq!(listed.written.load(Ordering::Relaxed), 2);

    let other = HistoryStoreMemory::new();
    other.append_bars("binance", "BTCUSDT", "1m", &bar, None).expect("the first listing again");
    assert_eq!(
        body(listed.step(&other, BodyCodec::Msgpack)),
        first,
        "another store's listing: its own bytes"
    );
    assert_eq!(listed.written.load(Ordering::Relaxed), 3);
}

/// A store that fails its listing is answered with its error, as a value, and holds nothing.
#[test]
fn a_failed_listing_is_its_error() {
    struct Down;
    impl HistoryStore for Down {
        fn list_series(&self) -> Result<Vec<SeriesId>, vike_data::HistoryStoreError> {
            Err(vike_data::HistoryStoreError::Query("the store is down".into()))
        }
        vike_data::hist_store_stubs!(inert: all);
    }
    let listed = Listed::new();
    match listed.step(&Down, BodyCodec::Msgpack) {
        Step::Reply(response) => {
            assert!(matches!(
                *response,
                Response::Refused(ref why) if why.id == "datahub.store.query" && why.text.contains("down")
            ))
        }
        _ => panic!("a failed listing is a value"),
    }
    assert_eq!(listed.written.load(Ordering::Relaxed), 0);
}
