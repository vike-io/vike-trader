//! EOD (daily-OHLCV) index/stock backfill CLI — pluggable multi-source (yahoo today; more slot in
//! behind the `EodSource` trait). Fetches daily bars for configurable symbols and ingests them as
//! `kind=bar`/interval `1d` into the `vike-store` DataFusion hist store. Idempotent per run-day.
//!
//!   cargo run -p vike-backfill --bin eod_backfill -- \
//!       --source yahoo --symbols SPX=^GSPC,VIX=^VIX,NDX=^NDX,DJI=^DJI --years 10
//!   cargo run -p vike-backfill --bin eod_backfill -- \
//!       --source yahoo --symbols SPX=^GSPC --years 5 --venue index
//!
//! `--symbols` is a comma list of `vikeSymbol=sourceTicker` pairs (bare `SYM` ⇒ ticker == symbol).
//! `--venue` defaults to the source name (provenance in the partition). `--store` defaults to
//! `<repo>/market_data/hist` in a checkout, else the project's own.

use vike_backfill::cli::{log_config, parse, parse_symbol_pairs, store_root};
use vike_backfill::eod::source_by_name;
use vike_data::HistStore;
use vike_store::DataFusionHist;

/// Backfill daily OHLCV index/stock history into the hist store as kind=bar, interval=1d.
/// Idempotent per run-day.
#[derive(clap::Parser, Debug)]
struct Args {
    /// The EOD provider (currently: yahoo). Stooq was dropped — it now fronts its CSV endpoint with
    /// a JS proof-of-work wall a plain client cannot pass.
    #[arg(long)]
    source: String,
    /// Comma list of `vikeSymbol=sourceTicker` pairs; a bare `SYM` means source == symbol (e.g.
    /// SPX=^GSPC,VIX=^VIX,AAPL)
    #[arg(long)]
    symbols: String,
    /// How many years back to request
    #[arg(long, default_value_t = 10)]
    years: u32,
    /// Venue label to store under (default: the source name)
    #[arg(long)]
    venue: Option<String>,
    /// Hist-store root (default: <repo>/market_data/hist, else the project's own)
    #[arg(long)]
    store: Option<String>,
}

