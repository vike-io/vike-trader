use super::{
    FeedCtx, Feeds, KlineEvent, MarkEvent, mark_subscribe_frame, okx_depth_decode, parse_trades,
    route_frame, route_mark_frame, subscribe_frame, trades_subscribe_frame,
};
use crate::book_checksum::ChecksumBook;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use vike_bridge_core::SubscribeAck;
use vike_bridge_core::depth::BookOp;
use vike_data::DataClient;

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

/// A network-free stand-in for [`super::feed_main`]: just polls its own stop flag, same
/// cadence as the real feed's WS loop, without ever touching a socket.
fn fake_feed_body(_inst: String, _interval: String, ctx: FeedCtx) {
    while !ctx.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn subscribe_returns_distinct_ids() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn_with("BTC-USDT", "1m", fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("ETH-USDT", "1m", fake_feed_body).expect("spawn ok");
    assert_ne!(id1, id2, "distinct ids per subscribe");
    feeds.shutdown();
}

#[test]
fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn_with("BTC-USDT", "1m", fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("ETH-USDT", "1m", fake_feed_body).expect("spawn ok");

    feeds.unsubscribe(id1);
    assert_eq!(feeds.registry.len(), 1, "only the unsubscribed stream is removed");
    assert!(feeds.registry.contains(id2), "the other subscription keeps running");

    feeds.shutdown();
    assert!(feeds.registry.is_empty());
}

#[test]
fn shutdown_joins_every_feed() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.spawn_with("BTC-USDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.spawn_with("ETH-USDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.shutdown();
    assert!(feeds.registry.is_empty());
}

/// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `*-SWAP` pairs a mark stream,
/// and only while the knob is on. Spot has no mark price; dated futures are deliberately out
/// of scope (see [`super::should_pair_mark`]).
#[test]
fn only_swaps_pair_a_mark_stream_and_only_while_the_knob_is_on() {
    assert!(super::should_pair_mark("BTC-USDT-SWAP", true));
    assert!(!super::should_pair_mark("BTC-USDT", true), "spot has no mark price");
    assert!(!super::should_pair_mark("BTC-USD-240329", true), "dated futures are out of scope");
    assert!(
        !super::should_pair_mark("BTC-USDT-SWAP", false),
        "a `mark_streams = 0` row suppresses"
    );
}

/// SPAWN side, network-free: `pair_mark_stream` is the production path `try_spawn` calls (only
/// `body` differs here). A SWAP spawns a companion `mark-price` stream; unsubscribing the bars
/// id stops BOTH, while unrelated subscriptions keep running.
#[test]
fn a_swap_bars_subscription_spawns_and_then_stops_its_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let bars_id = feeds.spawn_with("BTC-USDT-SWAP", "1m", fake_feed_body).expect("spawn ok");
    let other_id = feeds.spawn_with("ETH-USDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTC-USDT-SWAP", fake_feed_body);
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
    let bars_id = feeds.spawn_with("BTC-USDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTC-USDT", fake_feed_body);
    assert_eq!(feeds.registry.len(), 1, "no companion stream for spot");
    feeds.shutdown();
}

/// A `mark_streams = 0` row suppresses the spawn even for a SWAP — the knob's whole job, pinned.
#[test]
fn the_mark_streams_knob_off_suppresses_the_swap_spawn() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.mark_streams = false;
    let bars_id = feeds.spawn_with("BTC-USDT-SWAP", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTC-USDT-SWAP", fake_feed_body);
    assert_eq!(feeds.registry.len(), 1, "knob off -> no mark stream even for a SWAP");
    feeds.shutdown();
}

