//! W2-T4 mark-slot semantics gates — the VALUATION LAW, end to end.
//!
//! The split: `LiveDataSink::bar_close_tick` (a kline feed's candle-close snapshot) and
//! `LiveDataSink::mark_tick` (the venue's REAL mark stream) ride separate conflation slots and
//! land in different `PriceBoard` slots. The resolver chain is
//! `mark -> side-aware bid/ask -> last_trade -> bar_close`, so the split MOVED a candle close from
//! the HEAD of that chain to its TAIL. That is the intended correction (see `price_board.rs`'s
//! "THE VALUATION LAW" table): a live quote or a live trade is a better answer to "what is this
//! position worth right now" than the close of a window that has already ended.
//!
//! These tests pin all four rungs against the configurations that actually occur in production,
//! not just the convenient one:
//! - a genuinely bar-close-only venue is byte-identical to pre-split (nothing else fills a slot);
//! - a venue with a trade feed now values at the LAST TRADE, not the close;
//! - a venue with a quote/book feed now values SIDE-AWARE at bid (long) / ask (short);
//! - a venue with a real mark stream values at the mark, above all of them.
//!
//! Plus the `Account.marks` coherence rule: that scalar is untagged and read by the pre-trade
//! gate, the margin-call law and `LiveBroker.price`, so a fresh venue mark owns it and candle
//! closes are dropped from it while the mark stream is alive (in EITHER arrival order).

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, CoreLaneSink, spawn_core};
use vike_data::LiveDataSink;
use vike_exec::price_board::Resolution;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, ExecutionEngine, MarkSource, PriceCfg, PriceSource, RiskGate, RiskLimits,
};
use vike_model::events::FillEvent;

fn test_config(seed_cash: f64) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    }
}

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

