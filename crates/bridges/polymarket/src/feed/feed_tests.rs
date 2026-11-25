use super::{
    DEFAULT_TOKENS_PER_SOCKET, DataClientBusSpecPolymarket, DataClientContext, FeedLane,
    ShardInputs, ShardMembership, shard_label, tokens_per_socket,
};
use std::assert_matches;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use vike_bridge_core::data_client::bus::DataClientBus;
use vike_data::DataClient;
use vike_model::StreamLabel;

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

/// A network-free stand-in for [`super::shard_main`]: just polls its own stop flag, without
/// ever touching a socket. Lets the per-shard lifecycle (seat/pack/release/shutdown) be
/// exercised deterministically.
fn fake_feed_body(
    _mode: FeedLane,
    _membership: Arc<ShardMembership>,
    ctx: DataClientContext<ShardInputs>,
) {
    while !ctx.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A source over the release's seed rows.
fn seeded() -> DataClientBusSpecPolymarket {
    let urls = vike_venues::venue_urls::VenueUrls::seed("polymarket");
    DataClientBusSpecPolymarket::new(|| {}, &urls).expect("the seed holds every row the feed dials")
}

/// A client over a source with `k` seats per socket whose shards dial nothing, and the source to count its sockets.
fn client(
    k: usize,
) -> (DataClientBus<DataClientBusSpecPolymarket>, Arc<DataClientBusSpecPolymarket>) {
    let source = Arc::new(seeded().with_tokens_per_socket(k).with_body(fake_feed_body));
    let sink = Arc::new(RecordingSink::default());
    (DataClientBus::on(&source, sink), source)
}

#[test]
fn freshness_threshold_maps_book_and_quotes_tight_trades_loose() {
    // The per-mode mapping the whole change hinges on — asserted directly here, because the
    // scripted pump tests build the tracker from mirror constants and can't reach this private
    // method (only network-doing `shard_main` calls it). A swapped arm would otherwise slip by.
    assert_eq!(FeedLane::Book.freshness_threshold(), super::FRESHNESS_THRESHOLD_BOOK);
    assert_eq!(FeedLane::Quotes.freshness_threshold(), super::FRESHNESS_THRESHOLD_BOOK);
    assert_eq!(FeedLane::Trades.freshness_threshold(), super::FRESHNESS_THRESHOLD_TRADES);
}

/// **The producer half of the label tie.** A pump discloses its gaps under `FeedLane::as_str`, and
/// the datahub's routing sink, the market-data hub and the store recorder each decide that word
/// through `vike_model::StreamLabel`: a mode whose word is not the label of the lane it pumps drops
/// its disclosures at every one of them, with nothing red. `label_of` is an exhaustive `match`, so a
/// new mode fails to compile here until it names its label.
#[test]
fn every_pump_mode_discloses_under_the_label_of_the_lane_it_pumps() {
    fn label_of(mode: FeedLane) -> StreamLabel {
        match mode {
            FeedLane::Book => StreamLabel::Book,
            FeedLane::Quotes => StreamLabel::Quotes,
            FeedLane::Trades => StreamLabel::Trades,
        }
    }
    for mode in [FeedLane::Book, FeedLane::Quotes, FeedLane::Trades] {
        assert_eq!(mode.as_str(), label_of(mode).word(), "{mode:?}");
        assert_eq!(StreamLabel::from_word(mode.as_str()), Some(label_of(mode)), "{mode:?}");
    }
}

/// Ids stay distinct even when the subscriptions SHARE one socket — the caller-facing id space
/// is the engine's own, not the registry's shard keys.
#[test]
fn subscribe_returns_distinct_ids_when_batched_onto_one_socket() {
    let (mut client, source) = client(4);
    let id1 = client.subscribe_book("111").expect("seated");
    let id2 = client.subscribe_book("222").expect("seated");
    assert_ne!(id1, id2, "distinct ids per subscribe, even sharing a socket");
    assert_eq!(source.socket_count(), 1, "both tokens rode one socket");
    client.shutdown();
}

#[test]
fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
    let (mut client, source) = client(1);
    let id1 = client.subscribe_book("111").expect("seated");
    let _id2 = client.subscribe_book("222").expect("seated");

    client.unsubscribe(id1);
    assert_eq!(source.socket_count(), 1, "only the unsubscribed stream's socket is removed");
    assert_eq!(source.feed_count(), 1, "the other subscription's thread keeps running");

    client.shutdown();
    assert_eq!(source.feed_count(), 0);
}

