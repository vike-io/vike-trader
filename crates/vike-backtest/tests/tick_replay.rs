//! Tick-replay Phase 1 loader gate: `replay_ticks` over a real (temp-dir) `HistoryStoreDataFusion`.
//! This file builds a concrete `HistoryStoreDataFusion` fixture (the `vike-store` dev dependency),
//! so it needs the concrete backend, NOT just the trait-only double.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use vike_analytics::TradingRecord;
use vike_analytics::synthetic::{SyntheticOrderBookModel, order_book_stream};
use vike_backtest::run::tick_replay::{
    ProfileDataMark, ProfileDataReference, SeriesKind, TickReplayConfig, TickReplayError,
    replay_ticks,
};
use vike_data::{HistoryStore, HistoryStoreSynthetic, SYNTHETIC_VENUE, TimestampRange};
use vike_marketdata::test_support::quote;
use vike_marketdata::{L2Book, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_model::{OrderBroker, OrderPlacement, Strategy, SymbolProperties};
use vike_simulator::StrategyEngineConfig;
use vike_store::HistoryStoreDataFusion;

const VENUE: &str = "polymarket";
const SYMBOL: &str = "TKN";

fn trade(ts: i64, price: f64) -> TickTrade {
    TickTrade {
        timestamp: ts,
        local_timestamp: 0,
        price,
        size: 1.0,
        is_buyer_maker: false,
        symbol: Default::default(),
    }
}

/// Seeds a temp `HistoryStoreDataFusion` with a known 4-quote + 2-trade sequence for `(VENUE, SYMBOL)`.
/// ts-order once merged (quote-before-trade tie-break, R4): 1000Q,1500T,2000Q,3000Q,3500T,4000Q.
/// Every price is a half-integer so every intermediate f64 (mid, cash deltas, equity) is EXACT —
/// the hand-computed expectations below need no epsilon.
fn seed_store() -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
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
#[derive(Clone, Default)]
struct BuyOnceThenHold {
    bought: bool,
}

impl<B: OrderBroker> Strategy<B> for BuyOnceThenHold {
    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        if !self.bought {
            self.bought = true;
            broker.place(&q.symbol, "", 1, 1.0, OrderPlacement::Market, false);
        }
    }
}

