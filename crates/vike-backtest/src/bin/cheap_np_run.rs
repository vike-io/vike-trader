//! `cheap_np_run` — drive [`vike_backtest::CheapNp`] over the REAL hist store, window by window.
//!
//! This is the store-driven counterpart to `tests/cheap_np_parity.rs`. The parity gate proves the
//! GATE against a frozen, pre-joined, pre-filtered Python cache; this bin proves the whole path —
//! the raw ClickHouse tape ingested into the `vike-data` store, the sparse 1-second spot series,
//! `trailing_sigma` computing σ for itself, the real `StrategyEngine`, real market-order fills and
//! real binary-resolution settlement.
//!
//! ```sh
//! cheap_np_run --store DIR --from 2026-06-08 --to 2026-06-14 \
//!     --resolutions resolutions.csv [--poly-venue polymarket] [--spot-venue spot] \
//!     [--spot-symbol BTCUSDT] [--mode hold|flip] [--theta 0.055] [--size 1.0] \
//!     [--sigma-scale 1.0] [--latency-ms 0,1000,5000] [--max-windows N] [--signals-out FILE.csv] [--json]
//! ```
//!
//! # `--latency-ms` — the one thing SQL cannot do
//!
//! A comma-separated list of ORDER latencies in milliseconds; each value is a separate variant run
//! **over the same loaded ticks** (the store scan dominates, so N variants cost ~N engine folds and
//! ONE scan). `0` — the default — installs no latency model at all
//! ([`EngineParams::latency_model`] `= None`, the frozen zero-latency path); every non-zero value
//! installs [`LatencyModelKind::Constant`] on BOTH legs.
//!
//! What the entry leg buys here is not a price haircut bolted on afterwards — it is the honest
//! mechanism: an order submitted at print `T` becomes visible to the matching engine at `T + Δ`, so
//! it fills at the next print of ITS OWN token at `ts >= T + Δ`, at THAT print's price. And if no
//! such print arrives before the window resolves, `resolve_at` has already latched the symbol
//! resolved and `push_pending` refuses the delivery — **the order MISSES entirely**, which is
//! precisely what a next-print-fill SQL haircut cannot express. Misses are counted
//! (`unfilled_entries`) and reported as a rate.
//!
//! The response leg is wired for completeness but is INERT for this strategy: `CheapNp` tracks what
//! it holds in its own `WindowState` and never reads `Broker::position` or `on_fill`.
//!
//! RESOLUTION FLOOR, and it is load-bearing: `polymarket_trades` is an ON-CHAIN tape, so its `ts`
//! is the **Polygon block timestamp** — 100 % of April 2026's prints have `ts % 1000 == 0`, and
//! 301,059 of 301,608 consecutive distinct print-seconds are exactly **2 s** apart (measured on
//! the latency box; there is not a single 1-second gap). Every print of a block therefore shares one stamp,
//! and the observed gap from an entry print to the NEXT print of its own token is 2 s in 5,134 of
//! 5,151 April entries.
//!
//! Two consequences, both real rather than modelling artefacts:
//!
//! * Every Δ in `(0, 2000]` ms produces the IDENTICAL fill — it lands on the next block either
//!   way. The tape simply cannot resolve latency below one block, so a 65 ms and a 2 s round trip
//!   are indistinguishable here. What Δ > 0 does buy is real and large: it forfeits the ~49 other
//!   prints stamped in the entry's OWN block, which is where the Δ = 0 optimism lives.
//! * MISSING the fill is not this strategy's latency risk. A token prints every block while its
//!   window is live, so misses stay at 1 of 5,152 all the way out to Δ = 5 s, and only reach
//!   0.5 % at 15 s, 3.2 % at 30 s and 26 % at 2 min. The damage is the price of the next block.
//!
//! # Why per-WINDOW engine runs, not one big run
//!
//! Two independent reasons, both structural — this is not a shortcut:
//!
//! 1. **`StrategyEngine::run_ticks` is O(streams) per tick and O(symbols) per dispatch.** Its
//!    k-way merge rescans every stream head to pick the next tick, and it resolves a tick's symbol
//!    with a linear `position()` over the registered symbol list. Each 5-minute window is its own
//!    pair of ERC-1155 tokens, so a month of `btc-updown-5m` is ~17k symbols — a single run would
//!    be quadratic in the thing that grows.
//! 2. **`CheapNp` holds ONE window's state.** `roll_window` REPLACES `self.win` whenever a print
//!    from a different `sts` arrives, and the real tape interleaves adjacent windows (≈1 % of a
//!    window's prints land before its own open; the observed `t` range for a window is roughly
//!    `[−500, +370]` s). In one merged run the next window's early prints would evict the current
//!    window's `held`, and a later print of the current window would then re-enter it — a double
//!    entry. Per-window runs make the strategy's own invariant hold by construction.
//!
//! Nothing is lost by splitting: the strategy is one-entry-per-window, hold-to-resolution, sized
//! in absolute units, so windows are genuinely independent and their PnL is additive. That is the
//! same decomposition the published reference numbers use.
//!
//! # Two numbers are reported, deliberately
//!
//! * **SIGNAL** — `pnl = won − ask − fee(ask)` on the fired print's own price. This is the
//!   quantity the published reference set encodes, so it is the one that is comparable. It is
//!   invariant under `--latency-ms` by construction (the gate does not know about the order path).
//! * **EXECUTED** — what the engine actually did: a market order filled at the NEXT print of that
//!   token at-or-after its delivery stamp, settled at the on-chain payout. The gap between the two
//!   is real slippage, not noise, and an entry whose token never prints again before resolution
//!   does not fill at all (the engine cancels resting orders the moment a market resolves) —
//!   counted and reported.
//!
//! Under `--mode flip` the EXECUTED lane books one row per BUY fill (the entry and each flip's new
//! leg); a flip's exit is a real SALE at the next print, so its row's `won` is that sale price
//! rather than a 0/1 payout — which is why the aggregate charges the taker fee on BOTH legs. For a
//! held-to-resolution row the exit leg is a settlement at 0.0 or 1.0, where `0.072·p·(1−p)` is
//! exactly zero, so Hold's numbers are unchanged by that second fee term.
//!
//! # The fee (port backlog G7 — the engine seam is FIXED; the analytic subtraction is a CHOICE)
//!
//! The G7 trap this section used to warn about is closed (#648): `EngineParams::fee_schedule` CAN
//! price this strategy's cost now. `StrategyEngine::new` no longer flattens
//! `FeeSchedule::ProbabilityScaled` through `maker_taker_rates()` (the `(0.0, 0.0)` shape that
//! once charged exactly zero while looking configured) — that one schedule is carried VERBATIM to
//! the fill site and applied per fill via `FeeSchedule::commission`, the real `qty·0.072·p·(1−p)`.
//! Machine-checked:
//! `tests/cheap_np_profile.rs::the_probability_scaled_fee_is_charged_and_matches_the_driver` pins
//! that a profile-mounted `probability_scaled` schedule charges the engine path bit-identically to
//! this driver's arithmetic. This bin still runs the engine fee-free (`fee_rate: 0.0`) and
//! subtracts `0.072·p·(1−p)` analytically BY CHOICE, not necessity: it reports TWO lanes at
//! different transaction prices (SIGNAL never touches the engine; EXECUTED fills at the next
//! print), and pricing each leg's fee at that leg's own price inside the lane arithmetic is what
//! keeps the two PnLs attributable leg-by-leg — a single engine-side fee would cost the EXECUTED
//! lane correctly but leave the SIGNAL comparison number unpriced.
//!
//! # Resolutions
//!
//! `--resolutions` is a headerless-or-headered CSV of `slug,winning_index`, exported SELECT-only
//! from the latency box:
//!
//! ```sql
//! SELECT DISTINCT t.slug, r.winning_index
//! FROM data_polymarket.polymarket_resolutions_onchain r
//! INNER JOIN (SELECT DISTINCT slug, condition_id FROM data_polymarket.polymarket_trades
//!             WHERE slug LIKE 'btc-updown-5m-%' AND ts >= '...' AND ts < '...') t
//!   ON t.condition_id = r.condition_id
//! WHERE r.winning_index >= 0
//! FORMAT CSVWithNames
//! ```
//!
//! A `winning_index` outside `{0, 1}` is NOT coerced: the market has an outcome this binary model
//! cannot express, so the window is EXCLUDED from the run and counted in `windows_bad_resolution`.
//! (The full on-chain table contains at least one such row — a `winning_index` of 4.) Silently
//! mapping it to "both sides lose" would manufacture a 100 %-loss window out of a data problem.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_backtest::binutil::{arg, f64_arg, has_flag, store_root};
use vike_backtest::cheap_np::WINDOW_SECS;
use vike_backtest::engine::{EngineParams, StrategyEngine, Tick};
use vike_backtest::fair_value::{SIGMA_LOOKBACK_S, THETA, fee};
use vike_backtest::latency::LatencyModelKind;
use vike_backtest::{CheapNp, CheapNpMode, TokenId};
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::time::{days_from_civil, parse_ymd};
use vike_model::{Bar, QuoteTick, TradeTick, py_sum};

