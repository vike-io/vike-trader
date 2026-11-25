//! `vike-cli data splits` — THE OPERATOR'S SHARE SPLITS, one row per split of one stored series (decision 0289).
//!
//! ```text
//! vike-cli data splits add --venue dukascopy --symbol AAPL.CFD --date 2020-08-31 --ratio 4
//! vike-cli data splits rm --venue dukascopy --symbol AAPL.CFD --date 2020-08-31
//! vike-cli data splits rename --venue dukascopy --ticker FB --to META
//! vike-cli data splits ls [--venue V]
//! vike-cli data splits warnings [--venue V] [--addr HOST:PORT]
//! ```
//!
//! A backtest reading a series from the venue's own candles divides every price before a split by its ratio and
//! multiplies the volume by it, as TradingView shows a share's history; the report names each split it applied. The
//! stored bars are never rewritten, and removing the row undoes it. A split is DATA: nothing is compiled in, and no
//! venue the bridges talk to reports one. `vike-cli data catalog refresh` fills the rows of every stored share series
//! from Yahoo (the owner's ruling of 2026-11-16, decision 0289's amendment); `ls` prints each row's source
//! (`operator`, `yahoo` or `yahoo+fmp`: FMP confirmed it), an operator's row of the same date wins over a vendor's, and
//! `add` takes a vendor row over.
//! A Yahoo row `rm` removes comes back at the next refresh that reads it. `rename` records that a venue's ticker is
//! listed under another one now (`FB` is `META`), so the refresh asks Yahoo and FMP for that one; renaming a ticker
//! to itself renames nothing (`vike_secrets::stock_split::write_ticker_rename`).
//!
//! `--symbol` is the SERIES as the store names it, as `vike-cli data history ls` prints it (`AAPL.CFD`, the instrument's
//! user name: decision 0290), not the venue's instrument name (`AAPL.US/USD`): the series is what is adjusted. The row is written to the settings database this
//! CLI booted from (`vike_secrets::stock_split`), so run it on the box whose datahub serves the backtests; that datahub
//! reads the rows at each ask, so the next backtest sees a change with no restart. Exit: 0 when the act was done, 1
//! (`failed`) when it was not, 2 (`usage`) for a row that is no split.
//!
//! `warnings` is the one verb that asks the DATAHUB rather than the settings database: its open split warnings
//! (`Request::SplitWarnings`, a read), each a stored share series' overnight jump no split row covers, printed in the
//! one sentence the desktop's Overview shows (`vike_datahub_client::catalog::SplitWarning::message`). A datahub older
//! than the read is refused with nothing sent (exit 4, `refused`); an unreachable one is exit 3 (`connect`).

use std::path::Path;
use std::process::ExitCode;

use vike_model::StockSplit;
use vike_model::time::parse_ymd;
use vike_secrets::stock_split::{
    delete_stock_split, read_sourced_stock_splits, read_ticker_renames, write_stock_split,
    write_ticker_rename,
};

use vike_datahub_client::catalog::SplitWarning;
use vike_datahub_client::proto::FEATURE_SPLIT_WARNINGS;
use vike_hub_protocol::auth::{HubKeys, Scope};

use super::{connect, require_feature};
use crate::exit::{CliError, CommandResult};

/// The program name this group is read under.
const BIN: &str = "vike-cli data splits";

const PREAMBLE: &str = "\
THE OPERATOR'S SHARE SPLITS: one row per split of one stored series, in this box's settings
database. A backtest reading the series from the venue's own candles divides every price before
the split by its ratio and multiplies the volume by it (as TradingView shows a share), and its
report says `split-adjusted: SYMBOL DATE ×RATIO`. The stored bars are never rewritten. --symbol
is the series as `vike-cli data history ls` prints it (AAPL.CFD), not the venue's instrument name.";

// The five verbs. (`//` rather than `///`: `clap` would print a doc comment on this enum as help.)
#[derive(clap::Subcommand, Debug)]
#[command(long_about = PREAMBLE)]
pub(super) enum SplitsVerb {
    /// Add a split of one stored series, replacing the ratio of a row of the same date
    Add(SplitsAddFlags),
    /// Remove a split
    Rm(SplitsSeriesFlags),
    /// Have the refresh ask Yahoo and FMP for a venue's renamed ticker under its new one (FB is META)
    Rename(SplitsRenameFlags),
    /// Every split entered
    Ls(SplitsLsFlags),
    /// The datahub's open split warnings: a stored share series' overnight jump no split row covers
    Warnings(SplitsWarningsFlags),
}

