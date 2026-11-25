//! The CAPABILITY NEGOTIATION of the text-history read ([`Request::ScanTexts`],
//! [`FEATURE_SCAN_TEXTS`]), decision 0112's legs:
//!
//! - **the advertisement is a BUILD fact** — the real `serve` advertises it and answers it, over the
//!   RECEIVED time, paged through `HistoryStoreDatahub` so the point-in-time read
//!   (`HistoryStore::texts_as_of`) crosses whole;
//! - **new client, server without the capability** — refused CLIENT-SIDE with nothing sent.
//!
//! Loopback + in-memory only.

use std::sync::Arc;

use vike_data::{HistoryStore, HistoryStoreMemory, TextRow, TimestampRange};
use vike_datahub_client::proto::FEATURE_SCAN_TEXTS;
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};

use vike_datahub::test_support::spawn_server;
use vike_datahub_client::test_support::spawn_fake_peer;

/// A news item received at `received_ms`, published long before any of them.
fn item(received_ms: i64, url: &str) -> TextRow {
    TextRow {
        published_ms: 1_000,
        title: format!("headline {url}"),
        url: url.to_string(),
        ..TextRow::new("news", "CoinDesk", received_ms)
    }
}

/// Leg 1: the real server advertises the read and answers it; a soft page keeps one append's rows
/// (one received time) together, and the seam's point-in-time read equals the local store's.
#[test]
fn the_server_advertises_and_answers_the_text_read_over_the_received_time() {
    let store = Arc::new(HistoryStoreMemory::new());
    let first = [item(10_000, "https://a.test/1"), item(10_000, "https://a.test/2")];
    store.append_texts("news", "rss", &first, Some("a")).unwrap();
    store.append_texts("news", "rss", &[item(20_000, "https://a.test/3")], Some("b")).unwrap();
    let addr = spawn_server(Arc::clone(&store) as Arc<dyn HistoryStore + Send + Sync>);

    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(client.serves_texts(), "{:?}", client.features());
    let page = client.scan_texts("news", "rss", TimestampRange::all(), Some(1)).unwrap();
    assert_eq!(page, first.to_vec(), "a soft page of one keeps its received time whole");

    let remote = HistoryStoreDatahub::new(addr.to_string());
    for decision_ms in [5_000, 10_000, 15_000, 20_000] {
        assert_eq!(
            remote.texts_as_of("news", "rss", decision_ms, 60_000).unwrap(),
            store.texts_as_of("news", "rss", decision_ms, 60_000).unwrap(),
            "the wire answers what the store answers at {decision_ms}"
        );
    }
    assert_eq!(remote.texts_as_of("news", "rss", 15_000, 60_000).unwrap().len(), 2);
    assert!(
        remote.texts_as_of("economic_calendar", "forexfactory", 20_000, 60_000).unwrap().is_empty(),
        "a series nothing recorded is an honest empty"
    );
}

/// Leg 2: a server without `scan_texts` is refused BEFORE a frame is written, naming the capability.
#[test]
fn the_text_read_refuses_locally_when_unadvertised() {
    let (addr, rx, handle) = spawn_fake_peer(vec!["load_bars".to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    assert!(!client.serves_texts());
    let err = client
        .scan_texts("news", "rss", TimestampRange::all(), Some(10))
        .expect_err("refused client-side");
    assert!(err.contains(FEATURE_SCAN_TEXTS), "names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert!(received.is_empty(), "the client SENT {received:?} after a local refusal");
    handle.join().expect("fake joins");
}

/// The scores' read, the same two legs in one: the real server advertises and answers it over the
/// texts' received time, and a server without it is refused before a frame is written.
#[test]
fn the_text_score_read_is_served_over_the_received_time_and_refused_locally_when_unadvertised() {
    use vike_datahub_client::proto::FEATURE_SCAN_TEXT_SCORES;
    let score = |received_ms: i64| vike_data::TextScoreRow {
        received_ms,
        text_id: format!("news\u{1f}CoinDesk\u{1f}https://a.test/{received_ms}"),
        chunk: 0,
        scored_ms: 99_000,
        model: "finbert@0".to_string(),
        values: vike_data::TextScoreRow::joined_values(&[0.5, 0.25, 0.25]),
    };
    let store = Arc::new(HistoryStoreMemory::new());
    store.append_text_scores("news", "finbert", &[score(10_000), score(20_000)], None).unwrap();
    let addr = spawn_server(Arc::clone(&store) as Arc<dyn HistoryStore + Send + Sync>);
    let remote = HistoryStoreDatahub::new(addr.to_string());
    for decision_ms in [5_000, 10_000, 15_000, 20_000] {
        assert_eq!(
            remote.text_scores_as_of("news", "finbert", decision_ms, 60_000).unwrap(),
            store.text_scores_as_of("news", "finbert", decision_ms, 60_000).unwrap(),
            "the wire answers what the store answers at {decision_ms}"
        );
    }

    let (addr, rx, handle) = spawn_fake_peer(vec!["scan_texts".to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    let err = client
        .scan_text_scores("news", "finbert", TimestampRange::all(), Some(10))
        .expect_err("refused client-side");
    assert!(err.contains(FEATURE_SCAN_TEXT_SCORES), "names the capability: {err}");
    drop(client);
    assert!(rx.recv().expect("fake reports").is_empty(), "nothing was sent");
    handle.join().expect("fake joins");
}
