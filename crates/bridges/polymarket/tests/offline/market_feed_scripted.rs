//! Scripted (network-free) coverage of `market_feed::run_session` — the per-token pump loop
//! `Feeds::spawn_with`'s reconnect wrapper drives against a real `MarketStream`. Here a canned
//! frame queue drives it directly: no socket, no thread spawn, deterministic ordering.
//!
//! `tick_size = 1.0` throughout (rather than Polymarket's realistic `0.01`) is a deliberate test
//! choice: whole-number prices quantize through `L2Book`'s `tick_at`/`price_of` round-trip with NO
//! floating-point rounding uncertainty, so every assertion below — both the recorded sink calls
//! and the post-run `state.book.best_bid()/best_ask()` — can use exact `==`/`assert_eq!` rather
//! than an epsilon. The pump logic under test (mode gating, change-gated quote emission, malformed-
//! frame tolerance, wholesale book replace) does not care what the tick size's actual magnitude is.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use vike_bridge_core::{HealthEvent, StreamHealth};
use vike_data::{LiveDataSink, StreamStatus};
use vike_polymarket::{MarketStream, PumpMode, TokenState, Watchdog, run_session};

const TOKEN: &str = "111";
/// The scripted freshness threshold for the Book/Quotes wiring tests. Deliberately kept at the
/// pump's ORIGINAL 60s — production has since been tuned to `FRESHNESS_THRESHOLD_BOOK` = 300s
/// (2026-07-11 live measurement; see that const's doc) — because the wiring these tests prove is
/// threshold-AGNOSTIC: `StreamHealth` trips when a frame's ts is older than whatever threshold it
/// was built with, and that arithmetic is already unit-tested on `StreamHealth` itself. Scripting
/// against 60s keeps each crafted frame age (e.g. the 90s "stale" frames below) simply past
/// threshold; tracking the 300s production value would only inflate those ages for no added
/// coverage. Passed to `StreamHealth::new` the same way the idle tests pass a literal
/// `Duration::from_secs(30)` rather than reaching for the private production const.
const FRESHNESS_MS: i64 = 60_000;
/// The scripted freshness threshold for the sparse `Trades` lane — like `FRESHNESS_MS`, kept at the
/// pump's ORIGINAL 10 min (production is now `FRESHNESS_THRESHOLD_TRADES` = 30 min); the Trades
/// wiring under test is the same threshold-agnostic mechanism, scripted here against 10 min so the
/// 700s "past-threshold" frame below stays simply past it.
const FRESHNESS_MS_TRADES: i64 = 600_000;

// The shared scripted MarketStream double (testing-arch Phase 4c, vike-bridge-core `test-support`
// feature) — replaces the inline `Step`/`ScriptedStream` copy that used to live here. Same
// behavior pins: exhausting the script is a stream-closed error (so `run_session` returning
// `Err` at the end of a script is expected, not a test failure), `send_text` records into the
// log read back via `.sent()`, `stalled`/`push_timeout_with_stall` drive the §B idle watchdog.
use vike_bridge_core::scripted::ScriptedStream;

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string copy
// that used to live here; its canonical `calls()` forms ARE this file's original conventions
// (this copy was the richest of the ~13, so every literal below reads unchanged).
use vike_data::RecordingSink;

/// Run one session with a fresh, default-timing watchdog — used by the pump-behavior tests that do
/// NOT exercise §B gap disclosure. A fresh `StreamHealth` never emits `Live`, and the 10s keepalive
/// never fires inside a microsecond-fast scripted run, so the watchdog is inert here and every
/// pre-existing emission-sequence / `sent`-is-just-subscribe assertion still holds unchanged.
fn run<S: MarketStream>(
    stream: &mut S,
    sink: &dyn LiveDataSink,
    mode: PumpMode,
    state: &mut TokenState,
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut health = StreamHealth::new(FRESHNESS_MS);
    let mut wd = Watchdog::new(&mut health, "test");
    run_session(stream, sink, TOKEN, mode, state, stop, &mut wd, || {})
}

fn book_frame(bids: &[(&str, &str)], asks: &[(&str, &str)]) -> serde_json::Value {
    let levels = |arr: &[(&str, &str)]| -> serde_json::Value {
        serde_json::Value::Array(
            arr.iter().map(|(p, s)| serde_json::json!({"price": p, "size": s})).collect(),
        )
    };
    serde_json::json!({"event_type": "book", "asset_id": TOKEN, "bids": levels(bids), "asks": levels(asks)})
}

/// Real wire shape (brief, live capture 2026-07-08): the frame carries a PLURAL `price_changes`
/// array, and each entry — not the frame — carries its own `asset_id`. `entries` is
/// `(asset_id, price, size, side)`; passing `TOKEN` for every entry builds a single-asset frame,
/// mixing in a different asset_id builds a real multi-asset frame.
fn price_change_frame(entries: &[(&str, &str, &str, &str)]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = entries
        .iter()
        .map(|(asset_id, p, s, side)| {
            serde_json::json!({"asset_id": asset_id, "price": p, "size": s, "side": side})
        })
        .collect();
    serde_json::json!({"event_type": "price_change", "price_changes": arr})
}

fn last_trade_frame(price: &str, size: &str, side: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({"event_type": "last_trade_price", "asset_id": TOKEN, "price": price, "size": size});
    if let Some(s) = side {
        v["side"] = serde_json::Value::String(s.to_string());
    }
    v
}

