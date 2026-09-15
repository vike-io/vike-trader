//! `cheap_np_askgate` — what `cheap_np` is worth once entries are gated and priced on the
//! **resting ask** a taker can actually lift, and once Polymarket's **250 ms venue taker delay**
//! is applied.
//!
//! ```sh
//! cheap_np_askgate --store DIR --entries FILE.csv \
//!     [--venue polymarket] [--theta 0.055] [--size 1] [--delay-ms 0,250] \
//!     [--live-ms 954,4229] [--out per_entry.csv] [--json]
//! ```
//!
//! `--live-ms` adds one further lane per offset that reproduces the LIVE Dublin bot's OWN execution
//! law instead of the θ-gated one — see [`live_law_price`] for what that law is and why the two
//! disagree so violently on rejection rate. Omit it and the run is byte-identical to before.
//!
//! # The JSON says which percentile produced it, and an OLD file says nothing at all
//!
//! The `--json` summary carries one run-level `stats_provenance` object naming the percentile
//! convention behind `best_ask_minus_print.p50`. ⚠ **A file carrying no such object is OLDER than
//! the naming, and that median is NEAREST-RANK** — the upper of an even-length sample's two
//! middles, published under the same key. The rule has to work by absence: files already on disk
//! cannot be retro-stamped. The text lane names the same convention on its header line, for
//! terminal output that gets pasted. The block's shape, and why it names a method instead of
//! carrying a schema version, are on `crates/vike-backtest/src/binutil.rs`'s `stats_provenance`.
//!
//! # The flaw being measured
//!
//! `cheap_np` fires on a taker BUY **print** and books that print's price as the entry price. A
//! taker cannot obtain that price — the print is what somebody else already took. Worse, the gate
//! itself is `edge = prob_wc − price − fee(price) > θ` evaluated on the print, so paying more makes
//! the edge thinner and a signal that clears θ = 0.055 on the print may not clear it at all on the
//! ask. Those are not smaller trades; they are **not trades**.
//!
//! # Three lanes, and every one of them uses machinery that is not this bin's
//!
//! This bin is a DRIVER. It owns no pricing law of its own — that was the whole point of the
//! layering, and it is what keeps this number reproducible by the live engine:
//!
//! | lane | gate price | matched against |
//! |---|---|---|
//! | `print` | the tape print (today's published behaviour) | — |
//! | `ask` | the resting book at the decision instant | the same book |
//! | `ask+delay` | the resting book at the decision instant | the book at **decision + 250 ms** |
//!
//! * the book comes from [`vike_backtest::cheap_np_book`] (the archive fold + the
//!   venue-authoritative ghost repair + the snapshot-checkpoint integrity gate);
//! * what a taker PAYS is [`vike_backtest::fill_model::book_taker_price`] — literally the function
//!   [`vike_backtest::L2BookFillModel`] calls, so the engine would price these fills identically;
//! * the edge re-score and the θ-clearing limit are [`vike_backtest::cheap_np_ask`], which
//!   [`vike_backtest::CheapNp`] itself calls under [`vike_backtest::EntryPrice::RestingAsk`];
//! * 250 ms is [`vike_backtest::VENUE_HOLD_POLYMARKET_UPDOWN_MS`].
//!
//! # The delay is a venue RULE, not a latency haircut
//!
//! Polymarket holds a taker order on these crypto up/down markets for 250 ms, **runs validation
//! again**, and only then matches or books it (`docs.polymarket.com/concepts/order-lifecycle`).
//! While pending it cannot be cancelled and the balance stays reserved. So the model is: decide on
//! the book at `T`, match against the book at `T + 250 ms`, **with the limit frozen at what `T`
//! decided** — and an order that no longer crosses is BOOKED, i.e. it RESTS. Resting is a third
//! outcome, distinct from a fill and from a miss, and it is reported as one. A rested bid can still
//! be hit later by a seller, so this bin additionally scans forward to the window close and reports
//! how many rested orders the market came back to.
//!
//! Note the tape sits on the Polygon 2-second block grid, so 250 ms is SUB-BLOCK: the state that
//! moves inside the hold is the CLOB L2 book, which is why the delay is measured against the book
//! and not against the next print.
//!
//! # Two anchors, and which one is honest
//!
//! `pre_print` folds the book strictly BEFORE the entry's own CLOB print; `at_block` folds
//! everything up to and including the entry's Polygon block. **`at_block` is the honest anchor for
//! a taker acting on the signal**: the signal IS the print, so by the time the bot has seen it and
//! sent an order, that print's block has been applied. `pre_print` is reported too, as the
//! optimistic bound. (`cheap_np_depth`, which asks the different question "what was resting when
//! the print hit", headlines `pre_print` for that reason.)
//!
//! # A third anchor, `at_print`, and when it is the honest one instead
//!
//! `at_block` is only honest while `ts_ms` really IS a block stamp. When the entry set comes from
//! the LIVE bot's own record rather than the on-chain tape, it is not: that table's `ts` column is
//! `datetime.now()` at INSERT (`bot.py::_row`), which lands ~440 s after the decision and is
//! byte-identical across all three of its lanes — useless as an anchor. The recoverable decision
//! instant there is `sts + t_into`, and `t_into` is **whole seconds**, so the stamp arrives with
//! ±1 s of quantization — far too coarse to anchor a 250 ms hold against.
//!
//! `at_print` folds up to and including the CLOB print that [`match_clob_print`] matched (the same
//! stamp `pre_print` stops just short of). That match is a ±4 s / ±0.005 search for a print at the
//! entry's own price, so it ABSORBS the 1 s quantization and returns the venue's own millisecond
//! stamp for the very print the bot fired on. It is therefore the precise decision instant, and the
//! anchor to headline whenever the entry stamps are second-quantized. When no print matches it
//! degrades to the same one-block fallback `pre_print` uses.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;

