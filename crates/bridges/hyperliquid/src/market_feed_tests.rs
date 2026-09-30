use super::*;
use std::sync::atomic::Ordering;
use std::time::Duration;

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

/// A network-free stand-in for a real `*_main`: just polls its own stop flag.
fn fake_body(ctx: FeedCtx) {
    while !ctx.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn test_ctx(sink: Arc<dyn LiveDataSink>) -> FeedCtx {
    FeedCtx {
        sink,
        status: Arc::new(Mutex::new(String::new())),
        wake: Arc::new(|| {}),
        stop: Arc::new(AtomicBool::new(false)),
    }
}

#[test]
fn subscribe_messages_have_the_hl_shape() {
    let candle: Value = serde_json::from_str(&subscribe_candle_msg("BTC", "1m")).unwrap();
    assert_eq!(candle["method"], "subscribe");
    assert_eq!(candle["subscription"]["type"], "candle");
    assert_eq!(candle["subscription"]["coin"], "BTC");
    assert_eq!(candle["subscription"]["interval"], "1m");

    let bbo: Value = serde_json::from_str(&subscribe_bbo_msg("@107")).unwrap();
    assert_eq!(bbo["subscription"]["type"], "bbo");
    assert_eq!(bbo["subscription"]["coin"], "@107");

    let trades: Value = serde_json::from_str(&subscribe_trades_msg("ETH")).unwrap();
    assert_eq!(trades["subscription"]["type"], "trades");
    assert_eq!(trades["subscription"]["coin"], "ETH");

    let book: Value = serde_json::from_str(&subscribe_l2book_msg("BTC")).unwrap();
    assert_eq!(book["subscription"]["type"], "l2Book");
    assert_eq!(book["subscription"]["coin"], "BTC");
}

/// Open-time rollover: the previous candle is emitted as CLOSED exactly when a strictly-newer
/// open-time arrives; a same-open-time frame is an in-progress forming update.
#[test]
fn candle_rollover_closes_previous_bar() {
    let sink = Arc::new(RecordingSink::default());
    let ctx = test_ctx(sink.clone());
    let mut open_bar: Option<Bar> = None;
    let frame = |t: i64, c: &str| {
        format!(
            r#"{{"channel":"candle","data":{{"t":{t},"o":"1","h":"1","l":"1","c":"{c}","v":"1"}}}}"#
        )
    };
    handle_candle_frame(&frame(1000, "10"), "BTC", "1m", &ctx, &mut open_bar); // first → forming
    handle_candle_frame(&frame(1000, "11"), "BTC", "1m", &ctx, &mut open_bar); // update → forming
    handle_candle_frame(&frame(1060, "12"), "BTC", "1m", &ctx, &mut open_bar); // roll → close+forming
    assert_eq!(
        sink.calls(),
        vec![
            "bar_close_tick(hyperliquid,BTC,10,1000)".to_string(),
            "forming_bar(hyperliquid,BTC,1m,10)".to_string(),
            "bar_close_tick(hyperliquid,BTC,11,1000)".to_string(),
            "forming_bar(hyperliquid,BTC,1m,11)".to_string(),
            "close_bar(hyperliquid,BTC,1m,11)".to_string(),
            "bar_close_tick(hyperliquid,BTC,12,1060)".to_string(),
            "forming_bar(hyperliquid,BTC,1m,12)".to_string(),
        ]
    );
}

/// A strictly-older candle (a stale replay right after a reconnect) is dropped — the current
/// forming bar stands, no close, no forming.
#[test]
fn candle_stale_replay_is_dropped() {
    let sink = Arc::new(RecordingSink::default());
    let ctx = test_ctx(sink.clone());
    let mut open_bar: Option<Bar> = None;
    let frame = |t: i64| {
        format!(
            r#"{{"channel":"candle","data":{{"t":{t},"o":"1","h":"1","l":"1","c":"5","v":"1"}}}}"#
        )
    };
    handle_candle_frame(&frame(2000), "BTC", "1m", &ctx, &mut open_bar);
    let n_after_first = sink.calls().len();
    handle_candle_frame(&frame(1000), "BTC", "1m", &ctx, &mut open_bar); // older → dropped
    assert_eq!(sink.calls().len(), n_after_first, "stale frame emitted nothing");
}

#[test]
fn subscribe_returns_distinct_ids_and_shutdown_joins() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn("a", fake_body).expect("spawn ok");
    let id2 = feeds.spawn("b", fake_body).expect("spawn ok");
    assert_ne!(id1, id2, "distinct ids per subscribe");
    feeds.shutdown();
    assert!(feeds.registry.is_empty());
}

#[test]
fn unsubscribe_stops_only_that_one_feed() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn("a", fake_body).expect("spawn ok");
    let id2 = feeds.spawn("b", fake_body).expect("spawn ok");
    feeds.unsubscribe(id1);
    assert_eq!(feeds.registry.len(), 1, "only the unsubscribed stream is removed");
    assert!(feeds.registry.contains(id2), "the other keeps running");
    feeds.shutdown();
    assert!(feeds.registry.is_empty());
}

