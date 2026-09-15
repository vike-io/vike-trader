//! `cheap_np_depth` — measure the TRUE resting ask depth available to [`vike_backtest::CheapNp`]
//! at its own entry moments, from the recorded L2 book, and turn it into a size → PnL curve.
//!
//! Everything published about `cheap_np` so far is PER SHARE (`cheap_np_run`'s SIGNAL/EXECUTED
//! lanes both book one unit). Whether the strategy is worth $400/month or $40,000/month is a
//! question about the *other* axis entirely: how much size could actually have been bought at (or
//! near) the entry price. The trade tape answers a strictly weaker question — it shows executed
//! volume, so depth nobody took is invisible in it. This bin answers the real one by REBUILDING
//! THE BOOK from the recorded `kind=book` series (the pmxt L2 archive, ingested by
//! `pmxt_backfill`) and walking the ask side at each entry.
//!
//! ```sh
//! cheap_np_depth --store DIR --entries FILE.csv \
//!     [--venue polymarket] [--sizes 10,50,100,500,1000] [--theta 0.055] \
//!     [--out per_entry.csv] [--json]
//! ```
//!
//! # The JSON says which percentile produced it, and an OLD file says nothing at all
//!
//! The `--json` summary carries one run-level `stats_provenance` object naming the percentile
//! convention behind every `p10`..`p90` below (and behind the `in-edge depth` line). ⚠ **A file
//! carrying no such object is OLDER than the naming, and its percentiles are NEAREST-RANK.** That
//! is the whole reader-side rule, and it has to work by absence: files already on disk cannot be
//! retro-stamped. The two conventions move one sample's `p50` down and its `p90` up, so no factor
//! converts an old file into a new one — re-run rather than rescale. The text lane names the same
//! convention on its header line, for terminal output that gets pasted. The block's shape, and why
//! it names a method instead of carrying a schema version, are on
//! `crates/vike-backtest/src/binutil.rs`'s `stats_provenance`.
//!
//! # The entries file
//!
//! `sts,ts_ms,outcome_index,ask,edge,won,token_id` — the `cheap_np_run --signals-out` CSV joined
//! to the Polymarket CLOB `token_id` of the entered outcome (the `kind=book` series key; the
//! strategy's own symbols are `<slug>#<outcome_index>`, which the L2 archive does not use).
//!
//! # Two clocks, and why the anchor is chosen per entry
//!
//! The entry `ts_ms` comes from the ON-CHAIN trade tape, so it is a **Polygon block timestamp** —
//! whole seconds, one stamp shared by every print in the block. The book stream is the venue's own
//! CLOB message clock (millisecond). A block stamp can therefore trail the match that produced it
//! by up to a block, and reading the book at the block stamp reads a book the entry's own block has
//! already eaten.
//!
//! So the anchor is resolved in two ways and BOTH are reported:
//!
//! * **`pre_print`** (headline) — the recorded L2 trade tape for the same token is searched for the
//!   print itself (price within [`PRICE_MATCH_TOL`], nearest `ts` within
//!   [`TRADE_MATCH_WINDOW_MS`] of the block stamp); the book is folded over every update STRICTLY
//!   BEFORE that print. This is literally "what was resting when the entry print hit". Entries with
//!   no matched print fall back to one [`POLYGON_BLOCK_MS`] before the stamp — still a pre-entry
//!   reading — and are counted (`anchor_fallback`).
//! * **`at_block`** (conservative) — every update at or before the block stamp, i.e. after the
//!   entry's whole block has been applied. It is a lower bound on what was available.
//!
//! # Two sweeps, and two PnL lanes
//!
//! Each target size is swept twice — as a pure MARKET order (no price cap; on a binary token the
//! ask side runs all the way to 1.0, so this always "fills" and the number that matters is its
//! VWAP) and as a marketable LIMIT at `p_edge` (what the strategy would really send, so `filled`
//! plateaus at the in-edge depth and the curve reads directly as capacity).
//!
//! Both are scored two ways. `realized_*` uses the on-chain 0/1 payout — the truth, but ±1 share of
//! variance per entry, so at a few hundred entries it locates the capacity ceiling very poorly.
//! `model_edge_*` re-scores the SAME dual-β worst case the gate fired on at the VWAP the sweep
//! actually paid; it is the low-variance estimator, and the stated capacity number reads off it.
//!
//! # The edge ceiling is exact, not re-derived
//!
//! `CheapNpSignal::edge` is the dual-β worst case `prob_wc − ask − fee(ask)` at the entry ask, and
//! `prob_wc` does not depend on the price paid. So the worst-case probability is recovered exactly
//! as `prob_wc = edge + ask + fee(ask)`, and the edge at ANY other fill price `p` is
//! `prob_wc − p − fee(p)` with NO spot/σ re-evaluation. [`price_at_edge`] inverts that for the
//! highest price still clearing θ.
//!
//! # Book reconstruction
//!
//! The archive fold, the venue-authoritative GHOST REPAIR and the snapshot-checkpoint integrity
//! gate all live in [`vike_backtest::cheap_np_book`] — hoisted there out of this bin when
//! `cheap_np_askgate` needed the same book. Read that module for why a pure delta replay is wrong
//! here and how gaps are handled; every number below is read off exactly that fold.
//!
//! What this bin does NOT do is deduct the trade tape on top (`--consume-trades`): the integrity
//! gate says that double-counts, so the venue evidently does report match-consumed size in its
//! deltas and the ghosts are a residual ordering/omission effect, not a systematic missing-fill
//! lane.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;