/// The brief's canonical flow: book snapshot -> price_change (top moves) -> price_change (top
/// unchanged, deeper levels only) -> last_trade (ignored in Book mode) -> malformed frame
/// (ignored) -> book replace (wholesale). Exercises the full `PumpMode::Book` contract: `sink.book`
/// on every applied book/price_change frame, `sink.quote` change-gated on the derived top, and
/// tolerance of both an off-mode frame type and outright junk.
#[test]
fn book_mode_full_scripted_flow_matches_the_exact_emission_sequence() {
    let mut stream = ScriptedStream::from_json([
        book_frame(&[("51", "100")], &[("55", "40")]), // top (51,55,100,40) -> book+quote
        price_change_frame(&[(TOKEN, "54", "90", "SELL")]), // ask improves -> top (51,54,100,90) -> book+quote
        price_change_frame(&[(TOKEN, "40", "20", "BUY"), (TOKEN, "60", "15", "SELL")]), // deeper levels only -> book, NO quote
    ]);
    stream.push_text(&last_trade_frame("52", "5", Some("BUY")).to_string()); // Book mode: ignored
    stream.push_text("not json at all"); // malformed: ignored, session continues
    stream.push_text(&book_frame(&[("60", "10")], &[("62", "5")]).to_string()); // wholesale replace -> book+quote

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0); // exact quantization — see module doc
    let stop = AtomicBool::new(false);

    let result = run(&mut stream, &sink, PumpMode::Book, &mut state, &stop);
    assert!(
        result.is_err(),
        "the script runs out of frames, which surfaces as a stream-closed error"
    );
    assert_eq!(
        stream.sent(),
        vec![vike_polymarket::subscribe_message(&[TOKEN.to_string()])],
        "subscribes exactly once, before the first frame read"
    );

    assert_eq!(
        sink.calls(),
        vec![
            // Task 7: every applied book/price_change frame now ALSO emits a recordable book_update,
            // ordered BEFORE that frame's book/quote. seq is the contiguous feed-local chain.
            "book_update:polymarket:111:Snapshot:seq=1:bids=1:asks=1:local_ts_pos=true:tick=1"
                .to_string(),
            "book:polymarket:111:Some(53.0)".to_string(),
            "quote:polymarket:111:51/55:100x40".to_string(),
            "book_update:polymarket:111:Delta:seq=2:bids=0:asks=1:local_ts_pos=true:tick=1"
                .to_string(),
            "book:polymarket:111:Some(52.5)".to_string(),
            "quote:polymarket:111:51/54:100x90".to_string(),
            "book_update:polymarket:111:Delta:seq=3:bids=1:asks=1:local_ts_pos=true:tick=1"
                .to_string(),
            "book:polymarket:111:Some(52.5)".to_string(), // top unchanged -> no third quote
            "book_update:polymarket:111:Snapshot:seq=4:bids=1:asks=1:local_ts_pos=true:tick=1"
                .to_string(), // wholesale book replace -> a Snapshot book_update
            "book:polymarket:111:Some(61.0)".to_string(), // last_trade + junk produced no calls
            "quote:polymarket:111:60/62:10x5".to_string(),
        ],
        "exact emission sequence per the module doc's PumpMode::Book rules (incl. the Task 7 book_update stream)"
    );

    // quantized book tops after the run: the wholesale replace fully superseded the prior state.
    assert_eq!(state.book.best_bid(), Some((60.0, 10.0)));
    assert_eq!(state.book.best_ask(), Some((62.0, 5.0)));
    assert_eq!(state.last_top, Some((60.0, 62.0, 10.0, 5.0)));
}

/// Regression test for the real bug: the real `price_change` wire frame is multi-asset
/// (`price_changes` plural, each entry carrying its own `asset_id` — brief's live capture). This
/// drives the pump with a real-shaped frame so the book actually mutates from an incremental
/// delta (the old decoder read a nonexistent singular `changes` key and silently decoded every
/// `price_change` frame to zero updates, so this book-updates-from-a-delta path never ran at all).
/// The frame also carries a SECOND asset's entry (`"999"`, not the subscribed token) to prove the
/// multi-asset guard: only the matching `asset_id` entry may mutate this pump's state.
#[test]
fn multi_asset_price_change_frame_applies_only_the_subscribed_token() {
    let mut stream = ScriptedStream::from_json([
        book_frame(&[("51", "100")], &[("55", "40")]), // top (51,55,100,40) -> book+quote
        // real multi-asset shape: TOKEN's ask improves to 54; the "999" entry must be ignored.
        price_change_frame(&[(TOKEN, "54", "90", "SELL"), ("999", "1", "1", "BUY")]),
    ]);
    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let _ = run(&mut stream, &sink, PumpMode::Book, &mut state, &stop);

    assert_eq!(
        sink.calls(),
        vec![
            "book_update:polymarket:111:Snapshot:seq=1:bids=1:asks=1:local_ts_pos=true:tick=1"
                .to_string(),
            "book:polymarket:111:Some(53.0)".to_string(),
            "quote:polymarket:111:51/55:100x40".to_string(),
            "book_update:polymarket:111:Delta:seq=2:bids=0:asks=1:local_ts_pos=true:tick=1"
                .to_string(), // only TOKEN's entry -> a 1-ask delta
            "book:polymarket:111:Some(52.5)".to_string(), // TOKEN's ask delta applied
            "quote:polymarket:111:51/54:100x90".to_string(),
        ],
        "only the subscribed token's entry applied; the other asset's entry was ignored"
    );
    assert_eq!(
        state.book.best_ask(),
        Some((54.0, 90.0)),
        "the real-shaped price_changes delta actually mutated the book"
    );
}

/// Mirrors `market_feed`'s private production const (integration tests can't reach it) — the same
/// mirror-by-literal convention `FRESHNESS_MS`/`FRESHNESS_MS_TRADES` already use above. The pump
/// must NOT lower its real 512 to shrink this test; the test builds 512 delta frames in a loop.
const ANCHOR_EVERY_DELTAS: u64 = 512;

