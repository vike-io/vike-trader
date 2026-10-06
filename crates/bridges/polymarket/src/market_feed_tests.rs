use super::{
    DEFAULT_TOKENS_PER_SOCKET, FeedCtx, Feeds, PumpMode, ShardMembership, shard_label,
    tokens_per_socket,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use vike_data::DataClient;

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

/// A network-free stand-in for [`super::shard_main`]: just polls its own stop flag, without
/// ever touching a socket. Lets the per-shard lifecycle (spawn/pack/unsubscribe/shutdown) be
/// exercised deterministically.
fn fake_feed_body(_mode: PumpMode, _membership: Arc<ShardMembership>, ctx: FeedCtx) {
    while !ctx.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// K = 1 — the pre-batching shape: every subscription gets its own socket.
fn unbatched(sink: Arc<RecordingSink>) -> Feeds {
    Feeds::new(sink, || {}).with_tokens_per_socket(1)
}

#[test]
fn freshness_threshold_maps_book_and_quotes_tight_trades_loose() {
    // The per-mode mapping the whole change hinges on — asserted directly here, because the
    // scripted pump tests build the tracker from mirror constants and can't reach this private
    // method (only network-doing `shard_main` calls it). A swapped arm would otherwise slip by.
    assert_eq!(PumpMode::Book.freshness_threshold(), super::FRESHNESS_THRESHOLD_BOOK);
    assert_eq!(PumpMode::Quotes.freshness_threshold(), super::FRESHNESS_THRESHOLD_BOOK);
    assert_eq!(PumpMode::Trades.freshness_threshold(), super::FRESHNESS_THRESHOLD_TRADES);
}

#[test]
fn subscribe_returns_distinct_ids() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = unbatched(sink);
    let id1 = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("222", PumpMode::Trades, fake_feed_body).expect("spawn ok");
    assert_ne!(id1, id2, "distinct ids per subscribe");
    feeds.shutdown();
}

/// Ids stay distinct even when the subscriptions SHARE one socket — the caller-facing id space
/// is `Feeds`'s own, not the registry's shard keys.
#[test]
fn subscribe_returns_distinct_ids_when_batched_onto_one_socket() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(4);
    let id1 = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("222", PumpMode::Book, fake_feed_body).expect("spawn ok");
    assert_ne!(id1, id2, "distinct ids per subscribe, even sharing a socket");
    assert_eq!(feeds.socket_count(), 1, "both tokens rode one socket");
    feeds.shutdown();
}

#[test]
fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = unbatched(sink);
    let id1 = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
    let _id2 = feeds.spawn_with("222", PumpMode::Book, fake_feed_body).expect("spawn ok");

    feeds.unsubscribe(id1);
    assert_eq!(feeds.socket_count(), 1, "only the unsubscribed stream's socket is removed");
    assert_eq!(feeds.registry.len(), 1, "the other subscription's thread keeps running");

    feeds.shutdown();
    assert!(feeds.registry.is_empty());
}

#[test]
fn unsubscribe_of_an_unknown_id_is_a_no_op() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = unbatched(sink);
    let id = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
    feeds.unsubscribe(vike_data::SubscriptionId(id.0 + 100)); // never issued
    assert_eq!(feeds.registry.len(), 1, "unknown id must not disturb the real subscription");
    feeds.shutdown();
}

#[test]
fn shutdown_joins_every_feed() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = unbatched(sink);
    feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
    feeds.spawn_with("222", PumpMode::Quotes, fake_feed_body).expect("spawn ok");
    feeds.spawn_with("333", PumpMode::Trades, fake_feed_body).expect("spawn ok");
    feeds.shutdown();
    assert!(feeds.registry.is_empty());
    assert_eq!(feeds.socket_count(), 0);
}

// ---- WS batching: packing, K = 1 equivalence, per-mode isolation ----------------------------