/// The default this feed runs and the one `config show` prints are ONE fact: the charter constant
/// is `venue.okx.mark_streams`'s declared default.
#[test]
fn the_charter_default_is_the_declared_fields() {
    let declared =
        vike_model::venue_fields::venue_field("okx", "mark_streams").expect("declared").default;
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

/// Per-symbol dedupe: a 1m AND a 5m chart on the same SWAP share ONE mark socket.
#[test]
fn two_intervals_on_one_swap_share_a_single_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let bars_1m = feeds.spawn_with("BTC-USDT-SWAP", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_1m, "BTC-USDT-SWAP", fake_feed_body);
    let bars_5m = feeds.spawn_with("BTC-USDT-SWAP", "5m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_5m, "BTC-USDT-SWAP", fake_feed_body);
    assert_eq!(feeds.registry.len(), 3, "two bars feeds but only ONE mark stream");

    feeds.unsubscribe(bars_1m);
    assert_eq!(feeds.registry.len(), 2, "the mark stream the 5m chart still needs stays up");
    feeds.unsubscribe(bars_5m);
    assert!(feeds.registry.is_empty(), "the last release stops the mark stream too");
    feeds.shutdown();
}

#[test]
fn quotes_and_book_are_unsupported() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    assert!(matches!(
        feeds.subscribe_quotes("BTC-USDT"),
        Err(vike_data::LiveDataError::Unsupported(_))
    ));
    assert!(matches!(
        feeds.subscribe_book("BTC-USDT"),
        Err(vike_data::LiveDataError::Unsupported(_))
    ));
    // subscribe_trades is deliberately NOT asserted here — Task okxtrades wired it to a real
    // live feed (mirrors binance's subscribe_bars/subscribe_depth, which also aren't exercised
    // via a real subscribe call in unit tests since that would touch the network); its pure
    // parser (`parse_trades`) and subscribe-frame builder are covered below instead.
}

fn candle_frame(row: serde_json::Value) -> String {
    serde_json::json!({
        "arg": {"channel": "candle1m", "instId": "BTC-USDT"},
        "data": [row]
    })
    .to_string()
}

/// A `confirm="1"` candle → a CLOSED bar with exact o/h/l/c/v bit patterns + ts.
#[test]
fn confirmed_candle_classifies_closed_with_exact_bits() {
    // [ts, o, h, l, c, vol, volCcy, volCcyQuote, confirm]
    let frame = candle_frame(serde_json::json!([
        "1700000060000",
        "27010.25",
        "27100.00",
        "27000.00",
        "27080.10",
        "8.10000000",
        "219000.00",
        "5913000.00",
        "1"
    ]));
    match route_frame(&frame) {
        KlineEvent::Closed(b) => {
            assert_eq!(b.ts, 1_700_000_060_000);
            assert_eq!(b.open.to_bits(), 27010.25_f64.to_bits());
            assert_eq!(b.high.to_bits(), 27100.00_f64.to_bits());
            assert_eq!(b.low.to_bits(), 27000.00_f64.to_bits());
            assert_eq!(b.close.to_bits(), 27080.10_f64.to_bits());
            assert_eq!(b.volume.to_bits(), 8.1_f64.to_bits());
            assert!(b.symbol.is_none() && b.funding.is_none() && b.bid.is_none());
        }
        other => panic!("expected Closed, got {other:?}"),
    }
}

/// A `confirm="0"` candle is a still-forming bar (the `forming_bar` sink lane), not a close.
#[test]
fn unconfirmed_candle_classifies_forming() {
    let frame = candle_frame(serde_json::json!([
        "1700000120000",
        "27080.10",
        "27090.00",
        "27080.00",
        "27085.00",
        "1.5",
        "40000.0",
        "40000.0",
        "0"
    ]));
    match route_frame(&frame) {
        KlineEvent::Forming(b) => {
            assert_eq!(b.ts, 1_700_000_120_000);
            assert_eq!(b.close.to_bits(), 27085.00_f64.to_bits());
            assert_eq!(b.open.to_bits(), 27080.10_f64.to_bits());
        }
        other => panic!("expected Forming, got {other:?}"),
    }
}

