//! Benchmark for the two surviving engines, ONE strategy each:
//!   StrategyEngine       = event engine (any N; backtest + live). @N=1 single, @N=5 multi.
//!   VectorBacktestEngine = vectorized backtest kernel (any S). @S=1 single, @S=5 multi.
//!
//! ONE strategy everywhere (only symbol-count & engine vary): long/flat, target 50% of equity
//! PER SYMBOL, enter every 20 bars, close 10 bars later. Bars from the `market_data/bench_hist`
//! DataFusion store (NOT the long-retired `data/bench_bars.sqlite`), backfilled by the
//! `ingest_bench_bars` bin. Rust, 1 core.
//!   cargo bench -p vike-backtest --features bench-hist
//!
//! Folded in from the retired `vike-bench` crate (Phase 0 crate-reorg, D9): this is a `[[bench]]`
//! target (`harness = false` — it's a `main()` that prints a report, not a libtest bench), gated
//! behind the `bench-hist` feature so default `cargo test/clippy -p vike-backtest` never compiles
//! the DataFusion/Arrow tree. The engine<->kernel parity table this used to print is now also a
//! `#[test]` gate: see `tests/parity/engine_kernel_parity.rs`.

use std::time::Instant;

use serde_json::json;
use vike_backtest::{EngineParams, Matrix, SimBroker, StrategyEngine, VectorBacktestEngine};
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::Bar;
use vike_model::Strategy;

const REPEATS: usize = 7;
const WARMUP: usize = 2; // discarded runs before timing (kills cold-cache effects)

// ---- data config (was the SQLite `meta` table; consts now — the Python exporter is retired) ----
const TAKER_FEE: f64 = 0.0007;
const SLIPPAGE: f64 = 0.0003;
const INIT_CASH: f64 = 100_000.0;
const MULTIPLIER: f64 = 1.0;
const BUILD_TRADES: bool = true;
const SINGLE_SYMBOL: &str = "BTCUSDT";
const MULTI_SYMBOLS: [&str; 5] = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "DOGEUSDT", "ADAUSDT"];

/// This checkout's workspace root, derived at COMPILE time from the crate dir — so the bench
/// reads `market_data/bench_hist` and writes `bench/results_rust.json` in its OWN checkout and runs
/// correctly from any worktree, instead of a hardcoded absolute path that always hit one checkout.
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().to_path_buf()
}

/// Pick the store root from the bench binary's own arguments, defaulting to this checkout's
/// `market_data/bench_hist`.
///
/// ⚠ This MUST skip flags, and that is not cosmetic: `harness = false` only means libtest does not
/// parse our argv — cargo still injects its own. `cargo bench` passes **`--bench`** as the first
/// argument, so the previous `args().nth(1)` read it as a PATH, opened a store literally named
/// `--bench`, found it (of course) empty, and panicked telling the operator to go backfill a store
/// that was fine all along. The documented `cargo bench -p vike-backtest --features bench-hist`
/// invocation could therefore never work — only a direct binary call did.
///
/// So: take the first argument that is not a flag. A bare `--` separator and any `--flag` are
/// skipped; an explicit path still overrides the default, which is what a caller pointing the bench
/// at another checkout's store actually wants.
fn store_root_from_args(args: impl Iterator<Item = String>) -> String {
    args.filter(|a| !a.starts_with('-')).find(|a| !a.is_empty()).unwrap_or_else(|| {
        workspace_root().join("market_data").join("bench_hist").to_string_lossy().into_owned()
    })
}

// ---- the ONE unified strategy ----
const W_ON: usize = 20;
const W_OFF: usize = 10;
const W_PCT: f64 = 0.5;

#[inline]
fn target_at(i: usize) -> Option<f64> {
    if i.is_multiple_of(W_ON) {
        Some(W_PCT)
    } else if i % W_ON == W_OFF {
        Some(0.0)
    } else {
        None
    }
}
fn sym_key(instrument: &Option<String>) -> String {
    let s = instrument.as_deref().unwrap_or("_");
    s.split_once('.').map(|(a, _)| a).unwrap_or(s).to_string()
}