// The workspace's ONE percentile (numpy-`linear` interpolation), named at its real home. This bin
// used to spell its own `p50` as `d[d.len() / 2]` — the nearest-rank median, which reads the UPPER
// middle of an even-length sample — under the same key `cheap_np_depth` publishes.
use vike_analytics::metrics::percentile;
use vike_backtest::VENUE_HOLD_POLYMARKET_UPDOWN_MS;
use vike_backtest::binutil::{self, has_flag, store_root};
use vike_backtest::cheap_np_ask::{edge_at, price_at_edge, prob_wc};
use vike_backtest::cheap_np_book::{
    Checkpoints, POLYGON_BLOCK_MS, PRICE_GRID, ask_ladder_at, match_clob_print, verify_checkpoints,
};
use vike_backtest::fair_value::{THETA, fee};
use vike_backtest::fill_model::book_taker_price;
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::{BookUpdate, L2Book, QuoteTick, TradeTick, py_sum};

/// Window length in seconds — a rested order is watched until its market resolves.
const WINDOW_SECS: i64 = 300;

/// One entry row read from the `--entries` CSV (`cheap_np_run --signals-out`, joined to the CLOB
/// `token_id` of the entered outcome).
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
        let num = |i: usize| -> Result<f64, String> {
            f[i].parse::<f64>().map_err(|e| format!("field {i} of {line:?}: {e}"))
        };
        out.push(Entry {
            sts: f[0].parse().map_err(|e| format!("sts of {line:?}: {e}"))?,
            ts_ms: f[1].parse().map_err(|e| format!("ts_ms of {line:?}: {e}"))?,
            oidx: f[2].parse().map_err(|e| format!("oidx of {line:?}: {e}"))?,
            ask: num(3)?,
            edge: num(4)?,
            won: num(5)?,
            token_id: f[6].trim().to_string(),
        });
    }
    Ok(out)
}

/// One lane's accumulated result. Every field is a plain sum so the report can divide by whichever
/// denominator the question needs (entries taken vs signals fired).
#[derive(Debug, Default, Clone)]
struct Lane {
    /// Signals that produced a TRADE (a fill of non-zero size).
    filled: usize,
    /// Signals refused because nothing was resting at a price where the edge still cleared θ.
    /// **The headline rejection.**
    no_edge: usize,
    /// Signals whose order was BOOKED by the venue's second validation — the market moved inside
    /// the 250 ms hold. Neither a fill nor a miss.
    rested: usize,
    /// ...of which the market later came back to the limit before the window resolved, so the
    /// resting bid would have been hit (as a MAKER, at its own limit).
    rested_crossed: usize,
    /// Signals with no trustworthy book at the anchor (no snapshot / gap) — a DATA gap, never
    /// reported as a strategy result.
    no_book: usize,
    /// Σ price paid, Σ payout, Σ shares — the aggregate the report divides.
    px_sum: f64,
    won_sum: f64,
    qty_sum: f64,
    /// Σ realized PnL per share: `won − px − fee(px)`.
    pnl: Vec<f64>,
    /// Σ model edge at the price paid — the low-variance estimator (`cheap_np_depth`'s reasoning:
    /// the realized lane carries ±1 share of variance per entry).
    model_edge: Vec<f64>,
}

impl Lane {
    fn take(&mut self, px: f64, qty: f64, e: &Entry, pw: f64) {
        self.filled += 1;
        self.px_sum += px;
        self.won_sum += e.won;
        self.qty_sum += qty;
        self.pnl.push(e.won - px - fee(px));
        self.model_edge.push(edge_at(pw, px));
    }

    /// Signals this lane saw at all (a denominator that stays comparable across lanes).
    fn seen(&self) -> usize {
        self.filled + self.no_edge + self.rested + self.no_book
    }

