//! `tearsheet` — the live-journal report renderer, as a LIBRARY function.
//!
//! ⚠ This was `src/bin/tearsheet.rs`'s body until the multicall merge. `main` became [`run`],
//! taking BOTH argv and the environment as parameters. The environment is the `--datahub` route's
//! (the settings directory and the datahub hub keys); the journal directory is `--journal DIR`
//! and nothing else (decision 0111 retired `VIKE_JOURNAL_DIR`).
//!
//! ⚠ **It lived in `vike-report` until 2026-10-11 and moved UP into this package.** `vike-report`
//! was the journal READER library (layer 30) and this file was the only thing in it that needed
//! `vike-datahub-client`, behind a `hist` feature whose one job was to let the `tearsheet` bin
//! reach a store. A CLI is a composition root's business: it sits here, beside the dispatcher
//! that calls it, behind this package's `tearsheet` feature. The STORE reads moved with it, as
//! child modules: [`store`] (`equity_curve_from_store`, `mtm_curve_from_store`) and [`excursions`]
//! (`backfill_excursions`) name only the venue-agnostic `HistStore` trait, and this `--datahub`
//! path is their one caller. `vike-report` itself was dissolved on 2026-10-12: the journal READ
//! is `vike_journal::fills_from_journal`, and the fold from fills to the document is
//! `vike_analytics::tearsheet_from_fills` — the ONE spelling, which this file used to inline as
//! `build_realized` (and which the daemon's `Request::Tearsheet` arm and the desktop call too).
//!
//! Reads the command journal a daemon writes when the `config.journal_dir` row enables it (see
//! `vike_tradehub_core::journal_config_from`), reconstructs closed
//! [`vike_analytics::reconstruct_trades`] round-trips through the shared `compute_fill`
//! cost-basis primitive, and computes the standard performance tearsheet via
//! `vike_analytics::metrics` — bit-for-bit identical to a backtest over the same fills (see
//! `crates/vike-analytics/src/lib.rs`'s module doc).
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
//! ## `--datahub` (HistStore enrichment)
//!
//! `--datahub` reads the account's history from the data daemon and uses it for two enrichments that
//! replace the bare realized-only tearsheet. First, the equity curve is read from the store's
//! `kind=equity` series (`store::equity_curve_from_store`) — a true mark-to-market curve. The
//! vike-core equity sampler persists the cross-venue ACCOUNT rollup under `venue="portfolio"`,
//! `symbol="TOTAL"` (per-exchange rows use `symbol=<exchange>`), so that whole-account curve is
//! what the tearsheet reads; it falls back to the realized-only curve (with a stderr note) when no
//! such series exists, and ALSO when the journal records a ledger that is not the dollar's: `TOTAL`
//! is one currency's and states none (`store::default_equity_curve`, decision 0243). Second, each
//! reconstructed trade's `mae`/`mfe` is back-filled from the store's OHLC bars over the trade
//! window (`excursions::backfill_excursions`).
//!
//! The address is the `config.datahub_addr` row of this box's settings database, or the compiled
//! default, a LOOPBACK address — the row the daemons read, so a box has one answer to "where is my
//! datahub". Hub keys come from the credential STORE, and an unreadable store warns rather than
//! refusing; `vike_datahub_client::route`'s own doc argues both.
//!
//! `--interval STR` (default `1m`) is the bar series interval used for the excursion back-fill.
//! `--venue V` names the trades' venue for the excursion BAR lookups (the `Trade` type drops venue);
//! when omitted it is derived from the journal's fills if they all share one venue (else `--venue`
//! is required). The equity-curve lookup does NOT use it (that series is the `portfolio`/`TOTAL`
//! rollup, not per-venue).
//!
//! ⚠ **`--store DIR` is REFUSED since 2026-09-25, in every spelling including a blank one.** It opened
//! a local DataFusion+Parquet root in place, and the owner closed that door for every history
//! READER: every read goes through a datahub. A local run is
//! `vike-backend datahub --store DIR` beside this tool — it needs no keys and binds
//! loopback only — and `vike_model::run_request::flag_vocab::store_flag_removed` is the sentence this
//! tool prints to say so.
//!
//! ⚠ **The wire is still not the DEFAULT here**, and that difference from every other reader 0084
//! routed survives the ruling: with no flag this tool computes the realized-only tearsheet from the
//! journal alone and touches no store at all, so "no flag" was already spoken for. Making the wire
//! silent would turn every journal-only invocation into a network read.
//!
//! ⚠ **And nothing here links DataFusion.** Nothing in this file opens a concrete store (MEASURED
//! 2026-09-25: every `DataFusionHist` mention was prose), so this package's `tearsheet` feature
//! pulls no `vike-store`; the shipped multicall links it anyway, through
//! `backtest`, whose `data` verbs WRITE — the ruling was about readers.
//!
//! `--mtm` (requires `--datahub`) swaps the equity source: instead of the sampler's `kind=equity`
//! series, the curve is RECONSTRUCTED from the journal fills marked to market against the store's
//! `(--venue, --interval)` bars (`store::mtm_curve_from_store`) — the sampler stays the
//! default. It additionally renders a "Runtime exposure" section (turnover / gross+net exposure /
//! peak margin, `vike_analytics::RuntimeStats`) into the `--html` output. Without `--datahub` it
//! is a no-op. On the default (no `--mtm`) path everything is byte-identical to before.