// The workspace's ONE percentile, named at its real home rather than through vike-backtest's
// re-export, because WHICH crate owns the number is the whole point of the unification. `pct`
// below is a sort-then-delegate adapter over it and computes nothing of its own.
use vike_analytics::metrics::percentile;
use vike_backtest::binutil::{self, has_flag, store_root};
use vike_backtest::cheap_np_ask::price_at_edge;
use vike_backtest::cheap_np_book::{
    Checkpoints, POLYGON_BLOCK_MS, fold_until, match_clob_print, verify_checkpoints,
};
use vike_backtest::fair_value::{CHEAP_HI, THETA, fee};
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::{BookUpdate, py_sum};

/// One entry row read from the `--entries` CSV.
#[derive(Debug, Clone)]
struct Entry {
    sts: i64,
    ts_ms: i64,
    oidx: u8,
    ask: f64,
    edge: f64,
    won: f64,
    token_id: String,
}

fn read_entries(path: &PathBuf) -> Result<Vec<Entry>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("sts,") {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 7 {
            continue;
        }
        let parse = |i: usize| -> Result<f64, String> {
            f[i].parse::<f64>().map_err(|e| format!("field {i} of {line:?}: {e}"))
        };
        out.push(Entry {
            sts: f[0].parse().map_err(|e| format!("sts of {line:?}: {e}"))?,
            ts_ms: f[1].parse().map_err(|e| format!("ts_ms of {line:?}: {e}"))?,
            oidx: f[2].parse().map_err(|e| format!("oidx of {line:?}: {e}"))?,
            ask: parse(3)?,
            edge: parse(4)?,
            won: parse(5)?,
            token_id: f[6].trim().to_string(),
        });
    }
    Ok(out)
}

/// Per-(anchor, size) accumulator: what a taker sweeping `size` shares would have got.
///
/// Two sweeps are accumulated for every target size, because they answer different questions:
///
/// * **market** — take `size` shares off the displayed ask side however deep that goes. This is
///   the "how bad does it get" reading; on a binary token the far side of the book is stuffed with
///   near-1.0 offers, so a market sweep ALWAYS "fills" and the interesting number is its VWAP.
/// * **capped** — a marketable LIMIT at `p_edge` (the highest price still clearing θ). This is what
///   the strategy would actually send, so `filled` plateaus at the available in-edge depth and the
///   curve reads directly as capacity.
#[derive(Debug, Default, Clone)]
struct SizeAcc {
    /// Entries contributing (every measured entry does — a zero fill still counts, as zero PnL).
    n: usize,
    filled: f64,
    /// `Σ filled_i × vwap_i` — the notional actually paid.
    notional: f64,
    /// `Σ filled_i × (won_i − vwap_i − fee(vwap_i))` — REALIZED, on the on-chain 0/1 payout.
    pnl: f64,
    /// `Σ filled_i × (prob_wc_i − vwap_i − fee(vwap_i))` — the MODEL edge at the achieved VWAP.
    ///
    /// The realized lane is the truth but its per-trade variance is ±1 share, so at a few hundred
    /// entries it says almost nothing about where the edge dies. The model lane is the same
    /// quantity the gate itself scored, evaluated at the price the sweep actually paid, so it
    /// converges orders of magnitude faster — it is the estimator the capacity number reads off.
    model_edge: f64,
    /// Entries whose displayed ask side could not cover the full target.
    partial: usize,
    /// `Σ filled_i / size` — the mean fill ratio's numerator.
    fill_ratio_sum: f64,
    /// `Σ worst price touched` (over entries that filled anything).
    worst_px_sum: f64,
    worst_px_n: usize,
}

impl SizeAcc {
    fn add(&mut self, filled: f64, vwap: f64, worst_px: Option<f64>, e: &Entry, prob_wc: f64) {
        self.n += 1;
        self.filled += filled;
        self.notional += filled * vwap;
        self.pnl += filled * (e.won - vwap - fee(vwap));
        self.model_edge += filled * (prob_wc - vwap - fee(vwap));
        if let Some(w) = worst_px {
            self.worst_px_sum += w;
            self.worst_px_n += 1;
        }
    }

