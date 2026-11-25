//! The EXECUTION STREAM's negotiation (decision 0112): `Request::ExecutionStreamOpen` and
//! `Request::ExecutionStreams` are negotiated by `FEATURE_EXECUTION_STREAM`. A datahub that does not advertise it
//! is refused in words and sent NOTHING (an older one could not decode either verb), never half-served; one that
//! does is sent the exact open, and its reply switches the socket to the stream's frames. The peer is the crate's
//! recording fake, so "nothing was sent" is an empty frame list.

use std::time::Duration;

use vike_datahub_client::execution_stream::{
    ExecutionAccountPositionsWire, ExecutionStreamGapWire, ExecutionStreamStatusWire,
    ExecutionStreamUp,
};
use vike_datahub_client::proto::{FEATURE_EXECUTION_STREAM, Request, Response};
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};
use vike_hub_protocol::call::HubCallError;
use vike_hub_protocol::frame::write_frame;
use vike_model::AccountPosition;
use vike_model::events::PositionSide;

const JOURNAL: &str = "6f1c0b9e2a7d4c3b8e5f1a2d3c4b5a69";
const REPLY: Duration = Duration::from_secs(5);
const WRITE: Duration = Duration::from_secs(60);

fn open(client: DatahubClient, journal_id: &str) -> Result<Option<u64>, HubCallError> {
    client
        .open_execution_stream(journal_id, "prod", 1, 9, REPLY, WRITE)
        .map(|(info, _)| info.stored_through)
}

/// **An older datahub is sent nothing**: without the string both verbs are refused locally, naming it, and the
/// peer received no frame. A publisher facing it sends no record, and the journal keeps them.
#[test]
fn an_older_datahub_is_sent_nothing() {
    let (addr, received, peer) = spawn_fake_peer(Vec::new(), |_| None);
    let err = open(DatahubClient::connect(addr).expect("connect"), JOURNAL).expect_err("refused");
    assert!(matches!(err, HubCallError::NotOffered { .. }), "{err:?}");
    let text = err.to_string();
    assert!(text.contains(FEATURE_EXECUTION_STREAM) && text.contains("Nothing was sent"), "{text}");
    assert!(received.recv().expect("the peer reports").is_empty(), "a frame reached the peer");
    peer.join().expect("the peer ends cleanly");

    let (addr, received, peer) = spawn_fake_peer(Vec::new(), |_| None);
    let mut client = DatahubClient::connect(addr).expect("connect");
    let err = client.execution_streams().expect_err("refused");
    assert!(err.contains(FEATURE_EXECUTION_STREAM) && err.contains("Nothing was sent"), "{err}");
    drop(client);
    assert!(received.recv().expect("the peer reports").is_empty(), "a frame reached the peer");
    peer.join().expect("the peer ends cleanly");

    // ...and the position snapshots' read: the reader's store answers "not served" (it stores none), sending nothing.
    let (addr, received, peer) = spawn_fake_peer(Vec::new(), |_| None);
    let store = HistoryStoreDatahub::new(addr.to_string());
    assert_eq!(store.execution_account_positions(1).expect("not served is no error"), None);
    drop(store);
    assert!(received.recv().expect("the peer reports").is_empty(), "a frame reached the peer");
    peer.join().expect("the peer ends cleanly");
}

/// **The position snapshots' read asks once, with its time, and its rows are the answer**, unchanged.
#[test]
fn the_position_snapshot_read_is_asked_and_its_rows_are_the_answer() {
    let rows = vec![ExecutionAccountPositionsWire {
        generation: 2,
        sequence: 4_801,
        as_of_ms: 1_794_124_800_000,
        venue: "binance".into(),
        account: "DEMO".into(),
        positions: vec![AccountPosition {
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            size: 0.25,
            average_price: 64_999.5,
        }],
        unknown: None,
    }];
    let answer = rows.clone();
    let (addr, received, peer) =
        spawn_fake_peer(vec![FEATURE_EXECUTION_STREAM.to_string()], move |r| {
            matches!(r, Request::ExecutionAccountPositions { at_or_before_ms: 1_794_124_900_000 })
                .then(|| Response::ExecutionAccountPositions(answer.clone()))
        });
    let store = HistoryStoreDatahub::new(addr.to_string());
    assert_eq!(store.execution_account_positions(1_794_124_900_000).expect("read"), Some(rows));
    drop(store);
    let sent = received.recv().expect("the peer reports");
    assert!(matches!(sent.as_slice(), [Request::ExecutionAccountPositions { .. }]), "{sent:?}");
    peer.join().expect("the peer ends cleanly");
}