/// The pairing PREDICATE (mark-slot semantics, W2-T4): only a PERP (a bare coin) pairs a mark
/// stream, and only while the knob is on. A spot pair's ctx channel carries no `markPx`.
#[test]
fn only_perps_pair_a_mark_stream_and_only_while_the_knob_is_on() {
    assert!(should_pair_mark("BTC", true));
    assert!(!should_pair_mark("HYPE/USDC", true), "spot carries no markPx");
    assert!(!should_pair_mark("BTC", false), "a `mark_streams = 0` row suppresses");
}

/// SPAWN side, network-free: `pair_mark_stream` is the production path `subscribe_bars` calls
/// (only `body` differs here). A perp spawns a companion `activeAssetCtx` stream;
/// unsubscribing the bars id stops BOTH, while unrelated subscriptions keep running.
#[test]
fn a_perp_bars_subscription_spawns_and_then_stops_its_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let bars_id = feeds.spawn("bars-BTC@1m", fake_body).expect("spawn ok");
    let other_id = feeds.spawn("bars-ETH@1m", fake_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTC", fake_body);
    assert_eq!(feeds.registry.len(), 3, "bars + mark + the unrelated feed");

    feeds.unsubscribe(bars_id);
    assert_eq!(feeds.registry.len(), 1, "bars AND mark stopped together");
    assert!(feeds.registry.contains(other_id), "unrelated subscriptions keep running");
    assert!(feeds.mark_pairings.is_empty(), "the pairing is consumed");
    feeds.shutdown();
}

#[test]
fn a_spot_bars_subscription_spawns_no_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let bars_id = feeds.spawn("bars-HYPE/USDC@1m", fake_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "HYPE/USDC", fake_body);
    assert_eq!(feeds.registry.len(), 1, "no companion stream for spot");
    feeds.shutdown();
}

/// A `mark_streams = 0` row suppresses the spawn even for a perp — the knob's whole job, pinned.
#[test]
fn the_mark_streams_knob_off_suppresses_the_perp_spawn() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.mark_streams = false;
    let bars_id = feeds.spawn("bars-BTC@1m", fake_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTC", fake_body);
    assert_eq!(feeds.registry.len(), 1, "knob off -> no mark stream even for a perp");
    feeds.shutdown();
}

/// The default this feed runs and the one `config show` prints are ONE fact: the charter constant
/// is `venue.hyperliquid.mark_streams`'s declared default.
#[test]
fn the_charter_default_is_the_declared_fields() {
    let declared = vike_model::venue_fields::venue_field("hyperliquid", "mark_streams")
        .expect("declared")
        .default;
    assert_eq!(super::MARK_STREAM_DEFAULT_ON, declared == "1", "declared {declared:?}");
}

/// Decision 0095: the `mark_streams` row decides, and no row keeps the charter default (ON here).
#[test]
fn the_mark_streams_row_decides_and_no_row_keeps_the_charter_default() {
    let fresh = || Feeds::new(Arc::new(RecordingSink::default()), || {});
    assert!(fresh().mark_streams, "charter default: ON");
    assert!(!fresh().with_mark_streams(Some("0")).mark_streams);
    assert!(fresh().with_mark_streams(Some("1")).mark_streams);
    assert!(fresh().with_mark_streams(None).mark_streams);
}

/// Per-symbol dedupe: a 1m AND a 5m chart on the same perp share ONE mark socket.
#[test]
fn two_intervals_on_one_perp_share_a_single_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let bars_1m = feeds.spawn("bars-BTC@1m", fake_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_1m, "BTC", fake_body);
    let bars_5m = feeds.spawn("bars-BTC@5m", fake_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_5m, "BTC", fake_body);
    assert_eq!(feeds.registry.len(), 3, "two bars feeds but only ONE mark stream");

    feeds.unsubscribe(bars_1m);
    assert_eq!(feeds.registry.len(), 2, "the mark stream the 5m chart still needs stays up");
    feeds.unsubscribe(bars_5m);
    assert!(feeds.registry.is_empty(), "the last release stops the mark stream too");
    feeds.shutdown();
}

#[test]
fn book_is_unsupported() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    assert!(matches!(feeds.subscribe_book("BTC"), Err(LiveDataError::Unsupported(_))));
    // bars/quotes/trades/depth aren't exercised via a real subscribe here (that touches the
    // network); their subscribe-message builders + the candle rollover are covered above.
    feeds.shutdown();
}

/// A synthetic testnet spot symbology (BTC perp + HYPE/USDC spot → coin `@107`).
fn test_symbology() -> Symbology {
    let meta = serde_json::json!({"universe": [{"name": "BTC", "szDecimals": 5}]});
    let spot = serde_json::json!({
        "tokens": [
            {"name": "USDC", "szDecimals": 8, "index": 0},
            {"name": "HYPE", "szDecimals": 2, "index": 150}
        ],
        "universe": [{"name": "@107", "tokens": [150, 0], "index": 107}]
    });
    Symbology::from_meta(&meta, &spot)
}

