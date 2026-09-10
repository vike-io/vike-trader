//! `tearsheet` — the live-journal report renderer, as a LIBRARY function.
//!
//! ⚠ This was `src/bin/tearsheet.rs`'s body until the multicall merge. `main` became [`run`],
//! taking BOTH argv and the environment as parameters. The environment matters: this tool reads
//! `VIKE_JOURNAL_DIR`, and taking it from a caller-supplied map rather than `std::env::var` is what
//! moves its `vike_ops::settings::SETTINGS` row from `Layer::Binary` to `Layer::Injected` — the
//! registry's own declared target state, reached instead of fought.
//!
//! Everything below is the binary's own documentation, unchanged.
//! writes when `VIKE_JOURNAL_DIR` / `VIKE_RUN_PROFILE` enable it — see
//! `vike_core::journal_config_from_env`), reconstructs closed [`crate::reconstruct_trades`]
//! round-trips through the shared `compute_fill` cost-basis primitive, and computes the standard
//! performance tearsheet via `vike_analytics::metrics` — bit-for-bit identical to a backtest over
//! the same fills (see the `vike-report` crate doc).
//!
//! Usage:
//!   tearsheet --journal DIR [--seed CASH] [--name LABEL] [--periods-per-year N] [--json]
//!             [--html PATH] [--store DIR [--mtm] [--venue V] [--interval STR]]
//!
//! `--journal DIR` (required, or `$VIKE_JOURNAL_DIR`) is the journal segment directory. `--seed`
//! is the account's starting capital (default 10000) — it sets the base of the realized-only
//! equity curve, so it scales `total_return`/`cagr`/`sharpe`; pass the session's real starting
//! equity for accurate returns. `--periods-per-year` is the Sharpe/Sortino/Calmar/CAGR
//! annualization factor (default 252); NOTE the equity curve here is realized-per-trade, not a
//! fixed calendar cadence, so annualized ratios are approximate. `--json` prints the
//! `LiveTearsheet` as pretty JSON instead of the human table. `--html PATH` ADDITIONALLY writes
//! the self-contained HTML tearsheet (`crate::render_html` — metrics table, inline-SVG
//! equity + underwater-drawdown charts, monthly-returns table when timestamps allow) to `PATH`;
//! the terminal output (table or `--json`) is unchanged.
//!
//! ## `--store` (HistStore enrichment — requires `--features hist`)
//!
//! `--store DIR` points at a `vike-data` DataFusion+Parquet hist store root. When given AND the
//! bin was built `--features hist`, two enrichments replace the bare realized-only tearsheet. First,
//! the equity curve is read from the store's `kind=equity` series
//! (`crate::equity_curve_from_store`) — a true mark-to-market curve. The vike-core equity
//! sampler persists the cross-venue ACCOUNT rollup under `venue="portfolio"`, `symbol="TOTAL"` (per-
//! exchange rows use `symbol=<exchange>`), so that whole-account curve is what the tearsheet reads;
//! it falls back to the realized-only curve (with a stderr note) when no such series exists. Second,
//! each reconstructed trade's `mae`/`mfe` is back-filled from the store's OHLC bars over the trade
//! window (`crate::backfill_excursions`).
//!
//! `--interval STR` (default `1m`) is the bar series interval used for the excursion back-fill.
//! `--venue V` names the trades' venue for the excursion BAR lookups (the `Trade` type drops venue);
//! when omitted it is derived from the journal's fills if they all share one venue (else `--venue`
//! is required). The equity-curve lookup does NOT use it (that series is the `portfolio`/`TOTAL`
//! rollup, not per-venue). Built WITHOUT `--features hist`, `--store` errors (exit 2) — the
//! DataFusion backend isn't compiled in.
//!
//! `--mtm` (requires `--store`) swaps the equity source: instead of the sampler's `kind=equity`
//! series, the curve is RECONSTRUCTED from the journal fills marked to market against the store's
//! `(--venue, --interval)` bars (`crate::mtm_curve_from_store`) — the sampler stays the
//! default. It additionally renders a "Runtime exposure" section (turnover / gross+net exposure /
//! peak margin, `crate::RuntimeStats`) into the `--html` output. Without `--store` it is a
//! no-op. On the default (no `--mtm`) path everything is byte-identical to before.

use std::path::PathBuf;
use std::process::ExitCode;

// The shared argv glue, from the same vike-analytics crate this bin already links for the metrics
// catalog. `binutil` is feature-free and vike-analytics is DataFusion-free, so this bin stays one.
use crate::report::{DAILY_PERIODS_PER_YEAR, LiveTearsheet};
use vike_analytics::binutil::{arg, has_flag, parse_num};