    fn json(&self, print_n: usize) -> serde_json::Value {
        let n = self.filled.max(1) as f64;
        let mean =
            |v: &[f64]| if v.is_empty() { 0.0 } else { py_sum(v.iter().copied()) / v.len() as f64 };
        serde_json::json!({
            "signals_seen": self.seen(),
            "entries": self.filled,
            "entries_pct_of_print_gate": if print_n > 0 {
                100.0 * self.filled as f64 / print_n as f64
            } else { 0.0 },
            "refused_no_edge_at_ask": self.no_edge,
            "rested_after_venue_delay": self.rested,
            "rested_then_market_returned": self.rested_crossed,
            "rested_never_returned": self.rested - self.rested_crossed,
            "no_trustworthy_book": self.no_book,
            "avg_entry_price": self.px_sum / n,
            "win_rate": self.won_sum / n,
            "mean_qty": self.qty_sum / n,
            "pnl_per_entry": mean(&self.pnl),
            "pnl_total": py_sum(self.pnl.iter().copied()),
            "model_edge_per_entry": mean(&self.model_edge),
            // Per-entry standard error of the realized lane — the number that decides whether a
            // positive mean means anything at this sample size. Reported, never buried.
            "pnl_stderr": stderr(&self.pnl),
        })
    }
}

/// Standard error of the mean. The realized PnL of a binary payout has ~±0.5 of per-trade
/// dispersion, so at a few hundred entries this is the difference between "profitable" and
/// "indistinguishable from zero" — which is exactly the question being asked.
fn stderr(v: &[f64]) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let n = v.len() as f64;
    let m = py_sum(v.iter().copied()) / n;
    let var = py_sum(v.iter().map(|x| (x - m) * (x - m))) / (n - 1.0);
    (var / n).sqrt()
}

/// Did the market come back to `limit` before the window resolved? A resting BUY at `limit` is hit
/// when a seller crosses down to it, which the recorded book shows as its best ask reaching
/// `limit` or better. Folded forward from `after_ts` to the window close.
///
/// Deliberately a SUFFICIENT-not-necessary test: it can only under-count (a fill that happened
/// entirely between two recorded frames is invisible), so a rested order it reports as never
/// crossing really did sit unfilled in the recorded book.
fn rested_order_crossed(
    updates: &[BookUpdate],
    l1: &[QuoteTick],
    after_ts: i64,
    until_ts: i64,
    limit: f64,
) -> bool {
    // Re-fold from the start (the book has to be correct, not just recent), then watch.
    let mut book = L2Book::new(PRICE_GRID);
    let mut seq: u64 = 0;
    let mut li = 0usize;
    let mut anchored = false;
    for u in updates {
        if u.ts > until_ts {
            break;
        }
        seq += 1;
        while li < l1.len() && l1[li].ts <= u.ts {
            li += 1;
        }
        match u.kind {
            vike_model::BookUpdateKind::Snapshot => {
                book.apply_snapshot(seq, &u.bids, &u.asks);
                anchored = true;
            }
            vike_model::BookUpdateKind::Delta => {
                book.apply_delta(seq, &u.bids, &u.asks);
            }
            vike_model::BookUpdateKind::GapStart | vike_model::BookUpdateKind::Stale => {
                anchored = false
            }
            vike_model::BookUpdateKind::LiveResume => {}
        }
        if li > 0 {
            vike_backtest::cheap_np_book::prune_ghost_asks(&mut book, l1[li - 1].ask);
        }
        if anchored
            && u.ts >= after_ts
            && let Some((px, _)) = book.best_ask()
            && px <= limit
        {
            return true;
        }
    }
    false
}

/// Everything measured for ONE anchor policy.
#[derive(Debug, Default)]
struct Anchor {
    print: Lane,
    ask: Lane,
    delayed: Lane,
    /// One lane per `--live-ms` offset — the LIVE bot's own execution law (see [`live_law_price`]),
    /// priced off the FOLDED ladder. `(offset_ms, lane)`, in the order the flag listed them.
    live: Vec<(i64, Lane)>,
    /// The same law priced off the venue's own recorded top of book (see [`l1_ask_at`]) — the
    /// independent second estimate that makes fold bias visible.
    live_l1: Vec<(i64, Lane)>,
    /// best resting ask minus the tape print, over every measurable signal
    ask_minus_print: Vec<f64>,
    /// signals with ZERO resting size at the printed price
    nothing_at_print_px: usize,
}

