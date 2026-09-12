//! EOD (daily-OHLCV) index/stock backfill CLI — pluggable multi-source (yahoo today; more slot in
//! behind the `EodSource` trait). Fetches daily bars for configurable symbols and ingests them as
//! `kind=bar`/interval `1d` into the `vike-data` DataFusion hist store. Idempotent per run-day.
//!
//!   cargo run -p vike-backfill --bin eod_backfill -- \
//!       --source yahoo --symbols SPX=^GSPC,VIX=^VIX,NDX=^NDX,DJI=^DJI --years 10
//!   cargo run -p vike-backfill --bin eod_backfill -- \
//!       --source yahoo --symbols SPX=^GSPC --years 5 --venue index
//!
//! `--symbols` is a comma list of `vikeSymbol=sourceTicker` pairs (bare `SYM` ⇒ ticker == symbol).
//! `--venue` defaults to the source name (provenance in the partition). `--store` defaults to
//! `$VIKE_HIST_STORE` else `<repo>/market_data/hist`.

use vike_backfill::cli::{CliSpec, flag_value, log_config, parse_symbol_pairs, store_root};
use vike_backfill::eod::{now_ms, source_by_name};
use vike_data::{DataFusionHist, HistStore};

/// `Debug` so a parse that was supposed to FAIL can report what it produced instead
/// (`Result::expect_err` requires it) — the same reason `vike_backfill::cli::Parsed` derives it.
#[derive(Debug)]
struct Args {
    source: String,
    symbols: Vec<(String, String)>, // (vikeSymbol, sourceTicker)
    years: u32,
    venue: Option<String>,
    store: Option<String>,
}

/// The process-argv entry point: [`parse_args_from`] over `std::env::args().skip(1)`. The SOURCE of
/// argv is the only thing this wrapper decides — every rule lives in the function below, so the
/// parser is drivable from a test without a process.
fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

/// Parse an already-`argv[0]`-stripped argument stream.
///
/// Every valued flag resolves through [`vike_backfill::cli::flag_value`], so a flag that was
/// MENTIONED and not fed is an error HERE — a missing value, or a value that is itself a flag. It
/// used to be neither: `it.next()` yielded `None`, the option kept its default, and the only thing
/// refusing that command line was [`SPEC`]'s [`vike_backfill::cli::CliSpec::triage`], one function
/// away in `main`. A flag that was never mentioned still keeps its default, which is what leaves
/// `--venue`/`--store` genuinely optional.
fn parse_args_from(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut source = None;
    let mut symbols_spec = None;
    let mut years = 10u32;
    let mut venue = None;
    let mut store = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--source" => source = Some(flag_value("--source", it.next())?),
            "--symbols" => symbols_spec = Some(flag_value("--symbols", it.next())?),
            "--years" => {
                years = flag_value("--years", it.next())?
                    .parse()
                    .map_err(|_| "--years must be an integer")?
            }
            "--venue" => venue = Some(flag_value("--venue", it.next())?),
            "--store" => store = Some(flag_value("--store", it.next())?),
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    let source = source.ok_or("--source <yahoo> is required")?;
    let spec = symbols_spec.ok_or("--symbols SYM=ticker,... is required")?;
    let symbols = parse_symbol_pairs(&spec)?;
    Ok(Args { source, symbols, years, venue, store })
}

const USAGE: &str = "\
usage: eod_backfill --source yahoo --symbols SYM=ticker,... [--years N] [--venue V] [--store DIR]

