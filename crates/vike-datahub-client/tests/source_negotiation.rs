//! The `source=` capability's negotiation, against the fake peer: `seed_class_negotiation.rs`'s shape, and
//! the worst instance of its failure. `Request` has no `deny_unknown_fields`, so a daemon predating the field
//! decodes the frame, drops the lane and answers: a read returns the archive's rows as the tape's, and a
//! delete DELETES THE ARCHIVE. So the assertion that carries the weight is that NOTHING WAS SENT.

use vike_data::StoreKindId;
use vike_datahub_client::proto::{Request, SeriesSelector};
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_datahub_client::{DatahubClient, FEATURE_DELETE_SERIES, FEATURE_SOURCE};

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

    let read = client.scan_quotes_sourced_ms(VENUE, LANE, SYMBOL, None, None, None);
    assert_refused_unsent(&read.expect_err("the archive's rows are not the tape's"));
    let delete = client.delete_series(&selector(Some(LANE)), None, false);
    assert_refused_unsent(&delete.expect_err("this would delete the archive"));
    let plan = client.delete_series(&selector(Some(LANE)), None, true);
    assert_refused_unsent(&plan.expect_err("its plan would show the archive"));

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
    let _ = client.scan_quotes_ms(VENUE, SYMBOL, None, None, None);
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
    let _ = client.scan_quotes_sourced_ms(VENUE, LANE, SYMBOL, None, None, None);
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