    fn json(&self, size: f64) -> serde_json::Value {
        let vwap = if self.filled > 0.0 { self.notional / self.filled } else { 0.0 };
        serde_json::json!({
            "target_size": size,
            "entries": self.n,
            "mean_filled": if self.n > 0 { self.filled / self.n as f64 } else { 0.0 },
            "mean_fill_ratio": if self.n > 0 { self.fill_ratio_sum / self.n as f64 } else { 0.0 },
            "partial_fills": self.partial,
            "vwap": vwap,
            "mean_worst_px": if self.worst_px_n > 0 {
                self.worst_px_sum / self.worst_px_n as f64
            } else { 0.0 },
            "realized_pnl_total": self.pnl,
            "realized_pnl_per_entry": if self.n > 0 { self.pnl / self.n as f64 } else { 0.0 },
            "realized_pnl_per_share": if self.filled > 0.0 { self.pnl / self.filled } else { 0.0 },
            "model_edge_total": self.model_edge,
            "model_edge_per_entry": if self.n > 0 { self.model_edge / self.n as f64 } else { 0.0 },
            "model_edge_per_share": if self.filled > 0.0 {
                self.model_edge / self.filled
            } else { 0.0 },
        })
    }
}

/// Everything measured for ONE anchor policy (`pre_print` or `at_block`) across all entries.
#[derive(Debug, Default)]
struct AnchorAgg {
    measured: usize,
    no_snapshot: usize,
    no_book: usize,
    empty_ask: usize,
    status_markers: usize,
    /// Book events folded, and how stale the newest one was at the anchor.
    staleness_ms: Vec<f64>,
    /// Cumulative ask size at the entry's own print price OR BETTER.
    at_ask_size: Vec<f64>,
    /// Cumulative ask size at prices still clearing θ.
    within_edge_size: Vec<f64>,
    /// Cumulative ask size at prices still clearing θ AND inside the cheap band.
    within_band_size: Vec<f64>,
    /// Best ask minus the print price (negative = the book was cheaper than the print).
    best_ask_vs_print: Vec<f64>,
    /// Ghost ask levels removed by the L1 repair, summed over entries.
    ghosts_pruned: usize,
    /// Ask size deducted from the recorded trade tape, summed over entries.
    tape_consumed: f64,
    /// Entries where a displayed ask level carried a NEGATIVE size (an archive data fault — the
    /// level is clamped to 0 for every measurement, and the entry is counted here rather than
    /// silently absorbed).
    negative_levels: usize,
    by_size: Vec<SizeAcc>,
    capped_by_size: Vec<SizeAcc>,
}

/// The `q`-th percentile of `v`, which is sorted in place first.
///
/// ⚠ **A sort-then-DELEGATE adapter, deliberately not a second algorithm.** Every number comes
/// from [`percentile`] — `vike_analytics::metrics`' numpy-`linear` interpolation, the workspace's
/// one percentile. This body used to compute its own NEAREST-RANK answer instead
/// (`v[round((n - 1) * q)]`: always an observed sample, never an interpolant). The two agree only
/// where `(n - 1) * q` lands on an integer, or where the two samples being interpolated between
/// happen to be equal — and `(n - 1) * q` is NEVER an integer at `q = 0.5` on an even-length
/// vector. On `[0, 1, 2, 3]` the old body read `2.0` where the shared one reads `1.5`; on a
/// ten-sample in-edge depth vector its `p50` read the whole next observed sample up.
///
/// So this change is **not byte-identical**, and it moves numbers an operator sizes positions
/// from: the `p10`..`p90` in this bin's JSON and in its `in-edge depth` line. Every percentile
/// this bin has printed to date is a nearest-rank reading and cannot be compared against a run
/// from this commit forward; the commit message carries the before/after table.
///
/// The comparator is [`f64::total_cmp`] rather than the old
/// `partial_cmp().unwrap_or(Ordering::Equal)`: only the former is a total order, so only the
/// former actually leaves the slice sorted when a NaN is present — and [`percentile`] documents a
/// sorted input and does no comparison of its own to fall back on. The old empty-slice guard is
/// gone because [`percentile`]'s own is the superset (it answers `0.0` for an empty slice too).
fn pct(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    percentile(v, q)
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() { 0.0 } else { py_sum(v.iter().copied()) / v.len() as f64 }
}