/// Net-hardening br7: the OKX subscribe ACK (`event:"subscribe"`) now CONFIRMS the handshake
/// (`KlineEvent::Ack`) instead of being silently dropped — it is what disarms `run_live`'s
/// subscribe-ack watchdog. A bare-text `pong` and an unrelated `event` stay Ignored.
#[test]
fn subscribe_ack_is_classified_as_ack_pong_and_junk_ignored() {
    // OKX subscribe ack: has arg.channel but NO data → an Ack (confirms the subscribe)
    assert_eq!(
        route_frame(r#"{"event":"subscribe","arg":{"channel":"candle1m","instId":"BTC-USDT"}}"#),
        KlineEvent::Ack
    );
    // an unrelated event envelope (e.g. unsubscribe) is neither an error nor a confirm
    assert_eq!(
        route_frame(r#"{"event":"unsubscribe","arg":{"channel":"candle1m"}}"#),
        KlineEvent::Ignored
    );
    // bare-text keepalive is not JSON
    assert_eq!(route_frame("pong"), KlineEvent::Ignored);
}

/// Net-hardening br7 anchor (was `subscribe_ack_pong_and_junk_are_ignored` asserting Ignored):
/// an OKX `event:"error"` subscribe reject is now an ATTRIBUTABLE `KlineEvent::Error` carrying
/// the venue's `code`+`msg`, NOT silently dropped. `run_live` returns this as the session error.
#[test]
fn error_frame_is_attributable_not_ignored() {
    match route_frame(r#"{"event":"error","code":"60012","msg":"Invalid request: channel"}"#) {
        KlineEvent::Error(m) => {
            assert!(m.contains("60012"), "carries the OKX error code: {m}");
            assert!(m.contains("Invalid request: channel"), "carries the OKX message: {m}");
        }
        other => panic!("expected an attributable Error, got {other:?}"),
    }
}

/// Net-hardening br7 (scripted-clock, deterministic): a subscribe that never acks and never
/// delivers data trips the `SubscribeAck` watchdog once this venue's declared ack window (the
/// `MarketPumpSpec` row `pump_opts` consumes) elapses — the attributable "no ack/data" error
/// the shared driver returns. An ack (or first data) within the window disarms it. Same
/// clock-free harness `SubscribeAck`'s own unit tests use, but pinned to the venue's real row.
#[test]
fn a_missing_ack_within_the_window_becomes_an_attributable_error() {
    let sub_ack_timeout = vike_bridge_core::pump_spec::market_pump_spec(super::VENUE)
        .knobs()
        .ack_timeout
        .expect("okx declares the br7 ack watchdog");
    let timeout_ms = sub_ack_timeout.as_millis() as i64;
    let armed_at = 1_000; // scripted "subscribe sent" wall-clock ms
    let ack = SubscribeAck::new(armed_at, timeout_ms);
    assert!(!ack.overdue(armed_at + timeout_ms), "at the deadline, not yet overdue (strict >)");
    assert!(
        ack.overdue(armed_at + timeout_ms + 1),
        "one ms past the {}s window with no ack/data → attributable error",
        sub_ack_timeout.as_secs()
    );
    // the ACK path (route_frame → Ack → confirm) disarms it: no trip however far the clock runs
    let mut acked = SubscribeAck::new(armed_at, timeout_ms);
    assert_eq!(
        route_frame(r#"{"event":"subscribe","arg":{"channel":"candle1m"}}"#),
        KlineEvent::Ack
    );
    acked.confirm();
    assert!(!acked.overdue(armed_at + timeout_ms * 100), "a confirmed subscribe never trips");
}

#[test]
fn subscribe_frame_targets_the_candle_channel() {
    let f = subscribe_frame("1H", "BTC-USDT");
    assert!(f.contains("\"channel\":\"candle1H\""), "channel wrong: {f}");
    assert!(f.contains("\"instId\":\"BTC-USDT\""));
    assert!(f.contains("\"op\":\"subscribe\""));
}

// ---- OKX `trades` channel: parse_trades (Task okxtrades) ----

#[test]
fn trades_subscribe_frame_targets_the_trades_channel() {
    let f = trades_subscribe_frame("BTC-USDT");
    assert!(f.contains("\"channel\":\"trades\""), "channel wrong: {f}");
    assert!(f.contains("\"instId\":\"BTC-USDT\""));
    assert!(f.contains("\"op\":\"subscribe\""));
}

/// A `trades` push with a buy row and a sell row -> 2 `TradeTick`s with exact price/size/ts
/// bit patterns, and `is_buyer_maker` following Binance's `m` convention (true exactly when
/// the buyer was the MAKER, i.e. the taker/aggressor SOLD): `side=="buy"` -> false,
/// `side=="sell"` -> true.
#[test]
fn parse_trades_maps_buy_and_sell_rows_with_exact_bits() {
    let payload = serde_json::json!({
        "arg": {"channel": "trades", "instId": "BTC-USDT"},
        "data": [
            {
                "instId": "BTC-USDT",
                "tradeId": "130639474",
                "px": "64500.1",
                "sz": "0.012",
                "side": "buy",
                "ts": "1710000000000"
            },
            {
                "instId": "BTC-USDT",
                "tradeId": "130639475",
                "px": "64499.9",
                "sz": "0.5",
                "side": "sell",
                "ts": "1710000000123"
            }
        ]
    });
    let ticks = parse_trades(&payload);
    assert_eq!(ticks.len(), 2);

    assert_eq!(ticks[0].ts, 1_710_000_000_000);
    assert_eq!(ticks[0].price.to_bits(), 64500.1_f64.to_bits());
    assert_eq!(ticks[0].size.to_bits(), 0.012_f64.to_bits());
    assert!(!ticks[0].is_buyer_maker, "buy taker -> buyer is NOT the maker");
    assert_eq!(ticks[0].symbol, "BTC-USDT");
    assert_eq!(ticks[0].local_ts, 0, "the pure mapper leaves local_ts unstamped");

    assert_eq!(ticks[1].ts, 1_710_000_000_123);
    assert_eq!(ticks[1].price.to_bits(), 64499.9_f64.to_bits());
    assert_eq!(ticks[1].size.to_bits(), 0.5_f64.to_bits());
    assert!(ticks[1].is_buyer_maker, "sell taker -> buyer WAS the maker");
}

/// Non-`trades` pushes (a candle subscribe ack, and an actual candle data push) yield an
/// empty vec — `parse_trades` must not misclassify another channel's frame.
#[test]
fn parse_trades_ignores_non_trades_pushes() {
    let candle_ack = serde_json::json!({
        "event": "subscribe",
        "arg": {"channel": "candle1m", "instId": "BTC-USDT"}
    });
    assert!(parse_trades(&candle_ack).is_empty());

    let candle_push = serde_json::json!({
        "arg": {"channel": "candle1m", "instId": "BTC-USDT"},
        "data": [["1700000060000", "1", "1", "1", "1", "1", "1", "1", "1"]]
    });
    assert!(parse_trades(&candle_push).is_empty());

    assert!(parse_trades(&serde_json::json!({})).is_empty());
}

/// A malformed/unparseable row (bad price, zero size, unrecognized side, missing field) is
/// skipped in place — never panics, never produces a bogus tick.
#[test]
fn parse_trades_skips_malformed_rows_without_panicking() {
    let payload = serde_json::json!({
        "arg": {"channel": "trades", "instId": "BTC-USDT"},
        "data": [
            {"instId": "BTC-USDT", "px": "not-a-number", "sz": "0.012", "side": "buy", "ts": "1710000000000"},
            {"instId": "BTC-USDT", "px": "1.0", "sz": "0", "side": "buy", "ts": "1710000000000"},
            {"instId": "BTC-USDT", "px": "1.0", "sz": "1.0", "side": "unknown", "ts": "1710000000000"},
            {"instId": "BTC-USDT", "px": "1.0", "sz": "1.0", "side": "buy"},
            {"px": "1.0", "sz": "1.0", "side": "buy", "ts": "1710000000000"},
            "not even an object"
        ]
    });
    assert!(parse_trades(&payload).is_empty());
}

/// One bad row alongside a good one: the batch is NOT failed wholesale (mirrors
/// `vike_binance::trades::rest_agg_trades`'s per-element tolerance).
#[test]
fn parse_trades_keeps_good_rows_alongside_a_bad_one() {
    let payload = serde_json::json!({
        "arg": {"channel": "trades", "instId": "BTC-USDT"},
        "data": [
            {"instId": "BTC-USDT", "px": "bad", "sz": "1.0", "side": "buy", "ts": "1"},
            {"instId": "BTC-USDT", "px": "100.0", "sz": "1.0", "side": "buy", "ts": "2"}
        ]
    });
    let ticks = parse_trades(&payload);
    assert_eq!(ticks.len(), 1);
    assert_eq!(ticks[0].ts, 2);
}

// ---- OKX `books` CRC32 checksum validation (audit br1), scripted-frame style ----

/// A `[px, sz, "0", "1"]` level array (OKX's `books` shape; only `[0]`/`[1]` are read).
fn depth_levels(ls: &[(&str, &str)]) -> Vec<serde_json::Value> {
    ls.iter().map(|(p, s)| serde_json::json!([p, s, "0", "1"])).collect()
}

/// A scripted OKX `books` frame with an explicit `seqId`/`prevSeqId`/`checksum`.
fn books_frame(
    action: &str,
    seq: u64,
    prev_seq: i64,
    bids: &[(&str, &str)],
    asks: &[(&str, &str)],
    checksum: i32,
) -> String {
    serde_json::json!({
        "arg": {"channel": "books", "instId": "BTC-USDT-SWAP"},
        "action": action,
        "data": [{
            "bids": depth_levels(bids),
            "asks": depth_levels(asks),
            "ts": "1700000000000",
            "seqId": seq,
            "prevSeqId": prev_seq,
            "checksum": checksum,
        }]
    })
    .to_string()
}

fn owned(ls: &[(&str, &str)]) -> Vec<(String, String)> {
    ls.iter().map(|(p, s)| (p.to_string(), s.to_string())).collect()
}

/// The correct OKX checksum for a merged book of these already-sorted levels (bids high→low,
/// asks low→high) — computed through the same `ChecksumBook` the decoder uses.
fn merged_checksum(bids: &[(&str, &str)], asks: &[(&str, &str)]) -> i32 {
    let mut b = ChecksumBook::new();
    b.apply_snapshot(&owned(bids), &owned(asks));
    b.computed_checksum()
}

/// Happy path: a snapshot then an update, each carrying the CORRECT checksum of the merged book,
/// both decode to `Updated` (no gap). The update ships only the CHANGED levels but its checksum is
/// over the merged top-of-book — proving the decoder validates the MERGED book, not the delta.
#[test]
fn valid_snapshot_and_update_checksums_keep_the_book() {
    let snap_bids = [("100.0", "5"), ("99.0", "3")];
    let snap_asks = [("101.0", "4"), ("102.0", "2")];
    let snap = books_frame(
        "snapshot",
        10,
        -1,
        &snap_bids,
        &snap_asks,
        merged_checksum(&snap_bids, &snap_asks),
    );

    let mut cbook = ChecksumBook::new();
    let mut book: Option<vike_model::L2Book> = None;
    assert_eq!(
        okx_depth_decode(&mut cbook, &snap, &mut book, "BTC-USDT-SWAP"),
        BookOp::Updated(1_700_000_000_000)
    );

    // update: best bid 5→8, add a 99.5 bid. Checksum is over the MERGED book {100.0:8,99.5:1,99.0:3}.
    let merged_bids = [("100.0", "8"), ("99.5", "1"), ("99.0", "3")];
    let upd_cs = merged_checksum(&merged_bids, &snap_asks);
    let upd = books_frame("update", 11, 10, &[("100.0", "8"), ("99.5", "1")], &[], upd_cs);
    assert_eq!(
        okx_depth_decode(&mut cbook, &upd, &mut book, "BTC-USDT-SWAP"),
        BookOp::Updated(1_700_000_000_000)
    );
}

/// A structurally-valid update (seq chain intact) whose checksum DISAGREES with the merged book →
/// `BookOp::Gap`, which the shared driver turns into a reconnect+re-seed and a `GapStart`
/// disclosure. This is the silent-corruption class the `prevSeqId` chain cannot catch.
#[test]
fn a_bad_update_checksum_triggers_a_resync_gap() {
    let bids = [("100.0", "5")];
    let asks = [("101.0", "4")];
    let snap = books_frame("snapshot", 10, -1, &bids, &asks, merged_checksum(&bids, &asks));
    let mut cbook = ChecksumBook::new();
    let mut book: Option<vike_model::L2Book> = None;
    assert!(matches!(okx_depth_decode(&mut cbook, &snap, &mut book, "X"), BookOp::Updated(_)));

    let real = merged_checksum(&[("100.0", "7")], &[("101.0", "4")]);
    let wrong = 424_242; // a non-zero value that is not the real checksum
    assert_ne!(wrong, real, "the fixed wrong checksum must differ from the real one");
    let upd = books_frame("update", 11, 10, &[("100.0", "7")], &[], wrong);
    assert_eq!(okx_depth_decode(&mut cbook, &upd, &mut book, "X"), BookOp::Gap);
}

/// A corrupted SNAPSHOT (wrong checksum) is also caught → `Gap`.
#[test]
fn a_bad_snapshot_checksum_triggers_a_resync_gap() {
    let bids = [("100.0", "5"), ("99.0", "3")];
    let asks = [("101.0", "4")];
    let real = merged_checksum(&bids, &asks);
    let wrong = 424_242;
    assert_ne!(wrong, real);
    let snap = books_frame("snapshot", 10, -1, &bids, &asks, wrong);
    let mut cbook = ChecksumBook::new();
    let mut book: Option<vike_model::L2Book> = None;
    assert_eq!(okx_depth_decode(&mut cbook, &snap, &mut book, "X"), BookOp::Gap);
}

/// OKX deprecated the field on 2026-06-23 (fixed to `0`). A `0` checksum must be SKIPPED, never
/// validated — so a live OKX stream (which always sends 0) never false-trips a resync, even though
/// the real book checksum is non-zero.
#[test]
fn a_zero_checksum_is_skipped_so_live_okx_never_false_trips() {
    let bids = [("100.0", "5")];
    let asks = [("101.0", "4")];
    assert_ne!(merged_checksum(&bids, &asks), 0, "sanity: the real checksum is non-zero");
    let snap = books_frame("snapshot", 10, -1, &bids, &asks, 0);
    let mut cbook = ChecksumBook::new();
    let mut book: Option<vike_model::L2Book> = None;
    assert!(
        matches!(okx_depth_decode(&mut cbook, &snap, &mut book, "X"), BookOp::Updated(_)),
        "a 0 checksum is skipped, not validated"
    );
    let upd = books_frame("update", 11, 10, &[("100.0", "9")], &[], 0);
    assert!(matches!(okx_depth_decode(&mut cbook, &upd, &mut book, "X"), BookOp::Updated(_)));
}

/// A `prevSeqId` break is a `Gap` on the sequence chain regardless of checksum (the existing
/// resync path is unchanged; the checksum layer is additive).
#[test]
fn a_prev_seq_break_still_gaps() {
    let bids = [("100.0", "5")];
    let asks = [("101.0", "4")];
    let snap = books_frame("snapshot", 10, -1, &bids, &asks, merged_checksum(&bids, &asks));
    let mut cbook = ChecksumBook::new();
    let mut book: Option<vike_model::L2Book> = None;
    assert!(matches!(okx_depth_decode(&mut cbook, &snap, &mut book, "X"), BookOp::Updated(_)));
    // prevSeqId 99 != last seqId 10 → seq gap (checksum 0 here is irrelevant — the chain breaks first)
    let upd = books_frame("update", 11, 99, &[("100.0", "6")], &[], 0);
    assert_eq!(okx_depth_decode(&mut cbook, &upd, &mut book, "X"), BookOp::Gap);
}

/// An `update` before any `snapshot` is ignored (no book yet) and does not touch the mirror.
#[test]
fn an_update_before_the_snapshot_is_ignored() {
    let mut cbook = ChecksumBook::new();
    let mut book: Option<vike_model::L2Book> = None;
    let upd = books_frame("update", 5, 4, &[("100.0", "1")], &[], 999);
    assert_eq!(okx_depth_decode(&mut cbook, &upd, &mut book, "X"), BookOp::Ignored);
}

// ---- OKX `mark-price` channel (mark-slot semantics, W2-T4), scripted-frame style ----

/// A real-shaped `mark-price` data push → `MarkEvent::Mark` with the exact `markPx` bits and
/// the row's `ts` (both decimal strings on the wire).
#[test]
fn mark_price_frame_decodes_px_and_ts() {
    let frame = serde_json::json!({
            "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"},
            "data": [{"instType": "SWAP", "instId": "BTC-USDT-SWAP", "markPx": "27123.45", "ts": "1700000000123"}]
        })
        .to_string();
    match route_mark_frame(&frame) {
        MarkEvent::Mark { px, ts } => {
            assert_eq!(px.to_bits(), 27_123.45_f64.to_bits(), "markPx, not lastPx");
            assert_eq!(ts, 1_700_000_000_123);
        }
        other => panic!("expected Mark, got {other:?}"),
    }
}

/// The subscribe ACK envelope is classified `Ack`; the venue's error envelope is an
/// attributable `Error` carrying code+msg (drives a `Fatal` in the pump).
#[test]
fn mark_ack_and_error_envelopes_classify() {
    let ack = serde_json::json!({
        "event": "subscribe",
        "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"}
    })
    .to_string();
    assert_eq!(route_mark_frame(&ack), MarkEvent::Ack);

    let err = serde_json::json!({
        "event": "error", "code": "60012", "msg": "Invalid request"
    })
    .to_string();
    match route_mark_frame(&err) {
        MarkEvent::Error(m) => assert!(m.contains("60012") && m.contains("Invalid request")),
        other => panic!("expected Error, got {other:?}"),
    }
}

/// A non-`mark-price` push, a dead (0/neg/NaN) mark, a malformed row, and the bare-text
/// keepalive are all `Ignored` — never a bogus mark into the `PriceBoard` mark slot.
#[test]
fn mark_non_channel_dead_and_malformed_frames_are_ignored() {
    let candle = serde_json::json!({
        "arg": {"channel": "candle1m", "instId": "BTC-USDT-SWAP"},
        "data": [["1", "1", "1", "1", "1", "1", "1", "1", "1"]]
    })
    .to_string();
    assert_eq!(route_mark_frame(&candle), MarkEvent::Ignored);

    for dead in ["0", "-1.0", "not-a-number"] {
        let f = serde_json::json!({
            "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"},
            "data": [{"markPx": dead, "ts": "1"}]
        })
        .to_string();
        assert_eq!(route_mark_frame(&f), MarkEvent::Ignored, "dead mark {dead} dropped");
    }

    // empty data array, and the bare-text "pong" keepalive
    let empty = serde_json::json!({
        "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"}, "data": []
    })
    .to_string();
    assert_eq!(route_mark_frame(&empty), MarkEvent::Ignored);
    assert_eq!(route_mark_frame("pong"), MarkEvent::Ignored);
}

/// The subscribe frame targets the public `mark-price` channel on the given inst — the exact
/// `{"op":"subscribe","args":[{"channel":"mark-price","instId":...}]}` wire grammar.
#[test]
fn mark_subscribe_frame_targets_the_mark_price_channel() {
    let v: serde_json::Value =
        serde_json::from_str(&mark_subscribe_frame("BTC-USDT-SWAP")).unwrap();
    assert_eq!(v["op"], "subscribe");
    assert_eq!(v["args"][0]["channel"], "mark-price");
    assert_eq!(v["args"][0]["instId"], "BTC-USDT-SWAP");
}