fn base_config(range: TimestampRange) -> TickReplayConfig {
    TickReplayConfig {
        venue: VENUE.to_string(),
        symbols: vec![SYMBOL.to_string()],
        range,
        seed_bar_interval_ms: None,
        params: StrategyEngineConfig { cash: 1000.0, ..Default::default() },
        snap_to_properties: false,
        feed_latency: false,
        series: None,
        reference: Vec::new(),
        mark: Vec::new(),
        feed_status: false,
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
    let mut config_off = base_config(TimestampRange::all());
    config_off.snap_to_properties = false;
    let off = replay_ticks(df.clone(), BuyOnceThenHold::default(), config_off).unwrap();
    assert_ne!(off.final_equity, 1000.0, "snap-off must fill the buy (raw replay)");

    // snap ON: the opening fill is below the recorded min_qty → gated → no fill → equity untouched.
    let mut config_on = base_config(TimestampRange::all());
    config_on.snap_to_properties = true;
    let on = replay_ticks(df.clone(), BuyOnceThenHold::default(), config_on).unwrap();
    assert_eq!(on.final_equity, 1000.0, "snap-on must gate the sub-min_qty opening buy (no fill)");
}

#[test]
fn buy_once_then_hold_matches_hand_computed_result() {
    let (_dir, df) = seed_store();
    let config = base_config(TimestampRange::all());

    let result: TradingRecord =
        replay_ticks(df.clone(), BuyOnceThenHold::default(), config).unwrap();

    // Hand-computed walk (see module doc for the full tick-by-tick derivation):
    //  i=0 Quote@1000 (bid 10.0/ask 11.0) -> BuyOnceThenHold submits a 1-unit market buy (pending).
    //  i=1 Trade@1500 price 11.5 -> FillModelTick has no bid/ask on a trade bar, so it falls back
    //      to order_fill_price: Market -> bar.open == the trade price (11.5). Position opens
    //      size=1.0 @ average_price=11.5; cash = 1000.0 - 1.0*11.5 = 988.5 (fee_rate defaults to 0).
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
    let config = base_config(TimestampRange::of(10_000, 20_000));

    let result = replay_ticks(df.clone(), BuyOnceThenHold::default(), config).unwrap();

    assert_eq!(result.n_trades, 0);
    assert!(result.trades.is_empty());
    assert!(result.equity_curve.is_empty());
    assert_eq!(result.final_equity, 1000.0, "no fills -> final_equity == the seeded cash");
}

/// `seed_bar_interval_ms: Some(0)` must be rejected with `TickReplayError::InvalidBarInterval(0)`
/// rather than panicking. Without the `refuse_unrunnable` guard, `consolidate_quotes` would
/// compute `ts.rem_euclid(0)` for the fixture's first quote (ts=1000) — confirmed separately that
/// `1000i64.rem_euclid(0)` panics ("attempt to calculate the remainder with a divisor of zero"),
/// which is exactly the panic this validation exists to prevent.
#[test]
fn seed_bar_interval_zero_returns_error() {
    let (_dir, df) = seed_store();
    let mut config = base_config(TimestampRange::all());
    config.seed_bar_interval_ms = Some(0);

    let err = replay_ticks(df.clone(), BuyOnceThenHold::default(), config).unwrap_err();
    match err {
        TickReplayError::InvalidBarInterval(step) => assert_eq!(step, 0),
        other => panic!("expected InvalidBarInterval(0), got {other:?}"),
    }
}

/// Two-symbol bar-seeded replay where `consolidate_quotes` (step=1000ms) produces DIFFERENT bar
/// counts per symbol: `SYMBOL_A`'s 3 quotes land in 3 distinct 1000ms buckets (3 bars), `SYMBOL_B`'s 3
/// quotes all land in the same bucket (1 bar). Without the `run::tick_replay::replay_ticks` guard,
/// handing `bars_by_symbol = [(SYMBOL_A, 3 bars), (SYMBOL_B, 1 bar)]` straight to `StrategyEngine::new`
/// trips its `assert!(lengths.len() <= 1, "all symbol series must have the same length
/// (aligned)")` (engine.rs) — a hard panic, not a catchable error. This test asserts
/// `replay_ticks` instead returns `Err(TickReplayError::MisalignedSeededBars { .. })`.
#[test]
fn multi_symbol_seeded_returns_misaligned_error() {
    const SYMBOL_A: &str = "AAA";
    const SYMBOL_B: &str = "BBB";

    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());

    // SYMBOL_A: buckets at 1000/2000/3000 -> 3 bars.
    let quotes_a = vec![quote(1000, 10.0, 11.0), quote(2000, 12.0, 13.0), quote(3000, 14.0, 15.0)];
    // SYMBOL_B: all three ticks land inside the same [1000,2000) bucket -> 1 bar.
    let quotes_b = vec![quote(1000, 20.0, 21.0), quote(1100, 20.0, 21.0), quote(1200, 20.0, 21.0)];
    df.append_quotes(VENUE, SYMBOL_A, &quotes_a, None).unwrap();
    df.append_quotes(VENUE, SYMBOL_B, &quotes_b, None).unwrap();

    let config = TickReplayConfig {
        venue: VENUE.to_string(),
        symbols: vec![SYMBOL_A.to_string(), SYMBOL_B.to_string()],
        range: TimestampRange::all(),
        seed_bar_interval_ms: Some(1000),
        params: StrategyEngineConfig { cash: 1000.0, ..Default::default() },
        snap_to_properties: false,
        feed_latency: false,
        series: None,
        reference: Vec::new(),
        mark: Vec::new(),
        feed_status: false,
    };

    let err = replay_ticks(df.clone(), BuyOnceThenHold::default(), config).unwrap_err();
    match err {
        TickReplayError::MisalignedSeededBars { lengths } => {
            assert_eq!(lengths, vec![3, 1], "SYM_A -> 3 bars, SYM_B -> 1 bar");
        }
        other => panic!("expected MisalignedSeededBars, got {other:?}"),
    }
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

impl<B: OrderBroker> Strategy<B> for RecordFirstTickContext {
    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        let mut seen = self.seen.borrow_mut();
        if seen.is_none() {
            *seen = Some((broker.index(), broker.bars(&q.symbol).len()));
        }
    }
}

