//! Market funding-RATE backfill CLI — fetches a venue's public perpetual funding-rate schedule and
//! ingests it as `kind=bar` (the rate on `Bar.funding`, OHLCV/volume `0.0`) into the vike-data
//! DataFusion hist store, so a funding-CAPTURE backtest's `funding_source` seam can replay the real
//! per-interval rate. Keyless (both venues' funding-rate endpoints are public). Idempotent per
//! `(venue, symbol, [start,end])` window.
//!
//!   cargo run -p vike-backfill --bin funding_rate_backfill -- \
//!       --venue binance --symbols BTCUSDT,ETHUSDT --start 2024-01-01
//!   cargo run -p vike-backfill --bin funding_rate_backfill -- \
//!       --venue hyperliquid --symbols BTC=BTC,ETH=ETH --start 1704067200000 --interval 1h \
//!       --store /market_data/hist
//!
//! `--venue` selects the source AND names the store partition (`binance` | `hyperliquid`).
//! `--symbols` is a comma list of `vikeSymbol=sourceSymbol` pairs (bare `SYM` ⇒ source == symbol;
//! e.g. binance `BTCUSDT`, hyperliquid `BTC`). `--start`/`--end` are each an epoch-ms integer OR a
//! `YYYY-MM-DD` UTC date; `--end` defaults to now. `--interval` is the series interval label,
//! defaulting to the venue's funding cadence (`8h` binance / `1h` hyperliquid). `--store` defaults to
//! `$VIKE_HIST_STORE` else `<repo>/market_data/hist`.

use vike_backfill::cli::{flag_value, log_config, parse_symbol_pairs, store_root, CliSpec};
use vike_backfill::funding_rate::{backfill_funding_rate, source_by_name, SOURCES};
use vike_data::DataFusionHist;
use vike_model::{epoch_ms_to_utc_date, now_ms, parse_date_label};

/// `Debug` so a parse that was supposed to FAIL can report what it produced instead
/// (`Result::expect_err` requires it) — the same reason `vike_backfill::cli::Parsed` derives it.
#[derive(Debug)]
struct Args {
    venue: String,
    symbols: Vec<(String, String)>, // (vikeSymbol, sourceSymbol)
    start_ms: i64,
    end_ms: i64,
    store: Option<String>,
    interval: Option<String>,
}

/// The process-argv entry point: [`parse_args_from`] over `std::env::args().skip(1)`. The SOURCE of
/// argv is the only thing this wrapper decides.
fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

/// Parse an already-`argv[0]`-stripped argument stream.
///
/// ⚠ `--end` DEFAULTS TO `now_ms()`, so a parse with no `--end` is not deterministic — a test that
/// wants an exact window must name both ends.
///
/// Every valued flag resolves through [`vike_backfill::cli::flag_value`], so a MENTIONED flag with
/// no value — or one handed another flag as its value — is an error here rather than only in
/// [`SPEC`]'s triage. An unmentioned optional flag still keeps its default. Same shape as
/// `eod_backfill`'s, argued there.
fn parse_args_from(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut venue = None;
    let mut symbols_spec = None;
    let mut start = None;
    let mut end = None;
    let mut store = None;
    let mut interval = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--venue" => venue = Some(flag_value("--venue", it.next())?),
            "--symbols" => symbols_spec = Some(flag_value("--symbols", it.next())?),
            "--start" => {
                let s = flag_value("--start", it.next())?;
                start = Some(parse_date_label(&s)?);
            }
            "--end" => {
                let s = flag_value("--end", it.next())?;
                end = Some(parse_date_label(&s)?);
            }
            "--store" => store = Some(flag_value("--store", it.next())?),
            "--interval" => interval = Some(flag_value("--interval", it.next())?),
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    let venue = venue
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or("--venue <binance|hyperliquid> is required")?;
    let spec = symbols_spec.ok_or("--symbols vikeSym=sourceSym,... is required")?;
    let symbols = parse_symbol_pairs(&spec)?;
    let start_ms = start.ok_or("--start <epoch-ms|YYYY-MM-DD> is required")?;
    let end_ms = end.unwrap_or_else(now_ms);
    if start_ms > end_ms {
        return Err(format!("--start ({start_ms}) is after --end ({end_ms})"));
    }
    Ok(Args { venue, symbols, start_ms, end_ms, store, interval })
}

const USAGE: &str = "\
usage: funding_rate_backfill --venue binance|hyperliquid --symbols SYM=src,...
                             --start <epoch-ms|YYYY-MM-DD> [--end <epoch-ms|YYYY-MM-DD>]
                             [--interval IV] [--store DIR]