struct MultiTarget;
impl Strategy<SimBroker> for MultiTarget {
    fn on_bar(&mut self, ctx: &mut SimBroker, bar: &Bar) {
        if let Some(w) = target_at(ctx.index) {
            ctx.strategy_order_target_percent(&sym_key(&bar.symbol), w);
        }
    }
}

/// Run `f` (WARMUP + REPEATS) times; `f` returns (elapsed_ns, final_equity, n_trades) and does its
/// OWN timing so engine construction stays outside the stopwatch. Returns (timed samples, eq, nt).
fn bench<F: FnMut() -> (u128, f64, usize)>(mut f: F) -> (Vec<u128>, f64, usize) {
    let mut times = Vec::new();
    let (mut eq, mut nt) = (0.0, 0);
    for k in 0..(WARMUP + REPEATS) {
        let (dt, e, n) = f();
        if k >= WARMUP {
            times.push(dt);
        }
        eq = e;
        nt = n;
    }
    (times, eq, nt)
}
fn median(mut t: Vec<u128>) -> u128 {
    t.sort_unstable();
    t[t.len() / 2]
}
/// Intersect the ts sets of all series and filter each to that common grid (ts-ascending). The
/// vector kernel indexes every symbol by the same `t`, so the per-symbol series must share a ts
/// grid; raw store reads may not. (The retired Python exporter pre-aligned `bars_multi`.)
fn align_to_common_ts(series: &[Vec<Bar>]) -> Vec<Vec<Bar>> {
    use std::collections::BTreeSet;
    let mut common: Option<BTreeSet<i64>> = None;
    for s in series {
        let set: BTreeSet<i64> = s.iter().map(|b| b.ts).collect();
        common = Some(match common {
            None => set,
            Some(c) => c.intersection(&set).copied().collect(),
        });
    }
    let common = common.unwrap_or_default();
    series.iter().map(|s| s.iter().filter(|b| common.contains(&b.ts)).cloned().collect()).collect()
}