/// R6: `seed_bar_interval_ms: Some(60_000)` over a multi-minute single-symbol quote fixture must
/// give the strategy a NON-EMPTY closed-bar slice on its very first tick (documents that seeding
/// threads through `replay_ticks` into `StrategyEngine::new`'s `bars_by_symbol` and is visible via
/// `OrderBroker::bars`). Per the brief: assert only that bars are non-empty — the exact `index()` value
/// under `run_ticks` (here, the tick-loop counter, not a bar-aligned index) is Phase 2's concern.
#[test]
fn seed_bar_interval_gives_strategy_closed_bars() {
    const SEED_VENUE: &str = "polymarket";
    const SEED_SYMBOL: &str = "SEEDTKN";

    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    // 5 quotes, one per distinct 60_000ms bucket -> 5 consolidated bars once seeded, spanning
    // several minutes (0, 1, 2, 3, 4 minutes).
    let quotes: Vec<TickQuote> =
        (0..5i64).map(|k| quote(k * 60_000, 10.0 + k as f64, 11.0 + k as f64)).collect();
    df.append_quotes(SEED_VENUE, SEED_SYMBOL, &quotes, None).unwrap();

    let config = TickReplayConfig {
        venue: SEED_VENUE.to_string(),
        symbols: vec![SEED_SYMBOL.to_string()],
        range: TimestampRange::all(),
        seed_bar_interval_ms: Some(60_000),
        params: StrategyEngineConfig { cash: 1000.0, ..Default::default() },
        snap_to_properties: false,
        feed_latency: false,
        series: None,
        reference: Vec::new(),
        mark: Vec::new(),
        feed_status: false,
    };

    let strategy = RecordFirstTickContext::default();
    let seen_cell = Rc::clone(&strategy.seen);
    let _result = replay_ticks(df.clone(), strategy, config).unwrap();

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
    kind: L2BookUpdateKind,
    bids: Vec<L2BookLevel>,
    asks: Vec<L2BookLevel>,
) -> L2BookUpdate {
    L2BookUpdate {
        timestamp: ts,
        local_timestamp: 0,
        sequence: seq,
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

/// Submits ONE market buy on the first quote tick (a `BuyOnceThenHold`) AND records every
/// `on_order_book` top into a shared cell (like `book_replay.rs`'s `BookSpy`) — combined so one
/// `replay_ticks` call proves BOTH halves of the closed loop: the quote path still drives a
/// non-empty equity curve, and the recorded book series reaches `Strategy::on_order_book`.
#[derive(Clone, Default)]
struct BuyOnceAndSpyBook {
    buy: BuyOnceThenHold,
    tops: Rc<RefCell<Vec<(f64, f64)>>>,
}

impl<B: OrderBroker> Strategy<B> for BuyOnceAndSpyBook {
    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        self.buy.on_quote_tick(broker, q);
    }

    fn on_order_book(&mut self, _broker: &mut B, book: &L2Book) {
        if let (Some(L2BookLevel { price: bb, .. }), Some(L2BookLevel { price: ba, .. })) =
            (book.best_bid(), book.best_ask())
        {
            self.tops.borrow_mut().push((bb, ba));
        }
    }
}

/// End-to-end: record book events (`append_book_updates`) alongside quotes (`append_quotes`) into
/// a temp `HistoryStoreDataFusion`, run `replay_ticks`, and assert BOTH halves of the store -> merge ->
/// strategy loop close: `on_order_book` fires with the recorded book tops, AND the quote path
/// still produces a non-empty equity curve (Task 9 — closes the loop opened by Task 5's
/// `scan_book_updates` and Task 8's `Tick::Book` fold).
#[test]
fn book_and_quote_replay_close_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());

    let quotes = vec![quote(1000, 10.0, 11.0), quote(2000, 12.0, 13.0)];
    df.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();

    let books = vec![
        bu(
            500,
            1,
            L2BookUpdateKind::Snapshot,
            vec![L2BookLevel::new(0.45, 10.0)],
            vec![L2BookLevel::new(0.47, 5.0)],
        ),
        bu(1500, 2, L2BookUpdateKind::Delta, vec![L2BookLevel::new(0.46, 3.0)], vec![]), // best bid -> 0.46
    ];
    df.append_book_updates(VENUE, SYMBOL, &books, None).unwrap();

    let config = base_config(TimestampRange::all());
    let strategy = BuyOnceAndSpyBook::default();
    let tops = Rc::clone(&strategy.tops);

    let result = replay_ticks(df.clone(), strategy, config).unwrap();

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

/// Buys one unit on the first quote, sells it on the fiftieth, and records every `on_order_book` top.
#[derive(Clone, Default)]
struct RoundTripAndSpyBook {
    quotes: usize,
    tops: Rc<RefCell<Vec<(f64, f64)>>>,
}

impl<B: OrderBroker> Strategy<B> for RoundTripAndSpyBook {
    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        self.quotes += 1;
        match self.quotes {
            1 => broker.place(&q.symbol, "trip", 1, 1.0, OrderPlacement::Market, false),
            50 => broker.place(&q.symbol, "trip", -1, 1.0, OrderPlacement::Market, true),
            _ => {}
        }
    }

    fn on_order_book(&mut self, _broker: &mut B, book: &L2Book) {
        if let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) {
            self.tops.borrow_mut().push((bid.price, ask.price));
        }
    }
}

