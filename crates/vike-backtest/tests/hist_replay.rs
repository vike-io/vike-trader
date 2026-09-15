//! Tick-replay Phase 1 loader gate: `replay_ticks` (and `replay_ticks_streaming`) over a real
//! (temp-dir) `DataFusionHist`. `#![cfg(feature = "datafusion-store")]` — this file builds a
//! concrete `DataFusionHist` fixture (and exercises `replay_ticks_streaming`, itself behind that
//! feature), so it needs the concrete backend, NOT just the trait-only `hist-replay`.
#![cfg(feature = "datafusion-store")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use vike_backtest::{
    BacktestResult, EngineParams, ReplayError, Strategy, TickReplayConfig, replay_ticks,
    replay_ticks_streaming,
};
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::{
    BookUpdate, BookUpdateKind, Broker, L2Book, QuoteTick, SymbolProperties, TradeTick,
};

const VENUE: &str = "polymarket";
const SYMBOL: &str = "TKN";

fn quote(ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: String::new() }
}

fn trade(ts: i64, price: f64) -> TradeTick {
    TradeTick { ts, local_ts: 0, price, size: 1.0, is_buyer_maker: false, symbol: String::new() }
}

/// Seeds a temp `DataFusionHist` with a known 4-quote + 2-trade sequence for `(VENUE, SYMBOL)`.
/// ts-order once merged (quote-before-trade tie-break, R4): 1000Q,1500T,2000Q,3000Q,3500T,4000Q.
/// Every price is a half-integer so every intermediate f64 (mid, cash deltas, equity) is EXACT —
/// the hand-computed expectations below need no epsilon.
fn seed_store() -> (tempfile::TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let quotes = vec![
        quote(1000, 10.0, 11.0), // mid 10.5
        quote(2000, 12.0, 13.0), // mid 12.5
        quote(3000, 14.0, 15.0), // mid 14.5
        quote(4000, 16.0, 17.0), // mid 16.5
    ];
    let trades = vec![trade(1500, 11.5), trade(3500, 15.5)];
    df.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();
    df.append_trades(VENUE, SYMBOL, &trades, None).unwrap();
    (dir, df)
}

/// Submits ONE market buy (qty 1) on the very first `on_quote_tick`, then holds forever.
#[derive(Default)]
struct BuyOnceThenHold {
    bought: bool,
}

impl<B: Broker> Strategy<B> for BuyOnceThenHold {
    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        if !self.bought {
            self.bought = true;
            broker.submit_market(&q.symbol, 1, 1.0);
        }
    }
}

fn base_cfg(range: TsRange) -> TickReplayConfig {
    TickReplayConfig {
        venue: VENUE.to_string(),
        symbols: vec![SYMBOL.to_string()],
        range,
        seed_bar_interval_ms: None,
        params: EngineParams { cash: 1000.0, ..Default::default() },
        snap_to_properties: false,
        feed_latency: false,
        series: None,
    }
}

#[test]
fn snap_to_properties_gates_the_fill_when_opted_in() {
    // Record a PIT grid with an impossible min_qty (in effect from ts 0, so it covers every tick).
    let (_dir, df) = seed_store();
    df.append_symbol_properties(
        VENUE,
        SYMBOL,
        &[(0, SymbolProperties { min_qty: 1e9, ..Default::default() })],
        None,
    )
    .unwrap();

    // snap OFF (default): raw replay — the qty-1 opening buy fills, so equity moves off starting cash.
    let mut cfg_off = base_cfg(TsRange::all());
    cfg_off.snap_to_properties = false;
    let off = replay_ticks(df.clone(), BuyOnceThenHold::default(), cfg_off).unwrap();
    assert_ne!(off.final_equity, 1000.0, "snap-off must fill the buy (raw replay)");

    // snap ON: the opening fill is below the recorded min_qty → gated → no fill → equity untouched.
    let mut cfg_on = base_cfg(TsRange::all());
    cfg_on.snap_to_properties = true;
    let on = replay_ticks(df.clone(), BuyOnceThenHold::default(), cfg_on).unwrap();
    assert_eq!(on.final_equity, 1000.0, "snap-on must gate the sub-min_qty opening buy (no fill)");
}