fn main() {
    let _log_guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "engines-bench".to_string(),
        ..Default::default()
    });
    tracing::info!("engines-bench starting");
    tracing::info!(workspace_root = %workspace_root().display(), "resolved workspace root");
    let root = store_root_from_args(std::env::args().skip(1));
    let hist = DataFusionHist::open(&root).expect("open bench hist store");
    let (taker_fee, slippage, init_cash, multiplier) = (TAKER_FEE, SLIPPAGE, INIT_CASH, MULTIPLIER);
    let build_trades = BUILD_TRADES;
    let btc = SINGLE_SYMBOL.to_string();
    let load = |sym: &str| -> Vec<Bar> {
        hist.load_bars("binance", sym, "1m", TsRange::all())
            .unwrap_or_else(|e| panic!("load {sym}: {e}"))
    };

    // ---- data (via the HistStore seam; backfill first with the `ingest_bench_bars` bin) ----
    let bars1 = load(SINGLE_SYMBOL);
    assert!(
        !bars1.is_empty(),
        "bench hist store empty at {root} — run: cargo run -p vike-backfill --bin ingest_bench_bars --release"
    );
    let n1 = bars1.len();
    let opens: Vec<f64> = bars1.iter().map(|b| b.open).collect();
    let closes: Vec<f64> = bars1.iter().map(|b| b.close).collect();
    let ts: Vec<i64> = bars1.iter().map(|b| b.ts).collect();

    let symbols: Vec<String> = MULTI_SYMBOLS.iter().map(|s| s.to_string()).collect();
    let s_len = symbols.len();
    // raw per-symbol reads may not share a ts grid → align to the common ts (the vector kernel
    // indexes every symbol by the same `t`). The Python exporter used to pre-align `bars_multi`.
    let per_symbol = align_to_common_ts(&symbols.iter().map(|s| load(s)).collect::<Vec<_>>());
    let ts_m: Vec<i64> = per_symbol[0].iter().map(|b| b.ts).collect();
    let t_len = ts_m.len();

    let make_w = |t: usize, s: usize| {
        let mut w = vec![f64::NAN; t * s];
        for i in 0..t {
            if let Some(v) = target_at(i) {
                for si in 0..s {
                    w[i * s + si] = v;
                }
            }
        }
        w
    };
    let mparams = || EngineParams {
        fee_rate: taker_fee,
        cash: init_cash,
        slippage,
        multiplier,
        ..Default::default()
    };

    // ---- SINGLE (all on BTC bars1) ----
    let single_pair = vec![(btc.clone(), bars1.clone())];
    let (mse1_times, mse1_eq, mse1_nt) = bench(|| {
        let mut e = StrategyEngine::new(single_pair.clone(), MultiTarget, mparams());
        let t = Instant::now();
        let r = e.run();
        (t.elapsed().as_nanos(), r.final_equity, r.trades.len())
    });
    let (om1, cm1, fm1) = (
        Matrix::new(opens.clone(), n1, 1),
        Matrix::new(closes.clone(), n1, 1),
        Matrix::new(vec![0.0; n1], n1, 1),
    );
    let wm1 = Matrix::new(make_w(n1, 1), n1, 1);
    let btc_syms = vec![btc.clone()];
    let (mv1_times, mv1_eq, mv1_nt) = bench(|| {
        let t = Instant::now();
        let r = VectorBacktestEngine::run(
            &om1,
            &cm1,
            &fm1,
            &ts,
            &wm1,
            taker_fee, // maker side is dead in this kernel — see vector_engine's module doc
            taker_fee,
            slippage,
            init_cash,
            multiplier,
            Some(&btc_syms),
            build_trades,
        );
        (t.elapsed().as_nanos(), r.final_equity, r.n_trades)
    });

    // ---- MULTI (5 symbols) ----
    let bars_by_symbol: Vec<(String, Vec<Bar>)> =
        symbols.iter().cloned().zip(per_symbol.iter().cloned()).collect();
    let (mse5_times, mse5_eq, mse5_nt) = bench(|| {
        let mut e = StrategyEngine::new(bars_by_symbol.clone(), MultiTarget, mparams());
        let t = Instant::now();
        let r = e.run();
        (t.elapsed().as_nanos(), r.final_equity, r.trades.len())
    });
    let mut om = vec![0.0; t_len * s_len];
    let mut cm = vec![0.0; t_len * s_len];
    for (si, series) in per_symbol.iter().enumerate() {
        for (t, b) in series.iter().enumerate() {
            om[t * s_len + si] = b.open;
            cm[t * s_len + si] = b.close;
        }
    }
    let (om5, cm5, fm5) = (
        Matrix::new(om, t_len, s_len),
        Matrix::new(cm, t_len, s_len),
        Matrix::new(vec![0.0; t_len * s_len], t_len, s_len),
    );
    let wm5 = Matrix::new(make_w(t_len, s_len), t_len, s_len);
    let (mv5_times, mv5_eq, mv5_nt) = bench(|| {
        let t = Instant::now();
        let r = VectorBacktestEngine::run(
            &om5,
            &cm5,
            &fm5,
            &ts_m,
            &wm5,
            taker_fee, // maker side is dead in this kernel — see vector_engine's module doc
            taker_fee,
            slippage,
            init_cash,
            multiplier,
            Some(&symbols),
            build_trades,
        );
        (t.elapsed().as_nanos(), r.final_equity, r.n_trades)
    });

    // ============================ REPORT ============================
    let nspb = |t: &[u128], n: usize| median(t.to_vec()) as f64 / n as f64;
    println!(
        "\n=== UNIFIED strategy (long/flat 50%-equity, enter every {W_ON} bars, close +{W_OFF}) | Rust, 1 core ==="
    );
    println!("every engine runs the SAME decision; only symbol-count & engine vary\n");
    println!(
        "{:<24}{:>8}{:>6}{:>11}{:>10}{:>9}{:>16}",
        "engine", "bars", "syms", "ns/bar", "ns/cell", "trades", "final_equity"
    );
    let row = |name: &str, t: &[u128], nb: usize, ns: usize, nt: usize, eq: f64| {
        println!(
            "{:<24}{:>8}{:>6}{:>11.1}{:>10.2}{:>9}{:>16.4}",
            name,
            nb,
            ns,
            nspb(t, nb),
            nspb(t, nb * ns),
            nt,
            eq
        );
    };
    row("StrategyEngine@N=1", &mse1_times, n1, 1, mse1_nt, mse1_eq);
    row("VectorBacktestEngine@S=1", &mv1_times, n1, 1, mv1_nt, mv1_eq);
    row("StrategyEngine@N=5", &mse5_times, t_len, s_len, mse5_nt, mse5_eq);
    row("VectorBacktestEngine@S=5", &mv5_times, t_len, s_len, mv5_nt, mv5_eq);

    println!("\n-- parity (unified strategy => identical results expected) --");
    println!(
        "SINGLE (2 engines): trades [{mse1_nt},{mv1_nt}]  |Δequity|={:.2e}",
        (mse1_eq - mv1_eq).abs()
    );
    println!(
        "MULTI  (2 engines): trades [{mse5_nt},{mv5_nt}]  |Δequity|={:.2e}",
        (mse5_eq - mv5_eq).abs()
    );

    // ===== SCALING SWEEP: portfolio size — Merged engines only (synthetic bars) =====
    // SingleSymbolEngine / fast_backtest are single-symbol and CANNOT run a portfolio.
    const SWEEP_T: usize = 2000;
    println!(
        "\n===== SCALING: portfolio size — StrategyEngine (event) vs VectorBacktestEngine (vector) ====="
    );
    println!(
        "synthetic bars, T={SWEEP_T}; ns/cell = time per (bar x symbol). SingleSymbolEngine N/A (single-symbol only)."
    );
    println!(
        "{:>6}{:>16}{:>16}{:>12}{:>12}",
        "syms", "event ns/cell", "vector ns/cell", "event/vec", "parity|Δ|"
    );
    let mut sweep_json = Vec::new();
    for &nn in &[1usize, 5, 20, 50] {
        let names: Vec<String> = (0..nn).map(|si| format!("SYM{si}")).collect();
        let series: Vec<Vec<Bar>> = (0..nn)
            .map(|si| {
                (0..SWEEP_T)
                    .map(|i| {
                        let px = 100.0 + i as f64 * 0.01 + si as f64 * 0.5;
                        Bar {
                            ts: 1_700_000_000_000 + i as i64 * 60_000,
                            open: px,
                            high: px * 1.001,
                            low: px * 0.999,
                            close: px,
                            volume: 0.0,
                            funding: None,
                            bid: None,
                            ask: None,
                            symbol: None,
                        }
                    })
                    .collect()
            })
            .collect();
        let bbs: Vec<(String, Vec<Bar>)> =
            names.iter().cloned().zip(series.iter().cloned()).collect();
        let tsn: Vec<i64> = series[0].iter().map(|b| b.ts).collect();
        // event engine (median of 3, 1 warmup; construction outside timer)
        let mut et = Vec::new();
        let (mut eeq, mut ent) = (0.0, 0usize);
        for k in 0..4 {
            let mut e = StrategyEngine::new(bbs.clone(), MultiTarget, mparams());
            let t0 = Instant::now();
            let r = e.run();
            if k >= 1 {
                et.push(t0.elapsed().as_nanos());
            }
            eeq = r.final_equity;
            ent = r.trades.len();
        }
        // vector kernel
        let mut omn = vec![0.0; SWEEP_T * nn];
        let mut cmn = vec![0.0; SWEEP_T * nn];
        for (si, s) in series.iter().enumerate() {
            for (t, b) in s.iter().enumerate() {
                omn[t * nn + si] = b.open;
                cmn[t * nn + si] = b.close;
            }
        }
        let (omm, cmm, fmm) = (
            Matrix::new(omn, SWEEP_T, nn),
            Matrix::new(cmn, SWEEP_T, nn),
            Matrix::new(vec![0.0; SWEEP_T * nn], SWEEP_T, nn),
        );
        let wmm = Matrix::new(make_w(SWEEP_T, nn), SWEEP_T, nn);
        let mut vt = Vec::new();
        let (mut veq, mut vnt) = (0.0, 0usize);
        for k in 0..4 {
            let t0 = Instant::now();
            let r = VectorBacktestEngine::run(
                &omm,
                &cmm,
                &fmm,
                &tsn,
                &wmm,
                taker_fee, // maker side is dead in this kernel — see vector_engine's module doc
                taker_fee,
                slippage,
                init_cash,
                multiplier,
                Some(&names),
                build_trades,
            );
            if k >= 1 {
                vt.push(t0.elapsed().as_nanos());
            }
            veq = r.final_equity;
            vnt = r.n_trades;
        }
        let _ = (ent, vnt);
        let cells = (SWEEP_T * nn) as f64;
        let (ecell, vcell) = (median(et) as f64 / cells, median(vt) as f64 / cells);
        println!(
            "{:>6}{:>16.1}{:>16.2}{:>12.1}{:>12.1e}",
            nn,
            ecell,
            vcell,
            ecell / vcell,
            (eeq - veq).abs()
        );
        sweep_json.push(json!({"syms": nn, "event_ns_per_cell": ecell, "vector_ns_per_cell": vcell, "event_over_vector": ecell / vcell}));
    }

    let out = json!({
        "lang": "rust", "store": root, "repeats": REPEATS,
        "strategy": "unified long/flat 50% equity, enter every 20 bars, close +10",
        "scaling_sweep": sweep_json,
        "rows": {
            "StrategyEngine@N=1": {"bars": n1, "syms": 1, "ns_per_bar": nspb(&mse1_times, n1), "trades": mse1_nt, "final_equity": mse1_eq},
            "VectorBacktestEngine@S=1": {"bars": n1, "syms": 1, "ns_per_bar": nspb(&mv1_times, n1), "trades": mv1_nt, "final_equity": mv1_eq},
            "StrategyEngine@N=5": {"bars": t_len, "syms": s_len, "ns_per_bar": nspb(&mse5_times, t_len), "ns_per_cell": nspb(&mse5_times, t_len * s_len), "trades": mse5_nt, "final_equity": mse5_eq},
            "VectorBacktestEngine@S=5": {"bars": t_len, "syms": s_len, "ns_per_bar": nspb(&mv5_times, t_len), "ns_per_cell": nspb(&mv5_times, t_len * s_len), "trades": mv5_nt, "final_equity": mv5_eq}
        }
    });
    let out_path = workspace_root().join("bench").join("results_rust.json");
    std::fs::write(&out_path, serde_json::to_string_pretty(&out).unwrap()).unwrap();
    println!("\nwrote {}", out_path.display());
}

// NOTE: the `align_to_common_ts` unit test that used to live here was DELETED, not relocated —
// `[[bench]]` targets don't run `cargo test` unit tests (harness = false, and bench-hist is
// off by default anyway), so it never ran as part of the gate. Its coverage is replaced by
// `tests/parity/engine_kernel_parity.rs`, which exercises the ts-alignment path indirectly via the
// vectorized kernel on synthetic multi-symbol bars.