/// A synthetic order book (`vike_analytics::synthetic::order_book_stream`, the Cont-Stoikov-Talreja
/// model with its published defaults) replays through the tick lane from `HistoryStoreSynthetic`
/// (book W14): every update reaches `on_order_book`, the tops the strategy sees are the stream's
/// quotes, and the quote path fills and closes a round trip.
#[test]
fn a_synthetic_order_book_replays_through_the_tick_lane() {
    let start = 1_735_689_600_000;
    let model = SyntheticOrderBookModel::cont_stoikov_talreja();
    let book = order_book_stream(&model, 74_300.0, start, start + 30 * 60_000, 17).unwrap();
    assert!(book.quotes.len() > 50, "{} quotes", book.quotes.len());
    let store = HistoryStoreSynthetic::default()
        .with_book(SYMBOL, book.updates.clone(), book.quotes.clone(), book.trades.clone())
        .unwrap();
    let config = TickReplayConfig {
        venue: SYNTHETIC_VENUE.to_string(),
        params: StrategyEngineConfig { cash: 1_000_000.0, ..Default::default() },
        ..base_config(TimestampRange::all())
    };
    let strategy = RoundTripAndSpyBook::default();
    let tops = Rc::clone(&strategy.tops);

    let result = replay_ticks(Arc::new(store), strategy, config).unwrap();

    let seen = tops.borrow();
    assert_eq!(seen.len(), book.updates.len(), "every update reaches on_order_book");
    let changes = |pairs: &mut dyn Iterator<Item = (f64, f64)>| {
        let mut out: Vec<(f64, f64)> = Vec::new();
        for p in pairs {
            if out.last().is_none_or(|last| !top_eq(*last, p)) {
                out.push(p);
            }
        }
        out
    };
    let seen_changes = changes(&mut seen.iter().copied());
    let quoted_changes = changes(&mut book.quotes.iter().map(|q| (q.bid, q.ask)));
    assert_eq!(seen_changes.len(), quoted_changes.len());
    for (got, want) in seen_changes.iter().zip(&quoted_changes) {
        assert!(top_eq(*got, *want), "{got:?} vs {want:?}");
    }
    assert!(!result.equity_curve.is_empty());
    assert_eq!(
        result.n_trades,
        1,
        "the round trip closed: {:?}",
        result.simulator().unwrap().dropped
    );
}

// ---------------------------------------------------------------------------------------------
// Feed-latency replay (`TickReplayConfig::feed_latency`) — the recorded `local_timestamp` delivery lane.
// Default OFF is byte-identical; ON re-orders DELIVERY only, never a tick's venue `ts`.
// ---------------------------------------------------------------------------------------------

/// `quote` with an explicit machine receive stamp.
fn quote_lt(ts: i64, local_ts: i64, bid: f64, ask: f64) -> TickQuote {
    TickQuote { local_timestamp: local_ts, ..quote(ts, bid, ask) }
}

/// Records the VENUE ts of every quote the strategy is handed, in DELIVERY order — the exact
/// observable feed-latency mode changes (and must leave untouched when off).
#[derive(Clone, Default)]
struct RecordQuoteOrder {
    seen: Rc<RefCell<Vec<i64>>>,
}

impl<B: OrderBroker> Strategy<B> for RecordQuoteOrder {
    fn on_quote_tick(&mut self, _broker: &mut B, q: &TickQuote) {
        self.seen.borrow_mut().push(q.timestamp);
    }
}

/// Three quotes whose ARRIVAL order differs from their venue order: the 1000-stamped quote was
/// received very late (5000), the other two roughly on time. Venue order 1000/2000/3000; arrival
/// order 2000 (2100), 3000 (3100), 1000 (5000).
fn seed_lagging_store() -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let quotes = vec![
        quote_lt(1000, 5000, 10.0, 11.0),
        quote_lt(2000, 2100, 12.0, 13.0),
        quote_lt(3000, 3100, 14.0, 15.0),
    ];
    df.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();
    (dir, df)
}

/// Replay a store through `RecordQuoteOrder` and return the delivery order of the venue stamps.
fn delivered_order(df: &Arc<HistoryStoreDataFusion>, feed_latency: bool) -> Vec<i64> {
    let mut config = base_config(TimestampRange::all());
    config.feed_latency = feed_latency;
    let strategy = RecordQuoteOrder::default();
    let seen = Rc::clone(&strategy.seen);
    let _ = replay_ticks(df.clone(), strategy, config).unwrap();
    // Name the guard: as an unnamed tail temporary it would outlive `seen` (E0597). Declared
    // after `seen`, it drops first (reverse declaration order), so the borrow ends cleanly.
    let order = seen.borrow();
    order.to_vec()
}