/// Why the `--live-ms` lanes exist, and why they are NOT the ask gate.
///
/// The θ-gated `ask` / `ask + hold` lanes above model what the strategy *should* do: re-score the
/// edge at the price actually available and refuse the trade when it no longer clears θ. The LIVE
/// Dublin bot does not do that. Read from its own source
/// (`vike_db_data_jobs/trading/fair_value_bot/`):
///
/// * `cheap_bot.py::_cheap_tape_entry` — *"ignore the resting book ask, fire on the FIRST qualifying
///   cheap-band taker BUY print per window"*. So the live ENTRY gate is the **print** gate; the
///   resting ask plays no part in whether a signal is taken.
/// * `strategy.py::delayed_fill` — *"Take EVERY signal that fired, at WHATEVER ask is resting now …
///   there is NO band gate: the only non-fill is an empty book (no ask to buy at any price)."*
///
/// That is the whole explanation for the rejection-rate gap (live refuses <0.6%, the θ-gated ask
/// lane refuses a majority): **they are not the same rule.** A lane that reproduces the live law has
/// to price at the unconditional best resting ask — no limit, no θ re-check — and count only an
/// empty book as a miss. That is exactly what these lanes do, at whatever offsets `--live-ms` names
/// (the live bot's own `d_measured_ms` is the offset to use: ~954 ms for its `delayed +0s` lane,
/// ~4229 ms for `delayed +2s`).
///
/// The live bot also fills at TOP OF BOOK only (`best_ask`, `shares = min(1, displayed_size)`),
/// never walking the ladder — which at its 1-share size is the same thing a walk would return, so
/// [`book_taker_price`] with no limit reproduces it exactly rather than approximating it.
///
/// Returns the price paid, or `None` for the live bot's ONE non-fill: an empty book.
fn live_law_price(
    books: &[BookUpdate],
    l1: &[QuoteTick],
    at_ts: i64,
    size: f64,
) -> Option<Option<f64>> {
    // Outer `None` = no trustworthy book at all (a DATA gap — never reported as a strategy result).
    // Inner `None` = a trustworthy book that is EMPTY on the ask side, which is the live 'empty'.
    let ladder = ask_ladder_at(books, l1, &[], at_ts, false)?;
    Some(book_taker_price(&as_book(&ladder), 1, size, None))
}

