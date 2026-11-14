use super::*;
use std::assert_matches;
use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::data_client::ws::{DataClientWebsocket, run_plan_on};
use vike_bridge_core::market_feed_pins;
use vike_bridge_core::websocket_session::market::WebsocketSessionMarketOptions;
use vike_data::{DataClient, DataClientError, DataClientSink};

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

/// The spec on `network`'s rows of the release's seed (`VENUE_URL_SEED`).
fn on(network: VenueNetwork) -> DataClientSpecHyperliquid {
    let seed = vike_venues::venue_urls::VenueUrls::seed(crate::transport::consts::VENUE);
    DataClientSpecHyperliquid::new(network, &seed).expect("the seed rows")
}

/// The engine over the mainnet spec (built, never dialled).
fn fresh() -> DataClientWebsocket<DataClientSpecHyperliquid> {
    DataClientWebsocket::new(on(VenueNetwork::Mainnet), Arc::new(RecordingSink::default()), || {})
}

/// `lane` of `symbol` through the engine over ONE scripted session of `frames`, on the mainnet spec's
/// own plan (the lane stops at its first `ws error`): the sink, the statuses written, the frames sent.
fn engine_lane(
    lane: DataClientLane,
    symbol: &str,
    interval: &str,
    frames: &[&str],
) -> (Arc<RecordingSink>, Vec<String>, Vec<String>) {
    let sink = Arc::new(RecordingSink::default());
    let vike_bridge_core::scripted::LaneStatus { status, stop, wake, seen } =
        vike_bridge_core::scripted::LaneStatus::halting_at("ws error");
    let ctx =
        DataClientContext { venue: "hyperliquid", sink: sink.clone(), status, wake, stop, own: () };
    let plan = on(VenueNetwork::Mainnet).plan(lane, symbol, interval).expect("a plan");
    let sent = std::rc::Rc::default();
    let mut session = vike_bridge_core::scripted::one_session(frames, &sent);
    let dial = move |_url: &str, opts: &WebsocketSessionMarketOptions<'_>| session(opts);
    run_plan_on::<DataClientSpecHyperliquid, _>(plan, lane, symbol, interval, &ctx, dial);
    // The first confirmed frame is DATA here (HL's subscribe answer confirms nothing), so its wake
    // records the still-empty line before the session's first status write: not a status written.
    let seen = seen.lock().unwrap().iter().filter(|s| !s.is_empty()).cloned().collect();
    (sink, seen, sent.take())
}

/// A `candle` frame of the candle opening at `t`, closing at `c`.
fn candle(t: i64, c: &str) -> String {
    format!(
        r#"{{"channel":"candle","data":{{"t":{t},"o":"1","h":"9","l":"0.5","c":"{c}","v":"1"}}}}"#
    )
}

#[test]
fn subscribe_messages_have_the_hl_shape() {
    let candle: Value = serde_json::from_str(&subscribe_candle_message("BTC", "1m")).unwrap();
    assert_eq!(candle["method"], "subscribe");
    assert_eq!(candle["subscription"]["type"], "candle");
    assert_eq!(candle["subscription"]["coin"], "BTC");
    assert_eq!(candle["subscription"]["interval"], "1m");

    let bbo: Value = serde_json::from_str(&subscribe_bbo_message("@107")).unwrap();
    assert_eq!(bbo["subscription"]["type"], "bbo");
    assert_eq!(bbo["subscription"]["coin"], "@107");

    let trades: Value = serde_json::from_str(&subscribe_trades_message("ETH")).unwrap();
    assert_eq!(trades["subscription"]["type"], "trades");
    assert_eq!(trades["subscription"]["coin"], "ETH");

    let book: Value = serde_json::from_str(&subscribe_l2book_message("BTC")).unwrap();
    assert_eq!(book["subscription"]["type"], "l2Book");
    assert_eq!(book["subscription"]["coin"], "BTC");
}

