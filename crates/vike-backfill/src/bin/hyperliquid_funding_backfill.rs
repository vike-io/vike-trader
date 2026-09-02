//! Hyperliquid realized-funding backfill CLI. Fetches a master account's keyless `userFunding`
//! `/info` history (mainnet, public — NO credentials, just the address) over `[--start, --end]` and
//! ingests it as `kind=funding`/`venue=hyperliquid`/`symbol=<coin>` into the vike-data DataFusion
//! hist store, split by coin, idempotent per `(account, coin, window)`. The funding analog of
//! `hyperliquid_backfill` (candles).
//!
//!   cargo run -p vike-backfill --bin hyperliquid_funding_backfill -- \
//!       --account 0x8f0a3e01d916486735a8f6a2ffc0685a3fa57bf5 --start 2024-01-01
//!   cargo run -p vike-backfill --bin hyperliquid_funding_backfill -- \
//!       --account 0xABC... --start 1704067200000 --end 1706745600000 --store /market_data/hist
//!
//! `--account` is the master account address funding is read against (funding reads are keyless — the
//! agent-wallet pitfall the bridge guards against does NOT apply here; pass the MASTER address).
//! `--start`/`--end` are each an epoch-ms integer OR a `YYYY-MM-DD` UTC date; `--end` defaults to now.
//! `--store` defaults to `$VIKE_HIST_STORE` else `<repo>/market_data/hist`.

use vike_backfill::cli::{flag_value, log_config, store_root, CliSpec};
use vike_backfill::hyperliquid::{backfill_hyperliquid_funding, VENUE};
use vike_data::DataFusionHist;
use vike_model::{epoch_ms_to_utc_date, now_ms, parse_date_label};

/// `Debug` so a parse that was supposed to FAIL can report what it produced instead
/// (`Result::expect_err` requires it) — the same reason `vike_backfill::cli::Parsed` derives it.
#[derive(Debug)]
struct Args {
    account: String,
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
    let mut account = None;
    let mut start = None;
    let mut end = None;
    let mut store = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--account" => account = Some(flag_value("--account", it.next())?),
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
    let account = account
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or("--account 0x<master-address> is required")?;
    let start_ms = start.ok_or("--start <epoch-ms|YYYY-MM-DD> is required")?;
    let end_ms = end.unwrap_or_else(now_ms);
    if start_ms > end_ms {
        return Err(format!("--start ({start_ms}) is after --end ({end_ms})"));
    }
    Ok(Args { account, start_ms, end_ms, store })
}

const USAGE: &str = "\
usage: hyperliquid_funding_backfill --account 0xADDRESS --start <epoch-ms|YYYY-MM-DD>
                                    [--end <epoch-ms|YYYY-MM-DD>] [--store DIR]