Backfill a perpetual venue funding-rate history into the hist store. Idempotent per window.

  --venue V       binance | hyperliquid (required)
  --symbols SPEC  comma list of `vikeSymbol=sourceSymbol` pairs; a bare `SYM` means source == symbol
  --start T       window start, epoch milliseconds or YYYY-MM-DD (required)
  --end T         window end, same forms (default: now)
  --interval IV   funding interval override (default: the venue own)
  --store DIR     hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "funding_rate_backfill",
    usage: USAGE,
    valued: &["--venue", "--symbols", "--start", "--end", "--interval", "--store"],
    toggles: &[],
    positionals: 0,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    SPEC.short_circuit_or_exit(&args);
    let _log_guards = vike_log::init(log_config("funding-rate-backfill"));
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("funding_rate_backfill: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let Some(source) = source_by_name(&args.venue) else {
        eprintln!(
            "funding_rate_backfill: unknown --venue '{}' (have: {})",
            args.venue,
            SOURCES.join(", ")
        );
        std::process::exit(2);
    };
    let interval = args.interval.clone().unwrap_or_else(|| source.default_interval().to_string());
    let root = store_root(args.store.as_deref(), &std::env::vars().collect());
    let hist = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("funding_rate_backfill: open store {root:?}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(
        "funding_rate_backfill: venue={}, interval={interval}, [{}..{}] -> {root:?}",
        args.venue,
        epoch_ms_to_utc_date(args.start_ms),
        epoch_ms_to_utc_date(args.end_ms),
    );
    let mut total = 0usize;
    let mut premium_total = 0usize;
    let mut failed = 0usize;
    for (sym, source_sym) in &args.symbols {
        match backfill_funding_rate(
            &hist,
            source.as_ref(),
            sym,
            source_sym,
            &interval,
            args.start_ms,
            args.end_ms,
        ) {
            Ok(w) => {
                // Both halves are reported because they can legitimately disagree: a Binance
                // window ALWAYS lands 0 premium rows (that venue publishes none), and a store
                // whose funding bars predate the perp_metrics series lands rates it already had
                // as 0 while the premium half writes for the first time.
                tracing::info!(
                    "{sym} ({source_sym}): {} funding-rate points + {} premium rows ingested \
                     (0 = window already ingested / no data / venue sends no premium)",
                    w.rate_rows,
                    w.premium_rows,
                );
                total += w.rate_rows;
                premium_total += w.premium_rows;
            }
            Err(e) => {
                tracing::error!("{sym} ({source_sym}): backfill failed: {e} — skipped");
                failed += 1;
            }
        }
    }
    tracing::info!(
        "funding_rate_backfill done: {total} points + {premium_total} premium rows across {} \
         symbols ({failed} failed), venue={}, interval={interval}, store={root:?}",
        args.symbols.len(),
        args.venue,
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

    fn full(v: &[&str]) -> Vec<String> {
        std::iter::once("funding_rate_backfill")
            .chain(v.iter().copied())
            .map(str::to_string)
            .collect()
    }

    /// Every flag lands in its own field, and BOTH `--start`/`--end` spellings resolve to the same
    /// epoch-ms window — the mixed form is what an operator actually types.
    #[test]
    fn a_full_invocation_maps_every_flag_in_both_time_spellings() {
        let a = parse_args_from(args(&[
            "--venue",
            "binance",
            "--symbols",
            "BTCUSDT,ETH=ETHUSDT",
            "--start",
            "2024-01-01",
            "--end",
            "1706745600000",
            "--interval",
            "8h",
            "--store",
            "/d/hist",
        ]))
        .expect("a complete command line parses");
        assert_eq!(a.venue, "binance");
        assert_eq!(
            a.symbols,
            vec![
                ("BTCUSDT".to_string(), "BTCUSDT".to_string()),
                ("ETH".to_string(), "ETHUSDT".to_string()),
            ]
        );
        assert_eq!(a.start_ms, 1_704_067_200_000, "2024-01-01T00:00:00Z");
        assert_eq!(a.end_ms, 1_706_745_600_000);
        assert_eq!(a.interval.as_deref(), Some("8h"));
        assert_eq!(a.store.as_deref(), Some("/d/hist"));
        // …and the epoch-ms spelling of the SAME instant is accepted for --start too.
        let b = parse_args_from(args(&[
            "--venue",
            "binance",
            "--symbols",
            "BTCUSDT",
            "--start",
            "1704067200000",
            "--end",
            "1706745600000",
        ]))
        .expect("epoch-ms parses");
        assert_eq!(b.start_ms, a.start_ms);
    }

    /// The ORDER check — a window whose start is after its end is refused rather than paged
    /// backwards forever, and both bounds are echoed so the operator can see which way round it is.
    #[test]
    fn a_reversed_window_is_refused() {
        let e = parse_args_from(args(&[
            "--venue",
            "binance",
            "--symbols",
            "BTCUSDT",
            "--start",
            "2024-06-01",
            "--end",
            "2024-01-01",
        ]))
        .expect_err("start after end");
        assert!(e.contains("--start") && e.contains("--end"), "{e}");
    }

    /// The three required flags, and the one non-obvious rule: a BLANK `--venue` is treated as
    /// absent rather than resolving to a store partition named `""`.
    #[test]
    fn the_required_flags_are_required_and_a_blank_venue_counts_as_absent() {
        for (argv, want) in [
            (&["--symbols", "BTCUSDT", "--start", "0"][..], "--venue"),
            (&["--venue", "binance", "--start", "0"][..], "--symbols"),
            (&["--venue", "binance", "--symbols", "BTCUSDT"][..], "--start"),
            (&["--venue", "   ", "--symbols", "BTCUSDT", "--start", "0"][..], "--venue"),
        ] {
            let e = parse_args_from(args(argv)).expect_err("must be refused");
            assert!(e.contains(want), "expected {want} to be named: {e}");
        }
    }

    /// A malformed date is an ERROR, not a silent epoch-0 window that would page the venue's whole
    /// history — an out-of-range month is refused as firmly as a word is. Both time flags, plus the
    /// trailing-value shape of each.
    #[test]
    fn a_malformed_or_missing_date_is_an_error() {
        for argv in [
            &["--venue", "binance", "--symbols", "BTCUSDT", "--start", "yesterday"][..],
            &["--venue", "binance", "--symbols", "BTCUSDT", "--end", "2024-13-99"][..],
        ] {
            assert!(parse_args_from(args(argv)).is_err(), "{argv:?} must be refused");
        }
        for flag in ["--start", "--end"] {
            let e = parse_args_from(args(&["--venue", "binance", "--symbols", "BTCUSDT", flag]))
                .expect_err("a trailing time flag");
            assert!(e.contains(flag), "{e}");
        }
    }

    /// A typo'd flag is REJECTED, not ignored.
    #[test]
    fn an_unknown_flag_is_rejected_and_named() {
        let e = parse_args_from(args(&["--venue", "binance", "--sybmols", "BTCUSDT"]))
            .expect_err("a typo must not run");
        assert!(e.contains("--sybmols"), "{e}");
    }

    /// The `SPEC`/parser flag-set agreement, and the trailing-optional hole this bin shared with
    /// `eod_backfill` — now closed in the parser as well as in the SPEC. Both layers are asserted so
    /// neither can be removed on the assumption that the other covers it.
    #[test]
    fn the_spec_declares_exactly_what_the_parser_understands() {
        for flag in SPEC.valued.iter().copied() {
            if let Err(e) = parse_args_from(args(&[flag, "1"])) {
                assert!(!e.contains("unknown arg"), "{flag} declared but rejected: {e}");
            }
        }
        let line = &[
            "--venue",
            "binance",
            "--symbols",
            "BTCUSDT",
            "--start",
            "0",
            "--end",
            "1",
            "--store",
        ];
        let parser = parse_args_from(args(line))
            .expect_err("the parser now refuses a --store it was given no value for");
        assert!(parser.contains("--store"), "{parser}");
        let e = SPEC.triage(&full(line)).expect_err("and the SPEC still refuses it first");
        assert!(e.contains("--store"), "{e}");
        // …while an OMITTED --store is still just a default.
        let omitted =
            parse_args_from(args(&["--venue", "binance", "--symbols", "BTCUSDT", "--start", "0"]))
                .expect("an unmentioned optional flag is not a usage error");
        assert_eq!(omitted.store, None);
    }

    /// The swallow half: a valued flag may not eat a FLAG. `--symbols --start 0` used to parse the
    /// literal token `--start` as a symbol spec and then die on `0` as an unknown arg, so the
    /// diagnostic named the wrong token; the trailing shape parsed cleanly with a flag as a value.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        let e = parse_args_from(args(&["--venue", "binance", "--symbols", "--start", "0"]))
            .expect_err("a flag is not a symbol spec");
        assert!(e.contains("--symbols") && e.contains("--start"), "both tokens are named: {e}");
        let tail = parse_args_from(args(&[
            "--venue",
            "binance",
            "--symbols",
            "BTCUSDT",
            "--start",
            "0",
            "--store",
            "--interval",
        ]))
        .expect_err("…and the trailing shape, which used to parse cleanly");
        assert!(tail.contains("--store") && tail.contains("--interval"), "{tail}");
    }
}