/// Open-time rollover, through the engine: the previous candle is emitted as CLOSED exactly when a
/// strictly-newer open-time arrives; a same-open-time frame is an in-progress forming update.
#[test]
fn candle_rollover_closes_previous_bar() {
    let frames = [candle(1000, "10"), candle(1000, "11"), candle(1060, "12")]; // first, update, roll
    let frames: Vec<&str> = frames.iter().map(String::as_str).collect();
    let (sink, _, _) = engine_lane(DataClientLane::Bars, "BTC", "1m", &frames);
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
    let (first, older) = (candle(2000, "5"), candle(1000, "5"));
    let (sink, _, _) =
        engine_lane(DataClientLane::Bars, "BTC", "1m", &[first.as_str(), older.as_str()]);
    assert_eq!(
        sink.calls(),
        ["bar_close_tick(hyperliquid,BTC,5,2000)", "forming_bar(hyperliquid,BTC,1m,5)"],
        "the stale frame emitted nothing"
    );
}

/// The sink calls the retired bars `Own` loop made per frame (`handle_candle_frame`), statement for
/// statement: side A of the bars A/B.
fn own_loop_bars(frames: &[&str], series: &str, interval: &str, sink: &dyn DataClientSink) {
    let mut open_bar: Option<Bar> = None;
    for txt in frames {
        let Ok(v) = serde_json::from_str::<Value>(txt) else { continue };
        let Some(bar) = crate::feed::decode::candle_to_bar(&v) else { continue };
        if let Some(previous) = open_bar.as_ref() {
            if bar.timestamp < previous.timestamp {
                continue;
            }
            if bar.timestamp > previous.timestamp {
                sink.close_bar(VENUE, series, interval, previous.clone());
            }
        }
        sink.bar_close_tick(VENUE, series, bar.close, bar.timestamp);
        open_bar = Some(bar.clone());
        sink.forming_bar(VENUE, series, interval, bar);
    }
}

/// **A/B: the bars lane lands on the sink, call for call and byte for byte, what its `Own` loop landed**
/// (K1): the subscribe answer, a first candle, an update, the pong, a roll (the inferred close ALONE,
/// then the new candle ticked and forming), a stale replay, a skipped interval, a non-JSON frame. The
/// subscribe frame and the statuses are the loop's.
#[test]
fn the_bars_lane_lands_what_its_own_loop_landed() {
    let frames = [
        r#"{"channel":"subscriptionResponse","data":{"method":"subscribe","subscription":{"type":"candle","coin":"BTC","interval":"1m"}}}"#.to_string(),
        candle(60_000, "2"),
        candle(60_000, "2.5"),
        r#"{"channel":"pong"}"#.to_string(),
        candle(120_000, "3"),
        candle(60_000, "7"),
        candle(300_000, "4"),
        candle(300_000, "4.25"),
        "not json".to_string(),
    ];
    let frames: Vec<&str> = frames.iter().map(String::as_str).collect();
    let own = RecordingSink::default();
    own_loop_bars(&frames, "BTC", "1m", &own);
    let (engine, seen, sent) = engine_lane(DataClientLane::Bars, "BTC", "1m", &frames);
    assert_eq!(format!("{:?}", engine.recorded()), format!("{:?}", own.recorded()));
    assert_eq!(own.recorded().len(), 12, "every arm was reached: {:?}", own.calls());
    assert_eq!(own.closed_bars().len(), 2);
    assert_eq!(sent, [subscribe_candle_message("BTC", "1m")]);
    assert_eq!(seen, ["LIVE · Hyperliquid", "BTC@1m ws error (reconnecting): script exhausted"]);
}

