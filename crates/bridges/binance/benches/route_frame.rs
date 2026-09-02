//! BASELINE for the family market-data decode path (perf-program measurement scaffolding —
//! audit finding: `route_frame` parses every WS frame into a `serde_json::Value` INTERMEDIATE
//! before the per-stream decoders pick fields out of it; typed borrow structs were estimated to
//! cut the parse 2–5×). This bench answers: **what does one combined-stream frame cost today,
//! per kind, through the REAL [`vike_binance::market_data::route_frame`]?** — the number any
//! typed-decode change must beat. Run it BEFORE and AFTER on the same box; no assertion gates
//! the timings (measurement, not a gate).
//!
//! Plain `main()` + `std::time::Instant`, mirroring `crates/vike-backtest/benches/engines.rs`
//! and `crates/vike-chart/benches/render.rs` (`harness = false`, no criterion dependency,
//! median-of-N after a warmup).
//!
//!   cargo bench -p vike-binance --bench route_frame
//!
//! Frames are SYNTHESIZED inline to the documented combined-stream wire shape (the same
//! `{"stream":…,"data":…}` envelope + field letters the module's own fixture tests pin, with the
//! full real payload field sets — `e`/`E`/`s` context fields included, since materializing them
//! into the `Value` is precisely the flagged cost). The crate's committed fixtures
//! (`tests/fixtures/captured/`) hold only sanitized USER-DATA exec frames (ws_accepted/ws_fill/…),
//! no market-data frames, so there is nothing real to load for this path. Every frame is DISTINCT
//! (varying prices/qtys/seqs) so the run never times one hot cached string.
//!
//! Three kinds, matching the three lanes the pump routes:
//!
//! - `<sym>@depth@100ms` diff frames, 10 bids + 10 asks (~20 levels) each, on a CONTIGUOUS
//!   `U`/`u` chain folding into a 1000-level/side seeded book — every frame takes the real
//!   `apply_depth_event` → `L2Book::apply_delta` Applied path (asserted), never the Stale/Gap
//!   early-outs.
//! - `<sym>@bookTicker` → `QuoteTick` decode.
//! - `<sym>@trade` → `TradeTick` decode.

use std::time::Instant;

use vike_binance::market_data::{route_frame, MdEvent};
use vike_model::{L2Book, Level};

const FRAMES: usize = 16_384;
const REPEATS: usize = 15;
const WARMUP: usize = 3; // discarded runs before timing (kills cold-cache effects)
const SYMBOL: &str = "BTCUSDT";
/// Levels PER SIDE in each depth diff frame (2×10 ≈ the "~20 levels" realistic diff).
const DEPTH_LEVELS_PER_SIDE: usize = 10;
/// Update-ids spanned per depth frame (`u - U + 1`) — sizes the contiguous chain.
const SEQ_SPAN: u64 = 20;

/// One `<sym>@depth@100ms` combined frame: full real payload fields (`e`/`E`/`s` context +
/// `U`/`u`/`b`/`a`), 10 bids + 10 asks, deterministic per-index price/qty wiggle (no RNG, so
/// BEFORE/AFTER runs are exactly comparable). Frame `k` spans update-ids
/// `[SEQ_SPAN·k + 1, SEQ_SPAN·(k+1)]` — contiguous with frame `k-1`'s `u`, so a book whose
/// `last_seq` starts at 0 folds every frame (the Applied path).
fn depth_frame(k: usize) -> String {
    let first_u = SEQ_SPAN * k as u64 + 1;
    let final_u = SEQ_SPAN * (k as u64 + 1);
    let mut bids = Vec::with_capacity(DEPTH_LEVELS_PER_SIDE);
    let mut asks = Vec::with_capacity(DEPTH_LEVELS_PER_SIDE);
    for lvl in 0..DEPTH_LEVELS_PER_SIDE {
        let qty = 0.001 + ((k * 31 + lvl * 7) % 997) as f64 * 0.001;
        let bid_px = 60_000.0 - lvl as f64 * 0.1 - (k % 50) as f64 * 0.1;
        let ask_px = 60_000.1 + lvl as f64 * 0.1 + (k % 50) as f64 * 0.1;
        bids.push(serde_json::json!([format!("{bid_px:.2}"), format!("{qty:.5}")]));
        asks.push(serde_json::json!([format!("{ask_px:.2}"), format!("{qty:.5}")]));
    }
    serde_json::json!({
        "stream": "btcusdt@depth@100ms",
        "data": {
            "e": "depthUpdate",
            "E": 1_700_000_000_000_i64 + k as i64,
            "s": SYMBOL,
            "U": first_u,
            "u": final_u,
            "b": bids,
            "a": asks
        }
    })
    .to_string()
}