pub mod excursions;
pub mod store;

use std::path::PathBuf;
use std::process::ExitCode;

// The document and its default annualization, from the same crate — the tearsheet's pure core
// moved there on 2026-09-28, and this tool names the document at that crate's root like every
// consumer, and the annualization constant at `report::`, which is where its consumers name it.
use vike_analytics::{
    FillOrder, LiveTearsheet, report::DAILY_PERIODS_PER_YEAR, tearsheet_from_fills,
};
// The journal READ: the fills of the directory `--journal` names, in journal order, with the ledgers
// its snapshots record that are not the dollar's.
use vike_journal::fills_and_ledgers_from_journal;

/// Render the performance tearsheet of a live journal (a daemon's `config.journal_dir`).
///
/// With `--datahub` the account's history is read from the data daemon for two enrichments (a
/// true mark-to-market equity curve and each trade's excursions); without it the tool computes the
/// realized-only tearsheet from the journal alone and touches no store.
#[derive(clap::Parser, Debug)]
#[command(name = "tearsheet")]
struct Args {
    /// The journal segment directory the producing daemon's `config.journal_dir` row names.
    #[arg(long, value_name = "DIR")]
    journal: PathBuf,
    /// The account's starting capital (default 10000). It sets the base of the realized-only
    /// equity curve, so it scales `total_return`/`cagr`/`sharpe`.
    #[arg(long, value_name = "CASH")]
    seed: Option<f64>,
    /// A label for the tearsheet's header.
    #[arg(long, value_name = "LABEL")]
    name: Option<String>,
    /// The Sharpe/Sortino/Calmar/CAGR annualization factor (default 252). The curve here is
    /// realized-per-trade, not a fixed calendar cadence, so annualized ratios are approximate.
    #[arg(long, value_name = "N")]
    periods_per_year: Option<f64>,
    /// Print the `LiveTearsheet` as pretty JSON instead of the human table.
    #[arg(long)]
    json: bool,
    /// ADDITIONALLY write the self-contained HTML tearsheet to this path.
    #[arg(long, value_name = "PATH")]
    html: Option<String>,
    /// Read the history from the data daemon (the config.datahub_addr row): the equity curve from
    /// its `kind=equity` series and each trade's mae/mfe from its bars.
    #[arg(long)]
    datahub: bool,
    /// With --datahub: reconstruct the equity curve from the journal fills marked to market
    /// against the store's bars, and render a runtime-exposure section into --html.
    #[arg(long)]
    mtm: bool,
    /// With --datahub: the trades' venue for the excursion bar lookups (default: the journal's
    /// only venue; required when the fills span several).
    #[arg(long, value_name = "V")]
    venue: Option<String>,
    /// With --datahub: the bar series interval for the excursion back-fill (default `1m`).
    #[arg(long, value_name = "STR")]
    interval: Option<String>,
    /// REFUSED since 2026-09-25: every history read goes through a datahub (decision 0084).
    #[arg(long, num_args = 0..=1, hide = true)]
    store: Option<Option<String>>,
}