/// **A quote whose frame states no venue time takes its receipt time** (owner ruling B-21), through the
/// engine: the price board reads `ts`, and a `0` there is a price from 1970 that no age-bounded read
/// trusts. A stated time is kept, and the quote carries the series label.
#[test]
fn a_quote_with_no_venue_time_takes_its_receipt_time() {
    let sink = Arc::new(RecordingSink::default());
    let vike_bridge_core::scripted::LaneStatus { status, stop, wake, .. } =
        vike_bridge_core::scripted::LaneStatus::halting_at("ws error");
    let ctx =
        DataClientContext { venue: "hyperliquid", sink: sink.clone(), status, wake, stop, own: () };
    let frame = |time: &str| {
        format!(
            r#"{{"channel":"bbo","data":{{"coin":"BTC",{time}"bbo":[{{"px":"10.0","sz":"2","n":1}},{{"px":"10.5","sz":"3","n":1}}]}}}}"#
        )
    };
    let (bare, stamped) = (frame(""), frame(r#""time":1700000000000,"#));
    let lane = DataClientLane::Quotes;
    let plan = on(VenueNetwork::Mainnet).plan(lane, "BTC", lane.word("")).expect("a plan");
    let sent = std::rc::Rc::default();
    let mut session =
        vike_bridge_core::scripted::one_session(&[bare.as_str(), stamped.as_str()], &sent);
    let dial = move |_url: &str, opts: &WebsocketSessionMarketOptions<'_>| session(opts);
    run_plan_on::<DataClientSpecHyperliquid, _>(plan, lane, "BTC", "quotes", &ctx, dial);
    let quotes = sink.quotes();
    assert_eq!(quotes.len(), 2, "{quotes:?}");
    let (_, symbol, q) = &quotes[0];
    assert_eq!((symbol.as_str(), q.symbol.as_str(), q.bid, q.ask), ("BTC", "BTC", 10.0, 10.5));
    assert!(
        q.local_timestamp > 0 && q.timestamp == q.local_timestamp,
        "no venue time: the receipt time, {q:?}"
    );
    assert_eq!(quotes[1].2.timestamp, 1_700_000_000_000, "a venue time is kept");
}

/// The pairing PREDICATE (mark-slot semantics, W2-T4): only a PERP (a bare coin) pairs a mark
/// stream (the engine adds the venue's `mark_streams` row). A spot pair's ctx channel carries no `markPx`.
#[test]
fn only_perps_pair_a_mark_stream() {
    assert!(should_pair_mark("BTC"));
    assert!(!should_pair_mark("HYPE/USDC"), "spot carries no markPx");
    assert!(on(VenueNetwork::Mainnet).pairs_mark("BTC"), "the spec asks the one predicate");
}

/// The default this feed runs and the one `config show` prints are ONE fact: the charter constant
/// is `venue.hyperliquid.mark_streams`'s declared default.
#[test]
fn the_charter_default_is_the_declared_fields() {
    market_feed_pins::charter_default_is_declared(
        "hyperliquid",
        DataClientSpecHyperliquid::MARK_STREAMS_DEFAULT_ON,
    );
}

/// Every served lane is an engine session on the network's socket with the app-level ping, live-only
/// (none is the venue's own loop).
#[test]
fn the_lanes_are_engine_sessions_on_the_networks_socket() {
    let spec = on(VenueNetwork::Testnet);
    for (lane, want) in [
        (DataClientLane::Bars, subscribe_candle_message("BTC", "1m")),
        (DataClientLane::Mark, subscribe_active_asset_ctx_msg("BTC")),
        (DataClientLane::Quotes, subscribe_bbo_message("BTC")),
        (DataClientLane::Trades, subscribe_trades_message("BTC")),
        (DataClientLane::Depth, subscribe_l2book_message("BTC")),
    ] {
        let DataClientLanePlan::Session { url, subscribe, keepalive, seed, .. } =
            spec.plan(lane, "BTC", lane.word("1m")).expect("a plan")
        else {
            panic!("{lane:?} is an engine session")
        };
        assert_eq!(url, "wss://api.hyperliquid-testnet.xyz/ws", "{lane:?}");
        assert_eq!(subscribe, Some(want), "{lane:?}");
        assert_eq!(keepalive, Some(PING_MESSAGE), "{lane:?}");
        assert!(seed.is_none(), "{lane:?} is live-only");
    }
}

/// The sink calls the retired depth `Own` loop made per frame, statement for statement: side A of the
/// depth A/B.
fn own_loop_depth(frames: &[&str], series: &str, sink: &dyn DataClientSink) {
    for txt in frames {
        let Ok(v) = serde_json::from_str::<Value>(txt) else { continue };
        if let Some(snap) = crate::feed::decode::l2book_to_snapshot(&v) {
            let tick = infer_tick_size(&snap.bids, &snap.asks);
            sink.l2_snapshot(VENUE, series, tick, snap.bids, snap.asks, snap.time);
        }
    }
}

/// **A/B: the depth lane lands on the sink, call for call and byte for byte, what its `Own` loop
/// landed** (K2): two full snapshots (every level, the inferred grid, the venue's time), one with no
/// time and a side left empty, between the subscribe answer, the pong, another channel and a non-JSON
/// frame. The subscribe frame and the statuses are the loop's.
#[test]
fn the_depth_lane_lands_what_its_own_loop_landed() {
    let book = |time: &str, bids: &str, asks: &str| {
        format!(
            r#"{{"channel":"l2Book","data":{{"coin":"BTC",{time}"levels":[[{bids}],[{asks}]]}}}}"#
        )
    };
    let level = |px: &str, sz: &str| format!(r#"{{"px":"{px}","sz":"{sz}","n":2}}"#);
    let frames = [
        r#"{"channel":"subscriptionResponse","data":{"method":"subscribe","subscription":{"type":"l2Book","coin":"BTC"}}}"#.to_string(),
        book(
            r#""time":1700000000000,"#,
            &[level("27000.5", "1.25"), level("27000", "0.5"), level("26998.5", "3")].join(","),
            &[level("27001", "2"), level("27002.5", "0.125")].join(","),
        ),
        r#"{"channel":"pong"}"#.to_string(),
        r#"{"channel":"trades","data":[]}"#.to_string(),
        book("", &level("27000", "4"), ""),
        "not json".to_string(),
    ];
    let frames: Vec<&str> = frames.iter().map(String::as_str).collect();
    let own = RecordingSink::default();
    own_loop_depth(&frames, "BTC", &own);
    let (engine, seen, sent) = engine_lane(DataClientLane::Depth, "BTC", "depth", &frames);
    assert_eq!(format!("{:?}", engine.recorded()), format!("{:?}", own.recorded()));
    let snapshots = own.l2_snapshots();
    assert_eq!(snapshots.len(), 2, "both snapshots: {:?}", own.calls());
    assert_eq!(
        (snapshots[0].3.len(), snapshots[0].4.len(), snapshots[0].5),
        (3, 2, 1_700_000_000_000)
    );
    assert_eq!((snapshots[1].4.len(), snapshots[1].5), (0, 0), "an empty side, no time");
    assert_eq!(sent, [subscribe_l2book_message("BTC")]);
    assert_eq!(seen, ["LIVE · Hyperliquid", "BTC depth ws error (reconnecting): script exhausted"]);
}

/// The book refusal is the declared caps row's (`book: false`), in the words every venue's refusal reads
/// (owner ruling B-21).
#[test]
fn book_is_unsupported() {
    let mut feeds = fresh();
    assert_matches!(
        feeds.subscribe_book("BTC"),
        Err(DataClientError::Unsupported(
            "venue's declared VenueCapabilities.live_data serves no lossless L2 book lane"
        ))
    );
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
    let spec = on(VenueNetwork::Mainnet);
    assert_eq!(resolve_coin(&spec.symbology_cell(), "BTC"), "BTC");
    // On the seed's mainnet rows: byte for byte the socket and REST host it dialled before.
    assert_eq!(spec.ws, "wss://api.hyperliquid.xyz/ws", "mainnet");
    assert_eq!(spec.rest, "https://api.hyperliquid.xyz");
}

#[test]
fn resolve_coin_resolves_spot_via_symbology() {
    let spec = on(VenueNetwork::Testnet).with_symbology(Arc::new(test_symbology()));
    let cell = spec.symbology_cell(); // already populated
    assert_eq!(resolve_coin(&cell, "HYPE/USDC"), "@107", "spot symbol → @-coin");
    assert_eq!(resolve_coin(&cell, "BTC"), "BTC", "perp symbol == coin");
    assert_eq!(resolve_coin(&cell, "FOO/BAR"), "FOO/BAR", "loaded-but-unknown pair → fallback");
    assert_eq!(spec.ws, "wss://api.hyperliquid-testnet.xyz/ws", "the testnet's own row");
    assert_eq!(spec.rest, "https://api.hyperliquid-testnet.xyz");
}

/// A feed whose rows lack its network's `data` socket is refused, naming the row: nothing is dialled.
#[test]
fn a_feed_with_no_data_row_is_refused() {
    let none = vike_venues::venue_urls::VenueUrls::from_rows("hyperliquid", std::iter::empty());
    let Err(why) = DataClientSpecHyperliquid::new(VenueNetwork::Mainnet, &none) else {
        panic!("no row")
    };
    assert!(why.to_string().contains("no address row for live data ws"), "{why}");
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

/// **The hourly funding rate of an `activeAssetCtx` frame RECORDED on 2026-11-08** (the public socket, no
/// key): `ctx.funding` is the rate in force, the settlement is the next whole hour after the observation,
/// and the price it is charged on is the ORACLE price, not the mark. Another channel, or a frame whose
/// rate is missing, states nothing; a broken oracle price leaves the price out (the mark then decides).
#[test]
fn funding_from_a_recorded_active_asset_ctx_frame() {
    let recorded: Value = serde_json::from_str(
        r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"funding":"0.0000125","openInterest":"41717.9924","prevDayPx":"75740.0","dayNtlVlm":"457112613.834290266","premium":"-0.0003154781","oraclePx":"76075.0","markPx":"76050.0","midPx":"76050.5","impactPxs":["76050.0","76051.0"],"dayBaseVlm":"6028.04017"}}}"#,
    )
    .unwrap();
    let observed = 1_794_153_423_000; // 15:57:03 UTC
    let f = funding_from_frame(&recorded, "BTC", observed).expect("the frame states a rate");
    assert_eq!(
        f,
        vike_marketdata::FundingRate {
            timestamp: observed,
            symbol: "BTC".to_string(),
            rate: 0.0000125,
            interval_ms: Some(3_600_000),
            next_timestamp: Some(1_794_153_600_000), // 16:00 UTC
            predicted: None,
            price: Some(76_075.0),
        }
    );
    // An observation exactly ON the hour belongs to the NEXT settlement (the one at the hour is paid).
    let on_the_hour = funding_from_frame(&recorded, "BTC", 1_794_153_600_000).unwrap();
    assert_eq!(on_the_hour.next_timestamp, Some(1_794_157_200_000));
    let candle: Value = serde_json::from_str(r#"{"channel":"candle","data":{"t":1}}"#).unwrap();
    assert_eq!(funding_from_frame(&candle, "BTC", observed), None);
    let no_rate: Value = serde_json::from_str(
        r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"markPx":"1.0"}}}"#,
    )
    .unwrap();
    assert_eq!(funding_from_frame(&no_rate, "BTC", observed), None);
    let bad_oracle: Value = serde_json::from_str(
        r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"funding":"0.0001","oraclePx":"0"}}}"#,
    )
    .unwrap();
    assert_eq!(funding_from_frame(&bad_oracle, "BTC", observed).unwrap().price, None);
}

