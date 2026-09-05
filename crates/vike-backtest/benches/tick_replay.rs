//! BASELINE for the tick-replay path (perf-program measurement scaffolding — audit finding:
//! the per-tick loader/engine costs — `merge_ticks` cloning every tick into the merged stream,
//! `run_ticks` calling `equity_now()` per tick and cloning each tick out of its stream — were
//! estimated 20–40% recoverable). This bench answers: **what does one recorded tick cost today
//! through the REAL [`vike_backtest::replay_ticks`] (HistStore scan → merge → `run_ticks`), and
//! how does that split between the LOADER (scan+clone+merge) and the ENGINE fold?** Run it
//! BEFORE and AFTER any optimization on the same box; no assertion gates the timings
//! (measurement, not a gate).
//!
//! The clone-cut change acted on two of those three: `merge_ticks` now MOVES each element into the
//! merged stream and `run_ticks` BORROWS each tick out of it (and resolves the symbol slot from a
//! `&str`). The per-tick `equity_now()` pair was deliberately left alone — the two samples are
//! taken either side of the strategy callback and are not the same value (see `run_ticks`).
//!
//! Plain `main()` + `std::time::Instant`, mirroring `benches/engines.rs` (`harness = false`,
//! no criterion dependency, median-of-N after a warmup). `required-features = ["hist-replay"]`
//! — the TRAIT-ONLY replay lane, NO DataFusion in this bench's build graph.
//!
//!   cargo bench -p vike-backtest --features hist-replay --bench tick_replay
//!
//! STORE NOTE (a deliberate deviation, stated honestly): the intended in-memory double was
//! `vike_data::MemHistStore` (`test-support` feature), but its `scan_quotes`/`scan_trades`/
//! `scan_book_updates` are INERT stubs returning empty (that double exists for the
//! properties/equity/exec-log/funding/chain seams only — its module doc says so), so a replay
//! over it would replay ZERO ticks. Rather than widening a shared test double's documented
//! contract inside a measurement-only change, this bench carries its own minimal in-memory
//! [`HistStore`] (`BenchStore`: seeded vectors, real range filtering on the three tick scans,
//! inert everything else). If `MemHistStore` later grows real tick verbs, this bench can switch
//! to it and drop `BenchStore`.
//!
//! Four rows over one synthetic single-symbol tape (deterministic waves, no RNG):
//!
//! - `replay_ticks Q+T` — the production loader end-to-end: scan (clone out of the store), the
//!   3-way merge, engine construction and `run_ticks`.
//! - `replay_ticks Q+T+B` — same, plus a recorded book series (snapshot and contiguous deltas)
//!   folding through `apply_book_event`.
//! - `run_ticks only` — the ENGINE fold alone over the pre-merged Q+T stream (engine construction
//!   outside the timer): per-tick clock/fill-phase/`equity_now`×2/curve-push cost, isolated.
//! - `merge_ticks only` — the loader's merge half alone (the per-tick move into `Tick`; it cloned
//!   each element until the clone-cut change, which is one of the things this row measures).
//!
//! The strategy is a NO-OP (`Strategy<SimBroker>` default handlers), so every row measures the
//! path's own overhead, not strategy work.

use std::sync::Arc;
use std::time::Instant;

use vike_backtest::{
    EngineParams, FillModelKind, SimBroker, StrategyEngine, Tick, TickReplayConfig, merge_ticks,
    replay_ticks,
};
use vike_data::{DataError, ExecFillRow, ExecOrderRow, HistStore, TsRange};
use vike_model::{
    Bar, BookUpdate, BookUpdateKind, EquitySample, QuoteTick, Strategy, SymbolProperties, TradeTick,
};

const VENUE: &str = "bench";
const SYMBOL: &str = "TKN";
const T0: i64 = 1_700_000_000_000;
const N_QUOTES: usize = 400_000;
const N_TRADES: usize = 100_000;
const N_BOOKS: usize = 50_000;
const REPEATS: usize = 7;
const WARMUP: usize = 2; // discarded runs before timing (kills cold-cache effects)