/// Midnight-UTC epoch seconds of a `YYYY-MM-DD` day.
fn day_start_secs(ymd: &str) -> Result<i64, String> {
    let (y, m, d) = parse_ymd(ymd).map_err(|e| e.to_string())?;
    Ok(days_from_civil(y, m, d) * 86_400)
}

// -------------------------------------------------------------------------------------------
// resolutions
// -------------------------------------------------------------------------------------------

/// `slug -> winning_index`, restricted to the binary `{0, 1}` outcomes this model can express.
/// Returns the map plus the number of rows rejected for an out-of-domain `winning_index`.
///
/// The slug is unquoted before use. ClickHouse's `FORMAT CSVWithNames` — the export the module doc
/// prescribes — emits `"btc-updown-5m-1775001600",0`, and a quoted key matches NO token symbol, so
/// a reader that kept the quotes would resolve zero windows and report the whole month as
/// `windows_no_resolution` while looking like it had loaded 8,675 rows.
fn read_resolutions(path: &Path) -> Result<(HashMap<String, u8>, usize), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut map = HashMap::new();
    let mut rejected = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((slug, wi)) = line.split_once(',') else { continue };
        let slug = slug.trim().trim_matches('"');
        let Ok(wi) = wi.trim().parse::<i64>() else {
            continue; // the CSVWithNames header row lands here
        };
        match wi {
            0 | 1 => {
                map.insert(slug.to_string(), wi as u8);
            }
            // NOT coerced — see the module doc. An unexpected index means the market resolved to
            // something a two-outcome payout cannot represent.
            _ => rejected += 1,
        }
    }
    Ok((map, rejected))
}