/// One `<sym>@bookTicker` combined frame (full real payload: `u`/`s`/`b`/`B`/`a`/`A`).
fn book_ticker_frame(k: usize) -> String {
    let bid = 60_000.0 + (k % 500) as f64 * 0.1;
    let ask = bid + 0.1;
    serde_json::json!({
        "stream": "btcusdt@bookTicker",
        "data": {
            "u": 400_000_000_u64 + k as u64,
            "s": SYMBOL,
            "b": format!("{bid:.2}"),
            "B": format!("{:.5}", 1.0 + (k % 97) as f64 * 0.01),
            "a": format!("{ask:.2}"),
            "A": format!("{:.5}", 2.0 + (k % 89) as f64 * 0.01)
        }
    })
    .to_string()
}

/// One `<sym>@trade` combined frame (full real payload: `e`/`E`/`s`/`t`/`p`/`q`/`T`/`m`/`M`).
fn trade_frame(k: usize) -> String {
    let px = 60_000.0 + (k % 700) as f64 * 0.1;
    serde_json::json!({
        "stream": "btcusdt@trade",
        "data": {
            "e": "trade",
            "E": 1_700_000_000_000_i64 + k as i64,
            "s": SYMBOL,
            "t": 3_000_000_000_u64 + k as u64,
            "p": format!("{px:.2}"),
            "q": format!("{:.5}", 0.001 + (k % 61) as f64 * 0.001),
            "T": 1_700_000_000_000_i64 + k as i64,
            "m": k.is_multiple_of(2),
            "M": true
        }
    })
    .to_string()
}

/// Run `f` (WARMUP + REPEATS) times, returning only the timed (post-warmup) elapsed-ns samples.
/// `black_box`ed so the optimizer can't prove `f`'s result is unused and elide the work.
/// (Mirrors `vike-chart/benches/render.rs`.)
fn time_it<T>(reps: usize, warmup: usize, mut f: impl FnMut() -> T) -> Vec<u128> {
    let mut times = Vec::with_capacity(reps);
    for k in 0..(warmup + reps) {
        let t0 = Instant::now();
        let out = f();
        let dt = t0.elapsed().as_nanos();
        std::hint::black_box(out);
        if k >= warmup {
            times.push(dt);
        }
    }
    times
}

fn median(mut t: Vec<u128>) -> u128 {
    t.sort_unstable();
    t[t.len() / 2]
}

fn row(label: &str, times: &[u128], frames: usize, bytes_total: usize) {
    let med = median(times.to_vec()) as f64;
    let ns_per_frame = med / frames as f64;
    let frames_per_s = 1e9 / ns_per_frame;
    let mb_per_s = (bytes_total as f64 / frames as f64) * frames_per_s / 1e6;
    println!(
        "{label:<24}{frames:>8}{:>13.0}{ns_per_frame:>12.1}{frames_per_s:>13.0}{mb_per_s:>10.1}",
        bytes_total as f64 / frames as f64
    );
}