/// A non-`activeAssetCtx` channel, a missing ctx/markPx, and a dead (0/neg/NaN) mark all yield
/// `None` — never a bogus mark into the `PriceBoard` mark slot.
#[test]
fn mark_from_frame_rejects_wrong_channel_and_dead_marks() {
    let candle: Value =
        serde_json::from_str(r#"{"channel":"candle","data":{"t":1,"c":"5"}}"#).unwrap();
    assert!(mark_from_frame(&candle).is_none(), "wrong channel");

    let no_context: Value =
        serde_json::from_str(r#"{"channel":"activeAssetCtx","data":{"coin":"BTC"}}"#).unwrap();
    assert!(mark_from_frame(&no_context).is_none(), "missing ctx");

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

/// The four coin-keyed channels subscribe with ONE frame shape (`coin_subscribe_message`), each naming
/// its own type, and nothing else rides in it.
#[test]
fn the_coin_keyed_channels_subscribe_with_one_frame() {
    let parsed = |f: String| serde_json::from_str::<Value>(&f).unwrap();
    let want = |kind: &str| serde_json::json!({"method": "subscribe", "subscription": {"type": kind, "coin": "X"}});
    assert_eq!(parsed(subscribe_bbo_message("X")), want("bbo"));
    assert_eq!(parsed(subscribe_trades_message("X")), want("trades"));
    assert_eq!(parsed(subscribe_l2book_message("X")), want("l2Book"));
    assert_eq!(parsed(subscribe_active_asset_ctx_msg("X")), want("activeAssetCtx"));
}

/// **The background symbology's ladder, pinned before it moved to the shared ladder (`vike_utility::retry`)**: three tries
/// of the two reads (`meta` then `spotMeta`, both asked on every try), 5 s apart and no pause after the last,
/// one `warn` on `vike_hyperliquid` per failed try (the last included) naming the try and the first read's
/// error, and then the thread ends with the cell still empty. Every read is refused with a 400, which `/info`
/// never asks again. The lines are said on the symbology's own thread, so the process-wide capture reads them.
#[cfg(feature = "execution")]
#[test]
fn the_background_symbology_tries_three_times_five_seconds_apart_then_gives_up() {
    use std::time::{Duration, Instant};
    const REFUSED: &str =
        "HTTP/1.1 400 Bad Request\r\ncontent-length: 3\r\nconnection: close\r\n\r\nbad";
    const WORDS: &str =
        "the feed's spot symbology could not be read; a spot pair is priced only once it is";
    let journal = || -> Vec<(tracing::Level, String, String)> {
        vike_log::capture::global("vike_hyperliquid")
            .into_iter()
            .filter(|line| line.message == WORDS)
            .map(|line| {
                let field = |name: &str| line.field(name).unwrap_or_default().to_string();
                (line.level, field("attempt"), field("why"))
            })
            .collect()
    };
    assert!(journal().is_empty(), "the capture is installed before the thread exists");
    let (once, _) = crate::test_support::info_server(|_| REFUSED);
    let refused = crate::transport::HyperliquidTransport::new(VenueNetwork::Testnet, &once)
        .info(&serde_json::json!({ "type": "meta" }))
        .expect_err("refused");
    let arrived = Arc::new(Mutex::new(Vec::new()));
    let at = Arc::clone(&arrived);
    let (rest, bodies) = crate::test_support::info_server(move |_| {
        at.lock().unwrap().push(Instant::now());
        REFUSED
    });
    let spec = DataClientSpecHyperliquid {
        network: VenueNetwork::Testnet,
        ws: String::new(),
        rest,
        symbology: Arc::default(),
    };
    let cell = spec.symbology_cell();
    let started = Instant::now();
    let _spec = spec.with_background_symbology();
    // the thread holds a third handle on the cell (beside the spec's and this one) until it ends
    while Arc::strong_count(&cell) > 2 {
        assert!(started.elapsed() < Duration::from_secs(60), "the symbology thread never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(cell.lock().unwrap().is_none(), "nothing was read, so nothing is resolved");
    let kinds: Vec<String> = bodies
        .lock()
        .unwrap()
        .iter()
        .map(|body| {
            serde_json::from_str::<Value>(body).unwrap()["type"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(kinds, ["meta", "spotMeta", "meta", "spotMeta", "meta", "spotMeta"]);
    let at = arrived.lock().unwrap().clone();
    let five = Duration::from_secs(5);
    assert!(at[2] - at[1] >= five && at[4] - at[3] >= five, "5 s between tries: {at:?}");
    assert!(at[1] - at[0] < five && at[3] - at[2] < five && at[5] - at[4] < five, "{at:?}");
    let why = format!("{:?}", Some(refused.to_string()));
    let failed = |attempt: &str| (tracing::Level::WARN, attempt.to_string(), why.clone());
    assert_eq!(journal(), [failed("1"), failed("2"), failed("3")]);
}
