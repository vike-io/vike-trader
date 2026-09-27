//! IBKR historical-bar backfill CLI. Pages `vike_ibkr::HistoricalFetcher` (bounded below by
//! `head_timestamp`, or an explicit `--start`) for a symbol set + interval and ingests each window
//! as `kind=bar`/`venue=ibkr` into the vike-data DataFusion hist store, idempotent per window.
//! Needs a running TWS/IB Gateway (its login is the entitlement — history is looser than realtime).
//! Behind the `ibkr` feature.
//!
//!   cargo run -p vike-backfill --features ibkr --bin ibkr_backfill -- \
//!       --env demo --symbols AAPL=AAPL.SMART.USD,MSFT=MSFT.SMART.USD --interval 1d --years 5
//!
//! `--symbols` is a comma list of `vikeSymbol=IBKRcontract` (bare `SYM` ⇒ contract == symbol).
//! `--interval` ∈ {1d,1h,5m,1m}. `--years` bounds the fetch when head_timestamp is unavailable.
//! `--start YYYY-MM-DD` overrides the lower bound. `--what` ∈ {trades,midpoint,bid,ask,bidask}.
//! `--store` defaults to `$VIKE_HIST_STORE` else `<repo>/market_data/hist`. `--pacing-ms` (default 10000)
//! sleeps between window requests (IB historical pacing).

use std::time::Duration;

use vike_backfill::cli::{CliSpec, flag_value, log_config, parse_symbol_pairs, store_root};
use vike_backfill::ibkr::{backfill_commit_key, plan_window_ends, step_ms_for, window_for};
use vike_data::{DataFusionHist, HistStore};
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::load_workspace_dotenv;
use vike_ibkr::{Environment, HistWhat, HistoricalFetcher};
use vike_model::{epoch_ms_to_utc_date, now_ms, parse_date_label};

/// `Debug` so a parse that was supposed to FAIL can report what it produced instead
/// (`Result::expect_err` requires it) — the same reason `vike_backfill::cli::Parsed` derives it.
#[derive(Debug)]
struct Args {
    env: Environment,
    symbols: Vec<(String, String)>, // (vikeSymbol, IBKR contract)
    interval: String,
    years: u32,
    start_ms: Option<i64>,
    what: HistWhat,
    store: Option<String>,
    pacing_ms: u64,
}

/// The process-argv entry point: [`parse_args_from`] over `std::env::args().skip(1)`. The SOURCE of
/// argv is the only thing this wrapper decides.
fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

/// Parse an already-`argv[0]`-stripped argument stream.
///
/// Every valued flag resolves through [`vike_backfill::cli::flag_value`], so a MENTIONED flag with
/// no value — or one handed another flag as its value — is an error here rather than only in
/// [`SPEC`]'s triage. An unmentioned optional flag still keeps its default.
fn parse_args_from(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut env = Environment::Demo;
    let mut symbols_spec = None;
    let mut interval = "1d".to_string();
    let mut years = 5u32;
    let mut start_ms = None;
    let mut what = HistWhat::Trades;
    let mut store = None;
    let mut pacing_ms = 10_000u64;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--env" => {
                env = match flag_value("--env", it.next())?.as_str() {
                    "demo" | "paper" => Environment::Demo,
                    "live" => Environment::Live,
                    other => return Err(format!("bad --env {other:?}")),
                }
            }
            "--symbols" => symbols_spec = Some(flag_value("--symbols", it.next())?),
            "--interval" => interval = flag_value("--interval", it.next())?,
            "--years" => {
                years = flag_value("--years", it.next())?
                    .parse()
                    .map_err(|_| "--years must be an integer")?
            }
            "--start" => {
                let s = flag_value("--start", it.next())?;
                start_ms = Some(parse_date_label(&s)?);
            }
            "--what" => {
                let s = flag_value("--what", it.next())?;
                what = HistWhat::parse(&s).ok_or_else(|| format!("bad --what {s}"))?;
            }
            "--store" => store = Some(flag_value("--store", it.next())?),
            "--pacing-ms" => {
                pacing_ms = flag_value("--pacing-ms", it.next())?
                    .parse()
                    .map_err(|_| "--pacing-ms must be an integer")?
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    let spec = symbols_spec.ok_or("--symbols SYM=contract,... is required")?;
    let symbols = parse_symbol_pairs(&spec)?;
    if window_for(&interval).is_none() {
        return Err(format!("unsupported --interval {interval} (have 1d,1h,5m,1m)"));
    }
    Ok(Args { env, symbols, interval, years, start_ms, what, store, pacing_ms })
}

const VENUE: &str = "ibkr";