#[test]
fn buy_once_then_hold_matches_hand_computed_result() {
    let (_dir, df) = seed_store();
    let cfg = base_cfg(TsRange::all());

    let result: BacktestResult = replay_ticks(df.clone(), BuyOnceThenHold::default(), cfg).unwrap();

    // Hand-computed walk (see module doc for the full tick-by-tick derivation):
    //  i=0 Quote@1000 (bid 10.0/ask 11.0) -> BuyOnceThenHold submits a 1-unit market buy (pending).
    //  i=1 Trade@1500 price 11.5 -> TickFillModel has no bid/ask on a trade bar, so it falls back
    //      to order_fill_price: Market -> bar.open == the trade price (11.5). Position opens
    //      size=1.0 @ avg_price=11.5; cash = 1000.0 - 1.0*11.5 = 988.5 (fee_rate defaults to 0).
    //  i=2..5: quotes/trades only mark price (12.5, 14.5, 15.5, 16.5) — no more submits (bought).
    // The position is OPENED but never CLOSED -> apply_fill's FillKind::Open path never appends to
    // `trades` (only Reduce/Close/Flip do), so n_trades stays 0 despite one real fill.
    assert_eq!(result.n_trades, 0, "opening a position alone books no CLOSED trade");
    assert!(result.trades.is_empty());
    // final_equity = cash + pos.size * last_price * multiplier = 988.5 + 1.0*16.5*1.0
    assert_eq!(result.final_equity, 1005.0);
}

#[test]
fn replay_produces_empty_result_on_empty_window() {
    let (_dir, df) = seed_store();
    // Entirely after the fixture's last tick (ts=4000) -> both scans come back empty -> the lone
    // symbol is sparse-skipped (tracing::warn!) -> run_ticks([]) -> empty-but-valid result.
    let cfg = base_cfg(TsRange::of(10_000, 20_000));

    let result = replay_ticks(df.clone(), BuyOnceThenHold::default(), cfg).unwrap();

    assert_eq!(result.n_trades, 0);
    assert!(result.trades.is_empty());
    assert!(result.equity_curve.is_empty());
    assert_eq!(result.final_equity, 1000.0, "no fills -> final_equity == the seeded cash");
}

/// `seed_bar_interval_ms: Some(0)` must be rejected with `ReplayError::InvalidBarInterval(0)`
/// rather than panicking. Without the `replay_ticks_core` guard, `consolidate_quotes` would
/// compute `ts.rem_euclid(0)` for the fixture's first quote (ts=1000) — confirmed separately that
/// `1000i64.rem_euclid(0)` panics ("attempt to calculate the remainder with a divisor of zero"),
/// which is exactly the panic this validation exists to prevent.
#[test]
fn seed_bar_interval_zero_returns_error() {
    let (_dir, df) = seed_store();
    let mut cfg = base_cfg(TsRange::all());
    cfg.seed_bar_interval_ms = Some(0);

    let err = replay_ticks(df.clone(), BuyOnceThenHold::default(), cfg).unwrap_err();
    match err {
        ReplayError::InvalidBarInterval(step) => assert_eq!(step, 0),
        other => panic!("expected InvalidBarInterval(0), got {other:?}"),
    }
}