fn main() {
    // ⚠ FIRST — `--help`, `--version` and a usage error answer before any log file or store.
    let args: Args = parse("eod_backfill");
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let _log_guards = vike_log::init(log_config("eod-backfill", &env));
    // The pairs are judged AFTER the parse: `--symbols ,` is well-formed argv that names no symbol.
    let symbols = match parse_symbol_pairs(&args.symbols) {
        Ok(symbols) => symbols,
        Err(e) => {
            eprintln!("eod_backfill: {e}");
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
    let root = store_root(args.store.as_deref(), &env);
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
    for (sym, ticker) in &symbols {
        match source.fetch(ticker, args.years) {
            Ok(bars) if !bars.is_empty() => {
                let key = format!("{}:{sym}:{}y:{today}", source.name(), args.years);
                match hist.append_bars(&venue, sym, "1d", &bars, Some(&key)) {
                    Ok(n) => {
                        let (lo, hi) = (bars.first().unwrap().ts, bars.last().unwrap().ts);
                        tracing::info!(
                            "{sym} ({ticker}): {n} bars ingested [{}..{}] venue={venue}",
                            vike_model::time::epoch_ms_to_utc_date(lo),
                            vike_model::time::epoch_ms_to_utc_date(hi)
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
        symbols.len()
    );
    if failed == symbols.len() {
        std::process::exit(1); // every symbol failed → non-zero
    }
}

/// `YYYY-MM-DD` of the current UTC day — for the idempotency commit key (re-run same day = no-op).
/// Uses the shared [`vike_model::time::epoch_ms_to_utc_date`] (matches the store's `date=` partition key).
fn crate_today() -> String {
    vike_model::time::epoch_ms_to_utc_date(vike_model::now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_backfill::cli::try_parse_from;
    use vike_util::cli::CliExit;

    fn parsed(v: &[&str]) -> Result<Args, CliExit> {
        try_parse_from("eod_backfill", v.iter().copied())
    }

    fn refused(v: &[&str]) -> CliExit {
        parsed(v).expect_err("this command line must be refused")
    }

    /// Every flag lands in its own field — the parse nobody had ever executed.
    #[test]
    fn a_full_invocation_maps_every_flag() {
        let a = parsed(&[
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
        ])
        .expect("a complete command line parses");
        assert_eq!(a.source, "yahoo");
        assert_eq!(
            parse_symbol_pairs(&a.symbols).unwrap(),
            vec![("SPX".to_string(), "^GSPC".to_string()), ("VIX".to_string(), "VIX".to_string())]
        );
        assert_eq!(a.years, 3);
        assert_eq!(a.venue.as_deref(), Some("index"));
        assert_eq!(a.store.as_deref(), Some("/d/hist"));
    }

    /// The `--flag=value` spelling reads the same (decision 0155 reversed the old refusal).
    #[test]
    fn the_inline_spelling_maps_every_flag_too() {
        let a = parsed(&["--source=yahoo", "--symbols=SPX", "--years=3", "--store=/d/hist"])
            .expect("the inline spelling parses");
        assert_eq!((a.source.as_str(), a.symbols.as_str(), a.years), ("yahoo", "SPX", 3));
        assert_eq!(a.store.as_deref(), Some("/d/hist"));
    }

    /// The NUMERIC flag, both failure shapes. A silently-defaulted `--years` would page ten years of
    /// history when the operator asked for one, so neither may fall through to the default.
    #[test]
    fn a_bad_years_is_an_error_rather_than_the_default() {
        let unparseable = refused(&["--source", "yahoo", "--symbols", "SPX", "--years", "ten"]);
        assert_eq!(unparseable.code, 2);
        assert!(unparseable.text.contains("--years"), "names the flag: {}", unparseable.text);
        let missing = refused(&["--source", "yahoo", "--symbols", "SPX", "--years"]);
        assert!(missing.text.contains("--years"), "{}", missing.text);
        let negative = refused(&["--source", "yahoo", "--symbols", "SPX", "--years", "-5"]);
        assert!(negative.text.contains("--years"), "{}", negative.text);
        // …and the default is what an ABSENT flag gets, which is the property the errors protect.
        let defaulted = parsed(&["--source", "yahoo", "--symbols", "SPX"])
            .expect("the two required flags alone parse");
        assert_eq!(defaulted.years, 10);
    }

    /// Both required flags, each named in its own message.
    #[test]
    fn the_two_required_flags_are_required() {
        let no_source = refused(&["--symbols", "SPX"]);
        assert_eq!(no_source.code, 2);
        assert!(no_source.text.contains("--source"), "{}", no_source.text);
        let no_symbols = refused(&["--source", "yahoo"]);
        assert_eq!(no_symbols.code, 2);
        assert!(no_symbols.text.contains("--symbols"), "{}", no_symbols.text);
    }

    /// A symbol list that names nothing is well-formed argv; the program refuses it after the parse.
    #[test]
    fn a_symbol_list_naming_no_symbol_is_refused_after_the_parse() {
        let a = parsed(&["--source", "yahoo", "--symbols", ","]).expect("argv is well-formed");
        assert!(parse_symbol_pairs(&a.symbols).is_err());
    }

    /// A typo'd flag is REJECTED, not ignored.
    #[test]
    fn an_unknown_flag_is_rejected_and_named() {
        let e = refused(&["--source", "yahoo", "--symbols", "SPX", "--stroe", "/d"]);
        assert_eq!(e.code, 2);
        assert!(e.text.contains("--stroe"), "{}", e.text);
    }

    /// **A trailing OPTIONAL valued flag is refused.** It was once silently dropped by the parser —
    /// the backfill wrote to the DEFAULT hist store while the operator believed they had named one.
    #[test]
    fn a_trailing_optional_flag_is_refused() {
        let e = refused(&["--source", "yahoo", "--symbols", "SPX", "--store"]);
        assert!(e.text.contains("--store"), "{}", e.text);
        // …while an OMITTED --store is still just a default, not an error.
        let omitted = parsed(&["--source", "yahoo", "--symbols", "SPX"])
            .expect("an unmentioned optional flag is not a usage error");
        assert_eq!(omitted.store, None);
    }

    /// The swallow half of the same rule: a valued flag may not eat a FLAG. `--venue --store /d`
    /// used to set the store partition label to `--store`; `--store --venue` at the tail parsed
    /// CLEANLY with the operator's `--venue` gone.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        let e = refused(&["--source", "yahoo", "--symbols", "SPX", "--venue", "--store", "/d"]);
        assert_eq!(e.code, 2);
        assert!(e.text.contains("--venue"), "{}", e.text);
        let tail = refused(&["--source", "yahoo", "--symbols", "SPX", "--store", "--venue"]);
        assert!(tail.text.contains("--store"), "{}", tail.text);
    }
}