/// (a) OFF (the default) IGNORES `local_timestamp` entirely: delivery stays the frozen venue order, and
/// the `TradingRecord` over a STAMPED tape is bit-identical to the same tape with every stamp
/// zeroed — i.e. the column cannot move a single bit while the flag is off.
#[test]
fn feed_latency_off_is_byte_identical() {
    let (_dir, lagging) = seed_lagging_store();

    // Same prices/ts, every local_timestamp zeroed — the pre-`local_timestamp` shape of the same tape.
    let dir2 = tempfile::tempdir().unwrap();
    let unstamped = Arc::new(HistoryStoreDataFusion::open(dir2.path()).unwrap());
    let plain = vec![quote(1000, 10.0, 11.0), quote(2000, 12.0, 13.0), quote(3000, 14.0, 15.0)];
    unstamped.append_quotes(VENUE, SYMBOL, &plain, None).unwrap();

    assert_eq!(
        delivered_order(&lagging, false),
        vec![1000, 2000, 3000],
        "off must deliver in venue order regardless of the recorded receive stamps"
    );

    let mut config_off = base_config(TimestampRange::all());
    config_off.feed_latency = false;
    let stamped_run =
        replay_ticks(lagging.clone(), BuyOnceThenHold::default(), config_off).unwrap();
    let plain_run = replay_ticks(
        unstamped.clone(),
        BuyOnceThenHold::default(),
        base_config(TimestampRange::all()),
    )
    .unwrap();

    assert_eq!(
        stamped_run.final_equity.to_bits(),
        plain_run.final_equity.to_bits(),
        "off: the recorded local_timestamp column must not move a single bit of the result"
    );
    assert_eq!(stamped_run.n_trades, plain_run.n_trades);
    assert_eq!(stamped_run.trades, plain_run.trades);
    assert_eq!(stamped_run.equity_curve, plain_run.equity_curve);
    assert_eq!(stamped_run.equity_timestamp, plain_run.equity_timestamp);
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

/// (c) A tape with NO stamps (`local_timestamp == 0` — fixtures, backfill, pre-`local_timestamp` parquet parts)
/// falls back to the venue clock, so turning the mode ON changes nothing: bit-identical.
#[test]
fn feed_latency_on_unstamped_tape_falls_back_to_venue_order() {
    let (_dir, df) = seed_store(); // every local_timestamp == 0

    let mut config_on = base_config(TimestampRange::all());
    config_on.feed_latency = true;
    let on = replay_ticks(df.clone(), BuyOnceThenHold::default(), config_on).unwrap();
    let off =
        replay_ticks(df.clone(), BuyOnceThenHold::default(), base_config(TimestampRange::all()))
            .unwrap();

    assert_eq!(
        on.final_equity.to_bits(),
        off.final_equity.to_bits(),
        "an unstamped tape must replay identically in either mode"
    );
    assert_eq!(on.equity_curve, off.equity_curve);
    assert_eq!(on.equity_timestamp, off.equity_timestamp);
    // ...and it is still the hand-computed value the venue-ordered gate above pins.
    assert_eq!(on.final_equity, 1005.0);
}

/// (d) CLOCK SKEW (`local_timestamp < ts` — physically impossible) is CLAMPED UP to the venue ts, so a
/// skewed tick is never promoted ahead of an earlier one. Documented in the module doc.
#[test]
fn feed_latency_clamps_skewed_stamps() {
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
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
        "a skewed local_timestamp clamps to its venue ts — never delivered early"
    );
}

// ---------------------------------------------------------------------------------------------
// Two venues in one engine (book W34): what a live mount hears beside its market. Another venue's
// touch (`TickReplayConfig::reference`), an underlying's mark (`mark`) and its own feed's recorded
// health (`feed_status`), each heard as live hears it; absent, the run is the one it always was.
// ---------------------------------------------------------------------------------------------

/// The run's own venue, the venue that leads it, and ONE instrument name on both: the same-name
/// pair a cross-venue slice could not hold before (decision 0290 spells both `BTCUSDT.P`).
const OWN: &str = "bybit";
const LEAD: &str = "binance";
const PERP: &str = "BTCUSDT";

/// One recorded book event of `symbol`: a snapshot with one level a side at `top`, or a marker.
fn book_event(
    symbol: &str,
    ts: i64,
    seq: u64,
    kind: L2BookUpdateKind,
    top: (f64, f64),
) -> L2BookUpdate {
    let anchor = kind == L2BookUpdateKind::Snapshot;
    let side = |price| if anchor { vec![L2BookLevel::new(price, 2.0)] } else { Vec::new() };
    L2BookUpdate {
        timestamp: ts,
        local_timestamp: 0,
        sequence: seq,
        kind,
        tick_size: 0.5,
        bids: side(top.0),
        asks: side(top.1),
        symbol: symbol.to_string(),
    }
}

/// Bybit trails Binance: Binance's mid jumps from 100 to 103 at 2500, Bybit's at 4000. Bybit's book lane
/// records a feed loss (3200) and its recovery (3300); Binance's book anchors at 3500; an underlying
/// (Binance `ETHUSDT`) prints at 1200; a second Binance perp of another name (`BTCUSDC`) quotes.
fn seed_two_venues() -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let own: Vec<TickQuote> = [1000, 2000, 3000]
        .map(|ts| quote(ts, 99.5, 100.5))
        .into_iter()
        .chain([4000, 5000].map(|ts| quote(ts, 102.5, 103.5)))
        .collect();
    df.append_quotes(OWN, PERP, &own, None).unwrap();
    let own_book = vec![
        book_event(PERP, 900, 1, L2BookUpdateKind::Snapshot, (99.5, 100.5)),
        book_event(PERP, 3200, 0, L2BookUpdateKind::GapStart, (0.0, 0.0)),
        book_event(PERP, 3300, 0, L2BookUpdateKind::LiveResume, (0.0, 0.0)),
        book_event(PERP, 3400, 2, L2BookUpdateKind::Snapshot, (99.5, 100.5)),
    ];
    df.append_book_updates(OWN, PERP, &own_book, None).unwrap();
    let lead = vec![quote(1500, 99.5, 100.5), quote(2500, 102.5, 103.5)];
    df.append_quotes(LEAD, PERP, &lead, None).unwrap();
    let lead_book = vec![book_event(PERP, 3500, 1, L2BookUpdateKind::Snapshot, (102.5, 104.0))];
    df.append_book_updates(LEAD, PERP, &lead_book, None).unwrap();
    df.append_trades(LEAD, "ETHUSDT", &[trade(1200, 2000.5)], None).unwrap();
    let other = vec![quote(1500, 99.0, 99.5), quote(2500, 101.0, 101.5)];
    df.append_quotes(LEAD, "BTCUSDC", &other, None).unwrap();
    (dir, df)
}