/// K = 1 is the pre-batching shape: N subscriptions ⇒ N sockets, one token each.
#[test]
fn k_of_one_opens_one_socket_per_subscription() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = unbatched(sink);
    for token in ["111", "222", "333"] {
        feeds.spawn_with(token, PumpMode::Book, fake_feed_body).expect("spawn ok");
    }
    assert_eq!(feeds.socket_count(), 3, "K=1 never batches");
    assert_eq!(feeds.registry.len(), 3, "one feed thread per socket");
    feeds.shutdown();
}

/// The whole point: K tokens ride one socket, so N tokens cost `ceil(N/K)` sockets.
#[test]
fn tokens_pack_into_ceil_n_over_k_sockets() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(3);
    for token in ["1", "2", "3", "4", "5", "6", "7"] {
        feeds.spawn_with(token, PumpMode::Book, fake_feed_body).expect("spawn ok");
    }
    assert_eq!(feeds.socket_count(), 3, "7 tokens at K=3 ⇒ ceil(7/3) = 3 sockets");
    assert_eq!(feeds.registry.len(), 3);
    feeds.shutdown();
}

/// A shard carries ONE mode: the emission rules differ per mode, so a quotes token never shares
/// a socket with a trades token even when seats are free.
#[test]
fn modes_never_share_a_socket() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(8);
    feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
    feeds.spawn_with("222", PumpMode::Quotes, fake_feed_body).expect("spawn ok");
    feeds.spawn_with("333", PumpMode::Trades, fake_feed_body).expect("spawn ok");
    assert_eq!(feeds.socket_count(), 3, "one socket per mode despite 8 free seats each");
    feeds.shutdown();
}

/// The same token subscribed twice in one mode stays TWO independent streams (pre-batching
/// behavior): one wire subscription can only be delivered once, so the duplicate opens its own
/// socket rather than silently aliasing the first.
#[test]
fn a_duplicate_token_in_one_mode_gets_its_own_socket() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(8);
    let a = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
    let b = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
    assert_ne!(a, b);
    assert_eq!(feeds.socket_count(), 2, "a duplicate token never shares a seat");
    feeds.shutdown();
}

/// Releasing one seat of a shared socket keeps the socket (and its co-tenants) alive; releasing
/// the LAST seat stops+joins it.
#[test]
fn a_shared_socket_survives_until_its_last_seat_is_released() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(4);
    let a = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
    let b = feeds.spawn_with("222", PumpMode::Book, fake_feed_body).expect("spawn ok");
    assert_eq!(feeds.socket_count(), 1);

    feeds.unsubscribe(a);
    assert_eq!(feeds.socket_count(), 1, "the co-tenant keeps the socket open");
    assert_eq!(feeds.registry.len(), 1, "its feed thread was NOT joined");

    feeds.unsubscribe(b);
    assert_eq!(feeds.socket_count(), 0, "the last seat closes the socket");
    assert!(feeds.registry.is_empty(), "and joins its feed thread");
    feeds.shutdown();
}

/// A released seat frees room for the next token on the SAME socket — no socket leak from
/// churn.
#[test]
fn a_released_seat_is_reused_by_the_next_subscription() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {}).with_tokens_per_socket(2);
    let a = feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
    feeds.spawn_with("222", PumpMode::Book, fake_feed_body).expect("spawn ok");
    assert_eq!(feeds.socket_count(), 1, "the socket is full");
    feeds.unsubscribe(a);
    feeds.spawn_with("333", PumpMode::Book, fake_feed_body).expect("spawn ok");
    assert_eq!(feeds.socket_count(), 1, "the freed seat took the new token");
    feeds.shutdown();
}