/// Two-symbol bar-seeded replay where `consolidate_quotes` (step=1000ms) produces DIFFERENT bar
/// counts per symbol: `SYM_A`'s 3 quotes land in 3 distinct 1000ms buckets (3 bars), `SYM_B`'s 3
/// quotes all land in the same bucket (1 bar). Without the `hist_replay::replay_ticks` guard,
/// handing `bars_by_symbol = [(SYM_A, 3 bars), (SYM_B, 1 bar)]` straight to `StrategyEngine::new`
/// trips its `assert!(lengths.len() <= 1, "all symbol series must have the same length
/// (aligned)")` (engine.rs) — a hard panic, not a catchable error. This test asserts
/// `replay_ticks` instead returns `Err(ReplayError::MisalignedSeededBars { .. })`.
#[test]
fn multi_symbol_seeded_returns_misaligned_error() {
    const SYM_A: &str = "AAA";
    const SYM_B: &str = "BBB";

    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());

    // SYM_A: buckets at 1000/2000/3000 -> 3 bars.
    let quotes_a = vec![quote(1000, 10.0, 11.0), quote(2000, 12.0, 13.0), quote(3000, 14.0, 15.0)];
    // SYM_B: all three ticks land inside the same [1000,2000) bucket -> 1 bar.
    let quotes_b = vec![quote(1000, 20.0, 21.0), quote(1100, 20.0, 21.0), quote(1200, 20.0, 21.0)];
    df.append_quotes(VENUE, SYM_A, &quotes_a, None).unwrap();
    df.append_quotes(VENUE, SYM_B, &quotes_b, None).unwrap();

    let cfg = TickReplayConfig {
        venue: VENUE.to_string(),
        symbols: vec![SYM_A.to_string(), SYM_B.to_string()],
        range: TsRange::all(),
        seed_bar_interval_ms: Some(1000),
        params: EngineParams { cash: 1000.0, ..Default::default() },
        snap_to_properties: false,
        feed_latency: false,
        series: None,
    };

    let err = replay_ticks(df.clone(), BuyOnceThenHold::default(), cfg).unwrap_err();
    match err {
        ReplayError::MisalignedSeededBars { lengths } => {
            assert_eq!(lengths, vec![3, 1], "SYM_A -> 3 bars, SYM_B -> 1 bar");
        }
        other => panic!("expected MisalignedSeededBars, got {other:?}"),
    }
}

/// `replay_ticks` (bounded `HistStore` scan) and `replay_ticks_streaming` (concrete
/// `DataFusionHist` stream-collect) must land on the SAME `BacktestResult` for a well-formed
/// store — they only differ in how each symbol's quote/trade rows are obtained (R5); everything
/// downstream (`merge_quote_trade`, the optional bar seed, `StrategyEngine::run_ticks`) is the one
/// shared `replay_ticks_core`. Reuses `seed_store`'s known 4-quote + 2-trade fixture and the same
/// `BuyOnceThenHold` reference strategy as the bounded-path test above.
#[test]
fn streaming_equals_bounded_bit_eq() {
    let (_dir, df) = seed_store();

    let bounded =
        replay_ticks(df.clone(), BuyOnceThenHold::default(), base_cfg(TsRange::all())).unwrap();
    let streaming =
        replay_ticks_streaming(df.clone(), BuyOnceThenHold::default(), base_cfg(TsRange::all()))
            .unwrap();

    assert_eq!(
        bounded.final_equity.to_bits(),
        streaming.final_equity.to_bits(),
        "final_equity must be bit-identical between the bounded and streaming loaders"
    );
    assert_eq!(bounded.n_trades, streaming.n_trades);
    assert_eq!(bounded.trades, streaming.trades, "Trade derives PartialEq — compare directly");
}

/// A `#[cfg(test)]`-only strategy that, on the FIRST tick it sees (any tick type), records
/// `broker.index()` and `broker.bars(symbol).len()` into a shared cell — used to observe whether
/// R6 bar-context seeding actually threads a non-empty closed-bar slice through to the strategy
/// under `run_ticks`. `Rc<RefCell<_>>` (not `Arc<Mutex<_>>`) is fine: `StrategyEngine` runs the
/// strategy single-threaded.
#[derive(Clone, Default)]
struct RecordFirstTickContext {
    seen: Rc<RefCell<Option<(usize, usize)>>>,
}

impl<B: Broker> Strategy<B> for RecordFirstTickContext {
    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        let mut seen = self.seen.borrow_mut();
        if seen.is_none() {
            *seen = Some((broker.index(), broker.bars(&q.symbol).len()));
        }
    }
}