/// `args` is the command line WITHOUT the program name: the multicall's tail, or the standalone
/// bin's `argv[1..]`.
pub fn run(env: &std::collections::HashMap<String, String>, args: &[String]) -> ExitCode {
    let version = vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let parsed: Args =
        match vike_util::cli::Program::new("tearsheet", &version).try_parse_from(args) {
            Ok(parsed) => parsed,
            Err(exit) => exit.emit(),
        };
    // --journal DIR: the directory the producing daemon's `config.journal_dir` row names.
    let dir = parsed.journal.clone();

    // `--seed` / `--periods-per-year`: a malformed number is `clap`'s refusal now; it used to be
    // this function's `invalid {flag} value` line.
    let seed = parsed.seed.unwrap_or(10_000.0);
    let ppy = parsed.periods_per_year.unwrap_or(DAILY_PERIODS_PER_YEAR);

    // ⚠ `--store` in ANY spelling is REFUSED since 2026-09-25 — including a bare one. The owner
    // closed the local READ door, so the value is
    // never read and there is no longer a blank value to be careful about. What stays is the
    // posture: somebody who passed it believes a directory will be read, so the answer names what
    // changed rather than ignoring the flag: a key-less local datahub plus `--datahub`.
    if parsed.store.is_some() {
        eprintln!("{}", vike_model::run_request::flag_vocab::store_flag_removed("tearsheet"));
        return ExitCode::from(2);
    }
    let wire = parsed.datahub;
    // Store-backed path if `--datahub` selects a history; otherwise the unchanged realized-only
    // path. Both return the sheet PLUS the equity curve/timestamps it was assembled from — the
    // flat sheet does not retain them, and the --html renderer plots them.
    //
    // The route itself is resolved INSIDE `build_from_store`: it is only needed on this arm, and
    // a journal-only invocation must not resolve an address or read a key.
    // The journal's fills, read ONCE: the fills of a venue whose engine books off the dollar are left out (a
    // sheet adds one currency, decision 0183 item 13) and the sheet says what it left out; a symbol the name
    // says is quoted in another fiat currency refuses the sheet.
    let JournalRead { fills, ledger_label, off_dollar_ledgers } = match journal_fills(&dir) {
        Ok(read) => read,
        Err(code) => return code,
    };
    let (mut sheet, equity, equity_ts, runtime) = if wire {
        let interval = parsed.interval.clone().unwrap_or_else(|| "1m".to_string());
        let venue_flag = parsed.venue.clone();
        let mtm = parsed.mtm;
        let parts = build_from_store(
            fills,
            env,
            seed,
            ppy,
            &interval,
            venue_flag,
            mtm,
            off_dollar_ledgers.as_deref(),
        );
        match parts {
            Ok(parts) => parts,
            Err(code) => return code,
        }
    } else {
        // The journal's fills folded as held (`FillOrder::AsHeld`: journal order, never sorted).
        // `tearsheet_from_fills` hands the curve back with the sheet, which --html plots.
        let (sheet, equity, ts) = tearsheet_from_fills(fills, FillOrder::AsHeld, seed, ppy);
        (sheet, equity, ts, None)
    };
    sheet.name = match (parsed.name.clone(), ledger_label) {
        (Some(name), Some(label)) => Some(format!("{name} ({label})")),
        (name, label) => name.or(label),
    };

    // --html PATH: additionally write the self-contained HTML tearsheet (terminal output stays).
    // The runtime-stats section renders only on the --mtm path (`runtime` is Some); otherwise the
    // default `render_html` is byte-identical to before.
    if let Some(html_path) = parsed.html.clone() {
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

    if parsed.json {
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

/// What [`journal_fills`] hands the sheet.
struct JournalRead {
    fills: Vec<vike_model::events::FillEvent>,
    /// What the sheet left out (`None` when nothing was).
    ledger_label: Option<String>,
    /// Every ledger the journal records off the dollar, named (`None` on a journal of one currency).
    off_dollar_ledgers: Option<String>,
}

/// The fills of the journal in `dir`, in journal order, WITHOUT the fills of a venue whose engine books in a
/// currency that is not the dollar (the journal's snapshots say which: `fills_and_ledgers_from_journal`), and
/// the label that says what was left out (`None` when nothing was; the same words go to stderr). Refuses, via
/// [`refuse_foreign_quoted_fills`], what is left when it still holds a symbol quoted in another fiat currency.
///
/// `off_dollar_ledgers` of the answer names EVERY ledger the journal's snapshots record off the dollar (`"fxcm EUR"`),
/// fills or no fills (`None` on a journal of one currency): the stored equity series is the whole node's, not a
/// venue's ([`store::default_equity_curve`]).
fn journal_fills(dir: &std::path::Path) -> Result<JournalRead, ExitCode> {
    let (fills, ledgers) = match fills_and_ledgers_from_journal(dir) {
        Ok(read) => read,
        Err(e) => {
            eprintln!("tearsheet: failed to read journal at {dir:?}: {e}");
            return Err(ExitCode::FAILURE);
        }
    };
    let off_dollar = (!ledgers.is_empty()).then(|| {
        let named: Vec<String> =
            ledgers.iter().map(|(venue, ccy)| format!("{venue} {ccy}")).collect();
        named.join(", ")
    });
    let (fills, left_out) = vike_analytics::without_non_dollar_ledgers(fills, &ledgers);
    let label = (!left_out.is_empty()).then(|| vike_analytics::tearsheet_ledger_label(&left_out));
    if let Some(label) = &label {
        eprintln!("tearsheet: {label}");
    }
    refuse_foreign_quoted_fills(&fills)?;
    Ok(JournalRead { fills, ledger_label: label, off_dollar_ledgers: off_dollar })
}

/// Refuse fills on a symbol whose name says it is quoted in a fiat currency other than the dollar: the
/// fold reads every fill as one currency and holds no conversion rate, so it would print yen as
/// dollars (decision 0183, item 13). Says why on stderr and hands back the failing exit code; nothing
/// is printed on stdout.
fn refuse_foreign_quoted_fills(fills: &[vike_model::events::FillEvent]) -> Result<(), ExitCode> {
    match vike_catalog::foreign_fiat_quoted_list(fills.iter().map(|f| f.symbol.as_str())) {
        None => Ok(()),
        Some(list) => {
            eprintln!("{}", vike_analytics::tearsheet_foreign_quote_refusal(&list));
            Err(ExitCode::FAILURE)
        }
    }
}

/// The tearsheet plus the equity series it was assembled from (`render_html` plots the series,
/// which the flat `LiveTearsheet` does not retain) plus the optional runtime-exposure summary
/// (`Some` only on the `--mtm` path; `render_html_with_stats` renders it).
type SheetParts = (LiveTearsheet, Vec<f64>, Vec<i64>, Option<vike_analytics::RuntimeStats>);

/// The store-backed path: read the history from the data daemon, read the equity curve from its
/// `kind=equity` series (falling back to the realized-only curve if absent), and back-fill each
/// trade's `mae`/`mfe` from the store's OHLC bars. With `mtm`, the equity source is instead the
/// journal-fill mark-to-market reconstruction and a runtime-exposure summary is produced for
/// `--html`.
///
/// ⚠ **Every read below is a NETWORK read**, and this function performs one per enrichment plus one
/// per trade inside `backfill_excursions`. That is the cost
/// `docs/decisions/0084-only-the-datahub-touches-the-store.md` buys the single-reader property
/// with, stated the same way the `cheap_np` bins state it.
///
/// `off_dollar_ledgers` is [`JournalRead::off_dollar_ledgers`]: when it is `Some`, the stored `TOTAL` series is
/// not plotted ([`store::default_equity_curve`]).
fn build_from_store(
    fills: Vec<vike_model::events::FillEvent>,
    env: &std::collections::HashMap<String, String>,
    seed: f64,
    ppy: f64,
    interval: &str,
    venue_flag: Option<String>,
    mtm: bool,
    off_dollar_ledgers: Option<&str>,
) -> Result<SheetParts, ExitCode> {
    use vike_analytics::{
        RuntimeStats, equity_curve_from_trades, mtm_equity_curve, reconstruct_trades,
    };
    use vike_datahub_client::route::{
        datahub_addr_for_bin, history_route, open_routed_history, routed_hub_keys,
    };

    use self::excursions::backfill_excursions;
    use self::store::{default_equity_curve, mtm_curve_from_store};

    // Raw fills first, as `journal_fills` read them (Trade drops `venue`, so venue is derived from the fill
    // stream — or --venue).

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
    // The keys are read here, under this tool's warn-and-continue disposition (`routed_hub_keys`);
    // a malformed `VIKE_NO_LIVE_KEYS` refuses the start, as in every other root.
    let credentials = match vike_secrets::open_credential_store(env) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tearsheet: {e}");
            return Err(ExitCode::FAILURE);
        }
    };
    let keys = routed_hub_keys(
        credentials.hub_keys(vike_venues::credential_keys::is_datahub_pair_key),
        route.hub(),
    );
    let store = open_routed_history(&route, keys);
    // ONE coercion instead of three. The opener hands back a `Box<dyn HistStore + Send + Sync>`
    // — it has to, because the wire arm's handle crosses threads — while every reader below
    // takes the plain `&dyn HistStore`. Dropping the auto traits once, in ONE place,
    // is one cast rather than three coercions the compiler has to infer in argument position.
    //
    // ⚠ Spelled as an explicit `as`, which is the form this tree already compiles for exactly
    // this shape: `crates/vike-backtesthub/src/backtest_cli.rs` writes `h as &dyn HistStore` over the
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
        let (eq, t) = default_equity_curve(store, &trades, seed, off_dollar_ledgers);
        (eq, t, None)
    };

    let sheet = LiveTearsheet::from_result_parts(None, trades, equity.clone(), ts.clone(), ppy);
    Ok((sheet, equity, ts, runtime))
}
