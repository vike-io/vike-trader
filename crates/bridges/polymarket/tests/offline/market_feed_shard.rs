//! Scripted (network-free) coverage of **WS batching** — `market_feed::run_shard_session`, the
//! session body that carries K tokens on ONE socket (`market_feed`'s module doc). The sibling
//! `market_feed_scripted.rs` pins the one-token pump; this file pins what batching adds:
//!
//! 1. **Routing.** Frames for token A and token B arrive interleaved on one socket and must land in
//!    the right per-token state — proven from the sink's own per-token call lines, not from any
//!    ambient socket-level context.
//! 2. **Per-token seq isolation.** Polymarket carries no venue sequence number, so the pump keeps a
//!    feed-LOCAL monotonic `seq` per `TokenState`. A busy co-tenant must never bump a quiet token's
//!    seq — the invariant a naive shared counter would break, and the one a replay of the recorded
//!    `book_update` stream depends on.
//! 3. **Per-token health.** The §B disclosures (`GapStart`/`Live`/`Stale`) are keyed by token and
//!    driven by that token's own `StreamHealth`, so one token's staleness is not another's.
//! 4. **The batched subscribe frame** — `{"assets_ids":[a,b,c],"type":"market"}`, replayed verbatim
//!    at the start of every session (the driver's reconnect-==-resubscribe rule).
//! 5. **K = 1 equivalence** — a one-seat shard emits byte-identically to the pre-batching pump, run
//!    through both seams over the same script.
//!
//! `tick_size = 1.0` throughout, for the same exact-quantization reason `market_feed_scripted.rs`
//! documents.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use vike_bridge_core::scripted::ScriptedStream;
use vike_bridge_core::{HealthEvent, StreamHealth};
use vike_data::{LiveDataSink, RecordingSink, StreamStatus};
use vike_polymarket::{
    run_session, run_shard_session, subscribe_message, MarketStream, PumpMode, PumpTiming,
    TokenSlot, TokenState, Watchdog,
};

const A: &str = "aaa";
const B: &str = "bbb";
/// Mirrors `market_feed_scripted.rs`'s scripted Book/Quotes threshold (60 s) — see its doc for why
/// the tests deliberately do not track the 300 s production value.
const FRESHNESS_MS: i64 = 60_000;

fn slot(token: &str) -> TokenSlot {
    TokenSlot::new(token, 1.0, FRESHNESS_MS)
}

/// Timing that keeps both watchdogs out of the way (30 s idle ≫ `ScriptedStream`'s ZERO stall, a
/// huge ping interval), the same shape `market_feed_scripted.rs`'s `freshness_watchdog` uses.
fn timing(label: &str) -> PumpTiming<'_> {
    PumpTiming::with_timing(label, Duration::from_secs(30), Duration::from_secs(3600))
}

fn run_shard<S: MarketStream>(
    stream: &mut S,
    sink: &dyn LiveDataSink,
    mode: PumpMode,
    slots: &mut [TokenSlot],
) -> Result<(), String> {
    let stop = AtomicBool::new(false);
    run_shard_session(stream, sink, mode, slots, &stop, &timing("book"), || {})
}

fn book_frame(token: &str, bids: &[(&str, &str)], asks: &[(&str, &str)]) -> serde_json::Value {
    let levels = |arr: &[(&str, &str)]| -> serde_json::Value {
        serde_json::Value::Array(
            arr.iter().map(|(p, s)| serde_json::json!({"price": p, "size": s})).collect(),
        )
    };
    serde_json::json!({
        "event_type": "book", "asset_id": token,
        "bids": levels(bids), "asks": levels(asks)
    })
}

/// The real wire shape: ONE `price_change` frame whose plural `price_changes` array carries entries
/// for possibly SEVERAL assets — exactly the frame a batched socket receives.
fn price_change_frame(entries: &[(&str, &str, &str, &str)]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = entries
        .iter()
        .map(|(asset_id, p, s, side)| {
            serde_json::json!({"asset_id": asset_id, "price": p, "size": s, "side": side})
        })
        .collect();
    serde_json::json!({"event_type": "price_change", "price_changes": arr})
}