/// R6: `seed_bar_interval_ms: Some(60_000)` over a multi-minute single-symbol quote fixture must
/// give the strategy a NON-EMPTY closed-bar slice on its very first tick (documents that seeding
/// threads through `replay_ticks` into `StrategyEngine::new`'s `bars_by_symbol` and is visible via
/// `Broker::bars`). Per the brief: assert only that bars are non-empty — the exact `index()` value
/// under `run_ticks` (here, the tick-loop counter, not a bar-aligned index) is Phase 2's concern.
#[test]
fn seed_bar_interval_gives_strategy_closed_bars() {
    const SEED_VENUE: &str = "polymarket";
    const SEED_SYMBOL: &str = "SEEDTKN";

    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    // 5 quotes, one per distinct 60_000ms bucket -> 5 consolidated bars once seeded, spanning
    // several minutes (0, 1, 2, 3, 4 minutes).
    let quotes: Vec<QuoteTick> =
        (0..5i64).map(|k| quote(k * 60_000, 10.0 + k as f64, 11.0 + k as f64)).collect();
    df.append_quotes(SEED_VENUE, SEED_SYMBOL, &quotes, None).unwrap();

    let cfg = TickReplayConfig {
        venue: SEED_VENUE.to_string(),
        symbols: vec![SEED_SYMBOL.to_string()],
        range: TsRange::all(),
        seed_bar_interval_ms: Some(60_000),
        params: EngineParams { cash: 1000.0, ..Default::default() },
        snap_to_properties: false,
        feed_latency: false,
        series: None,
    };

    let strategy = RecordFirstTickContext::default();
    let seen_cell = Rc::clone(&strategy.seen);
    let _result = replay_ticks(df.clone(), strategy, cfg).unwrap();

    let (_index, bars_len) =
        seen_cell.borrow().expect("strategy should have observed at least one tick");
    // Per the brief: assert ONLY that bars are non-empty. `_index` (the `run_ticks` tick-loop
    // counter, not a bar-aligned index) is recorded for visibility but deliberately not asserted
    // on — its exact semantics are Phase 2's concern.
    assert!(bars_len > 0, "seed_bar_interval_ms should give the strategy >=1 closed bar, saw 0");
}

fn bu(
    ts: i64,
    seq: u64,
    kind: BookUpdateKind,
    bids: Vec<(f64, f64)>,
    asks: Vec<(f64, f64)>,
) -> BookUpdate {
    BookUpdate {
        ts,
        local_ts: 0,
        seq,
        kind,
        tick_size: 0.01,
        bids,
        asks,
        symbol: SYMBOL.to_string(),
    }
}

/// Top-of-book prices survive a tick-quantization round-trip (`price -> i64 tick -> price`), so
/// compare within a tick-scale epsilon rather than by exact bits (mirrors `tests/book_replay.rs`).
fn top_eq(got: (f64, f64), want: (f64, f64)) -> bool {
    (got.0 - want.0).abs() < 1e-9 && (got.1 - want.1).abs() < 1e-9
}

/// Submits ONE market buy on the first quote tick (like `BuyOnceThenHold`) AND records every
/// `on_order_book` top into a shared cell (like `book_replay.rs`'s `BookSpy`) — combined so one
/// `replay_ticks` call proves BOTH halves of the closed loop: the quote path still drives a
/// non-empty equity curve, and the recorded book series reaches `Strategy::on_order_book`.
#[derive(Clone, Default)]
struct BuyOnceAndSpyBook {
    bought: bool,
    tops: Rc<RefCell<Vec<(f64, f64)>>>,
}

impl<B: Broker> Strategy<B> for BuyOnceAndSpyBook {
    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        if !self.bought {
            self.bought = true;
            broker.submit_market(&q.symbol, 1, 1.0);
        }
    }

    fn on_order_book(&mut self, _broker: &mut B, book: &L2Book) {
        if let (Some((bb, _)), Some((ba, _))) = (book.best_bid(), book.best_ask()) {
            self.tops.borrow_mut().push((bb, ba));
        }
    }
}