#[derive(clap::Args, Debug)]
pub(super) struct SplitsSeriesFlags {
    #[arg(long, value_name = "V", help = "the venue, as its roster slug (dukascopy)")]
    venue: String,
    #[arg(
        long,
        value_name = "SYMBOL",
        help = "the series as the store names it, as `data history ls` prints it (AAPL.CFD)"
    )]
    symbol: String,
    #[arg(
        long,
        value_name = "YYYY-MM-DD",
        help = "the first day the series trades at the new price (from 00:00 UTC)"
    )]
    date: String,
}

#[derive(clap::Args, Debug)]
pub(super) struct SplitsAddFlags {
    #[command(flatten)]
    series: SplitsSeriesFlags,
    #[arg(
        long,
        value_name = "N",
        help = "new shares per old share: 4 for a 4-for-1 split, 0.1 for a 1-for-10 reverse split"
    )]
    ratio: f64,
}

#[derive(clap::Args, Debug)]
pub(super) struct SplitsRenameFlags {
    #[arg(long, value_name = "V", help = "the venue, as its roster slug (dukascopy)")]
    venue: String,
    #[arg(
        long,
        value_name = "TICKER",
        help = "the ticker the refresh maps the venue's row to (FB for FB.US/USD)"
    )]
    ticker: String,
    #[arg(
        long,
        value_name = "TICKER",
        help = "the ticker Yahoo and FMP list it under now (META); the ticker itself renames nothing"
    )]
    to: String,
}

#[derive(clap::Args, Debug)]
pub(super) struct SplitsLsFlags {
    #[arg(long, value_name = "V", help = "only this venue's splits")]
    venue: Option<String>,
}

#[derive(clap::Args, Debug)]
pub(super) struct SplitsWarningsFlags {
    #[arg(long, value_name = "V", help = "only this venue's warnings")]
    venue: Option<String>,
    #[arg(
        long,
        value_name = "HOST:PORT",
        help = "the datahub to ask (default: config.datahub_addr)"
    )]
    addr: Option<String>,
}

/// The split `add` writes, judged (`vike_model::StockSplit::new`).
fn split_of(flags: &SplitsAddFlags) -> Result<StockSplit, String> {
    let s = &flags.series;
    StockSplit::new(&s.venue, &s.symbol, &s.date, flags.ratio)
}

/// `rm`'s date, written as a row holds it (`2020-8-31` is `2020-08-31`).
fn canonical_date(date: &str) -> Result<String, String> {
    parse_ymd(date).map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"))
}

/// Run a `data splits …` line against the settings database in `settings_dir` (`warnings`: against the datahub).
pub(super) fn run(
    verb: SplitsVerb,
    settings_dir: Option<&Path>,
    keys: Option<&HubKeys>,
    configured_address: Option<&str>,
) -> ExitCode {
    match execute(verb, settings_dir, keys, configured_address) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{BIN}: {}", e.message);
            e.exit.into()
        }
    }
}

/// `warnings`: the datahub's open split warnings, in the desktop's words.
fn warnings(
    flags: &SplitsWarningsFlags,
    keys: Option<&HubKeys>,
    configured_address: Option<&str>,
) -> CommandResult<Vec<String>> {
    let addr =
        vike_client::resolved::datahub_dial_address(flags.addr.as_deref(), configured_address);
    let mut client = connect(&addr, keys, Scope::Read)?;
    require_feature(&client, FEATURE_SPLIT_WARNINGS, "report its split warnings")?;
    let warnings = client.split_warnings().map_err(CliError::failed)?;
    Ok(warning_lines(&warnings, flags.venue.as_deref()))
}

/// One line per open warning of `venue` (every venue when `None`), each [`SplitWarning::message`]; none says so.
fn warning_lines(warnings: &[SplitWarning], venue: Option<&str>) -> Vec<String> {
    let lines: Vec<String> = warnings
        .iter()
        .filter(|w| venue.is_none_or(|v| w.venue == v))
        .map(SplitWarning::message)
        .collect();
    if lines.is_empty() { vec!["no open split warnings".to_string()] } else { lines }
}