fn last_trade_frame(token: &str, price: &str, size: &str, side: &str) -> serde_json::Value {
    serde_json::json!({
        "event_type": "last_trade_price", "asset_id": token,
        "price": price, "size": size, "side": side
    })
}

/// (1) The batched subscribe frame: ONE socket subscribes its WHOLE token set, in seat order.
#[test]
fn a_shard_subscribes_its_whole_token_set() {
    let mut stream = ScriptedStream::from_json([book_frame(A, &[("51", "100")], &[("55", "40")])]);
    let sink = RecordingSink::default();
    let mut slots = [slot(A), slot(B)];
    let _ = run_shard(&mut stream, &sink, PumpMode::Book, &mut slots);

    assert_eq!(
        stream.sent(),
        vec![subscribe_message(&[A.to_string(), B.to_string()])],
        "one subscribe carrying both assets_ids, in seat order"
    );
}

/// (1) Routing: two tokens' frames interleaved on one socket land in their OWN state. Every emitted
/// call names the token whose frame produced it, and each token's book holds only its own levels.
#[test]
fn interleaved_frames_route_to_their_own_token() {
    let mut stream = ScriptedStream::from_json([
        book_frame(A, &[("51", "100")], &[("55", "40")]), // A top 51/55
        book_frame(B, &[("10", "5")], &[("12", "3")]),    // B top 10/12
        // the real multi-asset delta frame: A's ask improves AND B's bid improves in ONE frame
        price_change_frame(&[(A, "54", "90", "SELL"), (B, "11", "7", "BUY")]),
        // an asset NO seat on this socket owns — ignored, never applied to a neighbour
        book_frame("zzz", &[("1", "1")], &[("2", "1")]),
    ]);
    let sink = RecordingSink::default();
    let mut slots = [slot(A), slot(B)];
    let _ = run_shard(&mut stream, &sink, PumpMode::Quotes, &mut slots);

    assert_eq!(
        sink.calls(),
        vec![
            format!("quote:polymarket:{A}:51/55:100x40"),
            format!("quote:polymarket:{B}:10/12:5x3"),
            // the ONE multi-asset frame emits one quote per affected token, in frame order
            format!("quote:polymarket:{A}:51/54:100x90"),
            format!("quote:polymarket:{B}:11/12:7x3"),
        ],
        "each frame's updates were routed to the token named by its own asset_id"
    );

    assert_eq!(slots[0].state.book.best_ask(), Some((54.0, 90.0)), "A's ask delta hit A's book");
    assert_eq!(slots[0].state.book.best_bid(), Some((51.0, 100.0)), "…and A's bid is untouched");
    assert_eq!(slots[1].state.book.best_bid(), Some((11.0, 7.0)), "B's bid delta hit B's book");
    assert_eq!(slots[1].state.book.best_ask(), Some((12.0, 3.0)), "…and B's ask is untouched");
}