Backfill daily OHLCV index/stock history into the hist store as kind=bar, interval=1d. Idempotent
per run-day.

  --source S     the EOD provider (currently: yahoo). Stooq was dropped — it now fronts its CSV
                 endpoint with a JS proof-of-work wall a plain client cannot pass.
  --symbols SPEC comma list of `vikeSymbol=sourceTicker` pairs; a bare `SYM` means source == symbol
                 (e.g. SPX=^GSPC,VIX=^VIX,AAPL)
  --years N      how many years back to request (default 10)
  --venue V      venue label to store under (default: the source name)
  --store DIR    hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  -h, --help     print this and exit 0
  -V, --version  print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "eod_backfill",
    usage: USAGE,
    valued: &["--source", "--symbols", "--years", "--venue", "--store"],
    toggles: &[],
    positionals: 0,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    SPEC.short_circuit_or_exit(&args);
    let _log_guards = vike_log::init(log_config("eod-backfill"));
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("eod_backfill: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let Some(source) = source_by_name(&args.source) else {
        eprintln!(
            "eod_backfill: unknown --source '{}' (have: {})",
            args.source,
            vike_backfill::eod::SOURCES.join(", ")
        );
        std::process::exit(2);
    };
    let venue = args.venue.clone().unwrap_or_else(|| source.name().to_string());
    let root = store_root(args.store.as_deref(), &std::env::vars().collect());
    let hist = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("eod_backfill: open store {root:?}: {e}");
            std::process::exit(1);
        }
    };
    let today = crate_today();
    let mut total = 0usize;
    let mut failed = 0usize;
    for (sym, ticker) in &args.symbols {
        match source.fetch(ticker, args.years) {
            Ok(bars) if !bars.is_empty() => {
                let key = format!("{}:{sym}:{}y:{today}", source.name(), args.years);
                match hist.append_bars(&venue, sym, "1d", &bars, Some(&key)) {
                    Ok(n) => {
                        let (lo, hi) = (bars.first().unwrap().ts, bars.last().unwrap().ts);
                        tracing::info!(
                            "{sym} ({ticker}): {n} bars ingested [{}..{}] venue={venue}",
                            vike_model::epoch_ms_to_utc_date(lo),
                            vike_model::epoch_ms_to_utc_date(hi)
                        );
                        total += n;
                    }
                    Err(e) => {
                        tracing::error!("{sym}: append failed: {e}");
                        failed += 1;
                    }
                }
            }
            Ok(_) => {
                tracing::warn!("{sym} ({ticker}): source returned no bars — skipped");
                failed += 1;
            }
            Err(e) => {
                tracing::error!("{sym} ({ticker}): fetch failed: {e} — skipped");
                failed += 1;
            }
        }
    }
    tracing::info!(
        "eod_backfill done: {total} bars across {} symbols ({failed} failed), venue={venue}, store={root:?}",
        args.symbols.len()
    );
    if failed == args.symbols.len() {
        std::process::exit(1); // every symbol failed → non-zero
    }
}

