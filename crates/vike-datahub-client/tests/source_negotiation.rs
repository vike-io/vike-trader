//! The `source=` capability's negotiation, against the fake peer: `seed_class_negotiation.rs`'s shape, and
//! the worst instance of its failure. `Request` has no `deny_unknown_fields`, so a daemon predating the field
//! decodes the frame, drops the lane and answers: a read returns the archive's rows as the tape's, and a
//! delete DELETES THE ARCHIVE. So the assertion that carries the weight is that NOTHING WAS SENT.

use vike_data::TimestampRange;
use vike_data::store::removal::SeriesSelector;
use vike_datahub_client::proto::Request;
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_datahub_client::{
    DatahubClient, FEATURE_BAR_SOURCE, FEATURE_DELETE_SERIES, FEATURE_SOURCE,
};
use vike_model::StoreKindId;

const VENUE: &str = "dukascopy";
const SYMBOL: &str = "EURUSD";
const LANE: &str = "recorder";

/// A daemon that serves the read and the delete and predates the lane.
fn an_old_data_daemon() -> Vec<String> {
    ["load_bars", "list_series", "inventory", "series_gaps", FEATURE_DELETE_SERIES]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

fn selector(source: Option<&str>) -> SeriesSelector {
    let mut selector = SeriesSelector::new(StoreKindId::Quote, VENUE);
    selector.symbol = Some(SYMBOL.to_string());
    selector.source = source.map(str::to_string);
    selector
}

fn assert_refused_unsent(err: &str) {
    assert!(err.contains(FEATURE_SOURCE), "the refusal names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
    assert!(err.contains("DROP"), "and says why an old daemon is dangerous: {err}");
}

#[test]
fn a_lane_an_old_daemon_would_drop_is_refused_client_side_without_sending() {
    let (addr, rx, handle) = spawn_fake_peer(an_old_data_daemon(), |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");

    let read = client.scan_quotes_sourced(VENUE, LANE, SYMBOL, TimestampRange::all(), None);
    assert_refused_unsent(&read.expect_err("the archive's rows are not the tape's"));
    let delete = client.delete_series(&selector(Some(LANE)), None, false);
    assert_refused_unsent(&delete.expect_err("this would delete the archive").to_string());
    let plan = client.delete_series(&selector(Some(LANE)), None, true);
    assert_refused_unsent(&plan.expect_err("its plan would show the archive").to_string());

    drop(client);
    assert!(rx.recv().expect("frames").is_empty(), "the client sent NOTHING");
    handle.join().expect("join");
}

/// ⚠ **The other direction.** A request that names no lane asks for what it always asked for, so it is sent to a
/// daemon without the capability, in the frame that daemon knows: no `source` key at all.
#[test]
fn a_request_that_names_no_lane_is_sent_to_any_daemon_in_the_frame_it_knows() {
    let (addr, rx, handle) = spawn_fake_peer(an_old_data_daemon(), |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let _ = client.scan_quotes(VENUE, SYMBOL, TimestampRange::all(), None);
    let _ = client.delete_series(&selector(None), None, true);
    drop(client);

    let frames = rx.recv().expect("frames");
    assert_eq!(frames.len(), 2, "both were sent");
    for frame in &frames {
        let json = serde_json::to_string(frame).unwrap();
        assert!(!json.contains("source"), "{json}");
    }
    handle.join().expect("join");
}

#[test]
fn a_daemon_that_advertises_the_capability_receives_the_lane() {
    let mut features = an_old_data_daemon();
    features.push(FEATURE_SOURCE.to_string());
    let (addr, rx, handle) = spawn_fake_peer(features, |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let _ = client.scan_quotes_sourced(VENUE, LANE, SYMBOL, TimestampRange::all(), None);
    let _ = client.delete_series(&selector(Some(LANE)), None, true);
    drop(client);

    let frames = rx.recv().expect("frames");
    assert!(
        matches!(&frames[0], Request::ScanQuotes { source: Some(lane), .. } if lane == LANE),
        "{frames:?}"
    );
    assert!(
        matches!(&frames[1], Request::DeleteSeries { selector, .. }
            if selector.source.as_deref() == Some(LANE)),
        "{frames:?}"
    );
    handle.join().expect("join");
}

/// A daemon that has the QUOTE lane and predates the bar one would decode the frame, drop the lane and
/// answer the venue's candles as the lane's bars, so its own string is what opens the read.
#[test]
fn a_bar_lane_is_refused_unsent_by_a_daemon_that_only_has_the_quote_lane() {
    let mut features = an_old_data_daemon();
    features.push(FEATURE_SOURCE.to_string());
    let (addr, rx, handle) = spawn_fake_peer(features, |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let err = client
        .load_bars_sourced("binance", "aggtrades", "SOLUSDT.P", "1m", TimestampRange::all(), None)
        .unwrap_err();
    assert!(err.contains(FEATURE_BAR_SOURCE), "the refusal names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
    drop(client);
    assert!(rx.recv().expect("frames").is_empty(), "NOTHING WAS SENT");
    handle.join().expect("join");
}

#[test]
fn a_daemon_that_advertises_the_bar_capability_receives_the_lane() {
    let mut features = an_old_data_daemon();
    features.push(FEATURE_BAR_SOURCE.to_string());
    let (addr, rx, handle) = spawn_fake_peer(features, |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let all = TimestampRange::all();
    let _ = client.load_bars_sourced("binance", "aggtrades", "SOLUSDT.P", "1m", all, None);
    let _ = client.load_bars("binance", "SOLUSDT.P", "1m", all, None);
    drop(client);

    let frames = rx.recv().expect("frames");
    assert!(
        matches!(&frames[0], Request::LoadBars { source: Some(lane), .. } if lane == "aggtrades"),
        "{frames:?}"
    );
    let plain = serde_json::to_string(&frames[1]).unwrap();
    assert!(!plain.contains("source"), "a read naming no lane is the old frame: {plain}");
    handle.join().expect("join");
}

/// The `TimestampRange` verbs send exactly the `_ms` twins' frames: the lane, both bounds and the limit.
#[test]
fn the_range_verbs_send_the_lane_and_the_bounds() {
    let mut features = an_old_data_daemon();
    features.extend([FEATURE_SOURCE.to_string(), FEATURE_BAR_SOURCE.to_string()]);
    let (addr, rx, handle) = spawn_fake_peer(features, |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let range = vike_data::TimestampRange::of(1_000, 2_000);
    let _ = client.load_bars_sourced("binance", "aggtrades", "SOLUSDT.P", "1m", range, Some(5));
    let _ = client.scan_quotes_sourced(VENUE, LANE, SYMBOL, range, Some(7));
    drop(client);

    let frames = rx.recv().expect("frames");
    let Request::LoadBars { source, start, end, limit, .. } = &frames[0] else {
        panic!("{frames:?}")
    };
    assert_eq!(
        (source.as_deref(), *start, *end, *limit),
        (Some("aggtrades"), Some(1_000), Some(2_000), Some(5))
    );
    let Request::ScanQuotes { source, start, end, limit, .. } = &frames[1] else {
        panic!("{frames:?}")
    };
    assert_eq!(
        (source.as_deref(), *start, *end, *limit),
        (Some(LANE), Some(1_000), Some(2_000), Some(7))
    );
    handle.join().expect("join");
}