/// (2) The seq invariant: the feed-LOCAL monotonic counter lives in each `TokenState`, so a busy
/// token's frames can never advance a quiet co-tenant's chain. A carries five applied frames, B one
/// — B's single recorded `book_update` must be `seq=1`, not `seq=6`.
#[test]
fn one_tokens_frames_never_bump_another_tokens_seq() {
    let mut stream = ScriptedStream::from_json([
        book_frame(A, &[("51", "100")], &[("55", "40")]),
        price_change_frame(&[(A, "52", "10", "BUY")]),
        price_change_frame(&[(A, "53", "10", "BUY")]),
        price_change_frame(&[(A, "54", "10", "BUY")]),
        price_change_frame(&[(A, "56", "10", "SELL")]),
        book_frame(B, &[("10", "5")], &[("12", "3")]), // B's FIRST applied frame
    ]);
    let sink = RecordingSink::default();
    let mut slots = [slot(A), slot(B)];
    let _ = run_shard(&mut stream, &sink, PumpMode::Book, &mut slots);

    let updates: Vec<String> =
        sink.calls().into_iter().filter(|c| c.starts_with("book_update:")).collect();
    let a_seqs: Vec<&String> = updates.iter().filter(|c| c.contains(&format!(":{A}:"))).collect();
    let b_seqs: Vec<&String> = updates.iter().filter(|c| c.contains(&format!(":{B}:"))).collect();

    assert_eq!(a_seqs.len(), 5, "A applied five frames: {updates:?}");
    for (i, u) in a_seqs.iter().enumerate() {
        assert!(
            u.contains(&format!(":seq={}:", i + 1)),
            "A's chain is contiguous from 1: {u} (#{i})"
        );
    }
    assert_eq!(b_seqs.len(), 1, "B applied one frame: {updates:?}");
    assert!(
        b_seqs[0].contains(":seq=1:"),
        "B's first event is seq=1 — a co-tenant's five frames never touched it: {}",
        b_seqs[0]
    );
}

/// (2) Trades mode over a batched socket: every token's prints reach the sink under its OWN id, and
/// a frame for an unseated asset is dropped.
#[test]
fn trades_route_per_token_on_one_socket() {
    let mut stream = ScriptedStream::from_json([
        last_trade_frame(A, "0.52", "3", "SELL"),
        last_trade_frame("zzz", "0.99", "1", "BUY"), // unseated — dropped
        last_trade_frame(B, "0.53", "1", "BUY"),
    ]);
    let sink = RecordingSink::default();
    let mut slots = [slot(A), slot(B)];
    let stop = AtomicBool::new(false);
    let _ = run_shard_session(
        &mut stream,
        &sink,
        PumpMode::Trades,
        &mut slots,
        &stop,
        &timing("trades"),
        || {},
    );

    assert_eq!(
        sink.calls(),
        vec![
            format!("trade:polymarket:{A}:0.52/3:maker=true"),
            format!("trade:polymarket:{B}:0.53/1:maker=false"),
        ],
        "one trade per seated token, none for the unseated asset"
    );
}

/// (3) Per-token freshness: a shard whose busy token keeps the wire hot must STILL disclose the
/// quiet co-tenant's staleness. A's frame is stamped ~now, B's 90 s in the past (≫ the 60 s
/// threshold) — the next transport-alive tick discloses exactly one `Stale`, for B only.
#[test]
fn a_stale_co_tenant_is_disclosed_while_its_neighbour_stays_fresh() {
    let now = now_ms();
    let mut a_fresh = book_frame(A, &[("51", "100")], &[("55", "40")]);
    a_fresh["timestamp"] = serde_json::Value::String(now.to_string());
    let mut b_stale = book_frame(B, &[("10", "5")], &[("12", "3")]);
    b_stale["timestamp"] = serde_json::Value::String((now - 90_000).to_string());

    let mut stream = ScriptedStream::from_json([a_fresh, b_stale]);
    stream.push_timeout(); // transport-alive tick → per-slot check_freshness

    let sink = RecordingSink::default();
    let mut slots = [slot(A), slot(B)];
    let _ = run_shard(&mut stream, &sink, PumpMode::Quotes, &mut slots);

    let stale: Vec<String> = sink.calls().into_iter().filter(|c| c.contains("Stale")).collect();
    assert_eq!(stale.len(), 1, "exactly one Stale — the quiet token's: {stale:?}");
    assert!(
        stale[0].starts_with(&format!("stream_status:polymarket:{B}:")),
        "the Stale is keyed to the STALE token, not the socket: {}",
        stale[0]
    );
}