/// `YYYY-MM-DD` of the current UTC day — for the idempotency commit key (re-run same day = no-op).
/// Uses the shared [`vike_model::epoch_ms_to_utc_date`] (matches the store's `date=` partition key).
fn crate_today() -> String {
    vike_model::epoch_ms_to_utc_date(now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An argv stream as `parse_args_from` takes it — already `argv[0]`-stripped.
    fn args(v: &[&str]) -> std::vec::IntoIter<String> {
        v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>().into_iter()
    }

    /// The FULL argv (with `argv[0]`) that `main` hands [`SPEC`].
    fn full(v: &[&str]) -> Vec<String> {
        std::iter::once("eod_backfill").chain(v.iter().copied()).map(str::to_string).collect()
    }

    /// Every flag lands in its own field — the parse nobody had ever executed.
    #[test]
    fn a_full_invocation_maps_every_flag() {
        let a = parse_args_from(args(&[
            "--source",
            "yahoo",
            "--symbols",
            "SPX=^GSPC,VIX",
            "--years",
            "3",
            "--venue",
            "index",
            "--store",
            "/d/hist",
        ]))
        .expect("a complete command line parses");
        assert_eq!(a.source, "yahoo");
        assert_eq!(
            a.symbols,
            vec![("SPX".to_string(), "^GSPC".to_string()), ("VIX".to_string(), "VIX".to_string()),]
        );
        assert_eq!(a.years, 3);
        assert_eq!(a.venue.as_deref(), Some("index"));
        assert_eq!(a.store.as_deref(), Some("/d/hist"));
    }

    /// The NUMERIC flag, both failure shapes. A silently-defaulted `--years` would page ten years of
    /// history when the operator asked for one, so neither may fall through to the default.
    #[test]
    fn a_bad_years_is_an_error_rather_than_the_default() {
        let unparseable =
            parse_args_from(args(&["--source", "yahoo", "--symbols", "SPX", "--years", "ten"]))
                .expect_err("`ten` is not an integer");
        assert!(unparseable.contains("--years"), "the error names the flag: {unparseable}");
        let missing = parse_args_from(args(&["--source", "yahoo", "--symbols", "SPX", "--years"]))
            .expect_err("a trailing --years has no value");
        assert!(missing.contains("--years"), "{missing}");
        // …and the default is what an ABSENT flag gets, which is the property the two errors protect.
        let defaulted = parse_args_from(args(&["--source", "yahoo", "--symbols", "SPX"]))
            .expect("the two required flags alone parse");
        assert_eq!(defaulted.years, 10);
    }

    /// Both required flags, each named in its own message.
    #[test]
    fn the_two_required_flags_are_required() {
        let no_source =
            parse_args_from(args(&["--symbols", "SPX"])).expect_err("--source is required");
        assert!(no_source.contains("--source"), "{no_source}");
        let no_symbols =
            parse_args_from(args(&["--source", "yahoo"])).expect_err("--symbols is required");
        assert!(no_symbols.contains("--symbols"), "{no_symbols}");
    }

    /// A typo'd flag is REJECTED, not ignored — this parser's `other =>` arm.
    #[test]
    fn an_unknown_flag_is_rejected_and_named() {
        let e = parse_args_from(args(&["--source", "yahoo", "--symbols", "SPX", "--stroe", "/d"]))
            .expect_err("a typo must not run");
        assert!(e.contains("--stroe"), "{e}");
    }

    /// **The FINDING this test used to pin, now FIXED at both layers.** A trailing OPTIONAL valued
    /// flag was silently dropped by the parser — `it.next()` yielded `None`, `store` kept its
    /// default, and the backfill wrote to the DEFAULT hist store while the operator believed they
    /// had named one. Only [`SPEC`]'s triage refused it, one function away in `main`, so the
    /// SHIPPED binary was safe and the parser alone was not.
    ///
    /// Both layers are still asserted, and that is the point: the parser is now safe when read
    /// alone, AND the triage still refuses first, so neither can be removed on the assumption that
    /// the other covers it.
    #[test]
    fn a_trailing_optional_flag_is_refused_by_the_parser_and_by_the_spec() {
        let e = parse_args_from(args(&["--source", "yahoo", "--symbols", "SPX", "--store"]))
            .expect_err("the parser refuses a --store it was given no value for");
        assert!(e.contains("--store"), "{e}");
        let spec = SPEC
            .triage(&full(&["--source", "yahoo", "--symbols", "SPX", "--store"]))
            .expect_err("and the SPEC still refuses it first");
        assert!(spec.contains("--store"), "{spec}");
        // …while an OMITTED --store is still just a default, not an error.
        let omitted = parse_args_from(args(&["--source", "yahoo", "--symbols", "SPX"]))
            .expect("an unmentioned optional flag is not a usage error");
        assert_eq!(omitted.store, None);
    }

    /// The swallow half of the same rule: a valued flag may not eat a FLAG. `--venue --store /d`
    /// used to set the store partition label to `--store` and then die on `/d` as an unknown arg,
    /// so the diagnostic named the wrong token; `--store --venue` at the tail parsed CLEANLY with
    /// the operator's `--venue` gone.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        let e = parse_args_from(args(&[
            "--source",
            "yahoo",
            "--symbols",
            "SPX",
            "--venue",
            "--store",
            "/d",
        ]))
        .expect_err("a flag is not a venue label");
        assert!(e.contains("--venue") && e.contains("--store"), "both tokens are named: {e}");
        let tail =
            parse_args_from(args(&["--source", "yahoo", "--symbols", "SPX", "--store", "--venue"]))
                .expect_err("…and the trailing shape, which used to parse cleanly");
        assert!(tail.contains("--store"), "{tail}");
    }

    /// The two flag SETS must agree. `SPEC.triage` runs first and rejects anything it does not
    /// declare, so a flag this parser understands but `SPEC` does not is UNREACHABLE in the shipped
    /// binary — and a flag `SPEC` declares but the parser does not dies one line later with
    /// `unknown arg`. Neither direction produces a compile error, so it is checked here.
    #[test]
    fn every_spec_declared_flag_is_understood_by_the_parser() {
        for flag in SPEC.valued.iter().copied() {
            if let Err(e) = parse_args_from(args(&[flag, "1"])) {
                assert!(
                    !e.contains("unknown arg"),
                    "{flag} is declared by SPEC but the parser rejects it: {e}"
                );
            }
        }
        assert!(SPEC.toggles.is_empty(), "this bin declares no toggles");
    }
}