/// Task 7: in `PumpMode::Book` the pump emits the RECORDABLE `sink.book_update()` stream — one
/// `Snapshot` per applied `book` frame, one `Delta` per applied `price_change` frame (RAW wire
/// levels, freshly-bumped feed-local seq), plus a synthetic full-book `Snapshot` anchor every
/// `ANCHOR_EVERY_DELTAS` applied deltas so replay never seeks unboundedly. Drives one venue `book`
/// frame then exactly `ANCHOR_EVERY_DELTAS` `price_change` frames and asserts the whole recorded
/// stream: kinds, the contiguous seq chain (snapshot + 512 deltas + synthetic anchor, every seq +1),
/// the synthetic anchor carrying the FULL folded book (not just one frame's changed levels), the
/// dual-stamp (`local_ts > 0`) + carried `tick_size`, and the book_update-before-quote ordering that
/// matches the replay tie-break (Book < Quote).
#[test]
fn book_mode_emits_recordable_updates_with_synthetic_anchors() {
    // 1 snapshot (1 bid, 1 ask), then ANCHOR_EVERY_DELTAS price_change frames:
    //  - delta #1 (i==0) improves the ASK (6000 -> 5500), moving the top so it emits a quote too —
    //    a second book_update-before-quote ordering witness beyond the snapshot frame;
    //  - deltas #2.. add distinct DEEP bid levels (well below the 5000 best bid), so the top never
    //    moves again (no more quotes) BUT the folded book keeps growing — which is exactly what makes
    //    the synthetic anchor's full-book level count distinguishable from any single delta's.
    let mut frames: Vec<serde_json::Value> = Vec::with_capacity(1 + ANCHOR_EVERY_DELTAS as usize);
    frames.push(book_frame(&[("5000", "100")], &[("6000", "100")]));
    for i in 0..ANCHOR_EVERY_DELTAS {
        if i == 0 {
            frames.push(price_change_frame(&[(TOKEN, "5500", "10", "SELL")])); // ask improves -> quote
        } else {
            let price = (4000 - i as i64).to_string(); // 3999, 3998, … distinct deep bids, all > 0
            frames.push(price_change_frame(&[(TOKEN, &price, "10", "BUY")]));
        }
    }
    let mut stream = ScriptedStream::from_json(frames);

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0); // exact quantization — see module doc
    let stop = AtomicBool::new(false);

    let _ = run(&mut stream, &sink, PumpMode::Book, &mut state, &stop);

    let calls = sink.calls();
    let book_updates: Vec<&String> =
        calls.iter().filter(|c| c.starts_with("book_update:")).collect();

    // 1 venue snapshot + ANCHOR_EVERY_DELTAS deltas + 1 synthetic anchor.
    assert_eq!(
        book_updates.len() as u64,
        ANCHOR_EVERY_DELTAS + 2,
        "one snapshot, {ANCHOR_EVERY_DELTAS} deltas, one synthetic anchor: {book_updates:?}"
    );

    // (1) first book_update: Snapshot, seq 1, the wire snapshot's 1 bid / 1 ask.
    assert_eq!(
        book_updates[0],
        "book_update:polymarket:111:Snapshot:seq=1:bids=1:asks=1:local_ts_pos=true:tick=1",
        "first update is the venue snapshot anchor, seq 1, raw wire level counts"
    );

    // (2) the next ANCHOR_EVERY_DELTAS updates are Deltas with a contiguous seq 2..=ANCHOR+1.
    for idx in 0..ANCHOR_EVERY_DELTAS {
        let bu = book_updates[1 + idx as usize];
        assert!(bu.contains(":Delta:"), "update {} is a Delta: {bu}", 1 + idx);
        assert!(
            bu.contains(&format!(":seq={}:", idx + 2)),
            "delta #{} carries the freshly bumped seq {}: {bu}",
            idx + 1,
            idx + 2
        );
    }

    // (3) immediately after the ANCHOR_EVERY_DELTAS-th delta: ONE synthetic Snapshot at seq
    // ANCHOR_EVERY_DELTAS+2, carrying the FULL folded book — 512 bids (5000 + 511 deep) / 2 asks
    // (6000 + 5500) — NOT the single changed level a delta frame carries.
    assert_eq!(state.book.bid_levels(), 512, "folded book grew to 512 bid levels over the run");
    assert_eq!(
        state.book.ask_levels(),
        2,
        "folded book has 2 ask levels (6000 + the improved 5500)"
    );
    assert_eq!(
        book_updates[ANCHOR_EVERY_DELTAS as usize + 1],
        &format!(
            "book_update:polymarket:111:Snapshot:seq={}:bids={}:asks={}:local_ts_pos=true:tick=1",
            ANCHOR_EVERY_DELTAS + 2,
            state.book.bid_levels(),
            state.book.ask_levels(),
        ),
        "synthetic anchor: seq {}, the FULL folded book (not one frame's changed levels)",
        ANCHOR_EVERY_DELTAS + 2
    );

    // (5) every emitted book_update was dual-stamped (local_ts > 0) and carried the book's tick_size.
    for bu in &book_updates {
        assert!(bu.contains(":local_ts_pos=true:"), "book_update is dual-stamped: {bu}");
        assert!(bu.ends_with(":tick=1"), "book_update carries the book tick_size (== 1.0): {bu}");
    }

    // (4) ordering: within the book_update|quote subsequence every quote is IMMEDIATELY preceded by
    // a book_update — i.e. each frame's recordable book event precedes its derived quote (Book <
    // Quote replay tie-break). Both quote-emitting frames (the snapshot and the top-moving delta #1)
    // witness it.
    let bu_or_quote: Vec<&String> =
        calls.iter().filter(|c| c.starts_with("book_update:") || c.starts_with("quote:")).collect();
    let quote_count = bu_or_quote.iter().filter(|c| c.starts_with("quote:")).count();
    assert_eq!(quote_count, 2, "exactly two frames moved the top and emitted a quote");
    for (pos, c) in bu_or_quote.iter().enumerate() {
        if c.starts_with("quote:") {
            assert!(pos > 0, "a quote is never the very first recordable event: {bu_or_quote:?}");
            assert!(
                bu_or_quote[pos - 1].starts_with("book_update:"),
                "each derived quote is immediately preceded by its frame's book_update: {bu_or_quote:?}"
            );
        }
    }
}