fn main() {
    println!(
        "=== vike-binance route_frame bench — serde_json::Value-intermediate decode baseline ==="
    );
    println!(
        "{FRAMES} distinct frames/kind, {REPEATS} timed reps ({WARMUP} warmup discarded), median reported\n"
    );

    let depth_frames: Vec<String> = (0..FRAMES).map(depth_frame).collect();
    let quote_frames: Vec<String> = (0..FRAMES).map(book_ticker_frame).collect();
    let trade_frames: Vec<String> = (0..FRAMES).map(trade_frame).collect();
    let bytes = |v: &[String]| v.iter().map(String::len).sum::<usize>();

    // A realistically DEEP book to fold diffs into: 1000 levels/side, the spot snapshot
    // `limit=1000` shape (`BTreeMap` insert cost depends on the standing depth).
    let seed_bids: Vec<Level> = (0..1000).map(|i| (60_000.0 - i as f64 * 0.1, 1.0)).collect();
    let seed_asks: Vec<Level> = (0..1000).map(|i| (60_000.1 + i as f64 * 0.1, 1.0)).collect();
    let mut book = L2Book::new(0.01);

    // Correctness pre-check OUTSIDE the timed loops: one frame of each kind must route to the
    // expected lane, and depth frames 0→1 must chain (Applied twice) — so the timed loops below
    // provably measure the real decode/fold, not the Ignored/Stale early-outs.
    book.apply_snapshot(0, &seed_bids, &seed_asks);
    assert!(
        matches!(route_frame(&depth_frames[0], SYMBOL, &mut book), MdEvent::BookUpdated),
        "depth frame 0 must fold (Applied)"
    );
    assert!(
        matches!(route_frame(&depth_frames[1], SYMBOL, &mut book), MdEvent::BookUpdated),
        "depth frame 1 must chain off frame 0 (contiguous U/u)"
    );
    match route_frame(&quote_frames[0], SYMBOL, &mut book) {
        MdEvent::Quote(q) => assert_eq!(q.symbol, SYMBOL),
        other => panic!("expected Quote, got {other:?}"),
    }
    match route_frame(&trade_frames[0], SYMBOL, &mut book) {
        MdEvent::Trade(t) => assert!(t.price > 0.0),
        other => panic!("expected Trade, got {other:?}"),
    }

    // (a) depth diffs: reset the U/u chain each rep via a re-snapshot (seq back to 0 — amortized
    // ~0.3% of a rep, constant across BEFORE/AFTER runs), then fold all FRAMES diffs. Every frame
    // must take the Applied path — asserted per rep, so a chain bug can't silently turn this into
    // an early-out benchmark.
    let t_depth = time_it(REPEATS, WARMUP, || {
        book.apply_snapshot(0, &seed_bids, &seed_asks);
        let mut applied = 0usize;
        for f in &depth_frames {
            if matches!(route_frame(f, SYMBOL, &mut book), MdEvent::BookUpdated) {
                applied += 1;
            }
        }
        assert_eq!(applied, depth_frames.len(), "every depth frame must fold (Applied)");
        book.last_seq
    });

    // (b) bookTicker → QuoteTick (stateless decode; the QuoteTick String alloc is part of the
    // real path). Folding `bid` into an accumulator keeps the decode observable.
    let t_quote = time_it(REPEATS, WARMUP, || {
        let mut acc = 0.0_f64;
        for f in &quote_frames {
            if let MdEvent::Quote(q) = route_frame(f, SYMBOL, &mut book) {
                acc += q.bid;
            }
        }
        acc
    });

    // (c) trade → TradeTick (stateless decode).
    let t_trade = time_it(REPEATS, WARMUP, || {
        let mut acc = 0.0_f64;
        for f in &trade_frames {
            if let MdEvent::Trade(t) = route_frame(f, SYMBOL, &mut book) {
                acc += t.price;
            }
        }
        acc
    });

    // ============================ REPORT ============================
    println!(
        "{:<24}{:>8}{:>13}{:>12}{:>13}{:>10}",
        "kind", "frames", "avg B/frame", "ns/frame", "frames/s", "MB/s"
    );
    row(&format!("depth({}x2 lvl)", DEPTH_LEVELS_PER_SIDE), &t_depth, FRAMES, bytes(&depth_frames));
    row("bookTicker", &t_quote, FRAMES, bytes(&quote_frames));
    row("trade", &t_trade, FRAMES, bytes(&trade_frames));
    println!(
        "\nns/frame = whole route_frame call: serde_json::Value parse + stream-suffix routing +\n\
         per-kind decode (depth rows additionally fold the diff into a 1000-level/side book)."
    );
}
