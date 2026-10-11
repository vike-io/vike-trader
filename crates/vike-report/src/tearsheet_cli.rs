//! `tearsheet` — the live-journal report renderer, as a LIBRARY function.
//!
//! ⚠ This was `src/bin/tearsheet.rs`'s body until the multicall merge. `main` became [`run`],
//! taking BOTH argv and the environment as parameters. The environment is the `--datahub` route's
//! (the settings directory and the datahub node keys); the journal directory is `--journal DIR`
//! and nothing else (decision 0111 retired `VIKE_JOURNAL_DIR`).
//!
//! ⚠ It sat behind the crate's `journal` feature, and so did the `tearsheet` BIN
//! (`required-features` in the manifest), until 2026-09-28: this tool's whole input is a journal
//! directory, so it could not exist in a build that linked no journal reader. That build no longer
//! exists — the renderer half moved to `vike-analytics` and the feature was deleted with it — so
//! both are unconditional, and `crates/vike/Cargo.toml`'s `tearsheet` row needs only this crate's
//! `hist` forward.
//!
//! Everything below is the binary's own documentation, unchanged.
//! writes when the active run profile's `[sinks.journal]` or `config.journal_dir` enables it — see
//! `vike_core::journal_config_from`), reconstructs closed
//! [`vike_analytics::reconstruct_trades`] round-trips through the shared `compute_fill`
//! cost-basis primitive, and computes the standard performance tearsheet via
//! `vike_analytics::metrics` — bit-for-bit identical to a backtest over the same fills (see the
//! `vike-report` crate doc).
//!
//! Usage:
//!   tearsheet --journal DIR [--seed CASH] [--name LABEL] [--periods-per-year N] [--json]
//!             [--html PATH] [--datahub] [--mtm] [--venue V] [--interval STR]
//!
//! `--journal DIR` (required) is the journal segment directory. `--seed`
//! is the account's starting capital (default 10000) — it sets the base of the realized-only
//! equity curve, so it scales `total_return`/`cagr`/`sharpe`; pass the session's real starting
//! equity for accurate returns. `--periods-per-year` is the Sharpe/Sortino/Calmar/CAGR
//! annualization factor (default 252); NOTE the equity curve here is realized-per-trade, not a
//! fixed calendar cadence, so annualized ratios are approximate. `--json` prints the
//! `LiveTearsheet` as pretty JSON instead of the human table. `--html PATH` ADDITIONALLY writes
//! the self-contained HTML tearsheet (`vike_analytics::render_html` — metrics table, inline-SVG
//! equity + underwater-drawdown charts, monthly-returns table when timestamps allow) to `PATH`;
//! the terminal output (table or `--json`) is unchanged.
//!
//! ## `--datahub` (HistStore enrichment — requires `--features hist`)
//!
//! `--datahub` reads the account's history from the data daemon and uses it for two enrichments that
//! replace the bare realized-only tearsheet. First, the equity curve is read from the store's
//! `kind=equity` series (`crate::equity_curve_from_store`) — a true mark-to-market curve. The vike-core
//! equity sampler persists the cross-venue ACCOUNT rollup under `venue="portfolio"`, `symbol="TOTAL"`
//! (per-exchange rows use `symbol=<exchange>`), so that whole-account curve is what the tearsheet
//! reads; it falls back to the realized-only curve (with a stderr note) when no such series exists.
//! Second, each reconstructed trade's `mae`/`mfe` is back-filled from the store's OHLC bars over the
//! trade window (`crate::backfill_excursions`).
//!
//! The address is the `config.datahub_addr` row of this box's settings database, or the compiled
//! default, a LOOPBACK address — the row the daemons read, so a box has one answer to "where is my
//! datahub". Node keys come from the credential STORE, and an unreadable store warns rather than
//! refusing; `vike_datahub_client::route`'s own doc argues both.
//!
//! `--interval STR` (default `1m`) is the bar series interval used for the excursion back-fill.
//! `--venue V` names the trades' venue for the excursion BAR lookups (the `Trade` type drops venue);
//! when omitted it is derived from the journal's fills if they all share one venue (else `--venue`
//! is required). The equity-curve lookup does NOT use it (that series is the `portfolio`/`TOTAL`
//! rollup, not per-venue). Built WITHOUT `--features hist`, `--datahub` errors (exit 2).
//!
//! ⚠ **`--store DIR` is REFUSED since 2026-09-25, in every spelling including a blank one.** It opened
//! a local DataFusion+Parquet root in place, and the owner closed that door for every history
//! READER: every read goes through a datahub. A local run is
//! `vike-backend datahub --store DIR` beside this tool — it needs no keys and binds
//! loopback only — and `vike_datahub_client::flag_vocab::store_flag_removed` is the sentence this
//! tool prints to say so.
//!
//! ⚠ **The wire is still not the DEFAULT here**, and that difference from every other reader 0084
//! routed survives the ruling: with no flag this tool computes the realized-only tearsheet from the
//! journal alone and touches no store at all, so "no flag" was already spoken for. Making the wire
//! silent would turn every journal-only invocation into a network read.
//!
//! ⚠ **And `hist` no longer links DataFusion — the reverse of what this paragraph used to say.** It
//! argued that `--store DIR` "remains the documented way back, so the local backend has to stay
//! compiled in". With `--store` gone nothing in this crate opens a concrete store: MEASURED
//! 2026-09-25, every `DataFusionHist` mention under `src/` is prose. So `hist` stops forwarding
//! `vike-data/hist-datafusion` and a tearsheet build no longer drags the Arrow/DataFusion tree.
//!
//! `--mtm` (requires `--datahub`) swaps the equity source: instead of the sampler's `kind=equity`
//! series, the curve is RECONSTRUCTED from the journal fills marked to market against the store's
//! `(--venue, --interval)` bars (`crate::mtm_curve_from_store`) — the sampler stays the
//! default. It additionally renders a "Runtime exposure" section (turnover / gross+net exposure /
//! peak margin, `vike_analytics::RuntimeStats`) into the `--html` output. Without `--datahub` it
//! is a no-op. On the default (no `--mtm`) path everything is byte-identical to before.