/// Membership mutation bumps the epoch — the signal a live shard's stream watches to end its
/// session and resubscribe the whole set. A refused admission (full / duplicate) must NOT bump
/// it, or every subscribe attempt would churn an unrelated socket.
#[test]
fn membership_bumps_the_epoch_only_on_a_real_change() {
    let m = ShardMembership::new("a".into());
    assert_eq!(m.epoch(), 0);
    assert!(m.try_admit("b", 2));
    assert_eq!(m.epoch(), 1, "an admitted token bumps the epoch");
    assert!(!m.try_admit("c", 2), "full");
    assert!(!m.try_admit("a", 4), "duplicate");
    assert_eq!(m.epoch(), 1, "a refused admission never mutates");
    assert_eq!(m.tokens(), vec!["a".to_string(), "b".to_string()]);
    assert!(!m.release("a"), "still seated: b");
    assert_eq!(m.epoch(), 2);
    assert!(m.release("b"), "now empty");
    assert!(m.release("nope"), "releasing an unseated token is a no-op on an empty shard");
}

/// The label a shard's thread name / status lines carry: a one-seat shard reads exactly as it
/// did before batching (bare token id), a batched one names its opener plus the extra count.
#[test]
fn shard_label_is_the_bare_token_at_one_seat() {
    assert_eq!(shard_label(&["tok".to_string()]), "tok");
    assert_eq!(shard_label(&["tok".to_string(), "b".to_string(), "c".to_string()]), "tok+2");
    assert_eq!(shard_label(&[]), "");
}

/// K is the default until a caller sets it, and `with_tokens_per_socket` clamps 0 to 1
/// (a zero-seat socket could never accept a token).
#[test]
fn k_defaults_and_clamps() {
    let sink = Arc::new(RecordingSink::default());
    let feeds = Feeds::new(sink, || {});
    assert_eq!(
        feeds.tokens_per_socket(),
        DEFAULT_TOKENS_PER_SOCKET,
        "the default is the DEFAULT_TOKENS_PER_SOCKET value"
    );
    let sink = Arc::new(RecordingSink::default());
    assert_eq!(Feeds::new(sink, || {}).with_tokens_per_socket(0).tokens_per_socket(), 1);
}

/// Decision 0095: K is `venue.polymarket.ws_tokens_per_socket`, handed in by the root — and the
/// catalog's documented default IS this constant.
#[test]
fn k_is_the_declared_field_or_the_default() {
    let from = |v: Option<&str>| {
        tokens_per_socket(
            |f| if f == "ws_tokens_per_socket" { v.map(str::to_string) } else { None },
        )
    };
    assert_eq!(from(None), DEFAULT_TOKENS_PER_SOCKET);
    assert_eq!(from(Some("200")), 200);
    assert_eq!(from(Some("0")), DEFAULT_TOKENS_PER_SOCKET, "zero keeps the default");
    assert_eq!(from(Some("many")), DEFAULT_TOKENS_PER_SOCKET);
    let default = DEFAULT_TOKENS_PER_SOCKET.to_string();
    assert_eq!(
        vike_model::venues::venue_fields::venue_field("polymarket", "ws_tokens_per_socket")
            .map(|f| f.default),
        Some(default.as_str()),
        "the catalog's default must be the bridge's"
    );
}

#[test]
fn bars_are_unsupported() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    assert!(matches!(
        feeds.subscribe_bars("111", "1m"),
        Err(vike_data::LiveDataError::Unsupported(_))
    ));
}

#[test]
fn with_raw_capture_spawns_tap_and_shuts_down_clean() {
    let sink = Arc::new(RecordingSink::default());
    let dir = tempfile::tempdir().unwrap();
    let cfg = super::RawCaptureConfig { dir: dir.path().to_path_buf(), channel_cap: 16 };
    let mut feeds = Feeds::with_raw_capture(sink, || {}, cfg).expect("spawn ok");
    // a fake feed body that never touches a socket — just proves lifecycle with capture on
    feeds.spawn_with("111", PumpMode::Book, fake_feed_body).expect("spawn ok");
    feeds.shutdown(); // must join the feed thread AND the tap writer, no hang
    assert!(feeds.registry.is_empty());
}

#[test]
fn plain_new_has_no_capture() {
    let sink = Arc::new(RecordingSink::default());
    let feeds = Feeds::new(sink, || {});
    assert!(feeds.raw_tap.is_none(), "Feeds::new must not enable capture");
}