/// End-to-end: record book events (`append_book_updates`) alongside quotes (`append_quotes`) into
/// a temp `DataFusionHist`, run `replay_ticks`, and assert BOTH halves of the store -> merge ->
/// strategy loop close: `on_order_book` fires with the recorded book tops, AND the quote path
/// still produces a non-empty equity curve (Task 9 — closes the loop opened by Task 5's
/// `scan_book_updates` and Task 8's `Tick::Book` fold).
#[test]
fn book_and_quote_replay_close_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());

    let quotes = vec![quote(1000, 10.0, 11.0), quote(2000, 12.0, 13.0)];
    df.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();

    let books = vec![
        bu(500, 1, BookUpdateKind::Snapshot, vec![(0.45, 10.0)], vec![(0.47, 5.0)]),
        bu(1500, 2, BookUpdateKind::Delta, vec![(0.46, 3.0)], vec![]), // best bid -> 0.46
    ];
    df.append_book_updates(VENUE, SYMBOL, &books, None).unwrap();

    let cfg = base_cfg(TsRange::all());
    let strategy = BuyOnceAndSpyBook::default();
    let tops = Rc::clone(&strategy.tops);

    let result = replay_ticks(df.clone(), strategy, cfg).unwrap();

    // Quote path still drives equity: 1000Q, 2000Q -> two equity-curve points regardless of the
    // interleaved book events (book ticks are intercepted before the price/equity path).
    assert!(!result.equity_curve.is_empty(), "quote path should still produce an equity curve");
    assert_eq!(
        result.equity_curve.len(),
        2,
        "one equity point per quote tick, book ticks excluded"
    );

    // Book path: snapshot anchor, then the in-sequence delta raising the best bid.
    let seen = tops.borrow();
    assert_eq!(seen.len(), 2, "expected snapshot + in-sequence delta delivery, got {seen:?}");
    assert!(top_eq(seen[0], (0.45, 0.47)), "anchor top, got {:?}", seen[0]);
    assert!(top_eq(seen[1], (0.46, 0.47)), "post-delta top, got {:?}", seen[1]);
}

// ---------------------------------------------------------------------------------------------
// Feed-latency replay (`TickReplayConfig::feed_latency`) — the recorded `local_ts` delivery lane.
// Default OFF is byte-identical; ON re-orders DELIVERY only, never a tick's venue `ts`.
// ---------------------------------------------------------------------------------------------

/// `quote` with an explicit machine receive stamp.
fn quote_lt(ts: i64, local_ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { local_ts, ..quote(ts, bid, ask) }
}

/// Records the VENUE ts of every quote the strategy is handed, in DELIVERY order — the exact
/// observable feed-latency mode changes (and must leave untouched when off).
#[derive(Clone, Default)]
struct RecordQuoteOrder {
    seen: Rc<RefCell<Vec<i64>>>,
}

impl<B: Broker> Strategy<B> for RecordQuoteOrder {
    fn on_quote_tick(&mut self, _broker: &mut B, q: &QuoteTick) {
        self.seen.borrow_mut().push(q.ts);
    }
}

/// Three quotes whose ARRIVAL order differs from their venue order: the 1000-stamped quote was
/// received very late (5000), the other two roughly on time. Venue order 1000/2000/3000; arrival
/// order 2000 (2100), 3000 (3100), 1000 (5000).
fn seed_lagging_store() -> (tempfile::TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let quotes = vec![
        quote_lt(1000, 5000, 10.0, 11.0),
        quote_lt(2000, 2100, 12.0, 13.0),
        quote_lt(3000, 3100, 14.0, 15.0),
    ];
    df.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();
    (dir, df)
}

/// Replay a store through `RecordQuoteOrder` and return the delivery order of the venue stamps.
fn delivered_order(df: &Arc<DataFusionHist>, feed_latency: bool) -> Vec<i64> {
    let mut cfg = base_cfg(TsRange::all());
    cfg.feed_latency = feed_latency;
    let strategy = RecordQuoteOrder::default();
    let seen = Rc::clone(&strategy.seen);
    let _ = replay_ticks(df.clone(), strategy, cfg).unwrap();
    // Name the guard: as an unnamed tail temporary it would outlive `seen` (E0597). Declared
    // after `seen`, it drops first (reverse declaration order), so the borrow ends cleanly.
    let order = seen.borrow();
    order.to_vec()
}

