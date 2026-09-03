//! BEFORE baseline for the vike-chart render data-path (chart-perf phase, task T1), plus the
//! AFTER numbers for `ChartState::sync` (task T2), `ChartState::refresh_caches` (task T3), and
//! `render_overlay`'s visible-window map (task T4).
//!
//! Plain `main()` + `std::time::Instant`, mirroring `crates/vike-backtest/benches/engines.rs`
//! (`harness = false`, no `criterion` dependency, median-of-N after a warmup). Every BEFORE-row
//! timed block replicates a CURRENT hot-path loop from `vike-app`/`vike-chart` verbatim — a
//! `[[bench]]` target compiles as a separate binary that links `vike-chart` as an EXTERNAL crate,
//! so it can only reach `pub` items; the OLD `sync_from_core` loop (in `vike-app`, a different
//! crate entirely; now retired), `chart::heikin_ashi` (module-private, not `pub`), and
//! `render::overlay_visible_range` (`pub(crate)`, not `pub`) are reproduced inline below rather
//! than called, but are kept byte-for-byte identical to their source so the timings are real.
//! The AFTER rows for `sync`/`refresh_caches` call them directly — both are plain `pub fn`s in
//! vike-chart, reachable from this external bench binary with no need to reproduce them inline.
//!
//! T1 was measurement infrastructure only; later chart-perf tasks re-run this same bench to
//! produce AFTER numbers for the same rows (T2 adds the `sync` rows; T3 adds the
//! `refresh_caches` incremental-append row (a2b) and — since `sync` calls `refresh_caches`
//! internally — also speeds up the (a4) one-close `sync` row for free; T4 adds the (b-after)
//! overlay-map-visible-window rows).
//!
//!   cargo bench -p vike-chart --bench render

use std::sync::Arc;
use std::time::Instant;

use vike_chart::model::{Bar as ModelBar, ChartState, TransformParams};
use vike_chart::transforms;
use vike_chart::ChartStyle;

const N: usize = 200_000;
const REPEATS: usize = 7;
const WARMUP: usize = 2; // discarded runs before timing (kills cold-cache effects)

/// One transform-cache (T6) result row: `(style name, tier, cold, warm, static)` timing samples.
type T6Row = (&'static str, &'static str, Vec<u128>, Vec<u128>, Vec<u128>);

/// Deterministic per-index OHLCV wave (no RNG, so BEFORE/AFTER runs are exactly comparable):
/// a small sine wave around a base price for O/H/L/C, a varying volume. Shared by
/// `synth_raw_bars`/`synth_model_bars` so the two series round-trip to identical values.
fn wave(i: usize) -> (i64, f64, f64, f64, f64, f64) {
    let base = 100.0 + 10.0 * (i as f64 * 0.001).sin();
    let spread = 0.5 + 0.25 * (i as f64 * 0.01).cos().abs();
    let o = base;
    let c = base + 0.1 * (i as f64 * 0.05).sin();
    let h = o.max(c) + spread;
    let l = o.min(c) - spread;
    let v = 10.0 + 5.0 * (i as f64 * 0.02).sin().abs();
    let ot = 1_700_000_040_000 + i as i64 * 60_000;
    (ot, o, h, l, c, v)
}

/// `n` closed bars as the wire type `vike_model::Bar` — what `sync_from_core` reads FROM.
fn synth_raw_bars(n: usize) -> Arc<Vec<vike_model::Bar>> {
    Arc::new(
        (0..n)
            .map(|i| {
                let (ts, o, h, l, c, v) = wave(i);
                vike_model::Bar {
                    ts,
                    open: o,
                    high: h,
                    low: l,
                    close: c,
                    volume: v,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: None,
                }
            })
            .collect(),
    )
}

/// Same as [`synth_raw_bars`], but every bar's timestamp is shifted by `epoch_shift_ms` — same
/// length, same O/H/L/C/V shape, but a different `(first ts, last ts)` boundary. Used to force
/// `ChartState::sync`'s full-rebuild branch (a genuine reload/symbol-swap), since the "unchanged
/// prefix" fast path is keyed on `(len, first ts, last ts)`, not object identity.
fn synth_raw_bars_shifted(n: usize, epoch_shift_ms: i64) -> Arc<Vec<vike_model::Bar>> {
    Arc::new(
        (0..n)
            .map(|i| {
                let (ts, o, h, l, c, v) = wave(i);
                vike_model::Bar {
                    ts: ts + epoch_shift_ms,
                    open: o,
                    high: h,
                    low: l,
                    close: c,
                    volume: v,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: None,
                }
            })
            .collect(),
    )
}