fn fill(qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: "t1".into(), // a source literal — `TradeId: From<&'static str>`
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

/// Resolve a LONG (`is_long = true`) / SHORT position on the standard test cell.
fn resolve(eng: &ExecutionEngine<RecordingClient>, is_long: bool) -> (f64, PriceSource) {
    match eng.price_board.resolve("sim", "BTCUSDT", is_long, 1_000, &PriceCfg::default()) {
        Resolution::Priced { px, source, .. } => (px, source),
        other => panic!("must price: {other:?}"),
    }
}

// --- Rung 4: the genuinely bar-close-only venue (the preserved case) --------------------------

/// The behavior-preservation gate for a venue that fills NO other slot — no mark stream, no quote
/// feed, no trade feed subscribed (a chart-only alpaca/IBKR mount). With a long position on the
/// book, valuing it through the OLD routing (candle close → MARK slot) and the NEW routing
/// (candle close → BAR-CLOSE slot) resolves the identical px/ts and a byte-identical
/// resolver-priced decision equity; only the source LABEL changes.
///
/// This is deliberately the NARROW claim. It does NOT say a bar-close-only venue can never fill
/// the other slots — the very same feeds DO fill them whenever a quote or trade subscription is
/// open (alpaca's `dispatch` emits `quote`/`trade` alongside `bar_close_tick`; `ensure_trade_feed_on`
/// subscribes trades for ANY venue when an orderflow/tick chart is open). Those configurations are
/// the three tests below, and they change behavior on purpose.
#[test]
fn a_venue_filling_only_the_bar_close_slot_is_byte_identical() {
    let mut old_eng = engine();
    let mut new_eng = engine();
    old_eng.account.apply_fill(&fill(2.0, 100.0));
    new_eng.account.apply_fill(&fill(2.0, 100.0));

    old_eng.account.set_mark_from("sim", "BTCUSDT", 105.5, MarkSource::VenueMark, 0);
    old_eng.price_board.set_mark("sim", "BTCUSDT", 105.5, 42);
    new_eng.account.set_mark_from("sim", "BTCUSDT", 105.5, MarkSource::VenueMark, 0);
    new_eng.price_board.set_bar_close("sim", "BTCUSDT", 105.5, 42);

    let cfg = PriceCfg::default();
    let seed = 1_000.0;
    let old_eq = old_eng.resolved_equity(seed, &cfg);
    let new_eq = new_eng.resolved_equity(seed, &cfg);
    assert_eq!(
        old_eq.to_bits(),
        new_eq.to_bits(),
        "decision equity must be byte-identical: old(mark-slot)={old_eq} new(bar-close-slot)={new_eq}"
    );

    let old_res = old_eng.price_board.resolve("sim", "BTCUSDT", true, 42, &cfg);
    let new_res = new_eng.price_board.resolve("sim", "BTCUSDT", true, 42, &cfg);
    match (old_res, new_res) {
        (
            Resolution::Priced { px: opx, source: osrc, ts: ots },
            Resolution::Priced { px: npx, source: nsrc, ts: nts },
        ) => {
            assert_eq!(opx.to_bits(), npx.to_bits(), "identical resolved px");
            assert_eq!(ots, nts, "identical resolved ts");
            assert_eq!(osrc, PriceSource::Mark);
            assert_eq!(nsrc, PriceSource::BarClose, "only the source label changes");
        }
        other => panic!("both must price: {other:?}"),
    }
}

// --- Rung 3: a live trade now outranks the candle close (BEHAVIOR CHANGE) ---------------------

/// The alpaca / IBKR / binance-spot / `VIKE_MARK_STREAMS=0`-perp configuration: a kline feed AND a
/// trade feed on the same symbol. Post-split the candle close sits at the chain TAIL, so the LAST
/// TRADE prices the position. Pre-split the close occupied the MARK slot and would have won —
/// this test is the record of that intended change.
#[test]
fn a_live_trade_outranks_the_candle_close() {
    let mut eng = engine();
    eng.price_board.set_bar_close("sim", "BTCUSDT", 100.0, 500);
    eng.price_board.set_last_trade("sim", "BTCUSDT", 103.25, 900);

    let (px, source) = resolve(&eng, true);
    assert_eq!(source, PriceSource::LastTrade, "a real recent print beats a finished candle");
    assert_eq!(px.to_bits(), 103.25f64.to_bits());
    // pre-split the SAME close would have priced it, because it sat in the mark slot
    let mut pre = engine();
    pre.price_board.set_mark("sim", "BTCUSDT", 100.0, 500);
    pre.price_board.set_last_trade("sim", "BTCUSDT", 103.25, 900);
    assert_eq!(resolve(&pre, true).0.to_bits(), 100.0f64.to_bits(), "the pre-split behavior");
}

// --- Rung 2: a two-sided quote outranks the close, SIDE-AWARE (BEHAVIOR CHANGE) ---------------

/// The same venue class with a quote (or two-sided book top) subscription. The chain's quote rung
/// is SIDE-AWARE by design: a LONG is valued at the BID and a SHORT at the ASK — the side each
/// would have to cross to exit — so the spread is always booked AGAINST the position. That is a
/// spread-wide move versus the candle close this configuration used to value at.
#[test]
fn a_two_sided_quote_outranks_the_candle_close_and_is_side_aware() {
    let mut eng = engine();
    eng.price_board.set_bar_close("sim", "BTCUSDT", 100.0, 500);
    eng.price_board.set_quote("sim", "BTCUSDT", 99.5, 100.5, 900);

    let (long_px, long_src) = resolve(&eng, true);
    assert_eq!(long_src, PriceSource::Bid, "a long exits by hitting the bid");
    assert_eq!(long_px.to_bits(), 99.5f64.to_bits());

    let (short_px, short_src) = resolve(&eng, false);
    assert_eq!(short_src, PriceSource::Ask, "a short exits by lifting the ask");
    assert_eq!(short_px.to_bits(), 100.5f64.to_bits());

    // the spread is booked against the position in BOTH directions, never for it
    assert!(long_px < 100.0 && short_px > 100.0, "conservative liquidation-side valuation");
}

/// Full chain in one cell: the venue mark outranks quote, trade and close alike, for both sides.
#[test]
fn a_venue_mark_outranks_every_other_rung() {
    let mut eng = engine();
    eng.price_board.set_bar_close("sim", "BTCUSDT", 100.0, 500);
    eng.price_board.set_last_trade("sim", "BTCUSDT", 103.25, 900);
    eng.price_board.set_quote("sim", "BTCUSDT", 99.5, 100.5, 900);
    eng.price_board.set_mark("sim", "BTCUSDT", 101.75, 950);

    for is_long in [true, false] {
        let (px, source) = resolve(&eng, is_long);
        assert_eq!(source, PriceSource::Mark, "the venue mark is the head of the chain");
        assert_eq!(px.to_bits(), 101.75f64.to_bits());
    }
}

// --- `Account.marks` coherence: one concept, either arrival order -----------------------------

/// Spin until `f` holds (the core folds on its own thread), so the two-drain ordering below is
/// deterministic rather than sleep-timed.
fn wait_until(mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("condition never held within the deadline");
}

/// End-to-end through a spawned core: `CoreLaneSink::bar_close_tick` → `publish_bar_close` →
/// `drain_market` → `Account.set_mark`. A venue with NO mark stream still prices its account
/// through the candle close exactly as before the split.
#[test]
fn bar_close_tick_reaches_the_account_mark_lane() {
    let handle = spawn_core(engine(), test_config(0.0));
    let cell = handle.snapshot_cell();
    let sink = CoreLaneSink::new(handle.bar_sender(), handle.market_sender(), handle.tick_sender());
    sink.bar_close_tick("sim", "BTCUSDT", 424_242.0, 7);
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.marks.len(), 1);
    assert_eq!(snap.marks[0].2, 424_242.0, "the candle close landed in Account.marks");
}