// -------------------------------------------------------------------------------------------
// aggregation
// -------------------------------------------------------------------------------------------

/// One booked position, from either the SIGNAL lane or the EXECUTED lane.
#[derive(Debug, Clone, Copy)]
struct Booked {
    price: f64,
    won: f64,
}

#[derive(Debug, Default)]
struct Agg {
    n: usize,
    win_rate: f64,
    avg_price: f64,
    total_pnl: f64,
    pnl_per_trade: f64,
    ret_per_trade: f64,
}

/// `pnl = won − price − fee(price) − fee(won)` at 1 unit — the reference's own formula, with the
/// true `0.072·p·(1−p)` taker fee applied at EACH transacted price (see the module doc on G7).
///
/// The second term is what makes a `--mode flip` exit honest: a flip sells the held token into the
/// tape at a real price and pays taker fee on it. It costs the reference-comparable Hold lane
/// exactly nothing — a hold's exit is a settlement at `won ∈ {0, 1}` and `0.072·p·(1−p)` is
/// identically zero at both ends, so every SIGNAL row and every held-to-resolution EXECUTED row is
/// byte-identical to the single-fee formula.
fn aggregate(rows: &[Booked], size: f64) -> Agg {
    let n = rows.len();
    if n == 0 {
        return Agg::default();
    }
    let pnl = |b: &Booked| (b.won - b.price - fee(b.price) - fee(b.won)) * size;
    let total = py_sum(rows.iter().map(pnl));
    Agg {
        n,
        win_rate: py_sum(rows.iter().map(|b| b.won)) / n as f64,
        avg_price: py_sum(rows.iter().map(|b| b.price)) / n as f64,
        total_pnl: total,
        pnl_per_trade: total / n as f64,
        ret_per_trade: py_sum(rows.iter().map(|b| pnl(b) / (b.price * size))) / n as f64,
    }
}

