//! Hyperliquid historical candle backfill CLI. Pages HL's keyless `candleSnapshot` `/info` endpoint
//! (mainnet, public) for a symbol set + interval over `[--start, --end]` and ingests each series as
//! `kind=bar`/`venue=hyperliquid` into the vike-data DataFusion hist store, idempotent per window.
//! No credentials — history reads are keyless.
//!
//!   cargo run -p vike-backfill --bin hyperliquid_backfill -- \
//!       --symbols BTC=BTC,HYPE=HYPE --interval 1h --start 2024-01-01
//!   cargo run -p vike-backfill --bin hyperliquid_backfill -- \
//!       --symbols ETH --interval 1m --start 1704067200000 --store /market_data/hist
//!
//! `--symbols` is a comma list of `vikeSymbol=coin` (bare `SYM` ⇒ coin == symbol; HL perps use the
//! coin AS the symbol, e.g. `BTC`/`HYPE`). `--interval` ∈ {1d,1h,5m,1m}. `--start`/`--end` are each
//! an epoch-ms integer OR a `YYYY-MM-DD` UTC date; `--end` defaults to now. `--store` defaults to
//! `$VIKE_HIST_STORE` else `<repo>/market_data/hist`.

use vike_backfill::cli::{flag_value, log_config, parse_symbol_pairs, store_root, CliSpec};
use vike_backfill::hyperliquid::{backfill_hyperliquid_klines, VENUE};
use vike_data::DataFusionHist;
use vike_model::{epoch_ms_to_utc_date, now_ms, parse_date_label};

/// Intervals this CLI accepts — the HL `candleSnapshot`/WS `candle` interval codes (passed verbatim
/// to the endpoint; HL uses the standard `1m`/`1h`/`1d` strings, no venue-specific bar code).
const INTERVALS: [&str; 4] = ["1d", "1h", "5m", "1m"];

/// `Debug` so a parse that was supposed to FAIL can report what it produced instead
/// (`Result::expect_err` requires it) — the same reason `vike_backfill::cli::Parsed` derives it.
#[derive(Debug)]
struct Args {
    symbols: Vec<(String, String)>, // (vikeSymbol, HL coin)
    interval: String,
    start_ms: i64,
    end_ms: i64,
    store: Option<String>,
}

/// The process-argv entry point: [`parse_args_from`] over `std::env::args().skip(1)`. The SOURCE of
/// argv is the only thing this wrapper decides.
fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

/// Parse an already-`argv[0]`-stripped argument stream.
///
/// ⚠ `--end` DEFAULTS TO `now_ms()`, so a parse with no `--end` is not deterministic.
///
/// Every valued flag resolves through [`vike_backfill::cli::flag_value`], so a MENTIONED flag with
/// no value — or one handed another flag as its value — is an error here rather than only in
/// [`SPEC`]'s triage. An unmentioned optional flag still keeps its default.
fn parse_args_from(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut symbols_spec = None;
    let mut interval = "1h".to_string();
    let mut start = None;
    let mut end = None;
    let mut store = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--symbols" => symbols_spec = Some(flag_value("--symbols", it.next())?),
            "--interval" => interval = flag_value("--interval", it.next())?,
            "--start" => {
                let s = flag_value("--start", it.next())?;
                start = Some(parse_date_label(&s)?);
            }
            "--end" => {
                let s = flag_value("--end", it.next())?;
                end = Some(parse_date_label(&s)?);
            }
            "--store" => store = Some(flag_value("--store", it.next())?),
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    let spec = symbols_spec.ok_or("--symbols SYM=coin,... is required")?;
    let symbols = parse_symbol_pairs(&spec)?;
    if !INTERVALS.contains(&interval.as_str()) {
        return Err(format!("unsupported --interval {interval} (have {})", INTERVALS.join(",")));
    }
    let start_ms = start.ok_or("--start <epoch-ms|YYYY-MM-DD> is required")?;
    let end_ms = end.unwrap_or_else(now_ms);
    if start_ms > end_ms {
        return Err(format!("--start ({start_ms}) is after --end ({end_ms})"));
    }
    Ok(Args { symbols, interval, start_ms, end_ms, store })
}