/// (3) Per-token gap/recovery: a shard reconnect opens one gap PER seated token (what `shard_main`'s
/// error arm does), and the first frame back closes every one of them — each under its own id,
/// before that frame's own data.
#[test]
fn every_seated_token_gets_its_own_gap_and_live() {
    let sink = RecordingSink::default();
    let mut slots = [slot(A), slot(B)];

    // mirror shard_main's session-error arm: last_top invalidated + a gap opened per slot
    for s in slots.iter_mut() {
        s.state.last_top = None;
        if let Some(HealthEvent::Gap { at_ts_ms }) = s.health.enter_gap(1000) {
            sink.stream_status(
                "polymarket",
                &s.token_id,
                "book",
                StreamStatus::GapStart { at_ts_ms },
            );
        }
    }

    let mut stream = ScriptedStream::from_json([book_frame(A, &[("51", "100")], &[("55", "40")])]);
    let _ = run_shard(&mut stream, &sink, PumpMode::Quotes, &mut slots);

    let calls = sink.calls();
    let want = vec![
        format!("stream_status:polymarket:{A}:book:GapStart {{ at_ts_ms: 1000 }}"),
        format!("stream_status:polymarket:{B}:book:GapStart {{ at_ts_ms: 1000 }}"),
        // the first frame back proves the shared transport is alive for BOTH tokens…
        format!("stream_status:polymarket:{A}:book:Live {{ gap_started_ts_ms: Some(1000) }}"),
        format!("stream_status:polymarket:{B}:book:Live {{ gap_started_ts_ms: Some(1000) }}"),
    ];
    assert_eq!(
        calls[..4].to_vec(),
        want,
        "one Gap and one Live per seated token, recovery before the frame's own data: {calls:?}"
    );
    assert_eq!(calls[4], format!("quote:polymarket:{A}:51/55:100x40"), "…then the data: {calls:?}");
}

/// (5) K = 1 equivalence: a ONE-seat shard and the single-token `run_session` seam emit the exact
/// same call sequence over the same script, and leave the same book state. This is the pin behind
/// "K = 1 is byte-identical" — the two seams share one body, and this proves the sharing.
#[test]
fn a_one_seat_shard_matches_the_single_token_seam_exactly() {
    let script = || {
        let mut s = ScriptedStream::from_json([
            book_frame(A, &[("51", "100")], &[("55", "40")]),
            price_change_frame(&[(A, "54", "90", "SELL")]),
            price_change_frame(&[(A, "40", "20", "BUY"), ("zzz", "1", "1", "BUY")]),
        ]);
        s.push_text("not json at all");
        s.push_text(&book_frame(A, &[("60", "10")], &[("62", "5")]).to_string());
        s
    };

    // --- the batched seam, one seat ---
    let mut shard_stream = script();
    let shard_sink = RecordingSink::default();
    let mut slots = [slot(A)];
    let _ = run_shard(&mut shard_stream, &shard_sink, PumpMode::Book, &mut slots);

    // --- the pre-batching seam ---
    let mut single_stream = script();
    let single_sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);
    let mut health = StreamHealth::new(FRESHNESS_MS);
    let mut wd = Watchdog::with_timing(
        &mut health,
        "book",
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let _ = run_session(
        &mut single_stream,
        &single_sink,
        A,
        PumpMode::Book,
        &mut state,
        &stop,
        &mut wd,
        || {},
    );

    assert_eq!(
        shard_sink.calls(),
        single_sink.calls(),
        "a one-seat shard is the pre-batching pump, emission for emission"
    );
    assert_eq!(shard_stream.sent(), single_stream.sent(), "…including the subscribe frame");
    assert_eq!(slots[0].state.book.best_bid(), state.book.best_bid());
    assert_eq!(slots[0].state.book.best_ask(), state.book.best_ask());
    assert_eq!(slots[0].state.last_top, state.last_top);
}

/// Wall-clock ms — the same `SystemTime` source the pump's private `now_ms` uses (mirrors
/// `market_feed_scripted.rs`'s helper), so a crafted frame ts is comparable to the freshness clock.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