/// **A journal id of the wrong shape never leaves the client**: not 32 lowercase hex digits, refused in words.
#[test]
fn a_malformed_journal_id_is_refused_before_anything_is_sent() {
    for bad in [
        "",
        "6F1C0B9E2A7D4C3B8E5F1A2D3C4B5A69",
        "6f1c0b9e",
        "6f1c0b9e2a7d4c3b8e5f1a2d3c4b5a6g",
        "../x",
    ] {
        let (addr, received, peer) =
            spawn_fake_peer(vec![FEATURE_EXECUTION_STREAM.to_string()], |_| None);
        let err = open(DatahubClient::connect(addr).expect("connect"), bad).expect_err(bad);
        assert!(matches!(err, HubCallError::Protocol(_)), "{bad}: {err:?}");
        assert!(err.to_string().contains("Nothing was sent"), "{err}");
        assert!(received.recv().expect("the peer reports").is_empty(), "{bad} reached the peer");
        peer.join().expect("the peer ends cleanly");
    }
}

/// **A datahub with the string is sent the exact open**; its reply is unpacked, and the socket it hands back
/// carries the stream's frames, read bound cleared and write bound armed. A refusal is the daemon's own text.
#[test]
fn a_datahub_with_the_string_is_sent_the_open_and_its_reply_switches_the_socket() {
    let (addr, received, peer) = spawn_fake_peer(vec![FEATURE_EXECUTION_STREAM.to_string()], |r| {
        matches!(r, Request::ExecutionStreamOpen { .. }).then_some(
            Response::ExecutionStreamOpened {
                stored_through: Some(4800),
                stored_through_recorded_ms: Some(1_794_124_799_870),
                heartbeat_ms: 30_000,
            },
        )
    });
    let client = DatahubClient::connect(addr).expect("connect");
    let (info, mut stream) =
        client.open_execution_stream(JOURNAL, "prod", 1, 9, REPLY, WRITE).expect("opened");
    assert_eq!(
        (info.stored_through, info.stored_through_recorded_ms, info.heartbeat_ms),
        (Some(4800), Some(1_794_124_799_870), 30_000)
    );
    assert_eq!(stream.read_timeout().expect("read bound"), None);
    assert_eq!(stream.write_timeout().expect("write bound"), Some(WRITE));
    // The first frame after the open is the stream's own, which the positional peer cannot decode.
    write_frame(&mut stream, &ExecutionStreamUp::Heartbeat { head_sequence: 9 }).expect("written");
    drop(stream);
    let sent = received.recv().expect("the peer reports");
    let asked = Request::ExecutionStreamOpen {
        journal_id: JOURNAL.into(),
        tradehub: "prod".into(),
        oldest_sequence: 1,
        head_sequence: 9,
    };
    assert_eq!(
        sent.iter().map(|r| serde_json::to_string(r).unwrap()).collect::<Vec<_>>(),
        [serde_json::to_string(&asked).unwrap()],
        "the exact open, and nothing positional after it"
    );
    peer.join().expect("the peer ends cleanly");

    // The fake answers anything else with its error: a daemon-side refusal is carried unchanged.
    let (addr, _received, peer) =
        spawn_fake_peer(vec![FEATURE_EXECUTION_STREAM.to_string()], |_| None);
    let err = open(DatahubClient::connect(addr).expect("connect"), JOURNAL).expect_err("refused");
    assert!(matches!(err, HubCallError::Refused { id: None, .. }), "{err:?}");
    assert_eq!(err.to_string(), "fake peer: unexpected frame");
    peer.join().expect("the peer ends cleanly");
}

/// **The status read asks once and its rows are the answer**, unchanged.
#[test]
fn the_status_read_is_asked_and_its_rows_are_the_answer() {
    let streams = vec![ExecutionStreamStatusWire {
        journal_id: JOURNAL.into(),
        tradehub: "prod".into(),
        stored_through: Some(4812),
        last_stored_ms: Some(1_794_124_801_300),
        connected: true,
        gaps: vec![ExecutionStreamGapWire {
            kind: "pruned".into(),
            after_sequence: 120,
            resumes_at_sequence: 300,
            lost_from_ms: Some(1_793_520_000_000),
            lost_to_ms: Some(1_793_606_400_000),
            noted_ms: 1_794_124_800_500,
            reason: None,
            order_client_ids: Vec::new(),
        }],
    }];
    let answer = streams.clone();
    let (addr, received, peer) =
        spawn_fake_peer(vec![FEATURE_EXECUTION_STREAM.to_string()], move |r| {
            matches!(r, Request::ExecutionStreams)
                .then(|| Response::ExecutionStreams(answer.clone()))
        });
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert_eq!(client.execution_streams(), Ok(streams));
    drop(client);
    let sent = received.recv().expect("the peer reports");
    assert!(matches!(sent.as_slice(), [Request::ExecutionStreams]), "{sent:?}");
    peer.join().expect("the peer ends cleanly");
}