Backfill one Hyperliquid account funding-payment history into the hist store. Idempotent per
window: a re-run of the same range writes 0 rows.

  --account ADDR  the master account address, 0x-prefixed (required)
  --start T       window start, epoch milliseconds or YYYY-MM-DD (required)
  --end T         window end, same forms (default: now)
  --store DIR     hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "hyperliquid_funding_backfill",
    usage: USAGE,
    valued: &["--account", "--start", "--end", "--store"],
    toggles: &[],
    positionals: 0,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    SPEC.short_circuit_or_exit(&args);
    let _log_guards = vike_log::init(log_config("hyperliquid-funding-backfill"));
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("hyperliquid_funding_backfill: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let root = store_root(args.store.as_deref(), &std::env::vars().collect());
    let hist = match DataFusionHist::open(&root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("hyperliquid_funding_backfill: open store {root:?}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(
        "hyperliquid_funding_backfill: account={}, [{}..{}] -> {root:?}",
        args.account,
        epoch_ms_to_utc_date(args.start_ms),
        epoch_ms_to_utc_date(args.end_ms),
    );
    match backfill_hyperliquid_funding(&hist, &args.account, args.start_ms, args.end_ms) {
        Ok(n) => {
            tracing::info!(
                "hyperliquid_funding_backfill done: {n} funding rows ingested \
                 (0 = window already ingested), venue={VENUE}, store={root:?}"
            );
        }
        Err(e) => {
            tracing::error!("hyperliquid_funding_backfill: backfill failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> std::vec::IntoIter<String> {
        v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>().into_iter()
    }

    /// Every flag lands in its own field, and the account is TRIMMED — an address pasted with a
    /// trailing newline or space must reach the `/info` call as the address, not as one that 404s.
    #[test]
    fn a_full_invocation_maps_every_flag_and_trims_the_account() {
        let a = parse_args_from(args(&[
            "--account",
            "  0xabc  ",
            "--start",
            "2024-01-01",
            "--end",
            "1706745600000",
            "--store",
            "/d/hist",
        ]))
        .expect("a complete command line parses");
        assert_eq!(a.account, "0xabc");
        assert_eq!(a.start_ms, 1_704_067_200_000);
        assert_eq!(a.end_ms, 1_706_745_600_000);
        assert_eq!(a.store.as_deref(), Some("/d/hist"));
    }

    /// The two required flags, and the one non-obvious rule: a BLANK `--account` counts as ABSENT
    /// rather than being sent to the venue as an empty address.
    #[test]
    fn the_required_flags_are_required_and_a_blank_account_counts_as_absent() {
        for (argv, want) in [
            (&["--start", "0"][..], "--account"),
            (&["--account", "0xabc"][..], "--start"),
            (&["--account", "   ", "--start", "0"][..], "--account"),
        ] {
            let e = parse_args_from(args(argv)).expect_err("must be refused");
            assert!(e.contains(want), "expected {want} to be named: {e}");
        }
    }

    /// A reversed window, an unknown flag and a malformed date are all errors rather than a silently
    /// wrong request.
    #[test]
    fn a_reversed_window_an_unknown_flag_and_a_bad_date_are_errors() {
        let reversed = parse_args_from(args(&[
            "--account",
            "0xabc",
            "--start",
            "2024-06-01",
            "--end",
            "2024-01-01",
        ]))
        .expect_err("start after end");
        assert!(reversed.contains("--start") && reversed.contains("--end"), "{reversed}");
        let typo = parse_args_from(args(&["--account", "0xabc", "--acount", "0xdef"]))
            .expect_err("a typo must not run");
        assert!(typo.contains("--acount"), "{typo}");
        assert!(parse_args_from(args(&["--account", "0xabc", "--start", "soon"])).is_err());
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
            parse_args_from(args(&["--account", "0xabc", "--start", "0", "--end", "1", "--store"]))
                .expect_err("the parser now refuses a --store it was given no value for");
        assert!(parser.contains("--store"), "{parser}");
        let full: Vec<String> = [
            "hyperliquid_funding_backfill",
            "--account",
            "0xabc",
            "--start",
            "0",
            "--end",
            "1",
            "--store",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let e = SPEC.triage(&full).expect_err("and the SPEC still refuses it first");
        assert!(e.contains("--store"), "{e}");
        // …while an OMITTED --store is still just a default.
        let omitted = parse_args_from(args(&["--account", "0xabc", "--start", "0", "--end", "1"]))
            .expect("an unmentioned optional flag is not a usage error");
        assert_eq!(omitted.store, None);
    }

    /// The swallow half: a valued flag may not eat a FLAG. `--account --start 0` used to send the
    /// literal token `--account`'s successor as the master ADDRESS the funding history is read
    /// against, and then die on `0` as an unknown arg — a diagnostic about the wrong token.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        let e = parse_args_from(args(&["--account", "--start", "0"]))
            .expect_err("a flag is not an account address");
        assert!(e.contains("--account") && e.contains("--start"), "both tokens are named: {e}");
        let tail =
            parse_args_from(args(&["--account", "0xabc", "--start", "0", "--store", "--end"]))
                .expect_err("…and the trailing shape, which used to parse cleanly");
        assert!(tail.contains("--store") && tail.contains("--end"), "{tail}");
    }
}