pub fn run(env: &std::collections::HashMap<String, String>, args: &[String]) -> ExitCode {
    // --journal DIR, else $VIKE_JOURNAL_DIR (the same env var that turns journaling ON in the
    // producing binary — so a user points the reader at exactly what they enabled).
    let Some(dir) =
        arg(args, "--journal").or_else(|| env.get("VIKE_JOURNAL_DIR").cloned()).map(PathBuf::from)
    else {
        eprintln!(
            "tearsheet: --journal <dir> is required (or set $VIKE_JOURNAL_DIR)\n\
             usage: tearsheet --journal DIR [--seed CASH] [--name LABEL] [--periods-per-year N] \
             [--json] [--html PATH] [--store DIR [--venue V] [--interval STR]]"
        );
        return ExitCode::from(2);
    };

    let seed = match parse_num::<f64>(args, "--seed", 10_000.0) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tearsheet: {e}");
            return ExitCode::from(2);
        }
    };
    let ppy = match parse_num::<f64>(args, "--periods-per-year", DAILY_PERIODS_PER_YEAR) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tearsheet: {e}");
            return ExitCode::from(2);
        }
    };

    // Store-backed path if --store is present; otherwise the unchanged realized-only path. Both
    // return the sheet PLUS the equity curve/timestamps it was assembled from — the flat sheet
    // does not retain them, and the --html renderer plots them.
    let (mut sheet, equity, equity_ts, runtime) = if let Some(store_dir) = arg(args, "--store") {
        let interval = arg(args, "--interval").unwrap_or_else(|| "1m".to_string());
        let venue_flag = arg(args, "--venue");
        let mtm = has_flag(args, "--mtm");
        match build_from_store(&dir, &store_dir, seed, ppy, &interval, venue_flag, mtm) {
            Ok(parts) => parts,
            Err(code) => return code,
        }
    } else {
        // The same four steps LiveTearsheet::from_journal performs — inlined so the curve stays
        // in hand for --html; values are identical.
        match build_realized(&dir, seed, ppy) {
            Ok(parts) => parts,
            Err(e) => {
                eprintln!("tearsheet: failed to read journal at {dir:?}: {e}");
                return ExitCode::FAILURE;
            }
        }
    };
    sheet.name = arg(args, "--name");

    // --html PATH: additionally write the self-contained HTML tearsheet (terminal output stays).
    // The runtime-stats section renders only on the --mtm path (`runtime` is Some); otherwise the
    // default `render_html` is byte-identical to before.
    if let Some(html_path) = arg(args, "--html") {
        let html = match &runtime {
            Some(rs) => crate::render_html_with_stats(&sheet, &equity, &equity_ts, rs),
            None => crate::render_html(&sheet, &equity, &equity_ts),
        };
        if let Err(e) = std::fs::write(&html_path, html) {
            eprintln!("tearsheet: failed to write HTML tearsheet to {html_path:?}: {e}");
            return ExitCode::FAILURE;
        }
        eprintln!("tearsheet: wrote HTML tearsheet to {html_path}");
    }

    if has_flag(args, "--json") {
        match serde_json::to_string_pretty(&sheet) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                // NB a non-finite metric (the house `inf` profit_factor sentinel) does NOT land
                // here — serde_json writes those as `null`. Surface whatever did, don't panic.
                eprintln!(
                    "tearsheet: failed to serialize tearsheet as JSON ({e}). \
                     Try the default table output."
                );
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{sheet}");
    }

    ExitCode::SUCCESS
}

/// The tearsheet plus the equity series it was assembled from (`render_html` plots the series,
/// which the flat `LiveTearsheet` does not retain) plus the optional runtime-exposure summary
/// (`Some` only on the `--mtm` path; `render_html_with_stats` renders it).
type SheetParts = (LiveTearsheet, Vec<f64>, Vec<i64>, Option<crate::RuntimeStats>);

/// The realized-only path: exactly `LiveTearsheet::from_journal`'s four steps, inlined so the
/// equity curve/timestamps stay available for `--html`. Values are identical to `from_journal`.
fn build_realized(
    journal: &std::path::Path,
    seed: f64,
    ppy: f64,
) -> Result<SheetParts, crate::JournalReadError> {
    use crate::{equity_curve_from_trades, fills_from_journal, reconstruct_trades};

    let fills = fills_from_journal(journal)?;
    let trades = reconstruct_trades(&fills);
    let (equity, ts) = equity_curve_from_trades(seed, &trades);
    let sheet = LiveTearsheet::from_result_parts(None, trades, equity.clone(), ts.clone(), ppy);
    Ok((sheet, equity, ts, None))
}

/// The `--store` path, WITHOUT the `hist` feature: the concrete `DataFusionHist` backend isn't
/// compiled in, so there is nothing to open. Fail clearly (exit 2) rather than silently ignoring.
#[cfg(not(feature = "hist"))]
fn build_from_store(
    _journal: &std::path::Path,
    _store_dir: &str,
    _seed: f64,
    _ppy: f64,
    _interval: &str,
    _venue_flag: Option<String>,
    _mtm: bool,
) -> Result<SheetParts, ExitCode> {
    eprintln!("tearsheet: --store requires building with --features hist");
    Err(ExitCode::from(2))
}