/// Both verbs in ONE drain: the bar-close arm runs first, the real-mark arm second, so the venue
/// mark owns the account slot.
#[test]
fn a_real_mark_wins_the_account_slot_within_one_drain() {
    let handle = spawn_core(engine(), test_config(0.0));
    let cell = handle.snapshot_cell();
    let sink = CoreLaneSink::new(handle.bar_sender(), handle.market_sender(), handle.tick_sender());
    sink.bar_close_tick("sim", "BTCUSDT", 100.0, 1);
    sink.mark_tick("sim", "BTCUSDT", 100.5, 2);
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.marks.len(), 1);
    assert_eq!(snap.marks[0].2, 100.5, "the venue's REAL mark values the account, not the close");
}

/// The ACROSS-drains case, which the in-drain ordering alone does not cover and which is the one
/// that actually occurs live: klines arrive far more often than the 1s mark, so without the
/// staleness rule a later candle close would keep stealing the account slot back. A mark that
/// landed in an EARLIER drain must still own it.
#[test]
fn a_candle_close_in_a_later_drain_does_not_displace_a_fresh_real_mark() {
    let handle = spawn_core(engine(), test_config(0.0));
    let cell = handle.snapshot_cell();
    let sink = CoreLaneSink::new(handle.bar_sender(), handle.market_sender(), handle.tick_sender());

    sink.mark_tick("sim", "BTCUSDT", 100.5, 0);
    wait_until(|| cell.load_full().marks.iter().any(|(_, s, _)| s == "BTCUSDT"));
    // a separate, LATER drain carries the candle close
    sink.bar_close_tick("sim", "BTCUSDT", 100.0, 1);
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.marks.len(), 1);
    assert_eq!(
        snap.marks[0].2, 100.5,
        "a fresh venue mark keeps the account slot; the close only fills the board's own slot"
    );
}