/// (a) OFF (the default) IGNORES `local_ts` entirely: delivery stays the frozen venue order, and
/// the `BacktestResult` over a STAMPED tape is bit-identical to the same tape with every stamp
/// zeroed — i.e. the column cannot move a single bit while the flag is off.
#[test]
fn feed_latency_off_is_byte_identical() {
    let (_dir, lagging) = seed_lagging_store();

    // Same prices/ts, every local_ts zeroed — the pre-`local_ts` shape of the same tape.
    let dir2 = tempfile::tempdir().unwrap();
    let unstamped = Arc::new(DataFusionHist::open(dir2.path()).unwrap());
    let plain = vec![quote(1000, 10.0, 11.0), quote(2000, 12.0, 13.0), quote(3000, 14.0, 15.0)];
    unstamped.append_quotes(VENUE, SYMBOL, &plain, None).unwrap();

    assert_eq!(
        delivered_order(&lagging, false),
        vec![1000, 2000, 3000],
        "off must deliver in venue order regardless of the recorded receive stamps"
    );

    let mut cfg_off = base_cfg(TsRange::all());
    cfg_off.feed_latency = false;
    let stamped_run = replay_ticks(lagging.clone(), BuyOnceThenHold::default(), cfg_off).unwrap();
    let plain_run =
        replay_ticks(unstamped.clone(), BuyOnceThenHold::default(), base_cfg(TsRange::all()))
            .unwrap();

    assert_eq!(
        stamped_run.final_equity.to_bits(),
        plain_run.final_equity.to_bits(),
        "off: the recorded local_ts column must not move a single bit of the result"
    );
    assert_eq!(stamped_run.n_trades, plain_run.n_trades);
    assert_eq!(stamped_run.trades, plain_run.trades);
    assert_eq!(stamped_run.equity_curve, plain_run.equity_curve);
    assert_eq!(stamped_run.equity_ts, plain_run.equity_ts);
}

/// (b) ON with a lagging stamp: the strategy observes the tick that ARRIVED first first, even
/// though the venue stamped it later. The observed values are still the VENUE stamps — the mode
/// re-sequences delivery, it never rewrites a tick's `ts` (which is what matching runs on).
#[test]
fn feed_latency_on_delivers_in_arrival_order() {
    let (_dir, df) = seed_lagging_store();
    assert_eq!(
        delivered_order(&df, true),
        vec![2000, 3000, 1000],
        "on: delivery follows the recorded receive stamps (the 5000 arrival lands last)"
    );
}

/// (c) A tape with NO stamps (`local_ts == 0` — fixtures, backfill, pre-`local_ts` parquet parts)
/// falls back to the venue clock, so turning the mode ON changes nothing: bit-identical.
#[test]
fn feed_latency_on_unstamped_tape_falls_back_to_venue_order() {
    let (_dir, df) = seed_store(); // every local_ts == 0

    let mut cfg_on = base_cfg(TsRange::all());
    cfg_on.feed_latency = true;
    let on = replay_ticks(df.clone(), BuyOnceThenHold::default(), cfg_on).unwrap();
    let off =
        replay_ticks(df.clone(), BuyOnceThenHold::default(), base_cfg(TsRange::all())).unwrap();

    assert_eq!(
        on.final_equity.to_bits(),
        off.final_equity.to_bits(),
        "an unstamped tape must replay identically in either mode"
    );
    assert_eq!(on.equity_curve, off.equity_curve);
    assert_eq!(on.equity_ts, off.equity_ts);
    // ...and it is still the hand-computed value the venue-ordered gate above pins.
    assert_eq!(on.final_equity, 1005.0);
}

/// (d) CLOCK SKEW (`local_ts < ts` — physically impossible) is CLAMPED UP to the venue ts, so a
/// skewed tick is never promoted ahead of an earlier one. Documented in the module doc.
#[test]
fn feed_latency_clamps_skewed_stamps() {
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    // Every stamp claims arrival BEFORE the venue stamped it, and in an order that would REVERSE
    // the tape if the skewed values were trusted verbatim.
    let quotes = vec![
        quote_lt(1000, 10, 10.0, 11.0),
        quote_lt(2000, 20, 12.0, 13.0),
        quote_lt(3000, 30, 14.0, 15.0),
    ];
    df.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();

    assert_eq!(
        delivered_order(&df, true),
        vec![1000, 2000, 3000],
        "a skewed local_ts clamps to its venue ts — never delivered early"
    );
}
