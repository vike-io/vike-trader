//! `cheap_np_askgate` — what `cheap_np` is worth once entries are gated and priced on the
//! **resting ask** a taker can actually lift, and once Polymarket's **250 ms venue taker delay**
//! is applied.
//!
//! ```sh
//! cheap_np_askgate --entries FILE.csv \
//!     [--venue polymarket] [--theta 0.055] [--size 1] [--delay-ms 0,250] \
//!     [--live-ms 954,4229] [--out per_entry.csv] [--json]
//! ```
//!
//! ⚠ **`--store` is OPTIONAL and its absence is not a default directory — it is the WIRE.**
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` gives the hist store ONE reader,
//! the datahub, so without that flag this bin reads its book/trade/quote series over RPC from
//! `$VIKE_DATAHUB_ADDR` (default `127.0.0.1:7878`) rather than opening Parquet itself. Pass
//! `--store DIR` to read local files anyway — when the data daemon is down, or when a
//! whole-series scan per entry is not worth the transfer. Node keys come from the credential
//! STORE, not the environment, so a keyed datahub needs nothing exported. The run PRINTS which
//! route it took: on the human line, and in the `--json` summary's `store` field.
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
//! * the book comes from [`vike_data::cheap_np_book`] (the archive fold + the
//!   venue-authoritative ghost repair + the snapshot-checkpoint integrity gate);
//! * what a taker PAYS is [`vike_model::book_taker_price`] — literally the function
//!   `vike_fills::fill_model::L2BookFillModel` calls, so the engine would price these fills
//!   identically;
//! * the edge re-score and the θ-clearing limit are [`vike_strategy::strategies::cheap_np_ask`], which
//!   [`vike_strategy::CheapNp`] itself calls under [`vike_strategy::EntryPrice::RestingAsk`];
//! * 250 ms is [`vike_model::POLYMARKET_ITODE_HOLD_MS`].
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
use vike_analytics::binutil::has_flag;
use vike_analytics::metrics::percentile;
use vike_backtest::binutil;
use vike_backtest::research::anchor::{POLYGON_BLOCK_MS, match_clob_print};
use vike_backtest::research::entries::{Entry, read_entries};
use vike_data::cheap_np_book::{Checkpoints, PRICE_GRID, verify_checkpoints};
use vike_data::{HistStore, TsRange};
use vike_datahub_client::flag_vocab::store_flag_removed;
use vike_datahub_client::route::{datahub_addr_for_bin, history_route, open_routed_history};
#[cfg(test)]
use vike_model::book_taker_price;
use vike_model::{BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick, py_sum};
use vike_strategy::strategies::cheap_np_ask::edge_at;
#[cfg(test)]
use vike_strategy::strategies::cheap_np_ask::{price_at_edge, prob_wc};
use vike_strategy::strategies::fair_value::{THETA, fee};

mod lane;
mod measure;

#[cfg(doc)]
use lane::{l1_ask_at, live_law_price};
use measure::measure;

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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let arg = |flag: &str| vike_analytics::binutil::arg(&args, flag);
    let Some(entries_path) = arg("--entries").map(PathBuf::from) else {
        eprintln!(
            "usage: cheap_np_askgate --entries FILE.csv [--store DIR] [--venue polymarket] \
             [--theta 0.055] [--size 1] [--delay-ms 250] [--live-ms 954,4229] \
             [--out per_entry.csv] [--json]\n\n\
             History is read over the wire from the datahub \
             ($VIKE_DATAHUB_ADDR, default 127.0.0.1:7878) — see decision 0084. For local files, \
             start a key-less datahub on them first: VIKE_DATAHUB_STORE=DIR vike-backend datahub"
        );
        // The operator-facing statement of the percentile convention, RENDERED from the shared
        // constant rather than restated here — this surface and the JSON therefore cannot come to
        // disagree, which is the same failure the stamp itself exists to end.
        eprintln!("\n--json: {}", binutil::PERCENTILE_NOTE);
        return ExitCode::FAILURE;
    };
    // ⚠ **The default is the WIRE** (`docs/decisions/0084-only-the-datahub-touches-the-store.md`):
    // the hist store has ONE reader, the datahub, and everything else asks it over the wire. Only
    // `--store` ON THE LINE opts out — `$VIKE_HIST_STORE` still answers WHICH root a local read
    // opens and deliberately does NOT select the route. That break is `history_route`'s, stated
    // there for the compute daemon and true here for the same reason: a flag on the line is a
    // decision somebody made for one invocation, an inherited variable is one nobody remembers
    // making — and the deployed box sets that variable.
    //
    // The ADDRESS ladder is `$VIKE_DATAHUB_ADDR` > `vike_config::DEFAULT_DATAHUB_ADDR`, resolved by
    // that same function so this bin carries no second copy of the blank-value filter or the
    // default. It is genuinely a narrower ladder than `vike-cli`'s and that is not an omission: a
    // bare research bin loads no settings file, so `config.datahub_addr` is not a rung it can
    // reach. One function, two sources.
    //
    // ⚠ There is ONE arm now. `--store DIR` opted this bin out to local files until 2026-09-25,
    // when the owner closed the local READ door; it is REFUSED by name below, with the one command
    // that gives the local run back. The `<repo>/market_data/hist` default and its silent-CREATE
    // trap went with the local arm that needed them.
    let vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    if has_flag(&args, "--store") || arg("--store").is_some() {
        eprintln!("{}", store_flag_removed("cheap_np_askgate"));
        return ExitCode::FAILURE;
    }
    let route = history_route(datahub_addr_for_bin(&vars));
    // ⚠ What the two disclosure sites below PRINT — a path on the local arm, the ADDRESS on the
    // wire one, never a directory the server resolved. Rendered once so the JSON field and the
    // human line cannot come to disagree about where these numbers were read from.
    let store_label = route.label();
    let venue = arg("--venue").unwrap_or_else(|| "polymarket".to_string());
    let theta: f64 = arg("--theta").and_then(|v| v.parse().ok()).unwrap_or(THETA);
    let size: f64 = arg("--size").and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let delay_ms: i64 = arg("--delay-ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(i64::from(vike_model::POLYMARKET_ITODE_HOLD_MS));
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
    // Every `scan_*` below goes over the wire on the default arm. These callers scan a whole
    // series per entry (`TsRange::all()`), so that is the cost 0084 buys the single-reader
    // property with; `--store DIR` is the documented way back to local files.
    // ⚠ The type is ANNOTATED rather than inferred, and that is the point of the line: `store`
    // is a trait OBJECT now, not a `DataFusionHist`. Inference would have compiled either way
    // and left the reader to guess which. (It also keeps `HistStore` a USED import — a method
    // call through `Box<dyn HistStore>` needs no trait in scope, unlike the same call on the
    // concrete store this replaced, so dropping the annotation drops the import with it.)
    let store: Box<dyn HistStore + Send + Sync> = open_routed_history(&route, &vars);

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

        // The block-vs-CLOB clock anchor (see `research::anchor::match_clob_print`).
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
        // ⚠ The KEY is unchanged and the VALUE is no longer always a path: on the wire arm it
        // names the datahub's ADDRESS, because the resolved root is the SERVER's and a
        // directory printed client-side would be a guess about another box's filesystem. A
        // consumer that parsed this as a path was reading a provenance string all along.
        "store": store_label.as_str(),
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
        println!("cheap_np ask gate — {} entries, history from {store_label}", entries.len());
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

/// Rebuild an [`L2Book`] from a level ladder so the ENGINE's own taker law can price it. Cheap
/// (a few dozen levels) and it is what keeps this bin from growing a private walk.
fn as_book(asks: &[BookLevel]) -> L2Book {
    let mut b = L2Book::new(PRICE_GRID);
    // Negative displayed sizes are a real fault in this archive; clamped at the level so they
    // contribute nothing rather than subtracting from the walk.
    let levels: Vec<BookLevel> =
        asks.iter().map(|&BookLevel { price: p, qty: q }| BookLevel::new(p, q.max(0.0))).collect();
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
    /// `crates/vike-tradehub/tests/live_lock_claim_order_gate.rs` applies to the composition roots, and
    /// it carries that gate's rule with it: if this reddens because the emitting site was
    /// legitimately re-spelled, UPDATE the needle, never delete the test.
    ///
    /// Two properties make the needle worth trusting. It is ASSEMBLED rather than written out, so
    /// the joined string appears nowhere in this file and can only match the emitting site — a
    /// literal would have let the test pass on its own text, the self-reference hazard
    /// `crates/vike-ops/tests/docs/unrun_command_gate.rs` declares for itself. And EXACTLY one match is
    /// required, which fails a typo'd needle at zero (so the test cannot go vacuous) and a marker
    /// duplicated onto a per-anchor lane at two — this bin builds THREE anchor subtrees, and the
    /// stamp belongs above all three, not inside each.
    #[test]
    fn the_json_summary_stamps_the_percentile_convention_it_used() {
        let key = "stats_provenance";
        let needle = format!("\"{key}\": binutil::{key}()");
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/cheap_np_askgate/main.rs"
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
        let b = as_book(&[BookLevel::new(0.30, 100.0), BookLevel::new(0.31, 50.0)]);
        assert_eq!(b.quantity_for_price(1, 1.0), 150.0);
        let want: f64 = (0.30 * 100.0 + 0.31 * 20.0) / 120.0;
        let got = book_taker_price(&b, 1, 120.0, None).expect("the ladder covers 120 shares");
        assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        let faulty = as_book(&[BookLevel::new(0.30, -5.0), BookLevel::new(0.31, 7.0)]);
        assert_eq!(faulty.quantity_for_price(1, 1.0), 7.0);
    }

    /// The bin's whole claim in one test: the same signal, gated on the print vs on the ask.
    #[test]
    fn the_ask_gate_refuses_what_the_print_gate_takes() {
        let (print_px, edge) = (0.27, 0.06);
        let pw = prob_wc(print_px, edge);
        let limit = price_at_edge(pw, THETA).unwrap();
        // the measured April reality: the resting ask is ~4 cents worse than the print
        let book = as_book(&[BookLevel::new(0.31, 5_000.0)]);
        assert!(edge_at(pw, print_px) > THETA, "the print cleared the bar");
        assert_eq!(
            book_taker_price(&book, 1, 1.0, Some(limit)),
            None,
            "...and the resting ask does not: NOT a trade"
        );
    }
}