#[test]
fn unsubscribe_of_an_unknown_id_is_a_no_op() {
    let (mut client, source) = client(1);
    let id = client.subscribe_book("111").expect("seated");
    client.unsubscribe(vike_data::DataClientSubscriptionId(id.0 + 100)); // never issued
    assert_eq!(source.feed_count(), 1, "unknown id must not disturb the real subscription");
    client.shutdown();
}

#[test]
fn shutdown_joins_every_feed() {
    let (mut client, source) = client(1);
    client.subscribe_book("111").expect("seated");
    client.subscribe_quotes("222").expect("seated");
    client.subscribe_trades("333").expect("seated");
    client.shutdown();
    assert_eq!(source.feed_count(), 0);
    assert_eq!(source.socket_count(), 0);
}

/// `begin_shutdown` raises every shard's stop flag and joins nothing: the threads wind down while the client still
/// holds its keys, and the shutdown after it joins them.
#[test]
fn begin_shutdown_raises_every_stop_and_joins_nothing() {
    let (mut client, source) = client(1);
    client.subscribe_book("111").expect("seated");
    client.subscribe_trades("222").expect("seated");
    client.begin_shutdown();
    assert_eq!(source.socket_count(), 2, "nothing was released");
    client.shutdown();
    assert_eq!(source.feed_count(), 0);
}

// ---- WS batching: packing, K = 1 equivalence, per-mode isolation ----------------------------

/// K = 1 is the pre-batching shape: N subscriptions ⇒ N sockets, one token each.
#[test]
fn k_of_one_opens_one_socket_per_subscription() {
    let (mut client, source) = client(1);
    for token in ["111", "222", "333"] {
        client.subscribe_book(token).expect("seated");
    }
    assert_eq!(source.socket_count(), 3, "K=1 never batches");
    assert_eq!(source.feed_count(), 3, "one feed thread per socket");
    client.shutdown();
}

/// The whole point: K tokens ride one socket, so N tokens cost `ceil(N/K)` sockets.
#[test]
fn tokens_pack_into_ceil_n_over_k_sockets() {
    let (mut client, source) = client(3);
    for token in ["1", "2", "3", "4", "5", "6", "7"] {
        client.subscribe_book(token).expect("seated");
    }
    assert_eq!(source.socket_count(), 3, "7 tokens at K=3 ⇒ ceil(7/3) = 3 sockets");
    assert_eq!(source.feed_count(), 3);
    client.shutdown();
}

/// A shard carries ONE mode: the emission rules differ per mode, so a quotes token never shares
/// a socket with a trades token even when seats are free.
#[test]
fn modes_never_share_a_socket() {
    let (mut client, source) = client(8);
    client.subscribe_book("111").expect("seated");
    client.subscribe_quotes("222").expect("seated");
    client.subscribe_trades("333").expect("seated");
    assert_eq!(source.socket_count(), 3, "one socket per mode despite 8 free seats each");
    client.shutdown();
}

/// **One seat per token and mode, however many hold it** (owner ruling B-10): a second subscribe of the same
/// token in one mode is a second holder of the same stream, not a second socket, and the stream stops only when its
/// last holder leaves.
#[test]
fn a_duplicate_token_in_one_mode_shares_its_seat_until_the_last_holder_leaves() {
    let (mut client, source) = client(8);
    let a = client.subscribe_book("111").expect("seated");
    let b = client.subscribe_book("111").expect("a second holder");
    assert_ne!(a, b);
    assert_eq!(source.socket_count(), 1, "one stream for both holders");
    client.unsubscribe(a);
    assert_eq!(source.socket_count(), 1, "the other holder keeps it");
    client.unsubscribe(b);
    assert_eq!(source.socket_count(), 0, "the last holder stops it");
    client.shutdown();
}

/// Releasing one seat of a shared socket keeps the socket (and its co-tenants) alive; releasing
/// the LAST seat stops+joins it.
#[test]
fn a_shared_socket_survives_until_its_last_seat_is_released() {
    let (mut client, source) = client(4);
    let a = client.subscribe_book("111").expect("seated");
    let b = client.subscribe_book("222").expect("seated");
    assert_eq!(source.socket_count(), 1);

    client.unsubscribe(a);
    assert_eq!(source.socket_count(), 1, "the co-tenant keeps the socket open");
    assert_eq!(source.feed_count(), 1, "its feed thread was NOT joined");

    client.unsubscribe(b);
    assert_eq!(source.socket_count(), 0, "the last seat closes the socket");
    assert_eq!(source.feed_count(), 0, "and joins its feed thread");
    client.shutdown();
}