/// `PumpMode::Quotes`: identical book bookkeeping/change-gating as `Book`, but `sink.book` must
/// NEVER fire — only the derived quote.
#[test]
fn quotes_mode_never_calls_book_only_the_derived_quote() {
    let mut stream = ScriptedStream::from_json([
        book_frame(&[("10", "5")], &[("12", "3")]), // top (10,12,5,3)
        price_change_frame(&[(TOKEN, "11", "2", "SELL")]), // ask improves -> top (10,11,5,2)
        book_frame(&[("20", "1")], &[("21", "1")]), // wholesale replace -> top (20,21,1,1)
    ]);
    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let _ = run(&mut stream, &sink, PumpMode::Quotes, &mut state, &stop);

    assert_eq!(
        sink.calls(),
        vec![
            "quote:polymarket:111:10/12:5x3".to_string(),
            "quote:polymarket:111:10/11:5x2".to_string(),
            "quote:polymarket:111:20/21:1x1".to_string(),
        ],
        "Quotes mode maintains the book internally but never emits it"
    );
}

/// `PumpMode::Trades`: book/price_change frames are ignored outright (no book state is kept at
/// all — `state.book` stays empty), and every `last_trade` frame becomes one `sink.trade` call
/// with the documented `is_buyer_maker` mapping (taker buy -> maker was the seller -> `false`;
/// an absent `side` defaults to a conservative taker-buy assumption -> `false` as well, while an
/// explicit taker SELL -> maker was the buyer -> `true`).
#[test]
fn trades_mode_ignores_book_frames_and_emits_every_last_trade() {
    let mut stream = ScriptedStream::from_json([
        book_frame(&[("51", "100")], &[("55", "40")]), // ignored entirely in Trades mode
        last_trade_frame("0.52", "3", Some("SELL")), // taker sell -> maker was the buyer -> is_buyer_maker=true
        price_change_frame(&[(TOKEN, "50", "10", "BUY")]), // ignored entirely in Trades mode
        last_trade_frame("0.53", "1", None), // no side -> conservative taker-buy default -> maker=false
    ]);
    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let _ = run(&mut stream, &sink, PumpMode::Trades, &mut state, &stop);

    assert_eq!(
        sink.calls(),
        vec![
            "trade:polymarket:111:0.52/3:maker=true".to_string(),
            "trade:polymarket:111:0.53/1:maker=false".to_string(),
        ]
    );
    assert_eq!(state.book.best_bid(), None, "Trades mode never touches the book");
    assert_eq!(state.book.best_ask(), None);
}

/// Final-review fix: the decoder threads a frame's wire `timestamp` through to the tick this pump
/// emits — a `book` frame's ts lands on the derived `QuoteTick`, and a `last_trade_price` frame's
/// own ts lands on the `TradeTick`. Both the string and number JSON forms are exercised (mirroring
/// `ws.rs`'s decode-level coverage of both forms). The no-`timestamp` fallback (`now_ms()`) is
/// exercised implicitly by every other scripted test above, none of which set an exact-ts
/// expectation.
#[test]
fn wire_timestamp_threads_through_to_emitted_ticks() {
    // The shared sink's typed accessors capture the full QuoteTick/TradeTick, ts included —
    // no bespoke ts-capturing sink needed (Phase 4d).
    // Book mode: the derived quote must carry the `book` frame's wire ts (string JSON form).
    let mut stream = ScriptedStream::from_json([serde_json::json!({
        "event_type": "book", "asset_id": TOKEN, "timestamp": "1700000000000",
        "bids": [{"price":"51","size":"100"}], "asks": [{"price":"55","size":"40"}]
    })]);
    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);
    let _ = run(&mut stream, &sink, PumpMode::Book, &mut state, &stop);
    assert_eq!(
        sink.quotes().first().map(|(_, _, q)| q.ts),
        Some(1_700_000_000_000),
        "quote carries the book frame's wire ts"
    );

    // Trades mode: the trade must carry the `last_trade_price` frame's own wire ts (number form).
    let mut stream = ScriptedStream::from_json([serde_json::json!({
        "event_type": "last_trade_price", "asset_id": TOKEN,
        "price": "0.53", "size": "1", "timestamp": 1_700_000_000_001i64
    })]);
    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);
    let _ = run(&mut stream, &sink, PumpMode::Trades, &mut state, &stop);
    assert_eq!(
        sink.trades().first().map(|(_, _, t)| t.ts),
        Some(1_700_000_000_001),
        "trade carries its own frame's wire ts"
    );
}

/// Sanity check on the seam itself (not the pump): a stop flag already raised before the first
/// frame read returns `Ok(())` immediately without touching the stream.
#[test]
fn stop_flag_set_before_the_first_read_exits_cleanly() {
    let mut stream = ScriptedStream::from_json([book_frame(&[("1", "1")], &[("2", "1")])]);
    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(true);

    let result = run(&mut stream, &sink, PumpMode::Book, &mut state, &stop);
    assert!(result.is_ok());
    assert!(sink.calls().is_empty(), "a pre-raised stop must not process any frame");
}

// ---- net-hardening §B: idle watchdog + gap/recovery disclosure ---------------------------------

/// After a prior outage left a gap open (feed_main's `enter_gap`), the next session's FIRST inbound
/// frame must disclose `StreamStatus::Live` — closing that gap — and it must land BEFORE the frame's
/// own book/quote data, so a consumer sees "recovered" before it sees the recovered data.
#[test]
fn reconnect_discloses_live_on_the_first_frame_before_its_data() {
    let mut stream = ScriptedStream::from_json([book_frame(&[("51", "100")], &[("55", "40")])]);
    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let mut health = StreamHealth::new(FRESHNESS_MS);
    health.enter_gap(1000); // a prior session failed; the gap is open
    let mut wd = Watchdog::new(&mut health, "quotes");
    let _ =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd, || {});

    let calls = sink.calls();
    assert_eq!(
        calls[0], "stream_status:polymarket:111:quotes:Live { gap_started_ts_ms: Some(1000) }",
        "Live disclosed first, closing the gap the prior outage opened"
    );
    // then the frame's own data: its recordable book_update precedes the folded book (Task 7).
    assert_eq!(
        calls[1],
        "book_update:polymarket:111:Snapshot:seq=1:bids=1:asks=1:local_ts_pos=true:tick=1"
    );
    assert_eq!(calls[2], "book:polymarket:111:Some(53.0)", "then the frame's own folded book");
}