/// The no-op strategy: every `Strategy` handler defaults to a no-op, so `run_ticks` pays only
/// its own per-tick overhead (dispatch, clock, fill phase over an empty order book, equity).
struct NoopStrategy;
impl Strategy<SimBroker> for NoopStrategy {}

/// Minimal in-memory [`HistStore`] over pre-seeded tick vectors — see the STORE NOTE in the
/// module doc for why this exists instead of `MemHistStore`. Only the three tick scans carry
/// behavior (range-filtered clones, matching the `DataFusionHist` read contract of ts-ascending
/// owned rows); every other verb is an inert stub, exactly like the shared double's stubs.
#[derive(Default)]
struct BenchStore {
    quotes: Vec<QuoteTick>,
    trades: Vec<TradeTick>,
    books: Vec<BookUpdate>,
}

fn in_range(ts: i64, range: TsRange) -> bool {
    range.start.map(|s| ts >= s).unwrap_or(true) && range.end.map(|e| ts <= e).unwrap_or(true)
}

impl HistStore for BenchStore {
    fn load_bars(&self, _v: &str, _s: &str, _i: &str, _r: TsRange) -> Result<Vec<Bar>, DataError> {
        Ok(Vec::new())
    }
    fn scan_quotes(&self, _v: &str, _s: &str, r: TsRange) -> Result<Vec<QuoteTick>, DataError> {
        Ok(self.quotes.iter().filter(|q| in_range(q.ts, r)).cloned().collect())
    }
    fn scan_trades(&self, _v: &str, _s: &str, r: TsRange) -> Result<Vec<TradeTick>, DataError> {
        Ok(self.trades.iter().filter(|t| in_range(t.ts, r)).cloned().collect())
    }
    fn scan_book_updates(
        &self,
        _v: &str,
        _s: &str,
        r: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Ok(self.books.iter().filter(|b| in_range(b.ts, r)).cloned().collect())
    }
    fn append_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _b: &[Bar],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_quotes(
        &self,
        _v: &str,
        _s: &str,
        _t: &[QuoteTick],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_trades(
        &self,
        _v: &str,
        _s: &str,
        _t: &[TradeTick],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_book_updates(
        &self,
        _v: &str,
        _s: &str,
        _u: &[BookUpdate],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_symbol_properties(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[(i64, SymbolProperties)],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_symbol_properties(
        &self,
        _v: &str,
        _s: &str,
        _r: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Ok(Vec::new())
    }
    fn append_equity(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[EquitySample],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_equity(&self, _v: &str, _s: &str, _r: TsRange) -> Result<Vec<EquitySample>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_fills(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[ExecFillRow],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_fills(&self, _v: &str, _s: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_orders(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[ExecOrderRow],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_orders(&self, _v: &str, _s: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        Ok(Vec::new())
    }
    fn resample_quotes_to_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _r: TsRange,
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn resample_trades_to_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _r: TsRange,
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
}

/// Deterministic mid-price wave (no RNG — BEFORE/AFTER runs are exactly comparable).
fn px(i: usize) -> f64 {
    100.0 + 5.0 * (i as f64 * 0.0007).sin()
}

fn synth_quotes(n: usize) -> Vec<QuoteTick> {
    (0..n)
        .map(|i| {
            let mid = px(i);
            QuoteTick {
                ts: T0 + i as i64 * 5,
                local_ts: 0,
                bid: mid - 0.005,
                ask: mid + 0.005,
                bid_size: 1.0 + (i % 13) as f64 * 0.1,
                ask_size: 1.0 + (i % 11) as f64 * 0.1,
                symbol: SYMBOL.to_string(),
            }
        })
        .collect()
}

fn synth_trades(n: usize) -> Vec<TradeTick> {
    (0..n)
        .map(|i| TradeTick {
            ts: T0 + i as i64 * 20 + 1, // offset so quotes/trades interleave, never all-tie
            local_ts: 0,
            price: px(i * 4),
            size: 0.1 + (i % 7) as f64 * 0.05,
            is_buyer_maker: i.is_multiple_of(2),
            symbol: SYMBOL.to_string(),
        })
        .collect()
}

/// A recorded book series: one anchoring Snapshot (10 levels/side), then contiguous-seq Deltas
/// (2 levels/side) — the shape the live recorder writes, so `run_ticks`'s `apply_book_event`
/// integrity machine folds every event (no gap-drop).
fn synth_books(n: usize) -> Vec<BookUpdate> {
    (0..n)
        .map(|i| {
            let mid = px(i * 8);
            let (kind, bids, asks) = if i == 0 {
                let bids = (0..10).map(|l| (mid - 0.01 * (l + 1) as f64, 1.0)).collect();
                let asks = (0..10).map(|l| (mid + 0.01 * (l + 1) as f64, 1.0)).collect();
                (BookUpdateKind::Snapshot, bids, asks)
            } else {
                let bids = vec![(mid - 0.01, 1.0 + (i % 5) as f64), (mid - 0.02, 2.0)];
                let asks = vec![(mid + 0.01, 1.0 + (i % 3) as f64), (mid + 0.02, 2.0)];
                (BookUpdateKind::Delta, bids, asks)
            };
            BookUpdate {
                ts: T0 + i as i64 * 40 + 2, // offset past quotes/trades at the same ms
                local_ts: 0,
                seq: i as u64 + 1, // contiguous chain from the snapshot anchor
                kind,
                tick_size: 0.01,
                bids,
                asks,
                symbol: SYMBOL.to_string(),
            }
        })
        .collect()
}

/// Engine params for every row; `replay_ticks` overwrites `fill_model`/`default_venue` itself,
/// the direct `run_ticks` row sets the same values explicitly so the two folds match.
fn mk_params() -> EngineParams {
    EngineParams {
        fee_rate: 0.0007,
        cash: 100_000.0,
        slippage: 0.0,
        multiplier: 1.0,
        fill_model: FillModelKind::Tick,
        default_venue: Some(VENUE.to_string()),
        ..Default::default()
    }
}

fn mk_cfg() -> TickReplayConfig {
    TickReplayConfig {
        venue: VENUE.to_string(),
        symbols: vec![SYMBOL.to_string()],
        range: TsRange::all(),
        seed_bar_interval_ms: None,
        params: mk_params(),
        snap_to_properties: false,
        feed_latency: false,
        series: None,
    }
}

/// Run `f` (WARMUP + REPEATS) times; `f` does its OWN timing (so per-iteration setup — input
/// clones, engine construction — stays outside the stopwatch) and returns `(elapsed_ns, probe)`.
/// Returns the timed samples + the last probe. (Mirrors `benches/engines.rs`.)
fn bench<T, F: FnMut() -> (u128, T)>(mut f: F) -> (Vec<u128>, T) {
    let mut times = Vec::with_capacity(REPEATS);
    let mut out = None;
    for k in 0..(WARMUP + REPEATS) {
        let (dt, probe) = f();
        if k >= WARMUP {
            times.push(dt);
        }
        out = Some(probe);
    }
    (times, out.unwrap())
}

fn median(mut t: Vec<u128>) -> u128 {
    t.sort_unstable();
    t[t.len() / 2]
}

fn row(label: &str, times: &[u128], ticks: usize) -> f64 {
    let med = median(times.to_vec()) as f64;
    let ns_per_tick = med / ticks as f64;
    println!(
        "{label:<26}{ticks:>10}{:>13.1}{ns_per_tick:>12.1}{:>14.2}",
        med / 1e6,
        1e3 / ns_per_tick // Mticks/s == 1e9 / ns_per_tick / 1e6
    );
    ns_per_tick
}

fn main() {
    println!("=== vike-backtest tick-replay bench — loader vs engine per-tick baseline ===");
    println!(
        "single symbol, {N_QUOTES} quotes + {N_TRADES} trades (+ {N_BOOKS} book events), no-op \
         strategy, {REPEATS} timed reps ({WARMUP} warmup discarded), median reported\n"
    );

    let quotes = synth_quotes(N_QUOTES);
    let trades = synth_trades(N_TRADES);
    let books = synth_books(N_BOOKS);

    let store_qt: Arc<dyn HistStore + Send + Sync> =
        Arc::new(BenchStore { quotes: quotes.clone(), trades: trades.clone(), books: Vec::new() });
    let store_qtb: Arc<dyn HistStore + Send + Sync> =
        Arc::new(BenchStore { quotes: quotes.clone(), trades: trades.clone(), books });

    let n_qt = N_QUOTES + N_TRADES;
    let n_qtb = n_qt + N_BOOKS;

    // Sanity OUTSIDE the timed loops: the replay actually replays the whole tape (a no-op
    // strategy trades nothing; every quote/trade pushes one equity sample — book events do not).
    let probe = replay_ticks(Arc::clone(&store_qt), NoopStrategy, mk_cfg()).expect("replay");
    assert_eq!(probe.n_trades, 0, "no-op strategy must not trade");
    assert_eq!(probe.equity_curve.len(), n_qt, "every quote+trade tick must fold");
    let probe_b = replay_ticks(Arc::clone(&store_qtb), NoopStrategy, mk_cfg()).expect("replay");
    assert_eq!(probe_b.equity_curve.len(), n_qt, "book events fold but never sample equity");

    // (1) the production loader end-to-end, quotes+trades.
    let (t_replay_qt, _) = bench(|| {
        let store = Arc::clone(&store_qt);
        let t0 = Instant::now();
        let r = replay_ticks(store, NoopStrategy, mk_cfg()).expect("replay");
        (t0.elapsed().as_nanos(), r.final_equity)
    });

    // (2) same, plus the recorded book series (snapshot + contiguous deltas).
    let (t_replay_qtb, _) = bench(|| {
        let store = Arc::clone(&store_qtb);
        let t0 = Instant::now();
        let r = replay_ticks(store, NoopStrategy, mk_cfg()).expect("replay");
        (t0.elapsed().as_nanos(), r.final_equity)
    });

    // (3) the ENGINE fold alone: run_ticks over the pre-merged Q+T stream. Engine construction
    // stays OUTSIDE the stopwatch (the engines.rs discipline), so this row is pure per-tick fold.
    let merged_qt: Vec<Tick> = merge_ticks(quotes.clone(), trades.clone(), Vec::new());
    assert_eq!(merged_qt.len(), n_qt);
    let ticks_by_symbol = vec![(SYMBOL.to_string(), merged_qt)];
    let (t_run, _) = bench(|| {
        let mut eng =
            StrategyEngine::new(vec![(SYMBOL.to_string(), Vec::new())], NoopStrategy, mk_params());
        let t0 = Instant::now();
        let r = eng.run_ticks(&ticks_by_symbol);
        (t0.elapsed().as_nanos(), r.final_equity)
    });

    // (4) the merge alone (the loader's per-tick step into `Tick`). Input clones stay OUTSIDE
    // the stopwatch — merge_ticks consumes its inputs, so each iteration needs fresh ones.
    let (t_merge, merged_len) = bench(|| {
        let q = quotes.clone();
        let t = trades.clone();
        let t0 = Instant::now();
        let m = merge_ticks(q, t, Vec::new());
        (t0.elapsed().as_nanos(), m.len())
    });
    assert_eq!(merged_len, n_qt);

    // ============================ REPORT ============================
    println!("{:<26}{:>10}{:>13}{:>12}{:>14}", "row", "ticks", "median ms", "ns/tick", "Mticks/s");
    let r1 = row("replay_ticks Q+T", &t_replay_qt, n_qt);
    row("replay_ticks Q+T+B", &t_replay_qtb, n_qtb);
    let r3 = row("run_ticks only (Q+T)", &t_run, n_qt);
    let r4 = row("merge_ticks only (Q+T)", &t_merge, n_qt);
    println!(
        "\nloader overhead (scan-clone + merge + engine build) = replay(Q+T) − run_ticks(Q+T) \
         ≈ {:.1} ns/tick (merge alone ≈ {:.1} ns/tick)",
        r1 - r3,
        r4
    );
    println!(
        "run_ticks row = per-tick engine fold: k-way-merge clone, clock/latency no-ops, fill \
         phase over an empty order book, equity_now ×2, curve push."
    );
}