fn print_agg(label: &str, a: &Agg) {
    println!("  {label}");
    println!("    entries      : {}", a.n);
    println!("    win_rate     : {:.4}", a.win_rate);
    println!("    avg_ask      : {:.4}", a.avg_price);
    println!("    total_pnl    : {:.4}", a.total_pnl);
    println!("    pnl_per_trade: {:.6}", a.pnl_per_trade);
    println!("    ret_per_trade: {:.4}", a.ret_per_trade);
}

fn agg_json(a: &Agg) -> serde_json::Value {
    serde_json::json!({
        "entries": a.n,
        "win_rate": a.win_rate,
        "avg_ask": a.avg_price,
        "total_pnl": a.total_pnl,
        "pnl_per_trade": a.pnl_per_trade,
        "ret_per_trade": a.ret_per_trade,
    })
}

// -------------------------------------------------------------------------------------------
// the run
// -------------------------------------------------------------------------------------------

/// Every ingested token series that parses as a `cheap_np` window symbol, grouped by window open.
fn discover_windows(
    store: &DataFusionHist,
    venue: &str,
    from_s: i64,
    to_s: i64,
) -> Result<Vec<(i64, Vec<String>)>, String> {
    let series = store.list_series().map_err(|e| e.to_string())?;
    let mut by_window: HashMap<i64, Vec<String>> = HashMap::new();
    for s in series {
        if s.kind != "trade" || s.venue != venue {
            continue;
        }
        // Anything that is not a `<slug>#<0|1>` window token is skipped here for the same reason
        // the strategy ignores it: it carries no window identity.
        let Some(tok) = TokenId::parse(&s.symbol) else { continue };
        if tok.sts < from_s || tok.sts >= to_s {
            continue;
        }
        by_window.entry(tok.sts).or_default().push(s.symbol);
    }
    let mut out: Vec<(i64, Vec<String>)> = by_window.into_iter().collect();
    out.sort_by_key(|(sts, _)| *sts);
    for (_, syms) in out.iter_mut() {
        syms.sort(); // "#0" before "#1" — deterministic stream order
    }
    Ok(out)
}

/// The spot samples in `[lo_ms, hi_ms]`, as `Tick::Quote`s carrying `symbol`.
fn spot_slice(spot: &[QuoteTick], symbol: &str, lo_ms: i64, hi_ms: i64) -> Vec<Tick> {
    let a = spot.partition_point(|q| q.ts < lo_ms);
    let b = spot.partition_point(|q| q.ts <= hi_ms);
    spot[a..b]
        .iter()
        .map(|q| Tick::Quote(QuoteTick { symbol: symbol.to_string(), ..q.clone() }))
        .collect()
}

/// Everything that is the same for every latency variant (one store scan, one window discovery).
#[derive(Debug, Default)]
struct Shared {
    windows_run: usize,
    windows_no_resolution: usize,
    windows_bad_resolution: usize,
    windows_no_spot: usize,
    prints: usize,
}

/// The per-latency-variant accumulator. One of these per `--latency-ms` value.
struct Variant {
    /// Order latency in ms; `0` means no latency model is installed at all.
    latency_ms: i64,
    signal: Vec<Booked>,
    executed: Vec<Booked>,
    /// Gates that fired but whose BUY never reached a fill before the window resolved.
    unfilled: usize,
    signal_rows: Vec<String>,
}

impl Variant {
    fn new(latency_ms: i64) -> Self {
        Variant {
            latency_ms,
            signal: Vec::new(),
            executed: Vec::new(),
            unfilled: 0,
            signal_rows: Vec::new(),
        }
    }