/// A healthy first session (no prior gap) must disclose NOTHING — `recover()` is a no-op when no
/// gap is open, so neither `Live` nor `GapStart` may appear.
#[test]
fn a_healthy_first_session_discloses_no_stream_status() {
    let mut stream = ScriptedStream::from_json([
        book_frame(&[("51", "100")], &[("55", "40")]),
        book_frame(&[("60", "10")], &[("62", "5")]),
    ]);
    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let mut health = StreamHealth::new(FRESHNESS_MS);
    let mut wd = Watchdog::new(&mut health, "book");
    let _ =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd, || {});

    assert!(
        sink.calls().iter().all(|c| !c.starts_with("stream_status")),
        "no gap was ever open, so no §B disclosure may be emitted: {:?}",
        sink.calls()
    );
}

/// A stalled-but-open stream (read-timeouts, no frames) past the idle threshold must return `Err`
/// so `feed_main` reconnects — this is the dead-feed detection itself. Injected `idle_threshold =
/// ZERO` trips on the first timeout tick without a real 30s sleep.
#[test]
fn a_stalled_stream_trips_the_idle_watchdog() {
    let mut stream = ScriptedStream::stalled(Duration::from_secs(60)); // no frame of any kind for 60s
    stream.push_timeout(); // a read-timeout tick with the liveness clock already past the threshold

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let mut health = StreamHealth::new(FRESHNESS_MS);
    // 30s idle threshold vs the 60s stall above → the first timeout trips; ping interval huge so
    // the keepalive path stays out of the way.
    let mut wd = Watchdog::with_timing(
        &mut health,
        "book",
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let result =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd, || {});

    let err =
        result.expect_err("a stalled stream must Err so feed_main reconnects and opens a gap");
    assert!(
        err.contains("silent stall"),
        "the error names the idle-watchdog trip (the shared driver's attributable \"no frames \
         within {{N}}s (silent stall)\"): {err}"
    );
}

/// The robustness fix (adversarial-review finding): a connection kept alive purely by keepalive
/// traffic — a server WS-control ping, or the `"PONG"` to our `"PING"` — keeps `since_last_frame`
/// fresh, so MANY read-timeout ticks must be absorbed WITHOUT a false idle trip. Only true transport
/// silence (nothing inbound for the whole threshold) may trip. Here the 5s liveness clock stays
/// under the 30s threshold, so the session ends on the scripted close, not an idle trip.
#[test]
fn a_keepalive_fresh_stream_is_not_falsely_declared_idle() {
    let mut stream = ScriptedStream::stalled(Duration::from_secs(5)); // 5s ago — alive via keepalive
    stream.push_timeout();
    stream.push_timeout();
    stream.push_timeout();

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let mut health = StreamHealth::new(FRESHNESS_MS);
    let mut wd = Watchdog::with_timing(
        &mut health,
        "book",
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let err =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd, || {})
            .expect_err("the script ends by exhaustion (Closed), not an idle trip");
    assert!(
        !err.contains("silent stall"),
        "a keepalive-fresh stream must NOT trip the idle watchdog: {err}"
    );
    assert!(sink.calls().is_empty(), "no data flowed, no gap opened, nothing disclosed");
}

/// The CLOB client keepalive `"PING"` must actually be written to the wire (the market feed never
/// sent it before §B). Injected `ping_every = ZERO` forces a PING before the first read; idle huge
/// so no false trip.
#[test]
fn the_client_keepalive_ping_is_sent_on_the_wire() {
    let mut stream = ScriptedStream::from_json([book_frame(&[("51", "100")], &[("55", "40")])]);
    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let mut health = StreamHealth::new(FRESHNESS_MS);
    let mut wd =
        Watchdog::with_timing(&mut health, "book", Duration::from_secs(3600), Duration::ZERO);
    let _ =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd, || {});

    assert_eq!(
        stream.sent()[0],
        vike_polymarket::subscribe_message(&[TOKEN.to_string()]),
        "subscribe still goes first, before any keepalive"
    );
    assert!(
        stream.sent().iter().any(|s| s == "PING"),
        "the CLOB client keepalive PING must be sent; sent={:?}",
        stream.sent()
    );
}

// ---- net-hardening §B: DATA-freshness timer (stale-behind-a-live-socket) -----------------------
//
// The crux: freshness `check_freshness` reads the FREE wall-clock `now_ms()`, which a scripted test
// cannot advance. Approach A sidesteps clock injection entirely — `StreamHealth` judges
// `now_ms() - newest_ts` and the test owns `newest_ts` by crafting each frame's wire `timestamp`. A
// frame stamped 90s in the past is stale the instant it's applied; a frame stamped ≈now is fresh.
// The transport is held ALIVE throughout (`ScriptedStream`'s default `stall = ZERO`, far under the
// injected 30s idle threshold), so every `push_timeout` tick lands in the transport-ALIVE branch
// that runs the freshness check — never the idle-trip/reconnect path the transport-gap half owns.
// No production clock seam was needed; the `StreamHealth` arithmetic itself is already unit-tested
// in `vike-bridge-core`, so these tests prove only the pump WIRING (observe-at-each-sink-call +
// check-on-the-alive-tick).

/// Wall-clock ms — the same `SystemTime`-since-epoch source the pump's private `now_ms` uses, so a
/// crafted frame ts is directly comparable to the `now_ms()` the freshness `check_freshness` will
/// read.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A `book` frame carrying an explicit wire `timestamp` (string epoch-ms, the production form) so a
/// freshness test controls how far behind wall-clock the newest applied DATA ts is. Plain
/// `book_frame` omits `timestamp`, so the pump would stamp receive-time (`now_ms()`) — always fresh,
/// which is exactly why the freshness tests need this variant.
fn book_frame_at(bids: &[(&str, &str)], asks: &[(&str, &str)], ts_ms: i64) -> serde_json::Value {
    let mut v = book_frame(bids, asks);
    v["timestamp"] = serde_json::Value::String(ts_ms.to_string());
    v
}