/// `n` closed bars as the render-model type `vike_chart::model::Bar` — what `sync_from_core`
/// writes TO. Derived from the same `wave()` samples as `synth_raw_bars`, so `synth_model_bars(n)`
/// is exactly what running the current `sync_from_core::render` closure over `synth_raw_bars(n)`
/// would produce (round-trips to identical o/h/l/c/v/ot).
fn synth_model_bars(n: usize) -> Vec<ModelBar> {
    (0..n)
        .map(|i| {
            let (ot, o, h, l, c, v) = wave(i);
            ModelBar { t: i as f64, ot, o, h, l, c, v }
        })
        .collect()
}

/// Byte-for-byte copy of `chart::heikin_ashi` (`crates/vike-chart/src/chart/series.rs`'s
/// `heikin_ashi`) — that fn is module-private (`fn`, not `pub fn`), unreachable from this
/// external bench binary. Keep in sync if the source changes.
fn heikin_ashi_before(bars: &[ModelBar]) -> Vec<ModelBar> {
    let mut out: Vec<ModelBar> = Vec::with_capacity(bars.len());
    for (i, b) in bars.iter().enumerate() {
        let ha_close = (b.o + b.h + b.l + b.c) / 4.0;
        let ha_open = if i == 0 { (b.o + b.c) / 2.0 } else { (out[i - 1].o + out[i - 1].c) / 2.0 };
        out.push(ModelBar {
            t: b.t,
            ot: b.ot,
            o: ha_open,
            h: b.h.max(ha_open).max(ha_close),
            l: b.l.min(ha_open).min(ha_close),
            c: ha_close,
            v: b.v,
        });
    }
    out
}

/// Run `f` (WARMUP + REPEATS) times, returning only the timed (post-warmup) elapsed-ns samples.
/// `black_box`ed so the optimizer can't prove `f`'s result is unused and elide the work.
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

fn row(label: &str, times: &[u128], n: usize) {
    let med_ns = median(times.to_vec());
    println!(
        "{label:<38}{med_ns:>14} ns/op{:>14.3} ms/op{:>12.2} ns/elem",
        med_ns as f64 / 1e6,
        med_ns as f64 / n as f64
    );
}

// ---------------------------------------------------------------------------------------------
// Per-frame INDICATOR OVERLAY cost (chart-perf: the forming-bar preview)
// ---------------------------------------------------------------------------------------------

