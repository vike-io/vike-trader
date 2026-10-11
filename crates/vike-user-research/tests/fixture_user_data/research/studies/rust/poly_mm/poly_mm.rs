//! **`poly_mm` — the Polymarket market-making sweep, as a USER STUDY.**
//!
//! This is the ENTRY FILE `vike-user-research`'s `build.rs` finds and compiles.
//!
//! It lives under `tests/fixture_user_data/` beside two ~60-line fixtures, and it is not a fixture:
//! that tree is the ONLY user_data a CI checkout has, so a study here is the one kind a COMPILER
//! ever sees. Keeping the port compiler-checked was chosen over keeping it private, at a stated
//! price: `cargo test -p vike-user-research` compiles about 1,600 lines no shipped binary contains
//! (`tests/` code reaches no release and no dispatcher). The study is split into `matrix`,
//! `params`, `store` and `tests` siblings beside this entry file, each a plain `mod` it declares.
//! `crates/vike-user-research/src/contract.rs` is the contract it satisfies:
//!
//! ```ignore
//! pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError>
//! ```
//!
//! # What it ports
//!
//! The `poly_mm_batch` BINARY (`crates/vike-poly-research`, deleted with decision 0076). For each
//! market of a universe of Polymarket outcome tokens it runs the [`matrix::CONFIGS`] grid (a sweep
//! over each model's key knob: A–S `γ`, LMSR `b`, LS-LMSR `α`, Glosten–Milgrom `μ`) plus the
//! [`matrix::TRAILING_CONFIGS`] scalper pair over the recorded `kind = "tick"` lane (quotes +
//! trades + books, so the `prob_power` queue model fills resting quotes against the real taker
//! tape), one backtest per `(market, lane, strategy, config)` cell, and sums the return and trade
//! count and averages the win rate per `(lane, family, config)`.
//!
//! Every number is the binary's number. What changed is everything AROUND the numbers: a study
//! owns no argv, store, logger, stdout or exit code.
//!
//! | binary concern | what happened to it |
//! |---|---|
//! | `CliSpec`, `USAGE`, `arg`/`has_flag`, `ExitCode` | **DROPPED.** Every knob is a `params` key (below); `vike-backfill` is not a dependency (`crates/vike-user-research/src/lib.rs` argues why). |
//! | `--archive` / `--store`, `open_backtest_store` | **DROPPED.** The store ARRIVES; which backend, and its cost, is the caller's decision. |
//! | `window_clock_banner` | **DROPPED, a real loss**: which recorder timestamp the backend means is a property of the store the host opened; the host should print it. |
//! | `vike_log::init` + `log_config` | **DROPPED.** Only binaries initialise logging. |
//! | `eprintln!` progress | **DROPPED, a real loss**: the contract carries no progress. |
//! | `eprintln!` per-failure lines and summary | **TRANSLATED** to `failures.tsv` and metrics. |
//! | `println!` aggregate table | **TRANSLATED** to metrics plus `per_cell.tsv`. |
//! | `harness::run_backtest` + `BacktestReport` | **TRANSLATED** to ONE [`vike_user_research::StudySim::run_one`] call; [`vike_user_research::SimOutcome`] carries only the three numbers the table used (Sharpe, drawdown and the equity curve do not cross the seam). |
//! | `BacktestProfile::from_toml_str` | **KEPT as a CHECK, not a type.** The text is parsed here with `toml::from_str`; the schema check runs on the host and returns `Err(String)`. The ported tests assert well-formed TOML only, which is weaker, and say so. |
//! | `CachedHistStore` | **KEPT, and promoted** — next section. |
//!
//! # ⚠ The memoizing wrapper is the only way to call the seam
//!
//! [`vike_user_research::StudySim::run_one`] takes `Arc<dyn HistStore + Send + Sync>`, and
//! [`vike_user_research::StudyContext`] exposes no store handle, only read verbs. So a study must
//! PRESENT a store built from those verbs: [`ContextStore`], the binary's `CachedHistStore` with
//! its backing swapped to `ctx: StudyContext`. The memoization stays for the measured reason: every
//! cell of one market's matrix replays the IDENTICAL `(polymarket, token, [from, to])` slice.
//! One adapter per market, inside the `par_iter` closure, keeps memory to one market's slice.
//!
//! ⚠ **The adapter is narrower than the wrapper in three places, each a behaviour change:**
//!
//! 1. **The append half and `resample_*` REFUSE** with `DataError::Query`: required methods, and
//!    the contract gives a study nothing to write through. `Ok(0)` would report a write that did
//!    not happen.
//! 2. **`scan_symbol_properties`, `scan_equity`, `scan_exec_fills` and `scan_exec_orders` REFUSE**:
//!    no `StudyContext` verb is behind them. The largest risk of the port: a simulator that
//!    pre-fetches symbol properties (`properties_as_of` derives from `scan_symbol_properties`)
//!    fails every run. An empty `Ok` would make "nothing was recorded" and "this contract cannot
//!    ask" the same answer; the cure is a `properties` verb on `StudyContext`.
//! 3. **`list_series`/`inventory` inherit the trait's REFUSAL**, reversing the wrapper: "cannot
//!    enumerate" was false for a type fronting a store that can, and is true for one fronting a
//!    contract with no catalog verb.
//!
//! And one place it is WIDER: `scan_depth`, `scan_cohort` and `scan_perp_metrics` FORWARD to the
//! matching context verb, since inheriting a refusal where the context can answer would be the
//! same false statement in the other direction.
//!
//! # `params` — every knob the binary had
//!
//! ```toml
//! fee        = false     # bool. the real Polymarket 0.072·p(1−p) per-fill taker cost
//! floor      = 0.0       # number. `min_half_spread_ticks`; 0 = NO floor
//! latency_ms = 0         # integer ≥ 0. `[engine] order_latency_ms`
//! lane       = "l2"      # "l1" | "l2" | "both"
//! strategy   = "spread"  # "spread" | "trailing" | "both"
//!
//! [[universe]]           # required, ≥ 1 row
//! family      = "btc-5m" # the slug; a `-15m` substring picks the 15-minute tenor
//! token       = "…"      # the Polymarket outcome-token id, the store's `symbol`
//! end_date_ms = 0        # the market's close, epoch ms
//! ```
//!
//! **The universe is the token list ITSELF, not a path**: a study never resolves a path. The
//! binary's TSV columns became the three keys one for one; the price is a long recipe, and the
//! study stays a pure function of `(store, params)`.
//!
//! ⚠ **The readers are STRICTER than the binary's, deliberately.** `--floor banana` ran with NO
//! floor and a malformed TSV line silently left the universe; here a wrong-typed knob or a
//! malformed universe row is `StudyError::Study`, naming the key and what was found.
//!
//! # What it hands back
//!
//! Headline metrics first (`markets`, `runs`, `failed`, `cells`, the resolved parameters), then one
//! group of four per `(lane, family, config)` cell — `sum_ret`, `trades`, `avg_win`, `n` — under
//! the stable name `"{lane}/{family}/{config}/{metric}"`. Two artifacts: `per_cell.tsv` (the table
//! the binary printed, same columns) and `failures.tsv` (the lines it sent to stderr).
//!
//! # Determinism
//!
//! `rayon` fans out over MARKETS only, and `par_iter().map(..).collect()` keeps input order, so the
//! sequential fold adds each market's return in universe order on every run. The per-cell sum
//! stays a naive `+=`, NOT `vike_model::py_sum`: changing the fold would change the numbers this
//! port exists to preserve. `BTreeMap` orders the cells, so metric emission is stable; `HashMap`
//! is only the scan cache, whose order reaches no arithmetic.