/// Walk `levels` (ascending price, best first) taking at most `qty`, refusing any level priced
/// above `limit`. Returns `(filled, vwap, worst_px)`.
///
/// A plain fold over the level vector rather than [`L2Book::simulate_fill`], for two reasons the
/// measurement needs and the shared helper cannot give: a PRICE cap (the marketable-limit sweep),
/// and NEGATIVE displayed sizes — a real fault in this archive — clamped to zero at the level
/// rather than allowed to subtract from a cumulative depth reading.
fn sweep(levels: &[(f64, f64)], qty: f64, limit: f64) -> (f64, f64, Option<f64>) {
    let (mut filled, mut notional, mut worst) = (0.0, 0.0, None);
    for &(px, lvl) in levels {
        if px > limit || filled >= qty {
            break;
        }
        let take = (qty - filled).min(lvl.max(0.0));
        if take <= 0.0 {
            continue;
        }
        filled += take;
        notional += px * take;
        worst = Some(px);
    }
    (filled, if filled > 0.0 { notional / filled } else { 0.0 }, worst)
}

#[allow(clippy::too_many_arguments)]
fn measure(
    agg: &mut AnchorAgg,
    updates: &[BookUpdate],
    l1: &[vike_model::QuoteTick],
    tape: &[vike_model::TradeTick],
    cutoff: i64,
    strict: bool,
    e: &Entry,
    sizes: &[f64],
    theta: f64,
    per_entry: &mut Vec<String>,
    label: &str,
) {
    let f = fold_until(updates, l1, tape, cutoff, strict);
    agg.status_markers += f.status_markers;
    agg.ghosts_pruned += f.pruned;
    agg.tape_consumed += f.consumed;
    if f.applied == 0 {
        agg.no_book += 1;
        return;
    }
    if !f.anchored {
        agg.no_snapshot += 1;
        return;
    }
    // `top_n` over a bound no Polymarket book approaches — the whole displayed ask side, ascending.
    let asks: Vec<(f64, f64)> = f.book.top_n(1 << 16).1;
    if asks.is_empty() {
        agg.empty_ask += 1;
        return;
    }
    agg.measured += 1;
    if asks.iter().any(|&(_, q)| q < 0.0) {
        agg.negative_levels += 1;
    }
    let best_ask = asks[0].0;
    agg.staleness_ms.push((cutoff - f.last_ts) as f64);

    // Worst-case probability recovered EXACTLY from the fired signal (see the module doc).
    let prob_wc = e.edge + e.ask + fee(e.ask);
    let p_edge = price_at_edge(prob_wc, theta).unwrap_or(0.0);
    // The cheap band is an ENTRY filter on the print, not a limit price — reported separately
    // rather than folded into the headline, so both readings are visible.
    let p_band = p_edge.min(CHEAP_HI);

    let cum = |limit: f64| -> f64 {
        py_sum(asks.iter().filter(|&&(px, _)| px <= limit).map(|&(_, q)| q.max(0.0)))
    };
    let at_ask = cum(e.ask);
    let within_edge = cum(p_edge);
    let within_band = cum(p_band);
    agg.at_ask_size.push(at_ask);
    agg.within_edge_size.push(within_edge);
    agg.within_band_size.push(within_band);
    agg.best_ask_vs_print.push(best_ask - e.ask);

    let mut row = format!(
        "{label},{sts},{ts},{oidx},{ask:.6},{edge:.6},{won},{tok},{best_ask:.6},{at_ask:.4},\
         {p_edge:.6},{within_edge:.4},{within_band:.4}",
        sts = e.sts,
        ts = e.ts_ms,
        oidx = e.oidx,
        ask = e.ask,
        edge = e.edge,
        won = e.won,
        tok = e.token_id,
    );
    for (i, &s) in sizes.iter().enumerate() {
        // market sweep — no price cap, so it always "fills" against the far side of a binary book
        let (filled, vwap, worst) = sweep(&asks, s, f64::INFINITY);
        let acc = &mut agg.by_size[i];
        acc.add(filled, vwap, worst, e, prob_wc);
        acc.fill_ratio_sum += if s > 0.0 { filled / s } else { 0.0 };
        if filled + 1e-9 < s {
            acc.partial += 1;
        }
        // marketable LIMIT at p_edge — what the strategy would actually send
        let (cfilled, cvwap, cworst) = sweep(&asks, s, p_edge);
        let cacc = &mut agg.capped_by_size[i];
        cacc.add(cfilled, cvwap, cworst, e, prob_wc);
        cacc.fill_ratio_sum += if s > 0.0 { cfilled / s } else { 0.0 };
        if cfilled + 1e-9 < s {
            cacc.partial += 1;
        }
        row.push_str(&format!(",{filled:.4},{vwap:.6},{cfilled:.4},{cvwap:.6}"));
    }
    per_entry.push(row);
}