fn execute(
    verb: SplitsVerb,
    settings_dir: Option<&Path>,
    keys: Option<&HubKeys>,
    configured_address: Option<&str>,
) -> CommandResult<Vec<String>> {
    // Resolved by the arms that need a store: `warnings` reads the datahub, not this box's rows.
    let dir = || {
        settings_dir.ok_or_else(|| {
            CliError::failed(
                "no settings directory resolved, so there is no store to hold a split. Set VIKE_SETTINGS_DIR, \
                 or run from a project that has one — `vike-cli secrets path` prints where this box looks",
            )
        })
    };
    let store_error = |e: vike_secrets::StoreError| CliError::failed(e.to_string());
    let mut lines = Vec::new();
    let venue = match verb {
        SplitsVerb::Warnings(flags) => return warnings(&flags, keys, configured_address),
        SplitsVerb::Add(flags) => {
            let split = split_of(&flags).map_err(CliError::usage)?;
            write_stock_split(dir()?, &split).map_err(store_error)?;
            lines.push(format!(
                "ok      {} {} {} ×{}: a backtest of this series' own candles divides every price before {} by {}",
                split.venue, split.symbol, split.date, split.ratio, split.date, split.ratio
            ));
            Some(split.venue)
        }
        SplitsVerb::Rm(SplitsSeriesFlags { venue, symbol, date }) => {
            let date = canonical_date(&date).map_err(CliError::usage)?;
            if !delete_stock_split(dir()?, &venue, &symbol, &date).map_err(store_error)? {
                return Err(CliError::failed(format!(
                    "no split of {symbol} on {venue} dated {date}: nothing was removed (`{BIN} ls`)"
                )));
            }
            lines.push(format!("removed {venue} {symbol} {date}"));
            Some(venue)
        }
        SplitsVerb::Rename(SplitsRenameFlags { venue, ticker, to }) => {
            write_ticker_rename(dir()?, &venue, &ticker, &to).map_err(store_error)?;
            lines.push(format!(
                "ok      {venue} {ticker}: the next `vike-cli data catalog refresh` asks Yahoo and FMP for {to}"
            ));
            Some(venue)
        }
        SplitsVerb::Ls(SplitsLsFlags { venue }) => venue,
    };
    let dir = dir()?;
    let rows = read_sourced_stock_splits(dir).map_err(store_error)?;
    let rows: Vec<&(StockSplit, String)> =
        rows.iter().filter(|(r, _)| venue.as_deref().is_none_or(|v| r.venue == v)).collect();
    lines.push(format!("{} split(s) in {}:", rows.len(), vike_secrets::db_path_in(dir).display()));
    for (r, source) in rows {
        lines.push(format!(
            "  {:<10} {:<16} {} ×{:<8} {source}",
            r.venue, r.symbol, r.date, r.ratio
        ));
    }
    let renames = read_ticker_renames(dir).map_err(store_error)?;
    let renames: Vec<_> =
        renames.iter().filter(|(v, ..)| venue.as_deref().is_none_or(|w| v == w)).collect();
    if !renames.is_empty() {
        lines.push(format!("{} ticker rename(s):", renames.len()));
        lines.extend(renames.iter().map(|(v, ticker, to)| format!("  {v:<10} {ticker} as {to}")));
    }
    Ok(lines)
}

/// PARSE ONLY, for `crate::cmd::data::accepts`: an `add` that is no split and an `rm` with no calendar date are refused.
#[cfg(test)]
pub(super) fn judge(verb: &SplitsVerb) -> Result<(), String> {
    match verb {
        SplitsVerb::Add(flags) => split_of(flags).map(drop),
        SplitsVerb::Rm(flags) => canonical_date(&flags.date).map(drop),
        SplitsVerb::Rename(_) | SplitsVerb::Ls(_) | SplitsVerb::Warnings(_) => Ok(()),
    }
}

#[cfg(test)]
#[path = "splits_tests.rs"]
mod splits_tests;

/// Read a `data splits` argv tail — the verb word first — through the real grammar.
#[cfg(test)]
fn parse(argv: &[&str]) -> Result<SplitsVerb, crate::cmd::args::Stop> {
    #[derive(clap::Parser, Debug)]
    struct TestCli {
        #[command(subcommand)]
        verb: SplitsVerb,
    }
    let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
    let cli: TestCli = crate::cmd::args::parse(BIN, &argv)?;
    Ok(cli.verb)
}