const USAGE: &str = "\
usage: ibkr_backfill --symbols SYM=contract,... [--env demo|paper|live] [--interval 1d|1h|5m|1m]
                     [--years N] [--start <epoch-ms|YYYY-MM-DD>] [--what WHAT]
                     [--store DIR] [--pacing-ms N]

Page IBKR historical bars into the hist store as kind=bar under venue=ibkr, bounded below by the
contract head_timestamp unless --start says otherwise. Idempotent per paging window.

  Needs a running TWS or IB Gateway, and IBKR_{DEMO,LIVE}_ACCOUNT in the credential store.

  --symbols SPEC  comma list of `vikeSymbol=ibkrContract` pairs (required)
  --env E         demo (= paper, the default) or live
  --interval IV   1d | 1h | 5m | 1m (default 1d)
  --years N       how far back to page when --start is absent (default 5)
  --start T       explicit window start, epoch milliseconds or YYYY-MM-DD
  --what WHAT     the IBKR whatToShow field (default TRADES)
  --store DIR     hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  --pacing-ms N   delay between paging requests, to stay inside IBKR pacing limits (default 10000)
  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "ibkr_backfill",
    usage: USAGE,
    valued: &[
        "--symbols",
        "--env",
        "--interval",
        "--years",
        "--start",
        "--what",
        "--store",
        "--pacing-ms",
    ],
    toggles: &[],
    positionals: 0,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    SPEC.short_circuit_or_exit(&args);
    let _log_guards = vike_log::init(log_config("ibkr-backfill"));
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ibkr_backfill: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let Some(cfg) = load_ibkr_config_from(args.env, &load_workspace_dotenv()) else {
        eprintln!("ibkr_backfill: no IBKR_{}_ACCOUNT in .env", args.env.as_str());
        std::process::exit(2);
    };
    let fetcher = match HistoricalFetcher::connect(&cfg) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ibkr_backfill: connect failed (gateway down?): {e:?}");
            std::process::exit(1);
        }
    };
    let root = store_root(args.store.as_deref(), &std::env::vars().collect());
    let hist = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("ibkr_backfill: open store {root:?}: {e}");
            std::process::exit(1);
        }
    };
    let window = window_for(&args.interval).expect("validated in parse_args");
    let step_ms = step_ms_for(&args.interval).expect("validated in parse_args");
    let end_ms = now_ms();
    // `what` is part of the commit key so a re-run with a different series type actually writes.
    let what_str = format!("{:?}", args.what).to_lowercase();
    let mut grand_total = 0usize;
    // IB historical pacing (~60 req/10min): sleep `pacing_ms` before EVERY IB request except the
    // very first of the whole run — head_timestamp AND every window, across symbol boundaries.
    let mut first_request = true;
    let pace = |first: &mut bool| {
        if !*first {
            std::thread::sleep(Duration::from_millis(args.pacing_ms));
        }
        *first = false;
    };

    for (sym, contract) in &args.symbols {
        // Lower bound: explicit --start (no request), else head_timestamp (a paced request), else
        // now - years.
        let head_ms = if let Some(s) = args.start_ms {
            s
        } else {
            pace(&mut first_request);
            match fetcher.head_timestamp_ms(contract, args.what) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(
                        "{sym}: head_timestamp unavailable ({e:?}); bounding by --years"
                    );
                    end_ms - (args.years as i64) * 365 * 86_400_000
                }
            }
        };
        let ends = plan_window_ends(head_ms, end_ms, step_ms);
        tracing::info!(
            "{sym} ({contract}) {}: {} windows from {} to {}",
            args.interval,
            ends.len(),
            epoch_ms_to_utc_date(head_ms),
            epoch_ms_to_utc_date(end_ms)
        );
        let mut sym_total = 0usize;
        for (i, win_end) in ends.iter().enumerate() {
            pace(&mut first_request);
            match fetcher.fetch_window(contract, &args.interval, *win_end, window, args.what) {
                Ok(bars) if !bars.is_empty() => {
                    let key = backfill_commit_key(VENUE, sym, &args.interval, &what_str, *win_end);
                    match hist.append_bars(VENUE, sym, &args.interval, &bars, Some(&key)) {
                        Ok(n) => {
                            sym_total += n;
                            tracing::info!(
                                "{sym} window {}/{} end={}: {n} bars",
                                i + 1,
                                ends.len(),
                                epoch_ms_to_utc_date(*win_end)
                            );
                        }
                        Err(e) => tracing::error!("{sym} window end={win_end}: append failed: {e}"),
                    }
                }
                Ok(_) => tracing::info!("{sym} window {}/{}: empty", i + 1, ends.len()),
                Err(e) => tracing::error!("{sym} window end={win_end}: fetch failed: {e:?}"),
            }
        }
        tracing::info!(
            "{sym}: {sym_total} bars ingested (venue={VENUE}, interval={})",
            args.interval
        );
        grand_total += sym_total;
    }
    tracing::info!(
        "ibkr_backfill done: {grand_total} bars across {} symbols, interval={}, store={root:?}",
        args.symbols.len(),
        args.interval
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> std::vec::IntoIter<String> {
        v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>().into_iter()
    }

    /// Every flag lands in its own field, including both NUMERIC ones.
    #[test]
    fn a_full_invocation_maps_every_flag() {
        let a = parse_args_from(args(&[
            "--env",
            "live",
            "--symbols",
            "AAPL=AAPL.SMART.USD,MSFT",
            "--interval",
            "1h",
            "--years",
            "2",
            "--start",
            "2024-01-01",
            "--what",
            "midpoint",
            "--store",
            "/d/hist",
            "--pacing-ms",
            "250",
        ]))
        .expect("a complete command line parses");
        assert_eq!(a.env, Environment::Live);
        assert_eq!(
            a.symbols,
            vec![
                ("AAPL".to_string(), "AAPL.SMART.USD".to_string()),
                ("MSFT".to_string(), "MSFT".to_string()),
            ]
        );
        assert_eq!(a.interval, "1h");
        assert_eq!(a.years, 2);
        assert_eq!(a.start_ms, Some(1_704_067_200_000));
        assert_eq!(a.what, HistWhat::MidPoint);
        assert_eq!(a.store.as_deref(), Some("/d/hist"));
        assert_eq!(a.pacing_ms, 250);
    }

    /// **The DEFAULTS, written out.** `--env` defaults to DEMO, which is the one default here whose
    /// direction is a safety property: a forgotten `--env` must never reach a live account.
    #[test]
    fn the_defaults_are_demo_1d_5y_trades_and_ten_second_pacing() {
        let a = parse_args_from(args(&["--symbols", "AAPL"])).expect("only --symbols is required");
        assert_eq!(a.env, Environment::Demo, "a forgotten --env must not reach a live account");
        assert_eq!(a.interval, "1d");
        assert_eq!(a.years, 5);
        assert_eq!(a.start_ms, None);
        assert_eq!(a.what, HistWhat::Trades);
        assert_eq!(a.pacing_ms, 10_000, "IB historical pacing is ~60 requests / 10 min");
    }

    /// `--env` maps `paper` onto DEMO (the two spellings an operator uses interchangeably) and
    /// REFUSES anything else rather than falling through to a default — the flag that decides
    /// whether this connects to a live account may not be lenient.
    #[test]
    fn the_env_flag_accepts_only_its_three_spellings() {
        for (spelled, want) in
            [("demo", Environment::Demo), ("paper", Environment::Demo), ("live", Environment::Live)]
        {
            let a = parse_args_from(args(&["--symbols", "AAPL", "--env", spelled]))
                .unwrap_or_else(|e| panic!("--env {spelled}: {e}"));
            assert_eq!(a.env, want, "--env {spelled}");
        }
        for bad in ["LIVE", "prod", "", "1"] {
            let e = parse_args_from(args(&["--symbols", "AAPL", "--env", bad]))
                .expect_err("a value that is not demo|paper|live must be refused");
            assert!(e.contains("--env"), "the error names the flag: {e}");
        }
        // …and a TRAILING --env is refused too, rather than keeping the demo default silently.
        assert!(parse_args_from(args(&["--symbols", "AAPL", "--env"])).is_err());
    }

    /// Both NUMERIC flags refuse a non-integer rather than silently keeping their default — a
    /// `--pacing-ms` that fell back to 10 s when the operator asked for 250 ms would page IB at an
    /// unexpected rate, and a defaulted `--years` pages five years instead of the one requested.
    #[test]
    fn the_numeric_flags_reject_garbage_and_a_missing_value() {
        for flag in ["--years", "--pacing-ms"] {
            let garbage = parse_args_from(args(&["--symbols", "AAPL", flag, "soon"]))
                .expect_err("a non-integer must not fall through to the default");
            assert!(garbage.contains(flag), "the error names the flag: {garbage}");
            let missing = parse_args_from(args(&["--symbols", "AAPL", flag]))
                .expect_err("a trailing numeric flag");
            assert!(missing.contains(flag), "{missing}");
        }
        // A negative value is rejected by the unsigned parse itself, which is the intended shape.
        assert!(parse_args_from(args(&["--symbols", "AAPL", "--pacing-ms", "-1"])).is_err());
    }

    /// The interval allow-list is `window_for`'s, so this bin can never accept a code the pager has
    /// no window for — and one that is not on it is refused by name.
    #[test]
    fn the_interval_allow_list_is_window_fors() {
        for iv in ["1d", "1h", "5m", "1m"] {
            let a = parse_args_from(args(&["--symbols", "AAPL", "--interval", iv]))
                .unwrap_or_else(|e| panic!("{iv}: {e}"));
            assert_eq!(a.interval, iv);
            assert!(window_for(iv).is_some(), "the parser and the pager must agree on {iv}");
        }
        let e = parse_args_from(args(&["--symbols", "AAPL", "--interval", "4h"]))
            .expect_err("4h has no paging window");
        assert!(e.contains("4h"), "{e}");
    }

    /// **A FINDING, pinned rather than fixed.** `--what ""` is ACCEPTED and silently means TRADES —
    /// `vike_ibkr::HistWhat::parse` maps the empty string onto its default. The series type is part
    /// of this bin's commit key, so an empty `--what` (a shell variable that expanded to nothing)
    /// writes a TRADES series under a key that says trades, which is at least self-consistent; it is
    /// pinned here because it is the one value in this parser that means something other than what
    /// it looks like. A genuinely unrecognised value IS refused.
    #[test]
    fn an_empty_what_silently_means_trades_while_an_unknown_one_is_refused() {
        let a = parse_args_from(args(&["--symbols", "AAPL", "--what", ""]))
            .expect("the empty string is ACCEPTED — that is the finding");
        assert_eq!(a.what, HistWhat::Trades);
        let e = parse_args_from(args(&["--symbols", "AAPL", "--what", "volume"]))
            .expect_err("an unknown series type is refused");
        assert!(e.contains("volume"), "{e}");
    }

    /// A typo'd flag is REJECTED, not ignored; `--symbols` is the one required flag.
    #[test]
    fn an_unknown_flag_is_rejected_and_symbols_is_required() {
        let typo = parse_args_from(args(&["--symbols", "AAPL", "--pacing", "10"]))
            .expect_err("a typo must not run");
        assert!(typo.contains("--pacing"), "{typo}");
        let missing = parse_args_from(args(&["--env", "demo"])).expect_err("--symbols is required");
        assert!(missing.contains("--symbols"), "{missing}");
    }

    /// The `SPEC`/parser flag-set agreement, and the trailing-optional hole — now closed in the
    /// parser as well as in the SPEC. Both layers are asserted so neither can be removed on the
    /// assumption that the other covers it.
    #[test]
    fn the_spec_declares_exactly_what_the_parser_understands() {
        for flag in SPEC.valued.iter().copied() {
            if let Err(e) = parse_args_from(args(&[flag, "1d"])) {
                assert!(!e.contains("unknown arg"), "{flag} declared but rejected: {e}");
            }
        }
        let parser = parse_args_from(args(&["--symbols", "AAPL", "--store"]))
            .expect_err("the parser now refuses a --store it was given no value for");
        assert!(parser.contains("--store"), "{parser}");
        let full: Vec<String> = ["ibkr_backfill", "--symbols", "AAPL", "--store"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let e = SPEC.triage(&full).expect_err("and the SPEC still refuses it first");
        assert!(e.contains("--store"), "{e}");
        // …while an OMITTED --store is still just a default.
        assert_eq!(
            parse_args_from(args(&["--symbols", "AAPL"]))
                .expect("an unmentioned optional flag is not a usage error")
                .store,
            None
        );
    }

    /// The swallow half, and on this bin it reaches the flag that decides whether a LIVE account is
    /// touched: `--env --symbols AAPL` used to hand `--symbols` to the `--env` match, which refused
    /// it (`bad --env "--symbols"`) — safe, but by accident. `--symbols --env live` was the live
    /// hazard in the other direction: the symbol spec became the literal `--env`, the environment
    /// stayed at its DEMO default, and the run died on `live` as an unknown arg naming the wrong
    /// token. Both are now refused by name.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        let e = parse_args_from(args(&["--symbols", "--env", "live"]))
            .expect_err("a flag is not a symbol spec");
        assert!(e.contains("--symbols") && e.contains("--env"), "both tokens are named: {e}");
        let tail = parse_args_from(args(&["--symbols", "AAPL", "--store", "--pacing-ms"]))
            .expect_err("…and the trailing shape, which used to parse cleanly");
        assert!(tail.contains("--store") && tail.contains("--pacing-ms"), "{tail}");
    }
}