/// `last_trade_frame`'s freshness-test sibling — the `last_trade_price` counterpart of
/// `book_frame_at`, adding an explicit wire `timestamp` so a `Trades`-mode freshness test controls
/// how far behind wall-clock the newest applied DATA ts is (plain `last_trade_frame` omits it, so
/// the pump would stamp receive-time — always fresh).
fn last_trade_frame_at(
    price: &str,
    size: &str,
    side: Option<&str>,
    ts_ms: i64,
) -> serde_json::Value {
    let mut v = last_trade_frame(price, size, side);
    v["timestamp"] = serde_json::Value::String(ts_ms.to_string());
    v
}

/// Build a fresh-`StreamHealth` watchdog using `FRESHNESS_MS`, with a 30s idle threshold (≫ the
/// ZERO stall, so every timeout tick is transport-ALIVE) and a huge ping interval (keeps the
/// keepalive path out of the way). Shared by the freshness tests below.
fn freshness_watchdog(health: &mut StreamHealth) -> Watchdog<'_> {
    Watchdog::with_timing(health, "book", Duration::from_secs(30), Duration::from_secs(3600))
}

/// The crux wiring test. A book frame whose wire ts is 90s in the past (≫ the 60s `FRESHNESS_MS`) is
/// applied behind a LIVE socket; the next transport-alive watchdog tick must disclose EXACTLY ONE
/// `StreamStatus::Stale` and — critically — NO `GapStart` (the transport never went idle). This is
/// the failure a socket-liveness watchdog structurally cannot see.
#[test]
fn stale_data_behind_a_live_socket_discloses_exactly_one_stale_and_no_gap() {
    let stale_ts = now_ms() - 90_000; // ≫ FRESHNESS_MS; the real clock only widens the lag further
    let mut stream =
        ScriptedStream::from_json([book_frame_at(&[("51", "100")], &[("55", "40")], stale_ts)]);
    stream.push_timeout(); // transport-alive tick → health.check_freshness(now_ms()) → Stale

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let mut health = StreamHealth::new(FRESHNESS_MS);
    let mut wd = freshness_watchdog(&mut health);
    let _ =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd, || {});

    let calls = sink.calls();
    assert_eq!(
        calls.iter().filter(|c| c.contains("Stale")).count(),
        1,
        "exactly one Stale disclosed behind the live socket: {calls:?}"
    );
    assert!(
        calls.iter().all(|c| !c.contains("GapStart")),
        "the transport never went idle → no GapStart may appear: {calls:?}"
    );
}

/// After a stale episode opens, a FRESH data frame (wire ts ≈ now) resumes the stream: the next
/// transport-alive check must disclose EXACTLY ONE `Live`, closing the episode — with still exactly
/// one `Stale` total (no re-emit while frozen). The tracker's one-signal-per-episode contract,
/// proven end-to-end through the pump.
#[test]
fn fresh_data_after_a_stale_episode_discloses_exactly_one_live() {
    let stale_ts = now_ms() - 90_000;
    let mut stream =
        ScriptedStream::from_json([book_frame_at(&[("51", "100")], &[("55", "40")], stale_ts)]);
    stream.push_timeout(); // → Stale
    stream.push_timeout(); // still frozen → NO re-emit (once-per-episode, through the pump)
    // fresh data resumes (wire ts = now, new top) → book+quote + observe(now)
    stream.push_text(&book_frame_at(&[("60", "10")], &[("62", "5")], now_ms()).to_string());
    stream.push_timeout(); // → Live (freshness recovered)

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let mut health = StreamHealth::new(FRESHNESS_MS);
    let mut wd = freshness_watchdog(&mut health);
    let _ =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd, || {});

    let calls = sink.calls();
    assert_eq!(
        calls.iter().filter(|c| c.contains("Stale")).count(),
        1,
        "exactly one Stale across the whole episode: {calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|c| c.contains("Live")).count(),
        1,
        "exactly one Live closes the episode when fresh data resumes: {calls:?}"
    );
    assert!(calls.iter().all(|c| !c.contains("GapStart")), "no transport gap: {calls:?}");
}

/// Advancing data (each frame's wire ts ≈ now) with watchdog ticks interleaved never trips the
/// freshness timer: NO stream-status disclosure at all. The healthy baseline.
#[test]
fn advancing_data_never_discloses_stale() {
    let mut stream =
        ScriptedStream::from_json([book_frame_at(&[("51", "100")], &[("55", "40")], now_ms())]);
    stream.push_timeout(); // check: lag ≈ 0 → fresh, nothing
    stream.push_text(&book_frame_at(&[("60", "10")], &[("62", "5")], now_ms()).to_string());
    stream.push_timeout(); // check: still fresh, nothing

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let mut health = StreamHealth::new(FRESHNESS_MS);
    let mut wd = freshness_watchdog(&mut health);
    let _ =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd, || {});

    let calls = sink.calls();
    assert!(
        calls.iter().all(|c| !c.starts_with("stream_status")),
        "fresh, advancing data discloses no stream status at all: {calls:?}"
    );
}

/// A sparse-but-CURRENT stream: data arrives infrequently (many watchdog ticks between frames) but
/// each frame's wire ts is recent (within `FRESHNESS_MS`) → NEVER stale. The false-positive guard —
/// a legitimately quiet-but-connected Polymarket token must not be flagged.
#[test]
fn sparse_but_current_data_never_discloses_stale() {
    let recent = now_ms() - 5_000; // 5s old — well within the 60s threshold, "current" for a quiet token
    let mut stream =
        ScriptedStream::from_json([book_frame_at(&[("51", "100")], &[("55", "40")], recent)]);
    stream.push_timeout(); // several checks with no new data, all fresh (lag ≈ 5s < 60s)
    stream.push_timeout();
    stream.push_timeout();

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);

    let mut health = StreamHealth::new(FRESHNESS_MS);
    let mut wd = freshness_watchdog(&mut health);
    let _ =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd, || {});

    let calls = sink.calls();
    assert!(
        calls.iter().all(|c| !c.contains("Stale")),
        "data within the freshness threshold is never stale, however sparse: {calls:?}"
    );
}