/// A released seat frees room for the next token on the SAME socket — no socket leak from
/// churn.
#[test]
fn a_released_seat_is_reused_by_the_next_subscription() {
    let (mut client, source) = client(2);
    let a = client.subscribe_book("111").expect("seated");
    client.subscribe_book("222").expect("seated");
    assert_eq!(source.socket_count(), 1, "the socket is full");
    client.unsubscribe(a);
    client.subscribe_book("333").expect("seated");
    assert_eq!(source.socket_count(), 1, "the freed seat took the new token");
    client.shutdown();
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

/// **A token-set change is the bridge's own resubscribe, not a fault**: a session ended because
/// the shard's membership moved since it subscribed writes `Connecting`, and any other end (a real
/// ws or dial fault, the epoch unchanged) writes `Error`. Judged by the CAUSE, the epoch; the
/// words (`… ws error (reconnecting): token set changed — resubscribing`) are the same either way.
#[test]
fn a_token_set_change_reconnects_and_a_real_fault_is_an_error() {
    use vike_model::feed_status::DataClientConnectionState;
    let m = ShardMembership::new("a".into());
    let subscribed_at = m.epoch();
    assert_eq!(super::session_end_state(&m, subscribed_at), DataClientConnectionState::Error);
    assert!(m.try_admit("b", 2), "the 5-minute markets rotate a token in");
    assert_eq!(super::session_end_state(&m, subscribed_at), DataClientConnectionState::Connecting);
    assert_eq!(
        super::session_end_state(&m, m.epoch()),
        DataClientConnectionState::Error,
        "the next session subscribed at the new epoch: its own fault is an Error"
    );
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
    assert_eq!(
        seeded().tokens_per_socket(),
        DEFAULT_TOKENS_PER_SOCKET,
        "the default is the DEFAULT_TOKENS_PER_SOCKET value"
    );
    assert_eq!(seeded().with_tokens_per_socket(0).tokens_per_socket(), 1);
}

/// Decision 0095: K is `venue.polymarket.ws_tokens_per_socket`, handed in by the root — and the
/// catalog's documented default IS this constant.
#[test]
fn k_is_the_declared_field_or_the_default() {
    use vike_venues::venue_fields::WS_TOKENS_PER_SOCKET;
    let from = |v: Option<&str>| {
        tokens_per_socket(|f| if *f == WS_TOKENS_PER_SOCKET { v.map(str::to_string) } else { None })
    };
    assert_eq!(from(None), DEFAULT_TOKENS_PER_SOCKET);
    assert_eq!(from(Some("200")), 200);
    assert_eq!(from(Some("0")), DEFAULT_TOKENS_PER_SOCKET, "zero keeps the default");
    assert_eq!(from(Some("many")), DEFAULT_TOKENS_PER_SOCKET);
    let default = DEFAULT_TOKENS_PER_SOCKET.to_string();
    assert_eq!(
        vike_venues::venue_fields::venue_field("polymarket", &WS_TOKENS_PER_SOCKET)
            .map(|f| f.default),
        Some(default.as_str()),
        "the catalog's default must be the bridge's"
    );
}

#[test]
fn bars_are_unsupported() {
    let (mut client, _source) = client(1);
    assert_matches!(
        client.subscribe_bars("111", "1m"),
        Err(vike_data::DataClientError::Unsupported("polymarket serves ticks; bars via resample"))
    );
}

#[test]
fn with_raw_capture_spawns_tap_and_shuts_down_clean() {
    let dir = tempfile::tempdir().unwrap();
    let config = super::RawCaptureConfig { dir: dir.path().to_path_buf(), channel_cap: 16 };
    let urls = vike_venues::venue_urls::VenueUrls::seed("polymarket");
    let source = DataClientBusSpecPolymarket::with_raw_capture(|| {}, &urls, config)
        .expect("spawn ok")
        .with_body(fake_feed_body);
    let source = Arc::new(source);
    let mut client = DataClientBus::on(&source, Arc::new(RecordingSink::default()));
    // a fake feed body that never touches a socket — just proves lifecycle with capture on
    client.subscribe_book("111").expect("seated");
    client.shutdown(); // must join the feed thread AND the tap writer, no hang
    assert_eq!(source.feed_count(), 0);
    assert!(source.raw_tap_owner.lock().unwrap().is_none(), "the tap writer was joined");
}

#[test]
fn plain_new_has_no_capture() {
    assert!(seeded().raw_tap.is_none(), "DataClientBusSpecPolymarket::new must not enable capture");
}

/// A new source SETS `Connecting` before any shard dials; nothing reads it back from the words.
#[test]
fn a_new_source_is_connecting() {
    let state = seeded().status.lock().unwrap().state;
    assert_eq!(state, vike_model::feed_status::DataClientConnectionState::Connecting);
}

/// **The feed dials its `venue_url` rows** — the market channel is the row plus `/ws/market`, the
/// warmup the CLOB and Gamma rows — and a venue with no rows builds no feed at all.
#[test]
fn the_feed_dials_its_rows_and_refuses_without_them() {
    let source = seeded();
    let seed = vike_venues::venue_urls::VenueUrls::seed("polymarket");
    assert_eq!(Ok(source.market_websocket.clone()), crate::config::market_websocket_url(&seed));
    assert_eq!(Ok(source.clob.as_str()), crate::config::clob_base(&seed));
    assert_eq!(Ok(source.gamma.as_str()), crate::config::gamma_base(&seed));
    let none = vike_venues::venue_urls::VenueUrls::default();
    assert!(DataClientBusSpecPolymarket::new(|| {}, &none).is_err());
}

/// **PM-2**: a `tick_size_change` re-grids the token's book from that moment on, as the pmxt archive's mapper
/// reads it: the book lane records a snapshot carrying the new tick at the change, every later update carries it,
/// and a price on the finer grid keeps its digits. The same tick again records nothing.
#[test]
fn a_tick_size_change_regrids_the_book_and_every_later_update_carries_it() {
    use super::state::TokenSlot;
    let sink = RecordingSink::default();
    let mut slots = vec![TokenSlot::new("tok", 0.01, 60_000)];
    let snapshot = r#"{"event_type":"book","asset_id":"tok","timestamp":"1000",
        "bids":[{"price":"0.52","size":"10"}],"asks":[{"price":"0.6","size":"20"}]}"#;
    super::update::on_frame(snapshot, FeedLane::Book, &mut slots, &sink, "book", &|| {});
    let change = r#"{"event_type":"tick_size_change","asset_id":"tok","timestamp":"2000",
        "old_tick_size":"0.01","new_tick_size":"0.001"}"#;
    super::update::on_frame(change, FeedLane::Book, &mut slots, &sink, "book", &|| {});
    assert_eq!(slots[0].state.book.tick_size, 0.001);
    let delta = r#"{"event_type":"price_change","timestamp":"3000",
        "price_changes":[{"asset_id":"tok","price":"0.523","size":"5","side":"BUY"}]}"#;
    super::update::on_frame(delta, FeedLane::Book, &mut slots, &sink, "book", &|| {});
    super::update::on_frame(change, FeedLane::Book, &mut slots, &sink, "book", &|| {});
    let ticks: Vec<(i64, f64)> =
        sink.book_updates().iter().map(|(_, _, u)| (u.timestamp, u.tick_size)).collect();
    assert_eq!(ticks, [(1000, 0.01), (2000, 0.001), (3000, 0.001)], "{ticks:?}");
    let regridded = &sink.book_updates()[1].2;
    assert_eq!(regridded.bids[0].price, 0.52, "the levels survive the re-grid");
    assert_eq!(slots[0].state.book.best_bid().map(|level| level.price), Some(0.523));
}