const USAGE: &str = "\
usage: hyperliquid_backfill --symbols SYM=coin,... --start <epoch-ms|YYYY-MM-DD>
                            [--end <epoch-ms|YYYY-MM-DD>] [--interval IV] [--store DIR]

Page the keyless Hyperliquid candleSnapshot history into the hist store as kind=bar. Idempotent
per paging window; mainnet public reads only, no credentials.

  --symbols SPEC  comma list of `vikeSymbol=coin` pairs; a bare `SYM` means coin == symbol
  --start T       window start, epoch milliseconds or YYYY-MM-DD (required)
  --end T         window end, same forms (default: now)
  --interval IV   bar interval (default 1h)
  --store DIR     hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "hyperliquid_backfill",
    usage: USAGE,
    valued: &["--symbols", "--start", "--end", "--interval", "--store"],
    toggles: &[],
    positionals: 0,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    SPEC.short_circuit_or_exit(&args);
    let _log_guards = vike_log::init(log_config("hyperliquid-backfill"));
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("hyperliquid_backfill: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let root = store_root(args.store.as_deref(), &std::env::vars().collect());
    let hist = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("hyperliquid_backfill: open store {root:?}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(
        "hyperliquid_backfill: {} symbols, interval={}, [{}..{}] -> {root:?}",
        args.symbols.len(),
        args.interval,
        epoch_ms_to_utc_date(args.start_ms),
        epoch_ms_to_utc_date(args.end_ms),
    );
    let mut total = 0usize;
    let mut failed = 0usize;
    for (sym, coin) in &args.symbols {
        match backfill_hyperliquid_klines(
            &hist,
            sym,
            coin,
            &args.interval,
            args.start_ms,
            args.end_ms,
        ) {
            Ok(n) => {
                tracing::info!(
                    "{sym} ({coin}) {}: {n} bars ingested (0 = window already ingested), venue={VENUE}",
                    args.interval
                );
                total += n;
            }
            Err(e) => {
                tracing::error!("{sym} ({coin}): backfill failed: {e}");
                failed += 1;
            }
        }
    }
    tracing::info!(
        "hyperliquid_backfill done: {total} bars across {} symbols ({failed} failed), venue={VENUE}, store={root:?}",
        args.symbols.len()
    );
    if failed == args.symbols.len() {
        std::process::exit(1); // every symbol failed → non-zero
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> std::vec::IntoIter<String> {
        v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>().into_iter()
    }

    /// Every flag lands in its own field.
    #[test]
    fn a_full_invocation_maps_every_flag() {
        let a = parse_args_from(args(&[
            "--symbols",
            "BTC=BTC,HYPE",
            "--interval",
            "5m",
            "--start",
            "2024-01-01",
            "--end",
            "1706745600000",
            "--store",
            "/d/hist",
        ]))
        .expect("a complete command line parses");
        assert_eq!(
            a.symbols,
            vec![("BTC".to_string(), "BTC".to_string()), ("HYPE".to_string(), "HYPE".to_string()),]
        );
        assert_eq!(a.interval, "5m");
        assert_eq!(a.start_ms, 1_704_067_200_000);
        assert_eq!(a.end_ms, 1_706_745_600_000);
        assert_eq!(a.store.as_deref(), Some("/d/hist"));
    }

    /// The interval ALLOW-LIST, in both directions. Every accepted code is accepted (so the list and
    /// the check cannot drift apart), and one that is not on it is refused by name rather than sent
    /// verbatim to the venue — HL would answer an unknown code with an empty page, which reads
    /// downstream as "no history" rather than as a bad flag.
    #[test]
    fn the_interval_allow_list_is_enforced_in_both_directions() {
        for iv in INTERVALS {
            let a = parse_args_from(args(&[
                "--symbols",
                "BTC",
                "--start",
                "0",
                "--end",
                "1",
                "--interval",
                iv,
            ]))
            .unwrap_or_else(|e| panic!("{iv} is on INTERVALS but was refused: {e}"));
            assert_eq!(a.interval, iv);
        }
        let e = parse_args_from(args(&["--symbols", "BTC", "--start", "0", "--interval", "4h"]))
            .expect_err("4h is not an HL candle code");
        assert!(e.contains("4h"), "the error names the offending value: {e}");
        // …and the default when the flag is absent.
        assert_eq!(
            parse_args_from(args(&["--symbols", "BTC", "--start", "0", "--end", "1"]))
                .expect("defaults")
                .interval,
            "1h"
        );
    }

    /// The two required flags, plus the reversed-window refusal.
    #[test]
    fn required_flags_and_a_reversed_window_are_refused() {
        let no_symbols = parse_args_from(args(&["--start", "0"])).expect_err("--symbols required");
        assert!(no_symbols.contains("--symbols"), "{no_symbols}");
        let no_start = parse_args_from(args(&["--symbols", "BTC"])).expect_err("--start required");
        assert!(no_start.contains("--start"), "{no_start}");
        let reversed = parse_args_from(args(&[
            "--symbols",
            "BTC",
            "--start",
            "2024-06-01",
            "--end",
            "2024-01-01",
        ]))
        .expect_err("start after end");
        assert!(reversed.contains("--start") && reversed.contains("--end"), "{reversed}");
    }

    /// A typo'd flag is REJECTED, not ignored; a malformed date is an error rather than an epoch-0
    /// window that would page the venue's whole history.
    #[test]
    fn unknown_flags_and_malformed_dates_are_errors() {
        let typo = parse_args_from(args(&["--symbols", "BTC", "--stroe", "/d"]))
            .expect_err("a typo must not run");
        assert!(typo.contains("--stroe"), "{typo}");
        assert!(parse_args_from(args(&["--symbols", "BTC", "--start", "last week"])).is_err());
        let trailing =
            parse_args_from(args(&["--symbols", "BTC", "--start"])).expect_err("no value");
        assert!(trailing.contains("--start"), "{trailing}");
    }

    /// The `SPEC`/parser flag-set agreement, and the trailing-optional hole — now closed in the
    /// parser as well as in the SPEC. Both layers are asserted so neither can be removed on the
    /// assumption that the other covers it.
    #[test]
    fn the_spec_declares_exactly_what_the_parser_understands() {
        for flag in SPEC.valued.iter().copied() {
            if let Err(e) = parse_args_from(args(&[flag, "1"])) {
                assert!(!e.contains("unknown arg"), "{flag} declared but rejected: {e}");
            }
        }
        let parser =
            parse_args_from(args(&["--symbols", "BTC", "--start", "0", "--end", "1", "--store"]))
                .expect_err("the parser now refuses a --store it was given no value for");
        assert!(parser.contains("--store"), "{parser}");
        let full: Vec<String> =
            ["hyperliquid_backfill", "--symbols", "BTC", "--start", "0", "--end", "1", "--store"]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
        let e = SPEC.triage(&full).expect_err("and the SPEC still refuses it first");
        assert!(e.contains("--store"), "{e}");
        // …while an OMITTED --store is still just a default.
        let omitted = parse_args_from(args(&["--symbols", "BTC", "--start", "0", "--end", "1"]))
            .expect("an unmentioned optional flag is not a usage error");
        assert_eq!(omitted.store, None);
    }

    /// The swallow half: a valued flag may not eat a FLAG. `--symbols --interval 1h` used to parse
    /// the literal token `--interval` as a symbol spec and then die on `1h` as an unknown arg, so
    /// the diagnostic named the wrong token; the trailing shape parsed cleanly.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        let e = parse_args_from(args(&["--symbols", "--interval", "1h"]))
            .expect_err("a flag is not a symbol spec");
        assert!(e.contains("--symbols") && e.contains("--interval"), "both tokens are named: {e}");
        let tail = parse_args_from(args(&["--symbols", "BTC", "--start", "0", "--store", "--end"]))
            .expect_err("…and the trailing shape, which used to parse cleanly");
        assert!(tail.contains("--store") && tail.contains("--end"), "{tail}");
    }
}