/// A lead-lag strategy on the run's own venue: buys when the leading venue's mid is more than 1 above
/// its own; with `hedge` named, it also sells that leg once, on the first reference it hears. Every
/// hook it hears is a line.
#[derive(Clone, Default)]
struct LeadLag {
    heard: Rc<RefCell<Vec<String>>>,
    own_mid: f64,
    bought: bool,
    hedge: Option<&'static str>,
}

impl<B: OrderBroker> Strategy<B> for LeadLag {
    fn on_quote_tick(&mut self, _broker: &mut B, q: &TickQuote) {
        self.own_mid = (q.bid + q.ask) / 2.0;
        self.heard.borrow_mut().push(format!("quote {} {}", q.symbol, q.timestamp));
    }
    fn on_reference_quote(&mut self, broker: &mut B, venue: &str, q: &TickQuote) {
        let line = format!("reference {venue} {} {}/{} {}", q.symbol, q.bid, q.ask, q.timestamp);
        self.heard.borrow_mut().push(line);
        if let Some(leg) = self.hedge.take() {
            broker.place(leg, "", -1, 1.0, OrderPlacement::Market, false);
        }
        if !self.bought && (q.bid + q.ask) / 2.0 - self.own_mid > 1.0 {
            self.bought = true;
            broker.place("", "", 1, 1.0, OrderPlacement::Market, false);
        }
    }
    fn on_mark(&mut self, _broker: &mut B, mark: &vike_model::TickMark) {
        let line = format!("mark {} {} {}", mark.symbol, mark.price, mark.timestamp);
        self.heard.borrow_mut().push(line);
    }
    fn on_feed_status(&mut self, _broker: &mut B, status: vike_model::FeedStatus) {
        self.heard.borrow_mut().push(format!("status {status:?}"));
    }
    fn on_fill(&mut self, _broker: &mut B, fill: &vike_model::Fill) {
        let line = format!("fill {} {} @{} {}", fill.symbol, fill.side, fill.price, fill.timestamp);
        self.heard.borrow_mut().push(line);
    }
}

/// Bybit's `BTCUSDT` as the run's own series; Binance's `BTCUSDT` heard as the reference, Binance's
/// `ETHUSDT` prints as the mark, and the own feed's status. The tapes carry no receive time, so both
/// heard streams state their hop, zero here: each event is heard at its own stamp.
fn two_venue_config() -> TickReplayConfig {
    TickReplayConfig {
        venue: OWN.to_string(),
        symbols: vec![PERP.to_string()],
        reference: vec![ProfileDataReference {
            venue: LEAD.to_string(),
            symbol: PERP.to_string(),
            kind: SeriesKind::Tick,
            trade: false,
            delay_ms: Some(0),
        }],
        mark: vec![ProfileDataMark {
            venue: LEAD.to_string(),
            symbol: "ETHUSDT".to_string(),
            kind: SeriesKind::Trade,
            delay_ms: Some(0),
        }],
        feed_status: true,
        ..base_config(TimestampRange::all())
    }
}

/// Book W34's proof: ONE strategy over two venues' tapes in one engine. The leading venue's touch, of the
/// SAME instrument name as the run's own, is heard on `on_reference_quote` (its quotes, and its book as its
/// top), never on `on_quote_tick`; the order it triggers fills on the run's OWN venue (Bybit's ask 100.5,
/// not Binance's 103.5); the underlying's print is heard as a mark, and the own feed's recorded loss and
/// recovery as its status, each where it happened.
#[test]
fn one_strategy_hears_two_venues_and_trades_its_own() {
    let (_dir, df) = seed_two_venues();
    let strategy = LeadLag::default();
    let heard = Rc::clone(&strategy.heard);

    let result = replay_ticks(df.clone(), strategy, two_venue_config()).unwrap();

    assert_eq!(
        *heard.borrow(),
        vec![
            "quote BTCUSDT 1000",
            "mark ETHUSDT 2000.5 1200",
            "reference binance BTCUSDT 99.5/100.5 1500",
            "quote BTCUSDT 2000",
            "reference binance BTCUSDT 102.5/103.5 2500",
            "fill BTCUSDT 1 @100.5 3000",
            "quote BTCUSDT 3000",
            "status Disconnected",
            "status Live",
            "reference binance BTCUSDT 102.5/104 3500",
            "quote BTCUSDT 4000",
            "quote BTCUSDT 5000",
        ]
    );
    assert_eq!(result.equity_timestamp, vec![1000, 2000, 3000, 4000, 5000], "own quotes alone");
    assert_eq!(result.per_symbol_pnl.len(), 1, "the reference is heard, not held");
}