    /// `None` for `0` (the frozen zero-latency path — no in-flight queue is built at all), a
    /// both-legs constant otherwise. See the module doc's `--latency-ms` section.
    fn model(&self) -> Option<LatencyModelKind> {
        (self.latency_ms > 0).then(|| {
            let ns = self.latency_ms.saturating_mul(1_000_000);
            LatencyModelKind::constant(ns, ns)
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn run_window(
    store: &DataFusionHist,
    venue: &str,
    sts: i64,
    tokens: &[String],
    spot: &[QuoteTick],
    spot_symbol: &str,
    winning_index: u8,
    mode: CheapNpMode,
    theta: f64,
    size: f64,
    sigma_scale: f64,
    shared: &mut Shared,
    variants: &mut [Variant],
) -> Result<(), String> {
    let sts_ms = sts * 1000;
    let res_ms = (sts + WINDOW_SECS) * 1000;

    // σ needs a full lookback of history BEFORE the window opens; the tape runs a little past the
    // close (observed max ≈ +370 s), so keep a margin on the right too.
    let lo_ms = sts_ms - (SIGMA_LOOKBACK_S as i64) * 1000;
    let hi_ms = res_ms + 600_000;
    let spot_ticks = spot_slice(spot, spot_symbol, lo_ms, hi_ms);
    if spot_ticks.is_empty() {
        shared.windows_no_spot += 1;
        return Ok(());
    }

    // The SPOT stream is registered and merged FIRST: `run_ticks`'s k-way merge breaks an equal-ts
    // tie by stream order, so a spot sample stamped the same millisecond as a print reaches the
    // strategy BEFORE that print — the causally honest order.
    let mut series: Vec<(String, Vec<Tick>)> = vec![(spot_symbol.to_string(), spot_ticks)];
    for sym in tokens {
        let ticks: Vec<TradeTick> = store
            .scan_trades(venue, sym, TsRange::all())
            .map_err(|e| format!("scan_trades {sym}: {e}"))?;
        shared.prints += ticks.len();
        series.push((
            sym.clone(),
            ticks
                .into_iter()
                .map(|t| Tick::Trade(TradeTick { symbol: sym.clone(), ..t }))
                .collect(),
        ));
    }

    let symbols: Vec<(String, Vec<Bar>)> =
        series.iter().map(|(s, _)| (s.clone(), Vec::new())).collect();

    let wi = winning_index;

    for v in variants.iter_mut() {
        // The settlement source: a token of THIS window pays 1.0 when its outcome won, 0.0
        // otherwise, and only from the window close onward. Everything else (the spot symbol,
        // another window's token) returns `None` and is never settled or latched. Rebuilt per
        // variant because `ResolutionSource` is a boxed `FnMut`-shaped owner the engine consumes.
        let resolution = Box::new(move |sym: &str, ts: i64| -> Option<f64> {
            if ts < res_ms {
                return None;
            }
            let tok = TokenId::parse(sym)?;
            if tok.sts != sts {
                return None;
            }
            Some(if tok.oidx == wi { 1.0 } else { 0.0 })
        });

        let mut strat = CheapNp::new(spot_symbol);
        strat.mode = mode;
        strat.theta = theta;
        strat.size = size;
        strat.sigma_scale = sigma_scale;

        let mut engine = StrategyEngine::new(
            symbols.clone(),
            strat,
            EngineParams {
                cash: 1_000.0,
                // fee_rate stays 0.0 BY CHOICE — the engine can price the p(1−p) curve itself
                // since #648 (`EngineParams::fee_schedule` + `FeeSchedule::commission`), but both
                // lanes subtract it analytically for leg-level attribution (see the module doc's
                // fee section).
                fee_rate: 0.0,
                slippage: 0.0,
                resolution: Some(resolution),
                // A recorded tape stops at the trading halt, so the end-of-run sweep must probe at
                // the window close — never at the `i64::MAX` sentinel, which this source would
                // answer for any window and stamp the settlement with.
                resolution_end_ts: Some(res_ms),
                // `None` at Δ=0 — the frozen path, so the baseline variant is exactly the number
                // this bin produced before latency existed.
                latency_model: v.model(),
                ..Default::default()
            },
        );
        let res = engine.run_ticks(&series);

        // --- SIGNAL lane: what the gate fired on ---------------------------------------------
        // Invariant across variants (the gate never consults the order path) — recorded per
        // variant anyway, so a divergence would show up rather than be assumed away.
        for s in &engine.strategy.signals {
            if s.is_flip {
                continue; // a flip is an exit+entry, not a fresh window entry
            }
            let won = f64::from(u8::from(s.oidx == wi));
            v.signal.push(Booked { price: s.ask, won });
            v.signal_rows.push(format!(
                "{sts},{ts},{oidx},{wi},{ask:.17},{edge:.17},{won}",
                ts = s.ts,
                oidx = s.oidx,
                ask = s.ask,
                edge = s.edge,
            ));
        }

        // --- EXECUTED lane: what the engine actually did --------------------------------------
        // Every fired gate is a BUY, and every BUY that fills opens exactly one long trade — so
        // `signals.len()` is the right denominator in BOTH modes (in `Hold` it equals `entries`).
        let fired = engine.strategy.signals.len();
        let mut filled = 0usize;
        for t in &res.trades {
            if !t.is_long {
                continue;
            }
            filled += 1;
            // `exit_price` is whatever closed the leg: the 1.0/0.0 settlement payout for a held
            // position, or the real sale price when a `Flip` exited it into the tape.
            v.executed.push(Booked { price: t.entry_price, won: t.exit_price });
        }
        v.unfilled += fired.saturating_sub(filled);
    }
    shared.windows_run += 1;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (Some(from), Some(to), Some(res_path)) =
        (arg(&args, "--from"), arg(&args, "--to"), arg(&args, "--resolutions"))
    else {
        eprintln!(
            "usage: cheap_np_run --store DIR --from YYYY-MM-DD --to YYYY-MM-DD \
             --resolutions FILE.csv [--poly-venue polymarket] [--spot-venue spot] \
             [--spot-symbol BTCUSDT] [--mode hold|flip] [--theta 0.055] [--size 1.0] \
             [--sigma-scale 1.0] [--latency-ms 0,1000,5000] [--max-windows N] [--signals-out FILE.csv] [--json]"
        );
        return ExitCode::FAILURE;
    };

    // `--store` > `$VIKE_HIST_STORE` > `<repo>/market_data/hist` — the repo-root default replaces the
    // old CWD-relative `market_data/hist` (run from elsewhere, `DataFusionHist::open` silently CREATED
    // an empty store and the run reported "0 windows").
    let root: PathBuf =
        store_root(arg(&args, "--store").map(PathBuf::from), &std::env::vars().collect());
    let poly_venue = arg(&args, "--poly-venue").unwrap_or_else(|| "polymarket".to_string());
    let spot_venue = arg(&args, "--spot-venue").unwrap_or_else(|| "spot".to_string());
    let spot_symbol = arg(&args, "--spot-symbol").unwrap_or_else(|| "BTCUSDT".to_string());
    let mode = match arg(&args, "--mode").as_deref() {
        Some(m) if m.eq_ignore_ascii_case("flip") => CheapNpMode::Flip,
        _ => CheapNpMode::Hold,
    };
    let theta = f64_arg(&args, "--theta", THETA);
    // SENSITIVITY ONLY (see `CheapNp::sigma_scale`): re-run the month with σ scaled, to attribute
    // a divergence against an independently-measured answer to the σ estimator's gap policy.
    let sigma_scale = f64_arg(&args, "--sigma-scale", 1.0);
    let size = f64_arg(&args, "--size", 1.0);
    let max_windows = arg(&args, "--max-windows").and_then(|v| v.parse::<usize>().ok());
    let signals_out = arg(&args, "--signals-out").map(PathBuf::from);
    let json = has_flag(&args, "--json");

    // `--latency-ms A,B,C` — one variant per value, all folded over the SAME loaded ticks. An
    // unparseable or negative entry is a hard error rather than a silent default: reporting a
    // "latency" run that quietly had none would be exactly the misreport this bin exists to avoid.
    let latencies: Vec<i64> = match arg(&args, "--latency-ms") {
        None => vec![0],
        Some(s) => {
            let mut out = Vec::new();
            for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                match part.parse::<i64>() {
                    Ok(v) if v >= 0 => out.push(v),
                    _ => {
                        eprintln!("bad --latency-ms {part:?} (want non-negative integers)");
                        return ExitCode::FAILURE;
                    }
                }
            }
            out.dedup();
            if out.is_empty() { vec![0] } else { out }
        }
    };

    let (from_s, to_s) = match (day_start_secs(&from), day_start_secs(&to)) {
        // `--to` is INCLUSIVE of the whole day, like the collectors' ranges.
        (Ok(a), Ok(b)) => (a, b + 86_400),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("bad --from/--to: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (resolutions, bad_res_rows) = match read_resolutions(Path::new(&res_path)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("--resolutions: {e}");
            return ExitCode::FAILURE;
        }
    };

    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open hist store at {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };

    let mut windows = match discover_windows(&store, &poly_venue, from_s, to_s) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("discover windows: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(n) = max_windows {
        windows.truncate(n);
    }
    if windows.is_empty() {
        eprintln!(
            "no `{poly_venue}` kind=trade series parse as <slug>#<outcome> in [{from}, {to}] \
             — was the tape ingested with --symbol-key slug?"
        );
        return ExitCode::FAILURE;
    }

    // The spot series is loaded ONCE for the whole range (plus a σ lookback of left margin) and
    // sliced per window; a 1-second series is ~86k rows/day, so this is cheap and avoids one
    // store round trip per window.
    let spot_lo = (from_s - SIGMA_LOOKBACK_S as i64) * 1000;
    let spot_hi = (to_s + 600) * 1000;
    let spot = match store.scan_quotes(&spot_venue, &spot_symbol, TsRange::of(spot_lo, spot_hi)) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("scan_quotes {spot_venue}/{spot_symbol}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if spot.is_empty() {
        eprintln!("spot series {spot_venue}/{spot_symbol} is empty over the requested range");
        return ExitCode::FAILURE;
    }

    eprintln!(
        "cheap_np_run: {} window(s) [{from}, {to}], {} spot samples, {} resolutions, \
         mode={mode:?} theta={theta} size={size} sigma_scale={sigma_scale} \
         latency_ms={latencies:?}",
        windows.len(),
        spot.len(),
        resolutions.len(),
    );

    let mut shared = Shared { windows_bad_resolution: bad_res_rows, ..Default::default() };
    let mut variants: Vec<Variant> = latencies.iter().map(|&ms| Variant::new(ms)).collect();

    let total_windows = windows.len();
    for (i, (sts, tokens)) in windows.iter().enumerate() {
        // The slug is shared by both of a window's tokens, so read it off the first.
        let slug = tokens[0].rsplit_once('#').map(|(s, _)| s).unwrap_or(tokens[0].as_str());
        let Some(&wi) = resolutions.get(slug) else {
            shared.windows_no_resolution += 1;
            continue;
        };
        if let Err(e) = run_window(
            &store,
            &poly_venue,
            *sts,
            tokens,
            &spot,
            &spot_symbol,
            wi,
            mode,
            theta,
            size,
            sigma_scale,
            &mut shared,
            &mut variants,
        ) {
            eprintln!("window {sts}: {e}");
        }
        if (i + 1) % 250 == 0 {
            eprintln!("  ... {}/{total_windows} windows", i + 1);
        }
    }

    // The signal CSV is written from the FIRST variant only — the gate is latency-invariant, so
    // every variant's rows are the same set.
    if let (Some(path), Some(v)) = (&signals_out, variants.first()) {
        let mut csv = String::from("sts,ts_ms,outcome_index,winning_index,ask,edge,won\n");
        for r in &v.signal_rows {
            csv.push_str(r);
            csv.push('\n');
        }
        if let Err(e) = std::fs::write(path, csv) {
            eprintln!("--signals-out {}: {e}", path.display());
        }
    }

    if json {
        let per_variant: Vec<serde_json::Value> = variants
            .iter()
            .map(|v| {
                serde_json::json!({
                    "latency_ms": v.latency_ms,
                    "fills": v.executed.len(),
                    "unfilled_entries": v.unfilled,
                    "miss_rate": miss_rate(v),
                    "signal": agg_json(&aggregate(&v.signal, size)),
                    "executed": agg_json(&aggregate(&v.executed, size)),
                })
            })
            .collect();
        let out = serde_json::json!({
            "mode": format!("{mode:?}"),
            "theta": theta,
            "sigma_scale": sigma_scale,
            "windows_discovered": total_windows,
            "windows_run": shared.windows_run,
            "windows_no_resolution": shared.windows_no_resolution,
            "windows_bad_resolution": shared.windows_bad_resolution,
            "windows_no_spot": shared.windows_no_spot,
            "prints": shared.prints,
            "variants": per_variant,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        println!("\ncheap_np over the hist store — {from} .. {to} (UTC, inclusive), mode={mode:?}");
        println!("  windows discovered   : {total_windows}");
        println!("  windows run          : {}", shared.windows_run);
        println!("  windows w/o resolution: {}", shared.windows_no_resolution);
        println!("  windows w/o spot     : {}", shared.windows_no_spot);
        println!("  rows w/ non-binary winning_index: {}", shared.windows_bad_resolution);
        println!("  taker prints replayed: {}", shared.prints);
        for v in &variants {
            let tag = if v.latency_ms == 0 {
                "no latency model (frozen zero-latency path)".to_string()
            } else {
                format!("order latency {} ms on both legs", v.latency_ms)
            };
            println!("\n=== {tag} ===");
            println!(
                "  gates that never filled before resolution: {} ({:.2} % of fires)",
                v.unfilled,
                100.0 * miss_rate(v)
            );
            println!();
            print_agg("SIGNAL   (fired print's own ask — comparable to the reference)", &{
                aggregate(&v.signal, size)
            });
            println!();
            print_agg("EXECUTED (engine fill at the next print + on-chain settlement)", &{
                aggregate(&v.executed, size)
            });
        }
    }

    ExitCode::SUCCESS
}

/// Share of fired gates whose BUY never filled. Denominator is fires (fills + misses), so it is
/// comparable across variants even though the fill count moves.
fn miss_rate(v: &Variant) -> f64 {
    let fires = v.executed.len() + v.unfilled;
    if fires == 0 { 0.0 } else { v.unfilled as f64 / fires as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes ClickHouse's `FORMAT CSVWithNames` produces for the module doc's own
    /// resolutions query — header row, quoted slugs, bare integers.
    #[test]
    fn read_resolutions_unquotes_the_clickhouse_csv_and_rejects_non_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("res.csv");
        std::fs::write(
            &path,
            "\"slug\",\"winning_index\"\n\
             \"btc-updown-5m-1775001600\",0\n\
             \"btc-updown-5m-1775001900\",1\n\
             \"btc-updown-5m-1775002200\",4\n",
        )
        .unwrap();

        let (map, rejected) = read_resolutions(&path).unwrap();
        // The keys must be the BARE slugs — a quoted key matches no `<slug>#<oidx>` token symbol
        // and would silently resolve zero windows.
        assert_eq!(map.get("btc-updown-5m-1775001600"), Some(&0));
        assert_eq!(map.get("btc-updown-5m-1775001900"), Some(&1));
        assert_eq!(map.len(), 2, "the header row is not a resolution");
        // `winning_index = 4` is NOT coerced to "both sides lose" — it is counted and dropped.
        assert!(!map.contains_key("btc-updown-5m-1775002200"));
        assert_eq!(rejected, 1);
    }

    #[test]
    fn latency_variant_zero_installs_no_model_and_nonzero_is_symmetric_ns() {
        assert!(Variant::new(0).model().is_none(), "Δ=0 must stay on the frozen path");
        match Variant::new(90).model() {
            Some(LatencyModelKind::Constant { entry_ns, response_ns }) => {
                assert_eq!((entry_ns, response_ns), (90_000_000, 90_000_000), "ms → ns, both legs");
            }
            other => panic!("expected a constant model, got {other:?}"),
        }
    }
}