// ---- net-hardening §B: transport gap × data-freshness COMPOSED in one reconnect flow -----------
//
// Every test above proves ONE watchdog concern in isolation: the idle-trip tests never touch
// freshness, and the freshness tests never idle-trip (their streams use the default `stall = ZERO`,
// i.e. never idle). Neither exercises the one seam where the two halves of `StreamHealth` actually
// interact: `feed_main` owns ONE `StreamHealth` ACROSS reconnects (module doc), so a freshness
// episode that's still open when the transport itself dies must survive into the next session and
// close independently of the transport recovery — two distinct `Live` disclosures closing two
// distinct episodes, neither orphaned nor double-emitted.
//
// `run_session` only ever models ONE socket session; `feed_main`'s reconnect loop is what calls it
// repeatedly and performs `health.enter_gap` on failure — so this test calls `run_session` TWICE,
// sharing one `StreamHealth` across the boundary and manually performing that one `enter_gap` step
// in between, exactly as `reconnect_discloses_live_on_the_first_frame_before_its_data` already does
// for the transport-only case.

/// Coarse per-call classification used only to assert the ORDER/KIND of the composed sequence below
/// without over-committing to the exact wall-clock timestamps embedded in `Stale`/`GapStart`/`Live`'s
/// `{:?}` payloads (which genuinely vary run to run).
fn call_kind(call: &str) -> &'static str {
    if call.starts_with("book_update:") {
        "book_update"
    } else if call.starts_with("book:") {
        "book"
    } else if call.starts_with("quote:") {
        "quote"
    } else if call.contains("Stale") {
        "stale"
    } else if call.contains("GapStart") {
        "gap_start"
    } else if call.contains("Live") {
        "live"
    } else {
        "other"
    }
}

/// The composed flow the per-task tests don't cover: a data-freshness episode opens behind a LIVE
/// socket (transport fine, data frozen) — then the SAME socket later genuinely dies (idle-watchdog
/// trip, a transport gap) while the freshness episode is still open — then a reconnect delivers
/// fresh data, which must close BOTH the transport gap and the freshness episode (the two halves
/// [`StreamHealth`] fuses), without orphaning either signal or double-emitting either one.
#[test]
fn stale_episode_survives_a_transport_gap_and_both_signals_close_clean_on_reconnect() {
    // ---- session 1 (the original connection): data goes stale behind a live socket, THEN the same
    // socket goes genuinely idle and the session errors. `push_timeout` (default stall=ZERO) drives
    // the freshness check on the first tick; `push_timeout_with_stall` escalates the liveness clock
    // past the idle threshold for the second, tripping the idle watchdog on a session that already
    // has an open Stale episode — the composition `push_timeout` alone cannot express (see its doc).
    let stale_ts = now_ms() - 90_000; // ≫ FRESHNESS_MS
    let mut stream1 =
        ScriptedStream::from_json([book_frame_at(&[("51", "100")], &[("55", "40")], stale_ts)]);
    stream1.push_timeout(); // transport-alive tick (stall=ZERO) -> health.check_freshness -> Stale
    stream1.push_timeout_with_stall(Duration::from_secs(60)); // NOW the socket goes idle (60s ≫ 30s) -> idle trip

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);
    let mut health = StreamHealth::new(FRESHNESS_MS);

    let result1 = {
        let mut wd = freshness_watchdog(&mut health);
        run_session(&mut stream1, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd, || {})
    };
    let err1 = result1.expect_err("the socket idle-trips after the stale episode opens");
    assert!(
        err1.contains("silent stall"),
        "session 1 must end via the idle watchdog (the driver's attributable message): {err1}"
    );

    // ---- the reconnect boundary: mirrors feed_main's Err arm exactly ("the outer arms emit
    // Gap on failure" — run_session doc). run_session itself never calls enter_gap; only
    // feed_main does, which is why the test performs this step manually (same pattern
    // `reconnect_discloses_live_on_the_first_frame_before_its_data` already uses). `enter_gap` only
    // ever produces `HealthEvent::Gap`, so the pattern match below is exhaustive in practice — mapped
    // inline rather than via a shared helper since this is the ONE call site that needs it. ----
    state.last_top = None; // feed_main resets this on any session error too (module doc)
    if let Some(HealthEvent::Gap { at_ts_ms }) = health.enter_gap(now_ms()) {
        sink.stream_status("polymarket", TOKEN, "book", StreamStatus::GapStart { at_ts_ms });
    }

    // ---- session 2 (the reconnect): fresh data resumes. The first frame closes the transport gap
    // BEFORE its own data (run_session's contract), and a later watchdog tick closes the freshness
    // episode once the newest observed data ts is current again. ----
    let fresh_ts = now_ms();
    let mut stream2 =
        ScriptedStream::from_json([book_frame_at(&[("60", "10")], &[("62", "5")], fresh_ts)]);
    stream2.push_timeout(); // transport-alive tick -> health.check_freshness -> Live (closes the stale episode)

    let mut wd2 = freshness_watchdog(&mut health);
    let _ =
        run_session(&mut stream2, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd2, || {});

    let calls = sink.calls();
    let kinds: Vec<&str> = calls.iter().map(|c| call_kind(c)).collect();
    assert_eq!(
        kinds,
        vec![
            "book_update",
            "book",
            "quote",
            "stale",
            "gap_start",
            "live",
            "book_update",
            "book",
            "quote",
            "live"
        ],
        "the coherent end-to-end sequence: stale opens -> transport gap opens (freshness episode \
         still open) -> reconnect's first frame closes the transport gap -> fresh data resumes -> a \
         later tick closes the freshness episode. Each data frame's recordable book_update leads its \
         folded book (Task 7). calls={calls:?}"
    );

    // the deterministic (non-timestamped) payloads are asserted exactly, same style as the
    // pump-behavior tests above.
    assert_eq!(
        calls[0],
        "book_update:polymarket:111:Snapshot:seq=1:bids=1:asks=1:local_ts_pos=true:tick=1"
    );
    assert_eq!(calls[1], "book:polymarket:111:Some(53.0)");
    assert_eq!(calls[2], "quote:polymarket:111:51/55:100x40");
    assert_eq!(
        calls[6],
        "book_update:polymarket:111:Snapshot:seq=2:bids=1:asks=1:local_ts_pos=true:tick=1"
    );
    assert_eq!(calls[7], "book:polymarket:111:Some(61.0)");
    assert_eq!(calls[8], "quote:polymarket:111:60/62:10x5");

    // THE key assertion: no orphaned/duplicated signal. Exactly one Stale, one GapStart, and TWO
    // Lives (one per episode) — never a third Live, never a lingering open episode.
    assert_eq!(calls.iter().filter(|c| c.contains("Stale")).count(), 1, "{calls:?}");
    assert_eq!(calls.iter().filter(|c| c.contains("GapStart")).count(), 1, "{calls:?}");
    assert_eq!(
        calls.iter().filter(|c| c.contains("Live")).count(),
        2,
        "one Live closes the transport gap, a second (distinct episode) closes the freshness gap: \
         {calls:?}"
    );
    assert!(!health.in_gap(), "the transport half of StreamHealth ended healthy");
    assert!(!health.is_stale(), "the freshness half of StreamHealth ended healthy");
    assert!(
        calls.last().unwrap().contains("Live"),
        "the sequence ends clean on a recovery signal, not an open episode: {calls:?}"
    );
}