/// Absent, the three change nothing; and a strategy deaf to them runs the same with them heard.
#[test]
fn a_run_that_hears_nothing_more_is_the_run_it_always_was() {
    let (_dir, df) = seed_two_venues();
    let own = || TickReplayConfig {
        venue: OWN.to_string(),
        symbols: vec![PERP.to_string()],
        ..base_config(TimestampRange::all())
    };
    let plain = replay_ticks(df.clone(), BuyOnceThenHold::default(), own()).unwrap();
    let heard = replay_ticks(df.clone(), BuyOnceThenHold::default(), two_venue_config()).unwrap();
    assert_eq!(plain.trades, heard.trades);
    assert_eq!(plain.equity_curve, heard.equity_curve);
    assert_eq!(plain.equity_timestamp, heard.equity_timestamp);
    assert_eq!(plain.final_equity.to_bits(), heard.final_equity.to_bits());
    // ...and the lead-lag strategy that hears nothing more never sees the lead, so never buys.
    let strategy = LeadLag::default();
    let lines = Rc::clone(&strategy.heard);
    replay_ticks(df.clone(), strategy, own()).unwrap();
    assert!(lines.borrow().iter().all(|l| l.starts_with("quote ")), "{:?}", lines.borrow());
}

/// A reference the run also TRADES (`trade = true`, a hedge leg of another name on the other venue) is a
/// symbol slot: its touch is heard on `on_reference_quote` only, and the hedge sent on it fills on ITS
/// next quote (Binance `BTCUSDC`'s bid 101), as live routes an order naming a leg to the leg's venue.
#[test]
fn a_traded_reference_is_a_hedge_leg_on_its_own_venue() {
    let (_dir, df) = seed_two_venues();
    let strategy = LeadLag { hedge: Some("BTCUSDC"), bought: true, ..LeadLag::default() };
    let heard = Rc::clone(&strategy.heard);
    let config = TickReplayConfig {
        reference: vec![ProfileDataReference {
            venue: LEAD.to_string(),
            symbol: "BTCUSDC".to_string(),
            kind: SeriesKind::Quote,
            trade: true,
            delay_ms: Some(0),
        }],
        mark: Vec::new(),
        feed_status: false,
        ..two_venue_config()
    };

    let result = replay_ticks(df.clone(), strategy, config).unwrap();

    let lines = heard.borrow();
    let references: Vec<&String> = lines.iter().filter(|l| l.starts_with("reference ")).collect();
    assert_eq!(
        references,
        vec!["reference binance BTCUSDC 99/99.5 1500", "reference binance BTCUSDC 101/101.5 2500"]
    );
    assert!(lines.contains(&"fill BTCUSDC -1 @101 2500".to_string()), "{lines:?}");
    assert!(!lines.iter().any(|l| l.starts_with("quote BTCUSDC")), "{lines:?}");
    let held: Vec<&str> = result.per_symbol_pnl.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(held, vec![PERP, "BTCUSDC"]);
}

/// Every hook it hears is a line: the stamp the event carries and the clock it is heard at.
#[derive(Clone, Default)]
struct HeardAt {
    heard: Rc<RefCell<Vec<String>>>,
}

impl<B: OrderBroker> Strategy<B> for HeardAt {
    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        let line = format!("quote {} {} at {}", q.symbol, q.timestamp, broker.now());
        self.heard.borrow_mut().push(line);
    }
    fn on_reference_quote(&mut self, broker: &mut B, venue: &str, q: &TickQuote) {
        let line = format!("reference {venue} {} {} at {}", q.symbol, q.timestamp, broker.now());
        self.heard.borrow_mut().push(line);
    }
    fn on_mark(&mut self, broker: &mut B, mark: &vike_model::TickMark) {
        let line = format!("mark {} {} at {}", mark.symbol, mark.timestamp, broker.now());
        self.heard.borrow_mut().push(line);
    }
}

/// An UNSTAMPED tape (no receive time recorded) is heard after the hop its stream states, never at its
/// own stamp: Binance's touch stamped `T` reaches the strategy at `T + 1000`, after every Bybit quote of
/// `(T, T + 1000)`, with the clock at `T + 1000` and the touch still carrying `T`; a traded leg
/// (`BTCUSDC`) is heard the same way and reaches no other hook, and the mark after its own 300.
#[test]
fn a_heard_stream_reaches_the_strategy_its_delay_after_the_venue_stamped_it() {
    let (_dir, df) = seed_two_venues();
    let strategy = HeardAt::default();
    let heard = Rc::clone(&strategy.heard);
    let reference = |symbol: &str, kind, trade| ProfileDataReference {
        venue: LEAD.to_string(),
        symbol: symbol.to_string(),
        kind,
        trade,
        delay_ms: Some(1_000),
    };
    let config = TickReplayConfig {
        reference: vec![
            reference(PERP, SeriesKind::Tick, false),
            reference("BTCUSDC", SeriesKind::Quote, true),
        ],
        mark: vec![ProfileDataMark {
            venue: LEAD.to_string(),
            symbol: "ETHUSDT".to_string(),
            kind: SeriesKind::Trade,
            delay_ms: Some(300),
        }],
        feed_status: false,
        ..two_venue_config()
    };

    replay_ticks(df.clone(), strategy, config).unwrap();

    assert_eq!(
        *heard.borrow(),
        vec![
            "quote BTCUSDT 1000 at 1000",
            "mark ETHUSDT 1200 at 1500",
            "quote BTCUSDT 2000 at 2000",
            "reference binance BTCUSDT 1500 at 2500",
            "reference binance BTCUSDC 1500 at 2500",
            "quote BTCUSDT 3000 at 3000",
            "reference binance BTCUSDT 2500 at 3500",
            "reference binance BTCUSDC 2500 at 3500",
            "quote BTCUSDT 4000 at 4000",
            "reference binance BTCUSDT 3500 at 4500",
            "quote BTCUSDT 5000 at 5000",
        ]
    );
}