/// **NO GUESSED TICK** (owner, 2026-11-13): a token whose `/tick-size` lookup failed is seated but
/// REFUSED — a delta before its book snapshot folds into nothing and emits nothing — until the venue's own
/// `book` snapshot states its tick, and from then on its book is served on that grid. Red on a book
/// booked on a guessed grid, or on a pending token that is never served.
#[test]
fn a_token_with_no_tick_is_refused_until_its_book_snapshot_states_one() {
    use super::state::TokenSlot;
    let sink = RecordingSink::default();
    let mut slots = vec![TokenSlot::awaiting_tick("tok", 60_000)];
    let delta = r#"{"event_type":"price_change","timestamp":"1000",
        "price_changes":[{"asset_id":"tok","price":"0.41","size":"5","side":"BUY"}]}"#;
    super::update::on_frame(delta, FeedLane::Book, &mut slots, &sink, "book", &|| {});
    assert!(slots[0].tick_pending, "a delta states no tick");
    assert!(sink.calls().is_empty(), "a refused token emits nothing: {:?}", sink.calls());

    let snapshot = r#"{"event_type":"book","asset_id":"tok","timestamp":"2000",
        "bids":[{"price":"0.405","size":"10"}],"asks":[{"price":"0.6","size":"20"}],"tick_size":"0.001"}"#;
    super::update::on_frame(snapshot, FeedLane::Book, &mut slots, &sink, "book", &|| {});
    assert!(!slots[0].tick_pending, "the snapshot stated the tick");
    assert_eq!(slots[0].state.book.tick_size, 0.001, "the venue's own grid, never a guessed one");
    assert_eq!(slots[0].state.book.best_bid().map(|level| level.price), Some(0.405));
}