fn agg_json(a: &AnchorAgg, sizes: &[f64]) -> serde_json::Value {
    let mut at_ask = a.at_ask_size.clone();
    let mut within = a.within_edge_size.clone();
    let mut band = a.within_band_size.clone();
    let mut stale = a.staleness_ms.clone();
    let curve = |accs: &[SizeAcc]| -> Vec<serde_json::Value> {
        sizes.iter().zip(accs.iter()).map(|(&s, acc)| acc.json(s)).collect()
    };
    serde_json::json!({
        "measured": a.measured,
        "no_snapshot": a.no_snapshot,
        "no_book": a.no_book,
        "empty_ask": a.empty_ask,
        "status_markers": a.status_markers,
        "entries_with_negative_levels": a.negative_levels,
        "ghost_ask_levels_pruned": a.ghosts_pruned,
        "ask_size_consumed_from_tape": a.tape_consumed,
        "book_staleness_ms": { "mean": mean(&a.staleness_ms), "p50": pct(&mut stale, 0.5),
                               "p90": pct(&mut stale, 0.9) },
        "size_at_or_below_print_price": {
            "mean": mean(&a.at_ask_size),
            "p10": pct(&mut at_ask, 0.1),
            "p50": pct(&mut at_ask, 0.5),
            "p90": pct(&mut at_ask, 0.9),
        },
        "size_within_edge": {
            "mean": mean(&a.within_edge_size),
            "p10": pct(&mut within, 0.1),
            "p25": pct(&mut within, 0.25),
            "p50": pct(&mut within, 0.5),
            "p75": pct(&mut within, 0.75),
            "p90": pct(&mut within, 0.9),
        },
        "size_within_edge_and_band": {
            "mean": mean(&a.within_band_size),
            "p50": pct(&mut band, 0.5),
        },
        "best_ask_minus_print_mean": mean(&a.best_ask_vs_print),
        "curve_market_sweep": curve(&a.by_size),
        "curve_capped_at_edge": curve(&a.capped_by_size),
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let arg = |flag: &str| binutil::arg(&args, flag);
    let Some(entries_path) = arg("--entries").map(PathBuf::from) else {
        eprintln!(
            "usage: cheap_np_depth --store DIR --entries FILE.csv [--venue polymarket] \
             [--sizes 10,50,100,500,1000] [--theta 0.055] [--out per_entry.csv] [--json]"
        );
        // The operator-facing statement of the percentile convention, RENDERED from the shared
        // constant rather than restated here — this surface and the JSON therefore cannot come to
        // disagree, which is the same failure the stamp itself exists to end.
        eprintln!("\n--json: {}", binutil::PERCENTILE_NOTE);
        return ExitCode::FAILURE;
    };
    // `--store` > `$VIKE_HIST_STORE` > `<repo>/market_data/hist` — the repo-root default replaces the
    // old CWD-relative `market_data/hist` (run from elsewhere, `DataFusionHist::open` silently CREATED
    // an empty store and the run reported zero entries measured).
    let root: PathBuf = store_root(arg("--store").map(PathBuf::from), &std::env::vars().collect());
    let venue = arg("--venue").unwrap_or_else(|| "polymarket".to_string());
    let theta = arg("--theta").and_then(|v| v.parse().ok()).unwrap_or(THETA);
    let json = has_flag(&args, "--json");
    let order_local_ts = arg("--order").as_deref() == Some("local_ts");
    // The trade-tape ask deduction (`consume_ask`). OFF by default — the checkpoint gate says it
    // DOUBLE-COUNTS (see that function's doc); `--consume-trades` turns it on to re-check.
    let consume_trades = has_flag(&args, "--consume-trades");
    let out_path = arg("--out").map(PathBuf::from);
    let sizes: Vec<f64> = arg("--sizes")
        .map(|s| s.split(',').filter_map(|p| p.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1.0, 10.0, 50.0, 100.0, 500.0, 1000.0]);

    let entries = match read_entries(&entries_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("--entries: {e}");
            return ExitCode::FAILURE;
        }
    };
    if entries.is_empty() {
        eprintln!("--entries {} has no rows", entries_path.display());
        return ExitCode::FAILURE;
    }
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open hist store at {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };

    let fresh = || AnchorAgg {
        by_size: vec![SizeAcc::default(); sizes.len()],
        capped_by_size: vec![SizeAcc::default(); sizes.len()],
        ..Default::default()
    };
    let (mut pre, mut blk) = (fresh(), fresh());
    let mut per_entry: Vec<String> = Vec::new();
    let (mut no_series, mut anchor_fallback, mut anchor_matched) = (0usize, 0usize, 0usize);
    let mut offsets: Vec<f64> = Vec::new();

    // One entry per token by construction (`cheap_np` enters a window once), but the scan is
    // memoized anyway so a Flip-mode entries file re-reading the same token stays one scan.
    #[allow(clippy::type_complexity)]
    let mut cache: HashMap<
        String,
        (Vec<BookUpdate>, Vec<vike_model::TradeTick>, Vec<vike_model::QuoteTick>),
    > = HashMap::new();
    let mut verified_tokens: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ck = Checkpoints::default();

    for (i, e) in entries.iter().enumerate() {
        let (books, trades, l1) = match cache.entry(e.token_id.clone()) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let mut b = match store.scan_book_updates(&venue, &e.token_id, TsRange::all()) {
                    Ok(b) => b,
                    Err(err) => {
                        eprintln!("scan_book_updates {}: {err}", e.token_id);
                        Vec::new()
                    }
                };
                // `scan_book_updates` returns `(ts, seq)` order, and `ts` is pmxt's SOURCE
                // timestamp (clamped monotonic per asset per hour file). `local_ts` is the
                // archive's own `timestamp_received` — the true ingest order, and the column pmxt
                // itself sorts by. `--order local_ts` folds in that order instead, which is what
                // the integrity gate is used to ADJUDICATE rather than assume.
                if order_local_ts {
                    b.sort_by_key(|u| u.local_ts);
                }
                let t = store.scan_trades(&venue, &e.token_id, TsRange::all()).unwrap_or_default();
                // The venue's own top of book (`pmxt::l1_from_row`) — the ghost repair's source.
                let q = store.scan_quotes(&venue, &e.token_id, TsRange::all()).unwrap_or_default();
                v.insert((b, t, q))
            }
        };
        if books.is_empty() {
            no_series += 1;
            continue;
        }
        // The tape the fold consumes ask liquidity with — empty when the deduction is off, so the
        // two modes go down exactly the same code path.
        let consume_tape: &[vike_model::TradeTick] = if consume_trades { trades } else { &[] };
        // Integrity: does the delta stream between the archive's own full-book frames reproduce
        // them? Run once per token (the cache guarantees one visit per distinct token).
        if verified_tokens.insert(e.token_id.clone()) {
            ck.merge(verify_checkpoints(books, l1, consume_tape));
        }

        // Anchor: the recorded L2 print matching this on-chain entry (see the module doc's
        // "Two clocks" and `cheap_np_book::match_clob_print`).
        let pre_cutoff = match match_clob_print(trades, e.ts_ms, e.ask) {
            Some(ts) => {
                anchor_matched += 1;
                offsets.push((ts - e.ts_ms) as f64);
                ts
            }
            None => {
                // No matched print: fall back one Polygon block BEFORE the stamp, so the reading
                // is still pre-entry rather than post-entry. Counted, never silently blended.
                anchor_fallback += 1;
                e.ts_ms - POLYGON_BLOCK_MS
            }
        };

        measure(
            &mut pre,
            books,
            l1,
            consume_tape,
            pre_cutoff,
            true,
            e,
            &sizes,
            theta,
            &mut per_entry,
            "pre_print",
        );
        measure(
            &mut blk,
            books,
            l1,
            consume_tape,
            e.ts_ms,
            false,
            e,
            &sizes,
            theta,
            &mut per_entry,
            "at_block",
        );

        if (i + 1) % 100 == 0 {
            eprintln!("  ... {}/{} entries", i + 1, entries.len());
        }
    }

    if let Some(path) = &out_path {
        let mut csv = String::from(
            "anchor,sts,ts_ms,outcome_index,ask,edge,won,token_id,best_ask,size_at_or_below_print,\
             p_edge_max,size_within_edge,size_within_band",
        );
        for s in &sizes {
            csv.push_str(&format!(",filled_{s},vwap_{s},capped_filled_{s},capped_vwap_{s}"));
        }
        csv.push('\n');
        for r in &per_entry {
            csv.push_str(r);
            csv.push('\n');
        }
        if let Err(e) = std::fs::write(path, csv) {
            eprintln!("--out {}: {e}", path.display());
        }
    }

    let summary = serde_json::json!({
        // ⚠ RUN-LEVEL and first, never on a row: what convention produced every `p10`..`p90`
        // below. `vike_backtest::binutil`'s `stats_provenance` carries the shape, why absence is
        // the signal for an older file, and why no schema version rides beside it.
        "stats_provenance": binutil::stats_provenance(),
        "store": root.display().to_string(),
        "venue": venue,
        "theta": theta,
        "entries_in": entries.len(),
        "entries_without_book_series": no_series,
        "anchor_matched_l2_print": anchor_matched,
        "anchor_fallback_block_ts": anchor_fallback,
        "anchor_offset_ms_mean": mean(&offsets),
        // The delta stream's own integrity proof — see `verify_checkpoints`.
        "delta_integrity": {
            "snapshot_intervals_checked": ck.checked,
            "intervals_reproduced_exactly": ck.exact,
            "consume_trades": consume_trades,
            "ask_levels_compared": ck.levels,
            "ask_level_mismatches": ck.level_mismatches,
            "best_ask_reproduced": ck.best_ask_ok,
            "top5_ask_levels_reproduced": ck.top5_ok,
        },
        "fold_order": if order_local_ts { "local_ts" } else { "ts,seq" },
        "sizes": sizes,
        "pre_print": agg_json(&pre, &sizes),
        "at_block": agg_json(&blk, &sizes),
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    } else {
        println!("cheap_np resting-depth — {} entries, store {}", entries.len(), root.display());
        // The text lane prints `p10`..`p90` too, so it says which convention it used — RENDERED
        // from the constant, never restated, so this line cannot name a method the numbers did not
        // come from. Terminal output gets pasted into issues, where it has exactly the JSON's
        // problem and none of its structure.
        println!(
            "  percentiles: {} (--json stamps this as `stats_provenance`)",
            vike_analytics::metrics::PERCENTILE_METHOD
        );
        println!(
            "  anchor: {anchor_matched} matched to the L2 tape, {anchor_fallback} fell back \
             (mean offset {:.0} ms)",
            mean(&offsets)
        );
        println!(
            "  delta integrity ({} order): {}/{} intervals exact, {}/{} best-ask, {}/{} top-5 \
             ({} of {} ask levels mismatched)",
            if order_local_ts { "local_ts" } else { "ts,seq" },
            ck.exact,
            ck.checked,
            ck.best_ask_ok,
            ck.checked,
            ck.top5_ok,
            ck.checked,
            ck.level_mismatches,
            ck.levels
        );
        for (label, a) in [("pre_print", &pre), ("at_block", &blk)] {
            println!(
                "\n=== {label} === measured {} | no_snapshot {} | no_book {} | empty_ask {}",
                a.measured, a.no_snapshot, a.no_book, a.empty_ask
            );
            let mut w = a.within_edge_size.clone();
            println!(
                "  in-edge depth: mean {:.1}  p10 {:.1}  p50 {:.1}  p90 {:.1} shares",
                mean(&a.within_edge_size),
                pct(&mut w, 0.1),
                pct(&mut w, 0.5),
                pct(&mut w, 0.9)
            );
            println!(
                "  {:>8}  {:>10} {:>9} {:>12} {:>12}   |  {:>10} {:>9} {:>12} {:>12}",
                "size",
                "mkt_filled",
                "mkt_vwap",
                "mkt_edge/sh",
                "mkt_pnl/entry",
                "cap_filled",
                "cap_vwap",
                "cap_edge/sh",
                "cap_pnl/entry"
            );
            for (i, &s) in sizes.iter().enumerate() {
                let (m, c) = (&a.by_size[i], &a.capped_by_size[i]);
                let per = |acc: &SizeAcc| -> (f64, f64, f64, f64) {
                    (
                        if acc.n > 0 { acc.filled / acc.n as f64 } else { 0.0 },
                        if acc.filled > 0.0 { acc.notional / acc.filled } else { 0.0 },
                        if acc.filled > 0.0 { acc.model_edge / acc.filled } else { 0.0 },
                        if acc.n > 0 { acc.pnl / acc.n as f64 } else { 0.0 },
                    )
                };
                let (mf, mv, me, mp) = per(m);
                let (cf, cv, ce, cp) = per(c);
                println!(
                    "  {s:>8.0}  {mf:>10.1} {mv:>9.4} {me:>12.4} {mp:>12.3}   |  \
                     {cf:>10.1} {cv:>9.4} {ce:>12.4} {cp:>12.3}"
                );
            }
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The archive fold, the ghost repair, the checkpoint gate and `price_at_edge` moved to
    /// `vike_backtest::{cheap_np_book, cheap_np_ask}` (and their tests with them) when
    /// `cheap_np_askgate` needed the same machinery. What is left here is this bin's OWN sweep,
    /// which deliberately differs from the engine's taker law
    /// (`vike_backtest::fill_model::book_taker_price`): it fills PARTIALLY, because a capacity
    /// curve has to report what a size could NOT get, while the engine either fills an order in
    /// full or leaves it resting.
    #[test]
    fn sweep_respects_the_price_cap_and_clamps_negative_displayed_size() {
        let levels = [(0.20, 100.0), (0.25, 100.0), (0.90, 10_000.0)];
        // uncapped: a binary book's far side always "fills", at a ruinous VWAP
        let (f, v, w) = sweep(&levels, 300.0, f64::INFINITY);
        assert_eq!(f, 300.0);
        assert_eq!(w, Some(0.90));
        assert!((v - (0.20 * 100.0 + 0.25 * 100.0 + 0.90 * 100.0) / 300.0).abs() < 1e-12);
        // capped at 0.25: fills only the in-edge depth and stops
        let (f, v, w) = sweep(&levels, 300.0, 0.25);
        assert_eq!((f, w), (200.0, Some(0.25)));
        assert!((v - 0.225).abs() < 1e-12);
        // a negative displayed size contributes nothing rather than subtracting
        let (f, _, _) = sweep(&[(0.20, -5.0), (0.21, 7.0)], 100.0, 1.0);
        assert_eq!(f, 7.0);
    }

    /// `pct` must stay a sort-then-delegate adapter over `vike_analytics::metrics::percentile`
    /// and must never grow arithmetic of its own again.
    ///
    /// It carried a NEAREST-RANK body — `v[round((n - 1) * q)]` — for as long as this bin has
    /// existed, printing it under the `p10`..`p90` keys an operator sizes positions from while
    /// every other percentile in the workspace interpolated. This asserts agreement over the
    /// EXACT quantile set the bin emits (`agg_json`'s `p10`/`p25`/`p50`/`p75`/`p90` and the
    /// `in-edge depth` line's `p10`/`p50`/`p90`), including the two degenerate lengths, and then
    /// pins values the old body could not produce — an interpolant is not an element of the
    /// input, so a revert to indexing reddens here rather than silently moving a number.
    #[test]
    fn pct_is_the_shared_percentile_and_no_longer_nearest_rank() {
        // Every `q` this bin prints, in the order `agg_json` prints them.
        const PRINTED: [f64; 5] = [0.1, 0.25, 0.5, 0.75, 0.9];
        // The comparison is bit-exact on purpose: `pct` may sort, and nothing else.
        for case in [
            vec![],
            vec![7.0],
            vec![0.0, 1.0, 2.0, 3.0],
            vec![0.0, 1.0, 2.0, 3.0, 4.0],
            // an unsorted, duplicate-bearing, realistic in-edge depth sample
            vec![110.0, 12.0, 900.0, 40.0, 55.0, 400.0, 25.0, 240.0, 80.0, 150.0],
        ] {
            let mut sorted = case.clone();
            sorted.sort_by(f64::total_cmp);
            for q in PRINTED {
                let mut v = case.clone();
                assert_eq!(
                    pct(&mut v, q),
                    percentile(&sorted, q),
                    "pct diverged from the shared percentile at q={q} on {case:?}"
                );
            }
        }
        // ...and the values themselves, which nearest-rank cannot reach.
        let mut even = [0.0, 1.0, 2.0, 3.0];
        assert_eq!(pct(&mut even, 0.5), 1.5, "the old nearest-rank body read 2.0 here");
        assert_eq!(pct(&mut even, 0.25), 0.75, "the old body read 1.0");
        assert_eq!(pct(&mut even, 0.75), 2.25, "the old body read 2.0");
        // The degenerate lengths are where old and new agree, so pin them as a no-change claim.
        assert_eq!(pct(&mut [], 0.5), 0.0);
        assert_eq!(pct(&mut [7.0], 0.9), 7.0);
    }

    /// **The `--json` document SAYS which percentile convention produced its numbers**, and the
    /// block it stamps names the convention actually in force.
    ///
    /// Asserted against this bin's own SOURCE, because `main` assembles that document out of a
    /// dozen locals and no unit test can call it — the same device
    /// `crates/vike-ops/tests/live_lock_claim_order_gate.rs` applies to the composition roots, and
    /// it carries that gate's rule with it: if this reddens because the emitting site was
    /// legitimately re-spelled, UPDATE the needle, never delete the test.
    ///
    /// Two properties make the needle worth trusting. It is ASSEMBLED rather than written out, so
    /// the joined string appears nowhere in this file and can only match the emitting site — a
    /// literal would have let the test pass on its own text, the self-reference hazard
    /// `crates/vike-ops/tests/unrun_command_gate.rs` declares for itself. And EXACTLY one match is
    /// required, which fails a typo'd needle at zero (so the test cannot go vacuous) and a marker
    /// duplicated onto a per-entry row at two.
    #[test]
    fn the_json_summary_stamps_the_percentile_convention_it_used() {
        let key = "stats_provenance";
        let needle = format!("\"{key}\": binutil::{key}()");
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/cheap_np_depth.rs"
        ))
        .expect("this bin's own source");
        assert_eq!(
            src.matches(&needle).count(),
            1,
            "the --json summary must stamp `{needle}` exactly once, at RUN level"
        );
        assert_eq!(
            binutil::stats_provenance()["percentile_method"],
            serde_json::json!(vike_analytics::metrics::PERCENTILE_METHOD),
            "the stamp must name the percentile `pct` actually delegates to"
        );
    }
}