/// ⚠ **The per-frame cost of an overlay is the KERNEL RE-RUN, not the copy** — and the copy is the
/// intuitive wrong answer, which is why this says so.
///
/// `crates/vike-chart/src/indicators.rs`'s `Active::update` ends with a speculative forming-bar
/// preview on a THROWAWAY CLONE: `self.ind.clone_box().on_bar(&self.model_bar(f))`. `clone_box` does
/// copy the indicator whole, including the `hist: Vec<Bar>` that
/// `crates/vike-indicators/src/lib.rs`'s `keep_for` sizes — but that is the SMALL term. The large one
/// is `on_bar`: for a `hist_indicator!` indicator that is `vike_indicators`' `stream_tail`, which
/// re-runs the whole batch kernel over ALL retained history and keeps only the last element. For a
/// windowed kernel that is O(retained x period) per frame where O(period) would do.
///
/// Both terms scale with retained bars, so `WindowReach` still governs the cost — just through the
/// arithmetic rather than the memcpy. At period 1000 the measured frame is ~534 us AFTER retention
/// dropped 62x, which no plausible 124 KB copy could account for.
///
/// ⚠ **The CONTROL ROWS are the point of this table.** `sma` and `ema` are hand-written incremental
/// impls (`crates/vike-indicators/src/indicators/base.rs`) — they never call `keep_for`, so their
/// rows must NOT move when retention changes. If they do, the harness is measuring machine noise or
/// a compiler difference rather than the retention, and no other row here means anything.
fn bench_overlay_frames() -> Vec<(&'static str, Vec<f64>, Vec<u128>)> {
    // ⚠ NOT `N`. The overlay section deliberately uses its own, much smaller bar count, and the
    // reason is a finding in itself: `Active::new` folds every bar through `push_bar`, and each
    // `push_bar` calls `vike_indicators`' `stream_tail`, which RE-RUNS the batch kernel over all
    // retained history. Construction is therefore O(bars x retained x period). At `N` = 200,000
    // with a deep-period indicator that is ~10^13 operations — the first attempt at this bench sat
    // in `Active::new` for over an hour before being killed. `OVERLAY_N` is a realistic on-screen
    // window, which is what the per-frame number is actually about.
    const OVERLAY_N: usize = 5_000;
    // `Active::new` takes the chart's own `Bar`, not the model's.
    let closed: Vec<vike_chart::model::Bar> = (0..OVERLAY_N)
        .map(|i| {
            let (ot, o, h, l, c, v) = wave(i);
            vike_chart::model::Bar { t: i as f64, ot, o, h, l, c, v }
        })
        .collect();
    let forming = *closed.last().unwrap();

    // name, params (empty = registry default)
    let cases: &[(&str, &[f64])] = &[
        // MOVED by the Finite grants — the rows expected to fall.
        ("doji", &[]), // pattern family: KEEP_FLOOR 256 -> FINITE_FLOOR 64
        ("hammer", &[]),
        ("high_low_52w", &[1000.0]), // 64*999 = 63,936 retained -> 999+32 = 1,031
        ("midpoint", &[400.0]),
        ("linearreg", &[400.0]),
        ("kurtosis", &[400.0]),
        // ⚠ CONTROLS — must NOT move.
        ("sma", &[]),  // hand-written incremental, never trims
        ("ema", &[]),  // hand-written incremental, never trims
        ("kst", &[]),  // still Smoothed: smooth_defined over a series with interior NaN
        ("hvol", &[]), // still Smoothed, same reason
    ];

    let mut out = Vec::new();
    for (name, params) in cases {
        let Some(spec) = vike_chart::indicators::get(name) else {
            println!("  (skip {name}: not in the chart registry)");
            continue;
        };
        let mut a = vike_chart::indicators::Active::new(1, spec, &closed);
        if !params.is_empty() {
            a.set_params(params.to_vec(), &closed);
        }
        // ⚠ TWO rows, and both are needed. Timing only the repeated-identical-bar case would
        // measure the memo rather than the work it avoids; timing only the moving case would hide
        // the win entirely. A live chart is mostly HITs (repaints outnumber ticks) with a MISS on
        // every tick, so the honest report is both numbers side by side.
        let times_hit = time_it(REPEATS, WARMUP, || {
            a.update(&closed, Some(&forming));
        });
        // Perturb the forming bar every rep so the fingerprint always differs: this is the real
        // per-tick cost, and it is what the memo does NOT avoid.
        let mut tick = 0u32;
        let times_miss = time_it(REPEATS, WARMUP, || {
            tick = tick.wrapping_add(1);
            let mut f = forming;
            f.c += f64::from(tick) * 1e-7;
            a.update(&closed, Some(&f));
        });
        out.push((*name, params.to_vec(), times_hit));
        out.push((*name, params.to_vec(), times_miss));
    }
    out
}
fn main() {
    println!(
        "=== vike-chart render bench — BEFORE baseline (T1) + AFTER sync (T2) + AFTER refresh_caches (T3) + AFTER overlay map (T4) ==="
    );
    println!("n={N} bars, {REPEATS} timed reps ({WARMUP} warmup discarded), median reported\n");

    let raw = synth_raw_bars(N);
    let model_bars = synth_model_bars(N);
    assert_eq!(model_bars.len(), raw.len(), "raw/model bar counts must match");
    let mid = N / 2;
    assert_eq!(
        model_bars[mid].o, raw[mid].open,
        "raw/model bars must round-trip to the same values"
    );
    assert_eq!(
        model_bars[mid].ot, raw[mid].ts,
        "raw/model bars must round-trip to the same values"
    );

    // (a) the CURRENT full ChartState bar rebuild: `crates/vike-app/src/main.rs`'s
    // `sync_from_core` clears `cs.bars` and pushes every closed bar back on each snapshot.
    // `cs` is reused across iterations (as it is across real frames) so `clear()` keeps its
    // already-grown capacity — the realistic steady-state cost, not a cold first-ever allocation.
    // NOTE: this row deliberately EXCLUDES the trailing `cs.refresh_caches()`
    // (`crates/vike-app/src/main.rs`'s `sync_from_core`), which is O(1) on forming-ticks and O(N)
    // only on a bar CLOSE — measured separately in row (a2); rows (a) + (a2) together represent
    // the full per-snapshot cost.
    let mut cs = vike_chart::model::ChartState::default();
    let t_rebuild = time_it(REPEATS, WARMUP, || {
        cs.bars.clear();
        let render = |i: usize, b: &vike_model::Bar| ModelBar {
            t: i as f64,
            ot: b.ts,
            o: b.open,
            h: b.high,
            l: b.low,
            c: b.close,
            v: b.volume,
        };
        for (i, b) in raw.iter().enumerate() {
            cs.bars.push(render(i, b));
        }
        cs.closed_len = cs.bars.len();
        cs.bars.len()
    });

    // (a2) refresh_caches() on a bar CLOSE: `crates/vike-app/src/main.rs`'s `sync_from_core` ends
    // with `cs.refresh_caches()`. Its O(1) key-guard (key = (closed_len, first ot, last ot))
    // short-circuits on a forming-tick (closed prefix unchanged), so the interesting cost is only
    // on a bar CLOSE — when the key changes and it recomputes `y_ext` (an O(N) min/max fold) PLUS
    // `hour_marks` (a chrono `from_timestamp_millis` + `with_timezone(&Local)` + `.minute()` PER
    // bar, materially non-trivial). `refresh_caches` is a plain `pub fn`, called directly on a
    // warm ChartState; we bump the last closed bar's `ot` each iteration to force a key change
    // (defeat the O(1) short-circuit) so we time the real recompute, not the guarded no-op.
    let mut cs_rc = vike_chart::model::ChartState::default();
    cs_rc.bars = synth_model_bars(N);
    cs_rc.closed_len = N;
    cs_rc.refresh_caches(); // warm once: grows hour_marks capacity + seeds the key
    let last_idx = cs_rc.bars.len() - 1;
    let t_refresh = time_it(REPEATS, WARMUP, || {
        cs_rc.bars[last_idx].ot += 60_000; // monotonic → key differs each call → forces recompute
        cs_rc.refresh_caches();
        cs_rc.y_ext
    });

    // (a2b) AFTER (chart-perf T3): `refresh_caches`'s incremental append fast-path. Closed
    // bars are immutable/append-only, so on a ONE-bar close over the same warm 200k state as
    // (a2) above, it now does O(1) work — extend `y_ext` with just the new bar's low/high, and
    // append its (offset) `hour_mark_indices` contribution — instead of re-folding/re-scanning
    // all 200k closed bars. `cs_rc2` is warmed to len N exactly like `cs_rc` (one `refresh_caches`
    // call to seed `cache_key`/`y_ext`/`hour_marks`); each timed iteration appends exactly ONE
    // new closed bar (a distinct `wave()` sample per call, so every call is a genuine key change,
    // never a stale repeat) and times `refresh_caches` alone — the real incremental cost, not the
    // O(1) unchanged-key guard.
    let mut cs_rc2 = vike_chart::model::ChartState::default();
    cs_rc2.bars = synth_model_bars(N);
    cs_rc2.closed_len = N;
    cs_rc2.refresh_caches(); // warm once: seeds cache_key + y_ext + hour_marks at len N
    let rc2_steps = WARMUP + REPEATS;
    let rc2_extra: Vec<ModelBar> = (0..rc2_steps)
        .map(|k| {
            let (ot, o, h, l, c, v) = wave(N + k);
            ModelBar { t: (N + k) as f64, ot, o, h, l, c, v }
        })
        .collect();
    let mut rc2_idx = 0usize;
    let t_refresh_incremental = time_it(REPEATS, WARMUP, || {
        cs_rc2.bars.push(rc2_extra[rc2_idx]);
        cs_rc2.closed_len += 1;
        rc2_idx += 1;
        cs_rc2.refresh_caches();
        cs_rc2.y_ext
    });

    // (a3/a4/a5) AFTER (chart-perf T2 + T3): `ChartState::sync` replaces the (a) full clear+rebuild
    // loop above — it now lives in vike-chart itself (a plain `pub fn`, reachable from this
    // external bench binary) and folds `(closed, forming)` incrementally instead of rerendering
    // history every call. It still ENDS by calling `refresh_caches()` internally (rows (a) and
    // (a2) above were timed separately; `sync` is timed as ONE call, matching the real per-snapshot
    // cost). As of T3, `refresh_caches` is ALSO incremental on its append path (see (a2b) above),
    // so a genuine bar CLOSE below now pays O(delta) for both the bar-render loop (T2) AND the
    // y_ext/hour_marks fold (T3) — not just the render loop.
    let mut forming0 = {
        let (ts, o, h, l, c, v) = wave(N);
        vike_model::Bar {
            ts,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    };

    // (a3) forming-tick on a warm 200k state: `closed` (the same `raw` Arc, unchanged) hits
    // `sync`'s "unchanged prefix" fast path — O(1) (renders only the forming bar), and
    // `refresh_caches`'s key-guard also short-circuits (the closed prefix didn't change) — this
    // is the ~60Hz steady-state tick, the overwhelming majority of calls in practice.
    let mut cs_tick = ChartState::default();
    cs_tick.sync(&raw, Some(&forming0)); // warm: one-time full build, matches first-ever sync
    let t_sync_forming_tick = time_it(REPEATS, WARMUP, || {
        forming0.close += 0.001; // mutate the live bar only; `closed` (and its key) is untouched
        cs_tick.sync(&raw, Some(&forming0));
        cs_tick.bars.len()
    });

    // (a4) one bar closes on top of a warm 200k state: `sync`'s "append" fast path renders
    // exactly the ONE newly-closed bar (not the other 200k), and (as of T3) the `refresh_caches`
    // it calls internally now also extends `y_ext`/`hour_marks` with just that one bar (O(1), not
    // O(N)). Building each iteration's one-bar-longer `Arc<Vec<Bar>>` (the Vec clone + push a real venue
    // feed's snapshot layer would also pay) is done OUTSIDE the timed closure, so only `sync`
    // itself is measured.
    let steps = WARMUP + REPEATS;
    let mut growing: Vec<vike_model::Bar> = raw.as_ref().clone();
    let mut closed_steps: Vec<Arc<Vec<vike_model::Bar>>> = Vec::with_capacity(steps);
    let mut forming_steps: Vec<vike_model::Bar> = Vec::with_capacity(steps);
    for k in 0..steps {
        let (ts, o, h, l, c, v) = wave(N + k);
        growing.push(vike_model::Bar {
            ts,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
        closed_steps.push(Arc::new(growing.clone()));
        let (fts, fo, fh, fl, fc, fv) = wave(N + k + 1);
        forming_steps.push(vike_model::Bar {
            ts: fts,
            open: fo,
            high: fh,
            low: fl,
            close: fc,
            volume: fv,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
    }
    let mut cs_close = ChartState::default();
    cs_close.sync(&raw, Some(&forming0)); // warm to the same N-bar steady state as (a3)
    let mut close_idx = 0usize;
    let t_sync_one_close = time_it(REPEATS, WARMUP, || {
        cs_close.sync(&closed_steps[close_idx], Some(&forming_steps[close_idx]));
        close_idx += 1;
        cs_close.bars.len()
    });

    // (a5) reload: a structural change (symbol/timeframe swap, or history shrinking) that
    // `sync` cannot special-case — full clear+rebuild, same asymptotic cost as (a)+(a2)
    // combined. Alternates between two same-length, differently-epoched series so EVERY timed
    // call (including the warmup ones) is a genuine boundary mismatch, never an unchanged-prefix
    // repeat.
    let reload_raw = synth_raw_bars_shifted(N, 60_000);
    let mut cs_reload = ChartState::default();
    let mut toggle = false;
    let t_sync_reload = time_it(REPEATS, WARMUP, || {
        let src = if toggle { &reload_raw } else { &raw };
        toggle = !toggle;
        cs_reload.sync(src, Some(&forming0));
        cs_reload.bars.len()
    });

    // (b) render_overlay's full-series map+collect (`crates/vike-chart/src/render.rs`'s
    // `render_overlay`): `mapped: Vec<f64> = line.series.iter().map(|&v| map(v)).collect()`. Two
    // `map` closures: identity (linear scale) and log10 (log scale) — the two ScaleMode variants
    // render_overlay is called with.
    let closes: Vec<f64> = model_bars.iter().map(|b| b.c).collect();
    let identity: &dyn Fn(f64) -> f64 = &|v| v;
    let log_map: &dyn Fn(f64) -> f64 = &|v: f64| if v > 0.0 { v.log10() } else { f64::NAN };
    let t_map_identity = time_it(REPEATS, WARMUP, || {
        closes.iter().map(|&v| identity(v)).collect::<Vec<f64>>().len()
    });
    let t_map_log10 =
        time_it(REPEATS, WARMUP, || closes.iter().map(|&v| log_map(v)).collect::<Vec<f64>>().len());

    // (b-after) AFTER (chart-perf T4): `render_overlay` now slices to `overlay_visible_range`'s
    // `[lo, hi)` window BEFORE mapping (render.rs), instead of mapping the whole `line.series`.
    // `overlay_visible_range` is `pub(crate)`, unreachable from this external bench binary (same
    // reason `visible_slice` is reproduced inline for row (c) below) — reproduced here byte-for-
    // byte against its exact ±2-bar-guard formula. Windowed to a realistic ~200-bar visible
    // viewport centered in the 200k series (a normal zoomed-in chart view), not the full series
    // the BEFORE rows above measured.
    fn overlay_visible_range_after(n: usize, x0: f64, x1: f64) -> (usize, usize) {
        if n == 0 {
            return (0, 0);
        }
        let lo = (x0 - 2.0).floor().clamp(0.0, n as f64) as usize;
        let hi_f = (x1 + 2.0).floor() + 1.0;
        let hi = hi_f.clamp(0.0, n as f64) as usize;
        (lo, hi.max(lo))
    }
    let vis_x0 = (N / 2) as f64 - 100.0;
    let vis_x1 = (N / 2) as f64 + 100.0;
    let (vlo, vhi) = overlay_visible_range_after(closes.len(), vis_x0, vis_x1);
    let t_map_identity_after = time_it(REPEATS, WARMUP, || {
        closes[vlo..vhi].iter().map(|&v| identity(v)).collect::<Vec<f64>>().len()
    });
    let t_map_log10_after = time_it(REPEATS, WARMUP, || {
        closes[vlo..vhi].iter().map(|&v| log_map(v)).collect::<Vec<f64>>().len()
    });

    // (c) draw_volume_candles' vmax fold (`crates/vike-chart/src/render.rs`'s
    // `draw_volume_candles`): `visible_slice(bars, x0, x1).iter().map(|b| b.v).fold(0.0,
    // f64::max)`. Baselined here over the FULL series — the worst case, when the visible range
    // covers every bar (`visible_slice` itself is `pub(crate)`, unreachable from this external
    // bench binary; the full fold is what it degrades to when x0..x1 spans the whole series).
    let t_vmax = time_it(REPEATS, WARMUP, || {
        model_bars.iter().map(|b| b.v).fold(0.0_f64, f64::max).max(1e-9)
    });

    // (c-after) AFTER (chart-perf T5): `ChartState::visible_vol_max` — the CLOSED-bar max,
    // cached by `(lo, hi, cache_key)` — is `pub`, reachable from this external bench binary
    // directly (unlike `visible_slice`/`draw_volume_candles`, both unreachable, which is why
    // row (c) above reproduces the OLD inline fold instead of calling into render.rs). Two
    // rows over the same warm 200k-bar `ChartState` (closed_len = N, no forming bar — the
    // forming bar's volume is `.max()`'d in by the caller separately, an O(1) scalar compare
    // not timed here), windowed to the same ~200-bar viewport as the (b-after) rows:
    //   - STATIC (hit): repeated calls with the SAME (lo, hi) — the overwhelming majority of
    //     frames (pan/zoom unchanged, no bar close) — now O(1) instead of row (c)'s O(visible)
    //     every frame.
    //   - MOVING (miss): `lo`/`hi` shift by one bar each call (a continuous pan/follow-live
    //     tick), so `(lo, hi, cache_key)` differs every time — confirms the miss path costs
    //     no more than the same O(visible) fold row (c) already pays.
    let mut cs_vol = ChartState::default();
    cs_vol.bars = synth_model_bars(N);
    cs_vol.closed_len = N;
    cs_vol.refresh_caches(); // seeds cache_key
    cs_vol.visible_vol_max(vlo, vhi); // warm the cache once (this call is the genuine miss)
    let t_vmax_after_hit = time_it(REPEATS, WARMUP, || cs_vol.visible_vol_max(vlo, vhi));
    assert_eq!(
        cs_vol.vol_recompute_count.get(),
        1,
        "AFTER-hit row must be pure cache hits after the warm-up call"
    );

    let win = vhi - vlo;
    let mut cs_vol_moving = ChartState::default();
    cs_vol_moving.bars = synth_model_bars(N);
    cs_vol_moving.closed_len = N;
    cs_vol_moving.refresh_caches();
    let move_steps = WARMUP + REPEATS;
    let mut move_idx = 0usize;
    let t_vmax_after_miss = time_it(REPEATS, WARMUP, || {
        let lo = move_idx % (N - win);
        move_idx += 1;
        cs_vol_moving.visible_vol_max(lo, lo + win)
    });
    assert_eq!(
        cs_vol_moving.vol_recompute_count.get(),
        move_steps,
        "AFTER-miss row must recompute every call (window moves every time)"
    );

    // (d) each transform-style's full recompute, replicating exactly how
    // `crates/vike-chart/src/model.rs`'s `transform_full` calls them for that render style
    // (including the Kagi/PointFigure post-processing into owned Bar vecs, and the `reindex`
    // re-stamp every non-HeikinAshi style applies).
    let t_heikin_ashi = time_it(REPEATS, WARMUP, || heikin_ashi_before(&model_bars).len());
    let t_renko =
        time_it(REPEATS, WARMUP, || transforms::reindex(transforms::renko(&model_bars)).len());
    let t_range =
        time_it(REPEATS, WARMUP, || transforms::reindex(transforms::range_bars(&model_bars)).len());
    let t_line_break = time_it(REPEATS, WARMUP, || {
        transforms::reindex(transforms::line_break(&model_bars, 3)).len()
    });
    let first_ot = model_bars.first().map(|b| b.ot).unwrap_or(0);
    let t_kagi = time_it(REPEATS, WARMUP, || {
        let k = transforms::kagi(&model_bars);
        transforms::reindex(
            k.prices
                .iter()
                .map(|&p| ModelBar { t: 0.0, ot: first_ot, o: p, h: p, l: p, c: p, v: 0.0 })
                .collect(),
        )
        .len()
    });
    let t_point_figure = time_it(REPEATS, WARMUP, || {
        let (cols, _box) = transforms::point_and_figure(&model_bars, 3);
        transforms::reindex(
            cols.iter()
                .map(|c| ModelBar {
                    t: 0.0,
                    ot: first_ot,
                    o: c.bottom,
                    h: c.top,
                    l: c.bottom,
                    c: c.top,
                    v: 0.0,
                })
                .collect(),
        )
        .len()
    });

    // (e) AFTER (chart-perf T6): the two-tier `ChartState::transformed` cache replaces the per-frame
    // full recompute rows (d) above. Two rows per style over a warm 200k-closed + forming state:
    //   - COLD (rebuild): a bar close / style / param change invalidates the closed-prefix key, so
    //     the full transform runs — the same asymptotic cost as (d). Forced here by toggling the
    //     transform params each iteration (a genuine key change) so ONLY the transform is timed,
    //     with no `refresh_caches` folded in.
    //   - WARM (forming-tick): only the forming bar moved — the dominant ~60Hz path. HeikinAshi and
    //     LineBreak redo just the forming tail (per-bar recurrence / ≤1 appended block); the
    //     fallback styles (Renko/Range/Kagi/PnF — their `auto_box` reads the forming bar) still
    //     full-recompute here (a static frame with NO forming change is instead an O(1) cache hit,
    //     which this row does NOT measure — it mutates the forming bar every iteration).
    let t6_params = TransformParams { line_break_n: 3, pnf_reversal: 3 };
    let t6_styles = [
        (ChartStyle::HeikinAshi, "HeikinAshi", "two-tier"),
        (ChartStyle::Renko, "Renko", "fallback"),
        (ChartStyle::Range, "Range", "fallback"),
        (ChartStyle::LineBreak, "LineBreak", "two-tier"),
        (ChartStyle::Kagi, "Kagi", "fallback"),
        (ChartStyle::PointFigure, "PointFigure", "fallback"),
    ];
    let build_state = || {
        let mut cs = ChartState::default();
        cs.bars = synth_model_bars(N);
        let (ot, o, h, l, c, v) = wave(N);
        cs.bars.push(ModelBar { t: N as f64, ot, o, h, l, c, v }); // forming bar at index N
        cs.closed_len = N;
        cs.refresh_caches();
        cs
    };
    let mut t6_rows: Vec<T6Row> = Vec::new();
    for (style, name, tier) in t6_styles {
        // COLD: params toggle each call → key change → full closed-prefix recompute.
        let mut cs_cold = build_state();
        cs_cold.transformed(style, t6_params); // warm allocation capacity
        let mut toggle = false;
        let t_cold = time_it(REPEATS, WARMUP, || {
            toggle = !toggle;
            let p = TransformParams {
                line_break_n: if toggle { 3 } else { 4 },
                pnf_reversal: if toggle { 3 } else { 2 },
            };
            cs_cold.transformed(style, p).len()
        });

        // WARM: only the forming bar (index N) moves; the closed-prefix key is unchanged.
        let mut cs_warm = build_state();
        cs_warm.transformed(style, t6_params); // warm the closed-prefix cache once
        let base_count = cs_warm.transform_closed_recompute_count.get();
        let t_warm = time_it(REPEATS, WARMUP, || {
            cs_warm.bars[N].c += 0.001; // forming tick only
            cs_warm.transformed(style, t6_params).len()
        });
        if matches!(style, ChartStyle::HeikinAshi | ChartStyle::LineBreak) {
            assert_eq!(
                cs_warm.transform_closed_recompute_count.get(),
                base_count,
                "{name}: two-tier warm forming ticks must reuse the cached closed prefix (no reprocess)"
            );
        }

        // STATIC: nothing changed → O(1) `Rc`-clone cache hit (the dominant redraw path — a GUI
        // repaint with no new tick — for EVERY style, two-tier and fallback alike).
        let mut cs_static = build_state();
        cs_static.transformed(style, t6_params); // warm
        let base_static = cs_static.transform_closed_recompute_count.get();
        let t_static = time_it(REPEATS, WARMUP, || cs_static.transformed(style, t6_params).len());
        assert_eq!(
            cs_static.transform_closed_recompute_count.get(),
            base_static,
            "{name}: static frames must be pure cache hits (no recompute)"
        );

        t6_rows.push((name, tier, t_cold, t_warm, t_static));
    }

    // ============================ REPORT ============================
    println!("{:<38}{:>17}{:>17}{:>15}", "op", "median ns/op", "median ms/op", "ns/elem");
    row("BEFORE rebuild(clear+push_all)", &t_rebuild, N);
    row("BEFORE refresh_caches (bar close)", &t_refresh, N);
    row("AFTER refresh_caches (one-close, T3)", &t_refresh_incremental, N);
    row("AFTER sync (forming-tick, T2)", &t_sync_forming_tick, N);
    row("AFTER sync (one-close, T2)", &t_sync_one_close, N);
    row("AFTER sync (reload, T2)", &t_sync_reload, N);
    row("BEFORE overlay map (identity)", &t_map_identity, N);
    row("BEFORE overlay map (log10)", &t_map_log10, N);
    println!(
        "  overlay visible window (T4): {} of {} bars mapped (lo={vlo}, hi={vhi})",
        vhi - vlo,
        N
    );
    row("AFTER overlay map window (identity, T4)", &t_map_identity_after, N);
    row("AFTER overlay map window (log10, T4)", &t_map_log10_after, N);
    row("BEFORE volume vmax fold", &t_vmax, N);
    println!("  volume vmax window (T5): {win} of {N} bars folded on a miss (lo={vlo}, hi={vhi})");
    row("AFTER volume vmax (cache hit, T5)", &t_vmax_after_hit, N);
    row("AFTER volume vmax (cache miss, T5)", &t_vmax_after_miss, N);
    row("BEFORE transform HeikinAshi*", &t_heikin_ashi, N);
    row("BEFORE transform Renko", &t_renko, N);
    row("BEFORE transform Range", &t_range, N);
    row("BEFORE transform LineBreak(n=3)", &t_line_break, N);
    row("BEFORE transform Kagi", &t_kagi, N);
    row("BEFORE transform PointFigure(rev=3)", &t_point_figure, N);
    println!(
        "\n* HeikinAshi: `chart::heikin_ashi` is module-private (not `pub`), unreachable from this"
    );
    println!("  external bench binary — timed via `heikin_ashi_before`, a byte-for-byte copy in this file.");

    println!("\n--- AFTER transform cache (chart-perf T6): cold rebuild / warm forming-tick / static hit ---");
    for (name, tier, cold, warm, static_) in &t6_rows {
        row(&format!("AFTER transform {name} COLD ({tier})"), cold, N);
        row(&format!("AFTER transform {name} WARM tick ({tier})"), warm, N);
        row(&format!("AFTER transform {name} STATIC hit ({tier})"), static_, N);
    }
    println!(
        "  COLD  = closed-prefix rebuild (bar close / style / param change) — same asymptotic cost as (d).\n  WARM  = forming-tick recompute: HeikinAshi/LineBreak redo only the forming tail (in-place, Rc::make_mut);\n          the four fallback styles full-recompute (auto_box reads the forming bar).\n  STATIC = no change → O(1) Rc-clone cache hit — the dominant redraw path, every style."
    );

    println!("\n--- PER-FRAME INDICATOR OVERLAY (Active::update with a forming bar) ---");
    println!(
        "    The forming-bar preview deep-clones the indicator, INCLUDING its retained `hist`,"
    );
    println!(
        "    every frame. Cost is linear in retained bars, so `WindowReach` sets it directly."
    );
    for (i, (name, params, times)) in bench_overlay_frames().into_iter().enumerate() {
        let p = if params.is_empty() { String::new() } else { format!("({params:?})") };
        let kind = if i % 2 == 0 { "HIT (bar unchanged)" } else { "MISS (tick moved)" };
        row(&format!("overlay {name}{p} {kind}"), &times, 1);
    }
    println!(
        "\n  ⚠ `sma`/`ema` are hand-written incremental impls that never trim — their rows are"
    );
    println!(
        "    CONTROLS and must not move between retention settings. `kst`/`hvol` stay Smoothed"
    );
    println!("    (smooth_defined over a series that can carry an interior NaN), so they are the");
    println!("    second control: a change there would mean the grant leaked past its roster.");
}