// ---- net-hardening §B follow-up: PER-MODE data-freshness threshold -----------------------------
//
// Every freshness test above drives `PumpMode::Book` at the scripted `FRESHNESS_MS` (60s — the
// pump's ORIGINAL threshold, kept here for the reasons in that const's doc; production is now 300s).
// This section proves the per-mode WIRING: Book/Quotes and the far-sparser Trades lane each pick
// their OWN threshold, so `Trades` is built with `FRESHNESS_MS_TRADES` (600s here; 30 min in
// production). The relationship under test is `Book threshold < a Trades-mode pump's threshold`,
// which holds at both the scripted (60s < 600s) and production (300s < 1800s) values. Same Approach
// A as above: the test crafts each frame's wire `ts` relative to `now_ms()` and lets the tracker's
// own `now_ms() - newest_ts` arithmetic (already unit-tested in `vike-data`) do the judging; only
// the pump WIRING — which threshold a given mode's tracker was built with — is new here.

/// The crux proof: the SAME ~90s-old data frame trips `Stale` on a `Book`-mode pump (60s threshold)
/// but discloses NOTHING on a `Trades`-mode pump (600s threshold) — the freshness threshold is
/// driven by `PumpMode`, not one global constant.
#[test]
fn trades_mode_does_not_disclose_stale_at_an_age_that_would_trip_book_mode() {
    let stale_90s = now_ms() - 90_000; // > FRESHNESS_MS (60s) but < FRESHNESS_MS_TRADES (600s)

    // ---- Trades: the 90s-old last_trade is well within its OWN 600s threshold -> no Stale ------
    let mut stream =
        ScriptedStream::from_json([last_trade_frame_at("0.52", "3", Some("BUY"), stale_90s)]);
    stream.push_timeout(); // transport-alive tick -> health.check_freshness(now_ms())

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);
    let mut health = StreamHealth::new(FRESHNESS_MS_TRADES);
    let mut wd = Watchdog::with_timing(
        &mut health,
        "trades",
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let _ =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Trades, &mut state, &stop, &mut wd, || {});
    assert!(
        sink.calls().iter().all(|c| !c.contains("Stale")),
        "a 90s-old trade is well within the 600s trades threshold — no Stale disclosed: {:?}",
        sink.calls()
    );

    // ---- Book: the SAME 90s age, but past its tighter 60s threshold -> exactly one Stale --------
    let mut stream =
        ScriptedStream::from_json([book_frame_at(&[("51", "100")], &[("55", "40")], stale_90s)]);
    stream.push_timeout();

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);
    let mut health = StreamHealth::new(FRESHNESS_MS);
    let mut wd = freshness_watchdog(&mut health);
    let _ =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Book, &mut state, &stop, &mut wd, || {});
    assert_eq!(
        sink.calls().iter().filter(|c| c.contains("Stale")).count(),
        1,
        "the SAME 90s age trips Book mode's tighter 60s threshold — the mode drives the threshold: \
         {:?}",
        sink.calls()
    );
}

/// The `Trades` lane's looser threshold is not a disabled check: a `last_trade` frame stamped past
/// ITS OWN 600s threshold still discloses exactly one `Stale`.
#[test]
fn trades_mode_still_trips_stale_past_its_own_looser_threshold() {
    let stale_past_trades_threshold = now_ms() - 700_000; // > FRESHNESS_MS_TRADES (600s)
    let mut stream = ScriptedStream::from_json([last_trade_frame_at(
        "0.53",
        "1",
        None,
        stale_past_trades_threshold,
    )]);
    stream.push_timeout();

    let sink = RecordingSink::default();
    let mut state = TokenState::new(1.0);
    let stop = AtomicBool::new(false);
    let mut health = StreamHealth::new(FRESHNESS_MS_TRADES);
    let mut wd = Watchdog::with_timing(
        &mut health,
        "trades",
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let _ =
        run_session(&mut stream, &sink, TOKEN, PumpMode::Trades, &mut state, &stop, &mut wd, || {});

    assert_eq!(
        sink.calls().iter().filter(|c| c.contains("Stale")).count(),
        1,
        "a trades feed past its OWN looser threshold still trips exactly one Stale: {:?}",
        sink.calls()
    );
}