/// The mirror ordering — the candle close lands in an EARLIER drain and the real mark in a later
/// one. The mark always takes the slot; `Account.marks` ends on the same concept either way.
#[test]
fn a_real_mark_in_a_later_drain_takes_the_account_slot_from_a_candle_close() {
    let handle = spawn_core(engine(), test_config(0.0));
    let cell = handle.snapshot_cell();
    let sink = CoreLaneSink::new(handle.bar_sender(), handle.market_sender(), handle.tick_sender());

    sink.bar_close_tick("sim", "BTCUSDT", 100.0, 0);
    wait_until(|| cell.load_full().marks.iter().any(|(_, s, _)| s == "BTCUSDT"));
    sink.mark_tick("sim", "BTCUSDT", 100.5, 1);
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.marks.len(), 1);
    assert_eq!(snap.marks[0].2, 100.5, "the real mark takes over the account slot");
}

// --- Round 3: the CLOSED-BAR lane, which rounds 1 and 2 both missed --------------------------

/// A controllable core clock: the tests below need to jump past `mark_staleness_ms`, which the
/// monotonically-incrementing `test_config` clock would take ten million calls to reach.
fn config_with_clock(seed_cash: f64) -> (CoreConfig, Arc<AtomicI64>) {
    let t = Arc::new(AtomicI64::new(0));
    let t2 = Arc::clone(&t);
    (
        CoreConfig {
            seed_cash,
            clock: Box::new(move || t2.load(Ordering::Relaxed)),
            margin_call: Some(vike_exec::MarginCallConfig::default()),
            ..CoreConfig::default()
        },
        t,
    )
}