/// **A shard's reconnect waits, its idle cut and its keepalive, pinned verbatim before its loop took its dial and
/// its REST seat from outside** (connection-sync, 2026-11-22: the shard loop, `super::run_shard_on`, reachable over a
/// scripted session). The numbers are this venue's pump row's (`vike_bridge_core::websocket_session::spec`'s
/// `row`): three refused dials wait 500 ms, 1 s and 2 s; the session that opens sends its subscribe, then `"PING"`
/// every 10 s, is cut 30 s after its last frame as a silent stall (`Error`) and starts the doubling over at 500 ms;
/// seven more refusals double up to the 30 s ceiling; every open writes `Connected`.
#[test]
fn a_shards_waits_idle_and_keepalive_are_pinned_verbatim() {
    use std::collections::VecDeque;
    use vike_bridge_core::pump_clock::VirtualClock;
    use vike_bridge_core::scripted::{
        LaneStatus, UserScriptHeard, UserScriptLog, UserScriptStep,
        WebsocketSessionUserStreamScript,
    };
    use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};

    let clock = VirtualClock::new(1);
    let _seat = clock.enter();
    let lane = LaneStatus::halting_at("a status no shard writes");
    let ctx = DataClientContext {
        venue: "polymarket",
        sink: Arc::new(RecordingSink::default()),
        status: Arc::clone(&lane.status),
        wake: Arc::clone(&lane.wake),
        stop: Arc::clone(&lane.stop),
        own: (),
    };
    let log = UserScriptLog::default();
    // `Some`: a session of these steps; `None`: a refused dial.
    let mut script: VecDeque<Option<Vec<UserScriptStep>>> = [None, None, None]
        .into_iter()
        .chain([Some(vec![UserScriptStep::Stall(Duration::from_secs(300))])])
        .chain(std::iter::repeat_with(|| None).take(7))
        .chain([Some(Vec::new())])
        .collect();
    let mut opens = Vec::new();
    super::run_token_shard(FeedLane::Quotes, "111", 0.01, &ctx, || {
        opens.push(clock.offset(clock.now()).as_millis());
        let steps = script.pop_front().ok_or("a dial past the script")?.ok_or("refused")?;
        let session = WebsocketSessionUserStreamScript::from_steps(steps).logged(Arc::clone(&log));
        Ok(if script.is_empty() { session.stopping(Arc::clone(&ctx.stop)) } else { session })
    });
    // Each step is spread by up to half of it under the 30 s ceiling (the pump's ladder is a row's:
    // `vike_venues::reconnect`'s module doc), so each GAP is pinned: its step, and the session's 30 s life before it.
    assert_eq!(opens.len(), 12, "the script's dials: {opens:?}");
    let steps = [500, 1_000, 2_000, 500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000];
    for (at, (pair, step)) in opens.windows(2).zip(steps).enumerate() {
        let (gap, lived) = (pair[1] - pair[0], if at == 3 { 30_000 } else { 0 });
        assert!(
            lived + step <= gap && gap <= lived + (step + step / 2).min(30_000),
            "500 ms doubling to 30 s, from the start again after the session that opened; that session cut at \
             30 s: gap {at} is {gap} ms for the step {step} ms ({opens:?})"
        );
    }
    let (session, last) = (opens[3], opens[11]);
    let subscribe = crate::feed::decode::subscribe_message(&["111".to_string()]);
    let sent: Vec<(u128, String)> = log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(at, heard)| match heard {
            UserScriptHeard::Text(text) => Some((clock.offset(*at).as_millis(), text.clone())),
            UserScriptHeard::Pong => None,
        })
        .collect();
    let ping = super::PING_PAYLOAD.to_string();
    assert_eq!(
        sent,
        [
            (session, subscribe.clone()),
            (session + 10_000, ping.clone()),
            (session + 20_000, ping),
            (last, subscribe)
        ],
        "each session's subscribe at its open, and the row's 10 s ping"
    );
    let seen = lane.seen.lock().unwrap().clone();
    let stall = DataClientStatus::new(
        DataClientConnectionState::Error,
        "polymarket 111 ws error (reconnecting): no frames within 30s (silent stall)",
    );
    assert!(seen.contains(&stall), "the stall is an Error in the shard's words: {seen:?}");
    assert_eq!(
        seen.last(),
        Some(&DataClientStatus::new(DataClientConnectionState::Connected, "LIVE · Polymarket 111")),
        "the reopened session says Connected: {seen:?}"
    );
}