mod matrix;
mod params;
mod store;

use std::collections::BTreeMap;
use std::sync::Arc;

use rayon::prelude::*;

use vike_data::HistStore;
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

use matrix::{Agg, ConfigRow, MarketResult, profile_toml, run_matrix, window_for};
use params::{read_knobs, read_universe};
use store::ContextStore;

// ---- the study ---------------------------------------------------------------------------------

/// Sanitize one failure message into a single TSV field: the simulator's `Err` is free text and
/// may carry a newline or a tab, either of which would silently split a row of `failures.tsv`.
fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let markets = read_universe(params)?;
    let knobs = read_knobs(params)?;
    let matrix = run_matrix(&knobs.lanes, &knobs.strats, knobs.floor);

    // The host seam, resolved BEFORE any work: a study that needs an event-driven run says so and
    // stops, naming the work, rather than degrading into a number from a different machine. The
    // parameters are read first so the message can state the size of what was refused.
    let Some(sim) = ctx.sim() else {
        return Err(StudyError::NoSim(format!(
            "{} Polymarket maker backtests — {} markets × {} (lane × strategy × config) cells \
             over the recorded tick lane",
            markets.len() * matrix.len(),
            markets.len(),
            matrix.len()
        )));
    };

    // How many markets ask for history outside the window this run was handed. NOT clamped: a
    // market's window is its exact life and truncating it would silently change what the numbers
    // below mean. It is counted and emitted instead, so a run over a half-covered universe is
    // visible in the listing rather than only in the returns.
    let window = ctx.window();
    let outside = markets
        .iter()
        .filter(|m| {
            let (from, to) = window_for(&m.family, m.end_date_ms);
            window.start.is_some_and(|s| from < s) || window.end.is_some_and(|e| to > e)
        })
        .count() as u64;

    // Parallelize over MARKETS: each market runs its own matrix independently and returns its
    // per-config rows. `par_iter().map(..).collect()` preserves input order, which is what makes
    // the sequential fold below add the same f64s in the same order on every run — a parallel
    // reduction would not. `&dyn StudySim` crosses into the closures because `StudySim: Sync`.
    let per_market: Vec<MarketResult> = markets
        .par_iter()
        .map(|market| {
            let mut out: Vec<ConfigRow> = Vec::new();
            let mut failed = 0u64;
            let mut notes: Vec<String> = Vec::new();
            let (from, to) = window_for(&market.family, market.end_date_ms);

            // ONE cache per market, shared by every cell below: they all replay the identical
            // (venue = polymarket, token, [from, to]) tick slice, so the context's read verbs are
            // driven once here instead of once per cell. Never shared ACROSS markets (a fresh
            // adapter per `map` call), so memory stays bounded to one market's slice at a time.
            let market_store: Arc<dyn HistStore + Send + Sync> =
                Arc::new(ContextStore::new(ctx.clone()));

            for spec in &matrix {
                let text = profile_toml(&market.token, from, to, knobs.fee, knobs.latency_ms, spec);
                let profile = match toml::from_str::<toml::Value>(&text) {
                    Ok(p) => p,
                    Err(e) => {
                        // The binary parsed with `BacktestProfile::from_toml_str` and this is the
                        // weaker half of that check — well-formedness only. The SCHEMA half now
                        // runs on the host side and arrives as the `Err(String)` below.
                        notes.push(format!(
                            "{}\t{}\tprofile\t{}",
                            market.token,
                            spec.label,
                            one_line(&e.to_string())
                        ));
                        failed += 1;
                        continue;
                    }
                };
                let outcome = match sim.run_one(&profile, market_store.clone()) {
                    Ok(o) => o,
                    Err(e) => {
                        notes.push(format!(
                            "{}\t{}\trun\t{}",
                            market.token,
                            spec.label,
                            one_line(&e)
                        ));
                        failed += 1;
                        continue;
                    }
                };
                out.push((
                    spec.lane.label().to_string(),
                    market.family.clone(),
                    spec.label.to_string(),
                    outcome.total_return,
                    outcome.n_trades,
                    outcome.win_rate,
                ));
            }
            (out, failed, notes)
        })
        .collect();

    // The aggregation, sequential and in universe order. `+=` stays a naive fold, matching the
    // binary: `vike_model::py_sum` here would be a different number from the one this port exists
    // to reproduce.
    let mut agg: BTreeMap<(String, String, String), Agg> = BTreeMap::new();
    let (mut n_markets, mut runs, mut failed) = (0u64, 0u64, 0u64);
    let mut failures = String::from("token\tconfig\tstage\tmessage\n");
    for (results, f, notes) in per_market {
        failed += f;
        for note in notes {
            failures.push_str(&note);
            failures.push('\n');
        }
        if !results.is_empty() {
            n_markets += 1;
        }
        for (lane, family, label, ret, trades, win) in results {
            let cell = agg.entry((lane, family, label)).or_default();
            cell.ret += ret;
            cell.trades += trades;
            cell.win_sum += win;
            cell.n += 1;
            runs += 1;
        }
    }

    let mut outcome = StudyOutcome::new();
    // Headline first — the contract's own convention, so a run listing shows the shape of the
    // sweep without opening an artifact.
    outcome.metric("markets", n_markets as f64)?;
    outcome.metric("markets_requested", markets.len() as f64)?;
    outcome.metric("markets_outside_window", outside as f64)?;
    outcome.metric("runs", runs as f64)?;
    outcome.metric("failed", failed as f64)?;
    outcome.metric("cells", agg.len() as f64)?;
    outcome.metric("configs_per_market", matrix.len() as f64)?;
    // The three knobs the binary printed in its table header, so the manifest records the
    // configuration a number was produced under rather than leaving it to the recipe.
    outcome.metric("param/fee", if knobs.fee { 1.0 } else { 0.0 })?;
    outcome.metric("param/floor", knobs.floor)?;
    outcome.metric("param/latency_ms", knobs.latency_ms as f64)?;

    // One group of four per cell, in `BTreeMap` order so emission is stable across runs.
    //
    // ⚠ The name FLATTENS a tuple key, and `StudyOutcome::metric` refuses a duplicate. A family
    // slug containing `/` could therefore collide with another cell — which surfaces as a
    // `StudyError::Study` naming the metric, not as a silently merged row. That is the correct
    // failure and it is why the separator is `/` rather than `.`: a config label like `as-g0.05`
    // already contains a dot.
    let mut table = String::from("lane\tfamily\tconfig\tsum_ret\ttrades\tavg_win\tn\n");
    for ((lane, family, label), cell) in &agg {
        let avg_win = if cell.n > 0 { cell.win_sum / cell.n as f64 } else { 0.0 };
        let stem = format!("{lane}/{family}/{label}");
        outcome.metric(&format!("{stem}/sum_ret"), cell.ret)?;
        outcome.metric(&format!("{stem}/trades"), cell.trades as f64)?;
        outcome.metric(&format!("{stem}/avg_win"), avg_win)?;
        outcome.metric(&format!("{stem}/n"), cell.n as f64)?;
        table.push_str(&format!(
            "{lane}\t{family}\t{label}\t{}\t{}\t{}\t{}\n",
            cell.ret, cell.trades, avg_win, cell.n
        ));
    }

    // The table the binary printed, as a file. Deliberately RAW values rather than the binary's
    // `{:>+10.4}` / `×100` presentation: a `.tsv` is read by a program, and rounding a return to
    // four decimals on the way out is a lossy choice a formatter should make, not a study.
    outcome.artifact("per_cell.tsv", table)?;
    // ...and the lines that used to go to stderr. Always emitted, even with no failures, so a
    // missing file means the study did not get this far rather than "nothing went wrong".
    outcome.artifact("failures.tsv", failures)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests;