/// The `--store` path, WITH the `hist` feature: open the DataFusion hist store, read the equity
/// curve from its `kind=equity` series (falling back to the realized-only curve if absent), and
/// back-fill each trade's `mae`/`mfe` from the store's OHLC bars. With `mtm`, the equity source is
/// instead the journal-fill mark-to-market reconstruction and a runtime-exposure summary is
/// produced for `--html`.
#[cfg(feature = "hist")]
fn build_from_store(
    journal: &std::path::Path,
    store_dir: &str,
    seed: f64,
    ppy: f64,
    interval: &str,
    venue_flag: Option<String>,
    mtm: bool,
) -> Result<SheetParts, ExitCode> {
    use crate::{
        RuntimeStats, backfill_excursions, equity_curve_from_store, equity_curve_from_trades,
        fills_from_journal, mtm_curve_from_store, mtm_equity_curve, reconstruct_trades,
    };
    use vike_data::{DataFusionHist, TsRange};

    // Raw fills first (Trade drops `venue`, so venue is derived from the fill stream — or --venue).
    let fills = match fills_from_journal(journal) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("tearsheet: failed to read journal at {journal:?}: {e}");
            return Err(ExitCode::FAILURE);
        }
    };

    // Venue: explicit --venue wins; otherwise the journal's single unambiguous venue.
    let venue = match venue_flag {
        Some(v) => v,
        None => {
            let mut venues: Vec<String> = fills.iter().map(|f| f.venue.to_string()).collect();
            venues.sort();
            venues.dedup();
            match venues.as_slice() {
                [only] => only.clone(),
                [] => {
                    eprintln!(
                        "tearsheet: --store needs a venue but the journal has no fills; pass --venue V"
                    );
                    return Err(ExitCode::from(2));
                }
                _ => {
                    eprintln!(
                        "tearsheet: journal spans multiple venues {venues:?}; pass --venue V to pick one"
                    );
                    return Err(ExitCode::from(2));
                }
            }
        }
    };

    let mut trades = reconstruct_trades(&fills);

    let store = match DataFusionHist::open(store_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tearsheet: failed to open hist store at {store_dir:?}: {e}");
            return Err(ExitCode::FAILURE);
        }
    };

    // (b) Excursion back-fill: fill mae/mfe from the store's bars (best-effort; empty/err → 0.0).
    backfill_excursions(&mut trades, &store, &venue, interval);

    // Equity source. Default = the vike-core sampler's persisted kind=equity series (primary).
    // --mtm = the journal-fill mark-to-market reconstruction (an alternative) plus a runtime
    // exposure summary; falls back to the realized-only curve if it produces nothing.
    let (equity, ts, runtime) = if mtm {
        match mtm_curve_from_store(&store, &fills, seed, &venue, interval) {
            Ok(points) if !points.is_empty() => {
                let rs = RuntimeStats::from_points(&points);
                let (eq, t) = mtm_equity_curve(&points);
                (eq, t, Some(rs))
            }
            Ok(_) => {
                eprintln!(
                    "tearsheet: --mtm produced no samples (no fills?); \
                     using the realized-only equity curve"
                );
                let (eq, t) = equity_curve_from_trades(seed, &trades);
                (eq, t, None)
            }
            Err(e) => {
                eprintln!(
                    "tearsheet: --mtm reconstruction failed ({e}); \
                     using the realized-only equity curve"
                );
                let (eq, t) = equity_curve_from_trades(seed, &trades);
                (eq, t, None)
            }
        }
    } else {
        // Equity curve from the store's kind=equity series. The vike-core equity sampler persists
        // the cross-venue ACCOUNT rollup under venue="portfolio", symbol="TOTAL" (per-exchange rows
        // use symbol=<exchange>) — that whole-account curve is what a tearsheet wants, NOT a
        // per-trade-venue lookup. Fall back to the realized-only curve (with a stderr note) when no
        // such series exists (sampler wasn't running / store has no equity rows).
        const EQUITY_VENUE: &str = "portfolio";
        const EQUITY_SYMBOL: &str = "TOTAL";
        let read = equity_curve_from_store(&store, EQUITY_VENUE, EQUITY_SYMBOL, TsRange::all());
        let (eq, t) = match read {
            Ok((eq, ts)) if !eq.is_empty() => (eq, ts),
            Ok(_) => {
                eprintln!(
                    "tearsheet: no kind=equity series in store ({EQUITY_VENUE}/{EQUITY_SYMBOL}); \
                     using the realized-only equity curve"
                );
                equity_curve_from_trades(seed, &trades)
            }
            Err(e) => {
                eprintln!(
                    "tearsheet: store equity read failed ({e}); \
                     using the realized-only equity curve"
                );
                equity_curve_from_trades(seed, &trades)
            }
        };
        (eq, t, None)
    };

    let sheet = LiveTearsheet::from_result_parts(None, trades, equity.clone(), ts.clone(), ppy);
    Ok((sheet, equity, ts, runtime))
}