fn bar(ts: i64, close: f64) -> vike_model::Bar {
    vike_model::Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn trade_tick(price: f64, ts: i64) -> vike_model::TradeTick {
    vike_model::TradeTick {
        ts,
        local_ts: 0,
        price,
        size: 1.0,
        is_buyer_maker: false,
        symbol: String::new(),
    }
}

/// **THE round-3 regression gate.** Rounds 1 and 2 put the precedence rule in `drain_market` and
/// declared the law held — but `drive_strategy` (the LOSSLESS closed-bar lane, reached through
/// `BarSender`, NOT the conflated `bar_close_tick` lane) wrote `account.set_mark(bar.close)`
/// unconditionally, for exactly the symbols an engine holds positions in, ten lines above the
/// margin-call sweep. This test drives a REAL closed bar through the REAL `BarSender` and asserts
/// the fresh venue mark survives it.
///
/// It fails on the round-2 code and passes only because the law now lives inside
/// `Account::set_mark_from`, where no lane can route around it.
#[test]
fn a_closed_bar_on_the_real_bar_lane_does_not_stomp_a_fresh_venue_mark() {
    let (cfg, _clock) = config_with_clock(1_000.0);
    let handle = spawn_core(engine(), cfg);
    let cell = handle.snapshot_cell();
    let sink = CoreLaneSink::new(handle.bar_sender(), handle.market_sender(), handle.tick_sender());

    sink.mark_tick("sim", "BTCUSDT", 100.5, 0);
    wait_until(|| cell.load_full().marks.iter().any(|(_, s, _)| s == "BTCUSDT"));

    // the closed-bar lane — `close_bar`, not `bar_close_tick`
    sink.close_bar("sim", "BTCUSDT", "1m", bar(60_000, 80.0));
    wait_until(|| !cell.load_full().bars.is_empty());
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(
        snap.marks.iter().find(|(_, s, _)| s == "BTCUSDT").map(|(_, _, px)| *px),
        Some(100.5),
        "the closed-bar lane must not displace a fresh venue mark (it did until round 3)"
    );
}

/// The same lane, the same bar — but now asserting the CONSUMER side: the price basis every
/// `Account.marks` reader sees after the closed bar. `unrealized_pnl` is the sharpest probe
/// available through the public snapshot (it is `(mark - avg_px) * size * mult` straight off the
/// scalar), and it feeds `equity_all`, which the pre-trade risk gate compares limits against.
#[test]
fn after_a_closed_bar_the_account_readers_still_see_the_mark_not_the_close() {
    let (cfg, _clock) = config_with_clock(1_000.0);
    let mut eng = engine();
    eng.account.apply_fill(&fill(2.0, 100.0)); // long 2 @ 100
    let handle = spawn_core(eng, cfg);
    let cell = handle.snapshot_cell();
    let sink = CoreLaneSink::new(handle.bar_sender(), handle.market_sender(), handle.tick_sender());

    sink.mark_tick("sim", "BTCUSDT", 110.0, 0);
    wait_until(|| cell.load_full().marks.iter().any(|(_, s, _)| s == "BTCUSDT"));
    sink.close_bar("sim", "BTCUSDT", "1m", bar(60_000, 80.0));
    wait_until(|| !cell.load_full().bars.is_empty());
    handle.shutdown_and_join();

    let snap = cell.load_full();
    let mark = snap.marks.iter().find(|(_, s, _)| s == "BTCUSDT").map(|(_, _, px)| *px);
    assert_eq!(mark, Some(110.0));
    // (110 - 100) * 2 * 1 = +20 at the mark; the close would have said (80 - 100) * 2 = -40.
    let upnl = (mark.unwrap() - 100.0) * 2.0;
    assert_eq!(upnl.to_bits(), 20.0_f64.to_bits(), "valued at the mark, not the candle close");
}

/// The documented DEGRADATION path, end to end through the runtime: a mark stream that goes
/// silent must hand the account slot back, so valuation falls to the next-best concept instead
/// of freezing at a dead mark forever. Nothing tested this before round 3.
#[test]
fn a_mark_that_goes_silent_lets_the_closed_bar_reclaim_the_account_slot() {
    let (cfg, clock) = config_with_clock(1_000.0);
    let stale = cfg.mark_staleness_ms;
    let handle = spawn_core(engine(), cfg);
    let cell = handle.snapshot_cell();
    let sink = CoreLaneSink::new(handle.bar_sender(), handle.market_sender(), handle.tick_sender());

    sink.mark_tick("sim", "BTCUSDT", 100.5, 0);
    wait_until(|| cell.load_full().marks.iter().any(|(_, s, _)| s == "BTCUSDT"));

    // ... then the mark stream dies and the core clock runs past the ownership window
    clock.store(stale + 1, Ordering::Relaxed);
    sink.close_bar("sim", "BTCUSDT", "1m", bar(60_000, 80.0));
    wait_until(|| !cell.load_full().bars.is_empty());
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(
        snap.marks.iter().find(|(_, s, _)| s == "BTCUSDT").map(|(_, _, px)| *px),
        Some(80.0),
        "a silent mark must release the slot — valuation degrades, it never freezes"
    );
}

/// A venue with NO mark stream is untouched on the closed-bar lane too: with nothing owning the
/// slot, every close writes, exactly as it did before the law existed.
#[test]
fn a_no_mark_venue_still_marks_to_the_close_on_the_bar_lane() {
    let (cfg, _clock) = config_with_clock(1_000.0);
    let handle = spawn_core(engine(), cfg);
    let cell = handle.snapshot_cell();
    let sink = CoreLaneSink::new(handle.bar_sender(), handle.market_sender(), handle.tick_sender());

    sink.close_bar("sim", "BTCUSDT", "1m", bar(60_000, 80.0));
    wait_until(|| !cell.load_full().bars.is_empty());
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(
        snap.marks.iter().find(|(_, s, _)| s == "BTCUSDT").map(|(_, _, px)| *px),
        Some(80.0)
    );
}

/// The RECONCILE-mark decision, pinned at the engine level (a reconcile pass is not reachable
/// through `CoreLaneSink`). `ExecReport::position_mark_px` is filed as a genuine venue mark and
/// therefore OWNS the slot for its own (longer) `reconcile_staleness_ms` window — on every venue
/// that reports one, INCLUDING venues with no mark stream and perps under `VIKE_MARK_STREAMS=0`.
/// That is deliberate: it is the venue's own valuation of the position, and for a stream-less
/// venue it is the only real mark that exists. The window is sized to COVER the reconcile cadence
/// (default 60s), so its ownership is continuous between passes and does not alternate with the
/// closes; only once reconcile stops for longer than the window do closes reclaim the slot.
#[test]
fn a_reconcile_mark_owns_the_account_slot_on_a_venue_with_no_mark_stream() {
    let mut eng = engine();
    eng.now_ms = 1_000;
    eng.account.set_mark_from("sim", "BTCUSDT", 90.0, MarkSource::BarClose, 1_000);

    let snapshot = vike_exec::ReconcileSnapshot {
        position_mark_px: vec![("BTCUSDT".to_string(), 100.0)],
        ..Default::default()
    };
    eng.apply_snapshot(&snapshot);
    assert_eq!(eng.account.mark_of("sim", "BTCUSDT"), Some(100.0), "the venue's own mark wins");

    // it holds the slot against a candle close within the window...
    eng.account.set_mark_from("sim", "BTCUSDT", 80.0, MarkSource::BarClose, 5_000);
    assert_eq!(eng.account.mark_of("sim", "BTCUSDT"), Some(100.0));
    // ...and — the point of the per-source window — it STILL holds across the 60s reconcile
    // cadence, so a stream-less reconciled venue's slot no longer alternates every minute
    eng.account.set_mark_from("sim", "BTCUSDT", 80.0, MarkSource::BarClose, 61_000);
    assert_eq!(eng.account.mark_of("sim", "BTCUSDT"), Some(100.0));
    // ...only once reconcile stops for longer than `reconcile_staleness_ms` (150s default) do
    // closes reclaim the slot — valuation degrades, it never freezes at a stale mark
    eng.account.set_mark_from("sim", "BTCUSDT", 80.0, MarkSource::BarClose, 152_000);
    assert_eq!(eng.account.mark_of("sim", "BTCUSDT"), Some(80.0));
}

// --- Round 4: clock hygiene on the tick lane -------------------------------------------------

/// **The clock-hygiene gate.** The quote/trade tick lanes carry the VENUE event time as the
/// strategy-facing `now` (`ctx.now`, conditional firing, the broker drain), but the account mark
/// slot must be aged on the CORE clock on BOTH sides (`account.rs`). This drives a trade whose
/// `ts` is far past the ownership window while the core clock (a fixed loadable clock, held at 0)
/// stays inside it: the fresh venue mark must survive.
///
/// It fails on pre-round-4 code, where `drive_strategy_tick` filed the print under
/// `MarkSource::TradeTick` stamped with `trade.ts` — so a venue clock running ahead of the core
/// clock displaced a genuinely fresh mark. The fix stamps the mark write with the core clock,
/// leaving `trade.ts` as the (correct) strategy-facing event time.
#[test]
fn a_venue_time_skewed_trade_does_not_displace_a_fresh_venue_mark() {
    let (cfg, _clock) = config_with_clock(0.0); // fixed clock held at 0 — core time never advances
    let handle = spawn_core(engine(), cfg);
    let cell = handle.snapshot_cell();
    let sink = CoreLaneSink::new(handle.bar_sender(), handle.market_sender(), handle.tick_sender());

    sink.mark_tick("sim", "BTCUSDT", 100.5, 0);
    wait_until(|| cell.load_full().marks.iter().any(|(_, s, _)| s == "BTCUSDT"));

    // trade.ts is 10_000_000ms (far past the 10s window), but the CORE clock is fixed at 0
    sink.trade("sim", "BTCUSDT", trade_tick(9_999.0, 10_000_000));
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(
        snap.marks.iter().find(|(_, s, _)| s == "BTCUSDT").map(|(_, _, px)| *px),
        Some(100.5),
        "a venue-time-skewed print must not steal the account slot from a fresh venue mark"
    );
}
