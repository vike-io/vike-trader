//! A reader says what it has read only to a node that asks (`FEATURE_OBSERVE_NEWEST`): a node that does not advertise it gets
//! the `Subscribe` it always got and no frame it cannot decode. The real handshake against the scripted node, which records every
//! frame a client sends.

use std::time::{Duration, Instant};

use vike_tradehub_client::proto::{FEATURE_OBSERVE_NEWEST, Request, Response};
use vike_tradehub_client::test_support::fake_node::{features, scripted_node};
use vike_tradehub_client::wire::CoreSnapshotWire;
use vike_tradehub_client::{FrameAge, TradehubCoreHandle};

const KEY: &[u8] = b"DUMMY-observe-key-for-the-newest-negotiation";

/// What a client sent a node advertising `advertised` over two frames it pushed in answer.
fn names_sent_to_a_node_with(advertised: &[&str]) -> Vec<String> {
    let (addr, names) = scripted_node(KEY, features(advertised), |request| {
        let name = match request {
            Request::Subscribe { .. } => "subscribe",
            Request::SubscribeNewest { .. } => "subscribe-newest",
            Request::FrameRead => "frame-read",
            other => panic!("not an observer's frame: {other:?}"),
        };
        let mut frame = CoreSnapshotWire::empty();
        frame.sequence = 7;
        frame.published_ms = 1;
        (name.to_string(), Response::SnapshotFrame(Box::new(frame)))
    });
    let handle = TradehubCoreHandle::connect(addr, KEY).expect("the real handshake");
    let deadline = Instant::now() + Duration::from_secs(5);
    while handle.snapshot().sequence != 7 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(handle.snapshot().sequence, 7, "a frame arrived");
    // A frame's word is written after the frame is stored: give it the moment it takes.
    std::thread::sleep(Duration::from_millis(100));
    drop(handle);
    names.recv_timeout(Duration::from_secs(5)).expect("the node's record")
}

#[test]
fn a_node_that_asks_gets_a_word_per_frame_and_one_that_does_not_gets_what_it_always_got() {
    let asked = names_sent_to_a_node_with(&["observe", FEATURE_OBSERVE_NEWEST]);
    assert_eq!(asked[0], "subscribe-newest", "{asked:?}");
    assert!(asked.iter().skip(1).all(|name| name == "frame-read"), "{asked:?}");
    assert!(asked.len() >= 2, "the reader said it read the frame: {asked:?}");

    let older = names_sent_to_a_node_with(&["observe"]);
    assert_eq!(older, ["subscribe"]);
}

/// How old a frame is, off the node's stamp, clamped against a skew between the two clocks. A frame with no stamp has no age.
#[test]
fn a_frames_age_is_read_off_the_nodes_stamp() {
    assert_eq!(
        FrameAge::of(1_000, 1_400, 61_000),
        Some(FrameAge { age_ms: 60_000, arrived_after_ms: 400 })
    );
    assert_eq!(FrameAge::of(1_000, 900, 950), Some(FrameAge { age_ms: 0, arrived_after_ms: 0 }));
    assert_eq!(FrameAge::of(0, 1_400, 61_000), None);
}

/// The stamp is a pure addition: an older node's frame decodes with none, and a frame with none writes no key.
#[test]
fn the_stamp_is_absent_when_nobody_stamped_it() {
    let frame = CoreSnapshotWire::empty();
    let text = serde_json::to_string(&frame).expect("serializes");
    assert!(!text.contains("published_ms"), "{text}");
    let back: CoreSnapshotWire = serde_json::from_str(&text).expect("decodes");
    assert_eq!(back.published_ms, 0);
}