/// The venue's OWN recorded top-of-book ask as of `at_ts` — the archive's `best_ask` column, not a
/// price derived from folding its deltas.
///
/// This exists because the two disagree, and the disagreement is measurable rather than theoretical.
/// The live bot fills from `self.book.best.get(tok)` — the WS feed's own best ask — which is exactly
/// what this column records. Our folded ladder reconstructs the same quantity from snapshot+delta
/// replay, and `verify_checkpoints` already reports that it reproduces the recorded best ask on only
/// ~90% of snapshot intervals. So a lane priced off the fold and a lane priced off this column are
/// two independent estimates of the SAME live quantity, and reporting both makes any reconstruction
/// bias visible instead of silently charging it to the strategy.
///
/// `None` when no quote has been recorded at or before `at_ts`, or the recorded ask is non-positive.
fn l1_ask_at(l1: &[QuoteTick], at_ts: i64) -> Option<f64> {
    let i = l1.partition_point(|q| q.ts <= at_ts);
    l1[..i].iter().rev().find(|q| q.ask > 0.0).map(|q| q.ask)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let arg = |flag: &str| binutil::arg(&args, flag);
    let Some(entries_path) = arg("--entries").map(PathBuf::from) else {
        eprintln!(
            "usage: cheap_np_askgate --store DIR --entries FILE.csv [--venue polymarket] \
             [--theta 0.055] [--size 1] [--delay-ms 250] [--live-ms 954,4229] \
             [--out per_entry.csv] [--json]"
        );
        // The operator-facing statement of the percentile convention, RENDERED from the shared
        // constant rather than restated here — this surface and the JSON therefore cannot come to
        // disagree, which is the same failure the stamp itself exists to end.
        eprintln!("\n--json: {}", binutil::PERCENTILE_NOTE);
        return ExitCode::FAILURE;
    };
    // `--store` > `$VIKE_HIST_STORE` > `<repo>/market_data/hist` — the repo-root default replaces the
    // old CWD-relative `market_data/hist` (run from elsewhere, `DataFusionHist::open` silently CREATED
    // an empty store and the run reported zero entries priced).
    let root: PathBuf = store_root(arg("--store").map(PathBuf::from), &std::env::vars().collect());
    let venue = arg("--venue").unwrap_or_else(|| "polymarket".to_string());
    let theta: f64 = arg("--theta").and_then(|v| v.parse().ok()).unwrap_or(THETA);
    let size: f64 = arg("--size").and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let delay_ms: i64 =
        arg("--delay-ms").and_then(|v| v.parse().ok()).unwrap_or(VENUE_HOLD_POLYMARKET_UPDOWN_MS);
    let json = has_flag(&args, "--json");
    let out_path = arg("--out").map(PathBuf::from);
    // `--live-ms A,B,C` — offsets for the LIVE-law lanes (see `live_law_price`). Empty = off, so a
    // run without the flag is byte-identical to before it existed.
    let live_ms: Vec<i64> = match arg("--live-ms") {
        None => Vec::new(),
        Some(raw) => {
            let mut v = Vec::new();
            for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                match part.parse::<i64>() {
                    Ok(ms) if ms >= 0 => v.push(ms),
                    _ => {
                        eprintln!(
                            "bad --live-ms {part:?} (want non-negative integer milliseconds)"
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            v
        }
    };

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

    let (mut pre, mut blk, mut atp) = (Anchor::default(), Anchor::default(), Anchor::default());
    let (mut no_series, mut anchor_matched, mut anchor_fallback) = (0usize, 0usize, 0usize);
    let mut ck = Checkpoints::default();
    let mut verified: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut per_entry: Vec<String> = Vec::new();

    #[allow(clippy::type_complexity)]
    let mut cache: HashMap<String, (Vec<BookUpdate>, Vec<TradeTick>, Vec<QuoteTick>)> =
        HashMap::new();

    for (i, e) in entries.iter().enumerate() {
        let (books, trades, l1) = match cache.entry(e.token_id.clone()) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let b = store
                    .scan_book_updates(&venue, &e.token_id, TsRange::all())
                    .unwrap_or_else(|err| {
                        eprintln!("scan_book_updates {}: {err}", e.token_id);
                        Vec::new()
                    });
                let t = store.scan_trades(&venue, &e.token_id, TsRange::all()).unwrap_or_default();
                // The venue's own top of book — the ghost repair's source (see `cheap_np_book`).
                let q = store.scan_quotes(&venue, &e.token_id, TsRange::all()).unwrap_or_default();
                v.insert((b, t, q))
            }
        };
        if books.is_empty() {
            no_series += 1;
            continue;
        }
        if verified.insert(e.token_id.clone()) {
            ck.merge(verify_checkpoints(books, l1, &[]));
        }

        // The block-vs-CLOB clock anchor (see `cheap_np_book::match_clob_print`).
        let pre_cutoff = match match_clob_print(trades, e.ts_ms, e.ask) {
            Some(ts) => {
                anchor_matched += 1;
                ts
            }
            None => {
                anchor_fallback += 1;
                e.ts_ms - POLYGON_BLOCK_MS
            }
        };

        for (label, agg, cutoff, strict) in [
            ("pre_print", &mut pre, pre_cutoff, true),
            ("at_block", &mut blk, e.ts_ms, false),
            ("at_print", &mut atp, pre_cutoff, false),
        ] {
            measure(
                agg,
                books,
                l1,
                cutoff,
                strict,
                e,
                theta,
                size,
                delay_ms,
                label,
                &live_ms,
                &mut per_entry,
            );
        }

        if (i + 1) % 100 == 0 {
            eprintln!("  ... {}/{} entries", i + 1, entries.len());
        }
    }

    if let Some(path) = &out_path {
        let mut csv = String::from(
            "anchor,sts,ts_ms,outcome_index,print_px,edge,won,token_id,best_ask,p_edge,\
             ask_outcome,ask_px,ask_qty,delay_outcome,delay_px,delay_qty,rested_crossed",
        );
        for ms in &live_ms {
            csv.push_str(&format!(",live_{ms}ms_px"));
        }
        for ms in &live_ms {
            csv.push_str(&format!(",live_{ms}ms_l1"));
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

    let anchor_json = |a: &Anchor| {
        let n = a.print.filled;
        let mut d = a.ask_minus_print.clone();
        // `f64::total_cmp`, not `partial_cmp().unwrap_or(Equal)`: only the former is a total
        // order, so only the former actually leaves the slice sorted when a NaN is present —
        // and `percentile` below does no comparison of its own to fall back on.
        d.sort_by(f64::total_cmp);
        serde_json::json!({
            "print_gate": a.print.json(n),
            "ask_gate": a.ask.json(n),
            "ask_gate_with_venue_delay": a.delayed.json(n),
            // The live bot's own law at each `--live-ms` offset. `refused_no_edge_at_ask` here is
            // NOT a θ refusal — this lane has no θ — it is the live 'empty' (an empty ask side).
            "live_law": a.live.iter().map(|(ms, l)| serde_json::json!({
                "offset_ms": ms,
                "lane": l.json(n),
            })).collect::<Vec<_>>(),
            "live_law_recorded_l1": a.live_l1.iter().map(|(ms, l)| serde_json::json!({
                "offset_ms": ms,
                "lane": l.json(n),
            })).collect::<Vec<_>>(),
            "best_ask_minus_print": {
                "mean": if d.is_empty() { 0.0 } else { py_sum(d.iter().copied()) / d.len() as f64 },
                // ⚠ NOT byte-identical, and it was a THIRD spelling of the same statistic. This
                // read `d[d.len() / 2]`, which is exactly the nearest-rank median
                // `cheap_np_depth`'s `pct` computed (`round((n - 1) * 0.5)` and `n / 2` agree at
                // every length), so an even-length sample reported the UPPER of the two middles
                // under a `p50` key. It now reads the same interpolating `percentile` every other
                // `p50` in this family does.
                "p50": percentile(&d, 0.5),
            },
            "signals_with_nothing_resting_at_the_print_price": a.nothing_at_print_px,
        })
    };
    let summary = serde_json::json!({
        // ⚠ RUN-LEVEL and first, never on a row: what convention produced the `p50` below.
        // `vike_backtest::binutil`'s `stats_provenance` carries the shape, why absence is the
        // signal for an older file, and why no schema version rides beside it.
        "stats_provenance": binutil::stats_provenance(),
        "store": root.display().to_string(),
        "venue": venue,
        "theta": theta,
        "size": size,
        "venue_delay_ms": delay_ms,
        "entries_in": entries.len(),
        "entries_without_book_series": no_series,
        "anchor_matched_l2_print": anchor_matched,
        "anchor_fallback_block_ts": anchor_fallback,
        "delta_integrity": {
            "snapshot_intervals_checked": ck.checked,
            "intervals_reproduced_exactly": ck.exact,
            "best_ask_reproduced": ck.best_ask_ok,
            "top5_ask_levels_reproduced": ck.top5_ok,
            "ask_levels_compared": ck.levels,
            "ask_level_mismatches": ck.level_mismatches,
        },
        "pre_print": anchor_json(&pre),
        "at_block": anchor_json(&blk),
        "at_print": anchor_json(&atp),
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    } else {
        println!("cheap_np ask gate — {} entries, store {}", entries.len(), root.display());
        // The text lane prints the same median, so it says which convention it used — RENDERED
        // from the constant, never restated, so this line cannot name a method the numbers did not
        // come from. Terminal output gets pasted into issues, where it has exactly the JSON's
        // problem and none of its structure.
        println!(
            "  percentiles: {} (--json stamps this as `stats_provenance`)",
            vike_analytics::metrics::PERCENTILE_METHOD
        );
        println!(
            "  anchor: {anchor_matched} matched to the CLOB tape, {anchor_fallback} fell back one \
             block"
        );
        println!(
            "  delta integrity: {}/{} intervals exact, {}/{} best-ask, {}/{} top-5",
            ck.exact, ck.checked, ck.best_ask_ok, ck.checked, ck.top5_ok, ck.checked
        );
        for (label, a) in [
            ("pre_print (optimistic)", &pre),
            ("at_block (raw entry stamp)", &blk),
            ("at_print (honest)", &atp),
        ] {
            println!("\n=== {label} ===");
            println!(
                "  best_ask − print: mean {:+.4}   |   signals with NOTHING resting at the print \
                 price: {} / {}",
                if a.ask_minus_print.is_empty() {
                    0.0
                } else {
                    py_sum(a.ask_minus_print.iter().copied()) / a.ask_minus_print.len() as f64
                },
                a.nothing_at_print_px,
                a.print.filled
            );
            println!(
                "  {:<26} {:>8} {:>9} {:>8} {:>11} {:>10}  {:>7} {:>7} {:>7}",
                "lane",
                "entries",
                "avg_px",
                "win",
                "pnl/entry",
                "±stderr",
                "no_edge",
                "rest",
                "no_bk"
            );
            let live_names: Vec<String> =
                a.live.iter().map(|(ms, _)| format!("live law @ +{ms}ms")).collect();
            let lanes =
                [("print gate", &a.print), ("ask gate", &a.ask), ("ask + 250ms hold", &a.delayed)]
                    .into_iter()
                    .chain(
                        live_names.iter().map(String::as_str).zip(a.live.iter().map(|(_, l)| l)),
                    );
            for (name, l) in lanes {
                let n = l.filled.max(1) as f64;
                let m = if l.pnl.is_empty() {
                    0.0
                } else {
                    py_sum(l.pnl.iter().copied()) / l.pnl.len() as f64
                };
                println!(
                    "  {name:<26} {:>8} {:>9.4} {:>8.4} {:>11.5} {:>10.5}  {:>7} {:>7} {:>7}",
                    l.filled,
                    l.px_sum / n,
                    l.won_sum / n,
                    m,
                    stderr(&l.pnl),
                    l.no_edge,
                    l.rested,
                    l.no_book
                );
            }
            println!(
                "  rested orders the market later returned to: {} of {}",
                a.delayed.rested_crossed, a.delayed.rested
            );
        }
    }
    ExitCode::SUCCESS
}

/// Score ONE entry at ONE anchor, in all three lanes.
#[allow(clippy::too_many_arguments)]
fn measure(
    agg: &mut Anchor,
    books: &[BookUpdate],
    l1: &[QuoteTick],
    cutoff: i64,
    strict: bool,
    e: &Entry,
    theta: f64,
    size: f64,
    delay_ms: i64,
    label: &str,
    live_ms: &[i64],
    per_entry: &mut Vec<String>,
) {
    // Lane 1 — the PRINT gate: today's published behaviour, and the denominator everything else is
    // compared against. It needs no book at all, which is precisely the problem.
    let pw = prob_wc(e.ask, e.edge);
    agg.print.take(e.ask, size, e, pw);

    // Lane 4..n — the LIVE bot's own law (see `live_law_price`). Scored FIRST and independently of
    // the θ lanes: the live bot never consults θ at execution time, so its lanes must not inherit
    // any of the ask gate's early returns.
    if agg.live.is_empty() {
        agg.live = live_ms.iter().map(|&ms| (ms, Lane::default())).collect();
        agg.live_l1 = live_ms.iter().map(|&ms| (ms, Lane::default())).collect();
    }
    let mut live_px: Vec<f64> = Vec::with_capacity(live_ms.len() * 2);
    let mut live_l1_px: Vec<f64> = Vec::with_capacity(live_ms.len());
    for (i, &ms) in live_ms.iter().enumerate() {
        match l1_ask_at(l1, cutoff + ms) {
            Some(px) => {
                agg.live_l1[i].1.take(px, size, e, pw);
                live_l1_px.push(px);
            }
            None => {
                agg.live_l1[i].1.no_book += 1;
                live_l1_px.push(f64::NAN);
            }
        }
        match live_law_price(books, l1, cutoff + ms, size) {
            None => {
                agg.live[i].1.no_book += 1;
                live_px.push(f64::NAN);
            }
            // An empty ask side is the live bot's 'empty' status — a real miss, not a data gap.
            Some(None) => {
                agg.live[i].1.no_edge += 1;
                live_px.push(f64::NAN);
            }
            Some(Some(px)) => {
                agg.live[i].1.take(px, size, e, pw);
                live_px.push(px);
            }
        }
    }

    live_px.extend_from_slice(&live_l1_px);

    let Some(decision) = ask_ladder_at(books, l1, &[], cutoff, strict) else {
        agg.ask.no_book += 1;
        agg.delayed.no_book += 1;
        push_row(
            per_entry,
            label,
            e,
            f64::NAN,
            f64::NAN,
            "no_book",
            f64::NAN,
            "no_book",
            f64::NAN,
            false,
            size,
            &live_px,
        );
        return;
    };
    let best_ask = decision.iter().find(|&&(_, q)| q > 0.0).map(|&(p, _)| p).unwrap_or(f64::NAN);
    if best_ask.is_finite() {
        agg.ask_minus_print.push(best_ask - e.ask);
    }
    // Was there anything at all resting at the price the strategy claims it paid?
    let at_print =
        decision.iter().filter(|&&(p, _)| p <= e.ask).map(|&(_, q)| q.max(0.0)).sum::<f64>();
    if at_print <= 0.0 {
        agg.nothing_at_print_px += 1;
    }

    // The θ-clearing limit — the order the strategy would really send.
    let Some(limit) = price_at_edge(pw, theta) else {
        agg.ask.no_edge += 1;
        agg.delayed.no_edge += 1;
        push_row(
            per_entry,
            label,
            e,
            best_ask,
            f64::NAN,
            "no_edge",
            f64::NAN,
            "no_edge",
            f64::NAN,
            false,
            size,
            &live_px,
        );
        return;
    };

    // Lane 2 — the ASK gate: the ENGINE's own taker law, at the decision book.
    let ask_px = book_taker_price(&as_book(&decision), 1, size, Some(limit));
    match ask_px {
        Some(px) if edge_at(pw, px) > theta => agg.ask.take(px, size, e, pw),
        _ => agg.ask.no_edge += 1,
    }

    // Lane 3 — the VENUE HOLD: same decision, same frozen limit, matched against the book as it
    // stands `delay_ms` later. An order that no longer crosses is BOOKED — it rests.
    let (delay_outcome, delay_px, rested_crossed) = if ask_px.is_none() {
        // Nothing to submit: the ask gate already refused, so the hold never happens.
        agg.delayed.no_edge += 1;
        ("no_edge", f64::NAN, false)
    } else {
        match ask_ladder_at(books, l1, &[], cutoff + delay_ms, false) {
            None => {
                agg.delayed.no_book += 1;
                ("no_book", f64::NAN, false)
            }
            Some(later) => match book_taker_price(&as_book(&later), 1, size, Some(limit)) {
                Some(px) => {
                    agg.delayed.take(px, size, e, pw);
                    ("filled", px, false)
                }
                None => {
                    agg.delayed.rested += 1;
                    // A resting BUY at `limit` is hit when the market comes back to it. Watched to
                    // the window close; can only under-count (see the helper's doc).
                    let crossed = rested_order_crossed(
                        books,
                        l1,
                        cutoff + delay_ms,
                        (e.sts + WINDOW_SECS) * 1000,
                        limit,
                    );
                    if crossed {
                        agg.delayed.rested_crossed += 1;
                    }
                    ("rested", f64::NAN, crossed)
                }
            },
        }
    };

    push_row(
        per_entry,
        label,
        e,
        best_ask,
        limit,
        if ask_px.is_some() { "filled" } else { "no_edge" },
        ask_px.unwrap_or(f64::NAN),
        delay_outcome,
        delay_px,
        rested_crossed,
        size,
        &live_px,
    );
}

/// Append ONE per-entry row. Every early return in [`measure`] goes through here too, so the CSV
/// carries a row for EVERY (anchor, entry) pair — which is what lets a sharded run (one store per
/// slice of the archive) be aggregated back into exact totals: the reader can trust that a missing
/// row means a missing entry, never a silently-skipped outcome.
#[allow(clippy::too_many_arguments)]
fn push_row(
    per_entry: &mut Vec<String>,
    label: &str,
    e: &Entry,
    best_ask: f64,
    limit: f64,
    ask_outcome: &str,
    ask_px: f64,
    delay_outcome: &str,
    delay_px: f64,
    rested_crossed: bool,
    size: f64,
    live_px: &[f64],
) {
    let mut row = format!(
        "{label},{sts},{ts},{oidx},{print_px:.6},{edge:.6},{won},{tok},{best_ask:.6},{limit:.6},\
         {ask_outcome},{ask_px:.6},{ask_q:.4},{delay_outcome},{delay_px:.6},{delay_q:.4},{crossed}",
        sts = e.sts,
        ts = e.ts_ms,
        oidx = e.oidx,
        print_px = e.ask,
        edge = e.edge,
        won = e.won,
        tok = e.token_id,
        ask_q = if ask_outcome == "filled" { size } else { 0.0 },
        delay_q = if delay_outcome == "filled" { size } else { 0.0 },
        crossed = rested_crossed,
    );
    for px in live_px {
        row.push_str(&format!(",{px:.6}"));
    }
    per_entry.push(row);
}

/// Rebuild an [`L2Book`] from a level ladder so the ENGINE's own taker law can price it. Cheap
/// (a few dozen levels) and it is what keeps this bin from growing a private walk.
fn as_book(asks: &[(f64, f64)]) -> L2Book {
    let mut b = L2Book::new(PRICE_GRID);
    // Negative displayed sizes are a real fault in this archive; clamped at the level so they
    // contribute nothing rather than subtracting from the walk.
    let levels: Vec<(f64, f64)> = asks.iter().map(|&(p, q)| (p, q.max(0.0))).collect();
    b.apply_snapshot(1, &[], &levels);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This bin's `best_ask_minus_print.p50` must stay the shared interpolating percentile.
    ///
    /// It was spelled `d[d.len() / 2]` — the nearest-rank median, identical at every length to the
    /// `round((n - 1) * 0.5)` body `cheap_np_depth`'s `pct` carried — so an even-length sample
    /// published the UPPER of its two middles under a `p50` key while every other percentile in
    /// the workspace interpolated. The `assert_ne!` is the point: it pins that the two spellings
    /// genuinely disagree, so restoring the index-into-the-slice one reddens here.
    #[test]
    fn the_ask_minus_print_median_is_the_shared_percentile() {
        // Dyadic values throughout, so every assertion below is bit-exact rather than an epsilon:
        // the claim is about WHICH formula runs, and a tolerance would blur exactly that.
        let mut d = [0.5, 0.125, 0.375, 0.25];
        d.sort_by(f64::total_cmp);
        assert_eq!(percentile(&d, 0.5), 0.3125, "the interpolant between 0.25 and 0.375");
        assert_ne!(percentile(&d, 0.5), d[d.len() / 2], "nearest-rank would publish 0.375");
        // An odd length is where the two agree, and the empty guard the `if` used to carry is now
        // `percentile`'s own.
        let odd = [0.125, 0.25, 0.375];
        assert_eq!(percentile(&odd, 0.5), odd[odd.len() / 2]);
        assert_eq!(percentile(&[], 0.5), 0.0);
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
    /// duplicated onto a per-anchor lane at two — this bin builds THREE anchor subtrees, and the
    /// stamp belongs above all three, not inside each.
    #[test]
    fn the_json_summary_stamps_the_percentile_convention_it_used() {
        let key = "stats_provenance";
        let needle = format!("\"{key}\": binutil::{key}()");
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/cheap_np_askgate.rs"
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
            "the stamp must name the percentile `best_ask_minus_print.p50` actually calls"
        );
    }

    #[test]
    fn stderr_is_zero_below_two_samples_and_shrinks_with_n() {
        assert_eq!(stderr(&[]), 0.0);
        assert_eq!(stderr(&[1.0]), 0.0);
        let small: Vec<f64> = (0..10).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        let big: Vec<f64> = (0..1000).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        assert!(stderr(&big) < stderr(&small));
    }

    /// The ladder→book rebuild must preserve what the engine walks, and must not let an archive
    /// fault (a negative displayed size) subtract from the depth.
    #[test]
    fn as_book_preserves_the_ladder_and_clamps_negative_sizes() {
        let b = as_book(&[(0.30, 100.0), (0.31, 50.0)]);
        assert_eq!(b.quantity_for_price(1, 1.0), 150.0);
        let want: f64 = (0.30 * 100.0 + 0.31 * 20.0) / 120.0;
        let got = book_taker_price(&b, 1, 120.0, None).expect("the ladder covers 120 shares");
        assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        let faulty = as_book(&[(0.30, -5.0), (0.31, 7.0)]);
        assert_eq!(faulty.quantity_for_price(1, 1.0), 7.0);
    }

    /// The bin's whole claim in one test: the same signal, gated on the print vs on the ask.
    #[test]
    fn the_ask_gate_refuses_what_the_print_gate_takes() {
        let (print_px, edge) = (0.27, 0.06);
        let pw = prob_wc(print_px, edge);
        let limit = price_at_edge(pw, THETA).unwrap();
        // the measured April reality: the resting ask is ~4 cents worse than the print
        let book = as_book(&[(0.31, 5_000.0)]);
        assert!(edge_at(pw, print_px) > THETA, "the print cleared the bar");
        assert_eq!(
            book_taker_price(&book, 1, 1.0, Some(limit)),
            None,
            "...and the resting ask does not: NOT a trade"
        );
    }
}