use std::path::PathBuf;
use std::process::ExitCode;

// The shared argv glue, from the same vike-analytics crate this bin already links for the metrics
// catalog. `binutil` is feature-free and vike-analytics is DataFusion-free, so this bin stays one.
use vike_analytics::binutil::{arg, has_flag, parse_num};
// The document and its default annualization, from the same crate — the tearsheet's pure core
// moved there on 2026-09-28, and this tool names the document at that crate's root like every
// consumer, and the annualization constant at `report::`, which is where its consumers name it.
use vike_analytics::{LiveTearsheet, report::DAILY_PERIODS_PER_YEAR};

pub fn run(env: &std::collections::HashMap<String, String>, args: &[String]) -> ExitCode {
    // --journal DIR: the directory the producing daemon's `config.journal_dir` row names.
    let Some(dir) = arg(args, "--journal").map(PathBuf::from) else {
        eprintln!(
            "tearsheet: --journal <dir> is required\n\
             usage: tearsheet --journal DIR [--seed CASH] [--name LABEL] [--periods-per-year N] \
             [--json] [--html PATH] [--datahub] [--venue V] [--interval STR]"
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

    // ⚠ `--store` in ANY spelling is REFUSED since 2026-09-25 — including a blank `--store=`, which
    // used to have a refusal of its own here. The owner closed the local READ door, so the value is
    // never read and there is no longer a blank value to be careful about. What stays is the
    // posture: somebody who passed it believes a directory will be read, so the answer names what
    // changed rather than ignoring the flag. The two sentences differ by build because the ADVICE
    // does: with `hist` the replacement is a key-less local datahub plus `--datahub`; without it,
    // this binary cannot read history at all.
    if has_flag(args, "--store") || arg(args, "--store").is_some() {
        #[cfg(feature = "hist")]
        eprintln!("{}", vike_datahub_client::flag_vocab::store_flag_removed("tearsheet"));
        #[cfg(not(feature = "hist"))]
        eprintln!(
            "tearsheet: `--store DIR` no longer reads history locally \
             (docs/decisions/0084-only-the-datahub-touches-the-store.md). History is read with \
             `--datahub`, and this binary was built without `--features hist`, so it has none"
        );
        return ExitCode::from(2);
    }
    let wire = has_flag(args, "--datahub");
    // Store-backed path if `--datahub` selects a history; otherwise the unchanged realized-only
    // path. Both return the sheet PLUS the equity curve/timestamps it was assembled from — the
    // flat sheet does not retain them, and the --html renderer plots them.
    //
    // ⚠ The route itself is resolved INSIDE `build_from_store` rather than here, and it has to be:
    // `HistoryRoute` arrives with `vike-datahub-client/hist-route`, which only the `hist` feature
    // turns on, so this function — which compiles in every configuration — cannot name it.
    let (mut sheet, equity, equity_ts, runtime) = if wire {
        let interval = arg(args, "--interval").unwrap_or_else(|| "1m".to_string());
        let venue_flag = arg(args, "--venue");
        let mtm = has_flag(args, "--mtm");
        let parts = build_from_store(&dir, env, seed, ppy, &interval, venue_flag, mtm);
        match parts {
            Ok(parts) => parts,
            Err(code) => return code,
        }
    } else {
        // The same four steps `crate::tearsheet_from_journal` performs — inlined so the curve
        // stays in hand for --html; values are identical.
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
            Some(rs) => vike_analytics::render_html_with_stats(&sheet, &equity, &equity_ts, rs),
            None => vike_analytics::render_html(&sheet, &equity, &equity_ts),
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
type SheetParts = (LiveTearsheet, Vec<f64>, Vec<i64>, Option<vike_analytics::RuntimeStats>);

/// The realized-only path: exactly `crate::tearsheet_from_journal`'s four steps, inlined so the
/// equity curve/timestamps stay available for `--html`. Values are identical to that function's.
fn build_realized(
    journal: &std::path::Path,
    seed: f64,
    ppy: f64,
) -> Result<SheetParts, crate::JournalReadError> {
    use crate::fills_from_journal;
    use vike_analytics::{equity_curve_from_trades, reconstruct_trades};

    let fills = fills_from_journal(journal)?;
    let trades = reconstruct_trades(&fills);
    let (equity, ts) = equity_curve_from_trades(seed, &trades);
    let sheet = LiveTearsheet::from_result_parts(None, trades, equity.clone(), ts.clone(), ppy);
    Ok((sheet, equity, ts, None))
}

/// The store-backed path, WITHOUT the `hist` feature: no `vike_datahub_client::route` is compiled
/// in, so there is no way to read the history. Fail clearly (exit 2) rather than silently ignoring.
// ⚠ `#[allow(clippy::too_many_arguments)]` stood on BOTH arms while the list was 8 long, `env`
// having pushed it past clippy's ceiling of 7. The local arm's `store_dir` was the eighth
// parameter and went with the door on 2026-09-25, so the list is back AT the ceiling and neither
// arm needs the allow.
#[cfg(not(feature = "hist"))]
fn build_from_store(
    _journal: &std::path::Path,
    _env: &std::collections::HashMap<String, String>,
    _seed: f64,
    _ppy: f64,
    _interval: &str,
    _venue_flag: Option<String>,
    _mtm: bool,
) -> Result<SheetParts, ExitCode> {
    eprintln!("tearsheet: --datahub requires building with --features hist");
    Err(ExitCode::from(2))
}

/// The store-backed path, WITH the `hist` feature: read the history from the data daemon, read the equity curve from its `kind=equity` series (falling back
/// to the realized-only curve if absent), and back-fill each trade's `mae`/`mfe` from the store's
/// OHLC bars. With `mtm`, the equity source is instead the journal-fill mark-to-market
/// reconstruction and a runtime-exposure summary is produced for `--html`.
///
/// ⚠ **On the wire arm every read below is a NETWORK read**, and this function performs one per
/// enrichment plus one per trade inside `backfill_excursions`. That is the cost
/// `docs/decisions/0084-only-the-datahub-touches-the-store.md` buys the single-reader property
/// with, stated the same way the `cheap_np` bins state it.
#[cfg(feature = "hist")]
fn build_from_store(
    journal: &std::path::Path,
    env: &std::collections::HashMap<String, String>,
    seed: f64,
    ppy: f64,
    interval: &str,
    venue_flag: Option<String>,
    mtm: bool,
) -> Result<SheetParts, ExitCode> {
    use crate::{
        backfill_excursions, equity_curve_from_store, fills_from_journal, mtm_curve_from_store,
    };
    use vike_analytics::{
        RuntimeStats, equity_curve_from_trades, mtm_equity_curve, reconstruct_trades,
    };
    use vike_data::TsRange;
    use vike_datahub_client::route::{datahub_addr_for_bin, history_route, open_routed_history};

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
                        "tearsheet: --datahub needs a venue but the journal has no fills; pass --venue V"
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

    // WHERE the history comes from: the datahub, the ONE answer since the local arm closed on
    // 2026-09-25. A caller who did not pass `--datahub` never reaches this function at all.
    let route = history_route(datahub_addr_for_bin(env).as_deref());
    // ONE disclosure: the ADDRESS, never a directory — the resolved root is the SERVER's, and a path
    // printed here would be a guess about another box's filesystem.
    eprintln!("tearsheet: history from {}", route.label());
    let store = open_routed_history(&route, env);
    // ONE coercion instead of three. The opener hands back a `Box<dyn HistStore + Send + Sync>`
    // — it has to, because the wire arm's handle crosses threads — while every reader below
    // takes the plain `&dyn HistStore`. Dropping the auto traits once, in ONE place,
    // is one cast rather than three coercions the compiler has to infer in argument position.
    //
    // ⚠ Spelled as an explicit `as`, which is the form this tree already compiles for exactly
    // this shape: `crates/vike-backtest/src/backtest_cli.rs` writes `h as &dyn HistStore` over the
    // same `Box<dyn HistStore + Send + Sync>`. An annotated binding alone would lean on a deref
    // coercion chained to an auto-trait-dropping unsize, which is a narrower thing to ask for.
    let store = &*store as &dyn vike_data::HistStore;

    // (b) Excursion back-fill: fill mae/mfe from the store's bars (best-effort; empty/err → 0.0).
    backfill_excursions(&mut trades, store, &venue, interval);

    // Equity source. Default = the vike-core sampler's persisted kind=equity series (primary).
    // --mtm = the journal-fill mark-to-market reconstruction (an alternative) plus a runtime
    // exposure summary; falls back to the realized-only curve if it produces nothing.
    let (equity, ts, runtime) = if mtm {
        match mtm_curve_from_store(store, &fills, seed, &venue, interval) {
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
        let read = equity_curve_from_store(store, EQUITY_VENUE, EQUITY_SYMBOL, TsRange::all());
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