/// Bybit quotes each second; Binance's `BTCUSDT` quotes stamped 1500 and 2500, which the recording box
/// RECEIVED at 1800 and 3100, and Binance's `ETHUSDT` quote stamped 1200, received at 1400.
fn seed_stamped_lead() -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let own: Vec<TickQuote> = [1000, 2000, 3000, 4000].map(|ts| quote(ts, 99.5, 100.5)).into();
    df.append_quotes(OWN, PERP, &own, None).unwrap();
    let lead = vec![quote_lt(1500, 1800, 102.5, 103.5), quote_lt(2500, 3100, 102.5, 103.5)];
    df.append_quotes(LEAD, PERP, &lead, None).unwrap();
    df.append_quotes(LEAD, "ETHUSDT", &[quote_lt(1200, 1400, 2000.0, 2001.0)], None).unwrap();
    (dir, df)
}

/// The reference and the mark of [`seed_stamped_lead`], each with `delay_ms`.
fn stamped_config(delay_ms: Option<u64>) -> TickReplayConfig {
    let stream = |symbol: &str| (LEAD.to_string(), symbol.to_string());
    let (venue, symbol) = stream(PERP);
    let reference =
        ProfileDataReference { venue, symbol, kind: SeriesKind::Quote, trade: false, delay_ms };
    let (venue, symbol) = stream("ETHUSDT");
    TickReplayConfig {
        venue: OWN.to_string(),
        symbols: vec![PERP.to_string()],
        reference: vec![reference],
        mark: vec![ProfileDataMark { venue, symbol, kind: SeriesKind::Quote, delay_ms }],
        ..base_config(TimestampRange::all())
    }
}

/// A STAMPED tape is heard when the recording box received it, the hop it measured, with no key: the
/// touch stamped 2500 and received at 3100 comes after Bybit's 3000, the clock at 3100 and the touch
/// still carrying 2500. A stated `delay_ms` is added to the receive time.
#[test]
fn a_stamped_heard_stream_is_heard_at_its_receive_time_plus_any_stated_delay() {
    let (_dir, df) = seed_stamped_lead();
    let lines = |delay_ms| {
        let strategy = HeardAt::default();
        let heard = Rc::clone(&strategy.heard);
        replay_ticks(df.clone(), strategy, stamped_config(delay_ms)).unwrap();
        heard.take()
    };
    assert_eq!(
        lines(None),
        vec![
            "quote BTCUSDT 1000 at 1000",
            "mark ETHUSDT 1200 at 1400",
            "reference binance BTCUSDT 1500 at 1800",
            "quote BTCUSDT 2000 at 2000",
            "quote BTCUSDT 3000 at 3000",
            "reference binance BTCUSDT 2500 at 3100",
            "quote BTCUSDT 4000 at 4000",
        ]
    );
    assert_eq!(
        lines(Some(500)),
        vec![
            "quote BTCUSDT 1000 at 1000",
            "mark ETHUSDT 1200 at 1900",
            "quote BTCUSDT 2000 at 2000",
            "reference binance BTCUSDT 1500 at 2300",
            "quote BTCUSDT 3000 at 3000",
            "reference binance BTCUSDT 2500 at 3600",
            "quote BTCUSDT 4000 at 4000",
        ]
    );
}

/// An UNSTAMPED tape (archive or REST data) with no `delay_ms` is refused in words, never heard at its
/// venue's stamp, and nothing runs.
#[test]
fn an_unstamped_heard_stream_without_a_delay_is_refused() {
    let (_dir, df) = seed_two_venues();
    let mut config = two_venue_config();
    config.reference[0].delay_ms = None;
    match replay_ticks(df.clone(), HeardAt::default(), config) {
        Err(TickReplayError::HeardDelay(why)) => {
            let needle = "data.reference \"BTCUSDT\" on \"binance\": 3 of its 3 events carry no local \
                          receive time";
            assert!(why.contains(needle) && why.contains("state its delay_ms"), "{why}");
        }
        Err(other) => panic!("refused for another reason: {other}"),
        Ok(_) => panic!("an unstamped reference with no delay_ms must be refused"),
    }
}