#[test]
fn resolve_coin_bare_perp_needs_no_symbology() {
    // A perp coin has no `/`, so resolve_coin returns immediately even with an empty cell — it
    // must never wait for the (unneeded) symbology. Also pins the mainnet default.
    let feeds = Feeds::new(Arc::new(RecordingSink::default()), || {});
    assert_eq!(resolve_coin(&feeds.symbology_cell(), "BTC"), "BTC");
    assert_eq!(feeds.ws_url(), crate::consts::MAINNET_WS, "defaults to mainnet");
}

#[test]
fn resolve_coin_resolves_spot_via_symbology() {
    let feeds = Feeds::new(Arc::new(RecordingSink::default()), || {})
        .with_network(Network::Testnet)
        .with_symbology(Arc::new(test_symbology()));
    let cell = feeds.symbology_cell(); // already populated
    assert_eq!(resolve_coin(&cell, "HYPE/USDC"), "@107", "spot symbol → @-coin");
    assert_eq!(resolve_coin(&cell, "BTC"), "BTC", "perp symbol == coin");
    assert_eq!(resolve_coin(&cell, "FOO/BAR"), "FOO/BAR", "loaded-but-unknown pair → fallback");
    assert_eq!(feeds.ws_url(), crate::consts::TESTNET_WS, "with_network took effect");
}

#[test]
fn resolve_coin_waits_for_late_symbology() {
    // The exact race the in-thread wait closes: a spot subscribe fires BEFORE the background
    // symbology load finishes. resolve_coin must block (briefly, well under SYMBOLOGY_WAIT) until
    // the cell fills, then resolve the @-coin — never return the raw symbol.
    let cell: Arc<Mutex<Option<Arc<Symbology>>>> = Arc::new(Mutex::new(None));
    let writer = Arc::clone(&cell);
    let h = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(250));
        *writer.lock().unwrap() = Some(Arc::new(test_symbology()));
    });
    assert_eq!(resolve_coin(&cell, "HYPE/USDC"), "@107", "waited for the late load, then resolved");
    h.join().unwrap();
}

// ---- activeAssetCtx mark stream (mark-slot semantics, W2-T4), scripted-frame style ----

/// A real-shaped `activeAssetCtx` frame → the venue's `markPx` bits (a decimal string; a bare
/// number is tolerated too — the pump stamps receive time as the mark's ts).
#[test]
fn mark_from_active_asset_ctx_frame() {
    let frame: Value = serde_json::from_str(
            r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"markPx":"27123.5","oraclePx":"27120.0","funding":"0.0001"}}}"#,
        )
        .unwrap();
    assert_eq!(mark_from_frame(&frame).unwrap().to_bits(), 27_123.5_f64.to_bits());

    let numeric: Value = serde_json::from_str(
        r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"markPx":42.0}}}"#,
    )
    .unwrap();
    assert_eq!(mark_from_frame(&numeric).unwrap().to_bits(), 42.0_f64.to_bits());
}

/// A non-`activeAssetCtx` channel, a missing ctx/markPx, and a dead (0/neg/NaN) mark all yield
/// `None` — never a bogus mark into the `PriceBoard` mark slot.
#[test]
fn mark_from_frame_rejects_wrong_channel_and_dead_marks() {
    let candle: Value =
        serde_json::from_str(r#"{"channel":"candle","data":{"t":1,"c":"5"}}"#).unwrap();
    assert!(mark_from_frame(&candle).is_none(), "wrong channel");

    let no_ctx: Value =
        serde_json::from_str(r#"{"channel":"activeAssetCtx","data":{"coin":"BTC"}}"#).unwrap();
    assert!(mark_from_frame(&no_ctx).is_none(), "missing ctx");

    for dead in ["0", "-1.0", "nan"] {
        let f: Value = serde_json::from_str(&format!(
            r#"{{"channel":"activeAssetCtx","data":{{"ctx":{{"markPx":"{dead}"}}}}}}"#
        ))
        .unwrap();
        assert!(mark_from_frame(&f).is_none(), "dead mark {dead} rejected");
    }
}

/// The subscribe message targets the `activeAssetCtx` type for the given coin — the exact
/// `{"method":"subscribe","subscription":{"type":"activeAssetCtx","coin":...}}` wire grammar.
#[test]
fn active_asset_ctx_subscribe_targets_the_coin() {
    let v: Value = serde_json::from_str(&subscribe_active_asset_ctx_msg("BTC")).unwrap();
    assert_eq!(v["method"], "subscribe");
    assert_eq!(v["subscription"]["type"], "activeAssetCtx");
    assert_eq!(v["subscription"]["coin"], "BTC");
}
