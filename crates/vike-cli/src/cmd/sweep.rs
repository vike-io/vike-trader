//! `vike-cli sweep` — the parameter-grid-search sibling of the `backtest` command.
//!
//! Ship a backtest profile's TOML **verbatim** to a remote `vike-datahub` server, run the ranked
//! parameter sweep there (the EXISTING `vike_backtest::harness::run_sweep`, next to the data), and
//! print the ranked comparison — the compute-to-data offload, with **no DataFusion in this side's
//! build graph**.
//!
//! # Usage
//!
//! ```text
//! vike-cli sweep --profile <run.toml> [--addr 127.0.0.1:7878]
//!                [--rank-by sharpe|return|max_dd|equity] [--json]
//! ```
//!
//! - `--profile <path>` (required): a backtest profile `.toml`. Its text is read locally and shipped
//!   verbatim; the SERVER parses+validates it with `BacktestProfile::from_toml_str` and expands its
//!   `[sweep]` table (each key overrides `strategy.params.<key>` across a value grid). A profile
//!   with no `[sweep]` table comes back as a clean server-side error.
//! - `--addr <host:port>` (default `127.0.0.1:7878`): the datahub server address (reach a remote one
//!   over `ssh -L 7878:localhost:7878 the CI box`).
//! - `--rank-by <metric>`: `sharpe` (default) / `return` / `max_dd` / `equity`. The name is
//!   validated locally against the roster below, then applied SERVER-side by the canonical
//!   `harness::RankMetric` comparator, so the rows arrive already ordered best-first.
//! - `--json`: print the server's ranked `SweepReport` JSON.
//!
//! # Why the whole TOML crosses the wire
//!
//! This command used to RE-PARSE the profile into `WireSpec`/`WireSlice`/`WireSweep` DTOs — a
//! second profile parser and a second date parser in the workspace — and that mapping could not
//! carry `[engine].fee`, `[engine.impact]`, `[engine.resolution]`, `[risk]`, `snap_to_properties`,
//! a tick slice or a cross-venue `[[data.series]]` slice, so a sweep quietly ran under DIFFERENT
//! costs than the same profile's `vike-cli backtest`. Shipping the text (the `backtest` command's
//! idiom since #719) leaves ONE profile parser in the workspace and makes the sweep honor the whole
//! profile. It likewise removed a local re-implementation of sharpe / max-drawdown / total-return:
//! ranking and every printed statistic are now the server's own `BacktestReport` numbers — the
//! workspace rule that no metric is reimplemented outside vike-backtest.
//!
//! The Studio's DTO-shaped `RunSweep` wire verb is untouched and still serves the GUI, which holds
//! a picker's `spec`/`slice`, not a profile file.

use std::process::ExitCode;

use serde_json::Value;
use vike_datahub_client::DatahubClient;

use crate::cmd::args::{self, Flags};

/// The default datahub listen address (mirrors the `backtest` command + the deploy runbook).
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

const USAGE: &str = "usage: vike-cli sweep --profile <run.toml> [--addr 127.0.0.1:7878] [--rank-by sharpe|return|max_dd|equity] [--json]";

/// The `--rank-by` names the server's `harness::RankMetric::from_str_ci` accepts. Validated HERE so
/// a typo is a local usage error instead of a wasted round-trip; the metric itself is computed and
/// applied server-side (this list is a spelling check, not a second implementation).
const RANK_METRICS: [&str; 4] = ["sharpe", "return", "max_dd", "equity"];

/// Validate a `--rank-by` value against [`RANK_METRICS`], returning it owned.
fn parse_rank_by(s: &str) -> Result<String, String> {
    if RANK_METRICS.contains(&s) {
        Ok(s.to_string())
    } else {
        Err(format!("--rank-by must be {}, got {s:?}", RANK_METRICS.join("|")))
    }
}

// ---------------------------------------------------------------------------------------------
// Command entry
// ---------------------------------------------------------------------------------------------

/// The parsed `sweep` command line.
#[derive(Debug)]
struct Args {
    profile_path: String,
    addr: String,
    /// `None` = let the server rank by its own default metric (annualized Sharpe).
    rank_by: Option<String>,
    json: bool,
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `sweep` subcommand.
pub fn run(args: impl Iterator<Item = String>) -> ExitCode {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("sweep", USAGE, &msg),
    };
    match execute(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("vike-cli sweep: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Hand-rolled arg parser (mirrors the `backtest` command, over the shared [`crate::cmd::args`]
/// glue): `--flag value` and `--flag=value`.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut profile_path: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut rank_by: Option<String> = None;
    let mut json = false;

    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--profile" => profile_path = Some(flags.value(&flag, inline)?),
            "--addr" => addr = Some(flags.value(&flag, inline)?),
            "--rank-by" => rank_by = Some(parse_rank_by(&flags.value(&flag, inline)?)?),
            "--json" => {
                args::no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Args {
        profile_path: profile_path.ok_or("missing required --profile <run.toml>")?,
        addr: addr.unwrap_or_else(|| DEFAULT_ADDR.to_string()),
        rank_by,
        json,
    })
}

/// Read the profile, ship its TOML to the datahub server, and print the ranked result. Every
/// failure path funnels into ONE `Err(String)` the caller prints to stderr.
fn execute(args: &Args) -> Result<(), String> {
    let profile_toml = std::fs::read_to_string(&args.profile_path)
        .map_err(|e| format!("cannot read profile {}: {e}", args.profile_path))?;

    let mut client = DatahubClient::connect(&args.addr)
        .map_err(|e| format!("cannot connect to datahub at {}: {e}", args.addr))?;
    let report_json = client.run_sweep_profile(&profile_toml, args.rank_by.as_deref())?;

    if args.json {
        // Verbatim: exactly the JSON the server emitted (the `backtest --sweep --json` shape).
        println!("{report_json}");
        return Ok(());
    }
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server sweep report was not valid JSON: {e}"))?;
    print_sweep(&report);
    Ok(())
}

/// Print the server's ranked `SweepReport` as a human table — one row per grid point, in the order
/// the SERVER ranked them. Every number printed is read straight out of that row's `BacktestReport`
/// (nothing is recomputed here).
fn print_sweep(report: &Value) {
    let rows = report.get("rows").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    if rows.is_empty() {
        println!("(sweep returned no grid points)");
        return;
    }
    let rank_by = report.get("rank_by").and_then(Value::as_str).unwrap_or("sharpe");
    println!("parameter sweep — {} point(s), ranked server-side by {rank_by}", rows.len());
    println!(
        "{:>4}  {:<34}  {:>14}  {:>9}  {:>8}  {:>8}  {:>7}",
        "rank", "overrides", "final_equity", "return", "sharpe", "max_dd", "trades"
    );
    for (i, row) in rows.iter().enumerate() {
        let overrides = fmt_overrides(row.get("overrides"));
        match row.get("report").filter(|r| !r.is_null()) {
            Some(r) => println!(
                "{:>4}  {:<34}  {:>14.2}  {:>+8.2}%  {:>8.4}  {:>8.4}  {:>7}",
                i + 1,
                overrides,
                num(r, "final_equity"),
                num(r, "total_return") * 100.0,
                num(r, "sharpe"),
                num(r, "max_drawdown"),
                num(r, "n_trades") as i64,
            ),
            // A per-point failure never fails the whole sweep server-side — it comes back as this
            // row's `error`, and the row still prints (which point failed, and why).
            None => println!(
                "{:>4}  {:<34}  FAILED: {}",
                i + 1,
                overrides,
                row.get("error").and_then(Value::as_str).unwrap_or("(unknown error)")
            ),
        }
    }
}

/// Render one row's `[[name, value], …]` overrides as `k=v, k=v`. The values are TOML scalars
/// serialized into JSON, so they print through [`Value`]'s own `Display` (nothing is re-typed).
fn fmt_overrides(overrides: Option<&Value>) -> String {
    let Some(pairs) = overrides.and_then(Value::as_array) else {
        return String::new();
    };
    pairs
        .iter()
        .filter_map(|p| {
            let pair = p.as_array()?;
            let (k, v) = (pair.first()?.as_str()?, pair.get(1)?);
            Some(format!("{k}={v}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One numeric field of a server `BacktestReport`; `0.0` when absent or non-numeric (a
/// `"sharpe": null` — serde_json's encoding of a NaN Sharpe over a flat curve — prints as `0.0000`
/// rather than breaking the table).
fn num(report: &Value, field: &str) -> f64 {
    report.get(field).and_then(Value::as_f64).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server `SweepReport` JSON as the wire delivers it: rows already ranked, each with its own
    /// `BacktestReport` (or an `error`), plus the metric that ranked them.
    const SWEEP_JSON: &str = r#"{
      "rows": [
        {"overrides": [["fast", 5], ["slow", 20]],
         "report": {"final_equity": 1200.0, "total_return": 0.2, "n_trades": 4,
                    "win_rate": 0.75, "sharpe": 1.8, "max_drawdown": 0.03,
                    "profit_factor": 2.5, "per_symbol_pnl": [], "funding_paid": 0.0},
         "error": null},
        {"overrides": [["fast", 10], ["slow", 30]], "report": null, "error": "boom"}
      ],
      "rank_by": "sharpe"
    }"#;

    #[test]
    fn rank_by_is_validated_locally_against_the_server_roster() {
        assert_eq!(parse_rank_by("max_dd").unwrap(), "max_dd");
        let err = parse_rank_by("bogus").unwrap_err();
        assert!(err.contains("--rank-by"), "{err}");
        assert!(err.contains("sharpe"), "names the valid set: {err}");
    }

    #[test]
    fn arg_parsing_requires_a_profile_and_reads_the_flags() {
        assert!(parse_args(["--json".to_string()].into_iter()).unwrap_err().contains("--profile"));

        let args = parse_args(
            ["--profile", "run.toml", "--addr", "1.2.3.4:9", "--rank-by", "return", "--json"]
                .map(String::from)
                .into_iter(),
        )
        .unwrap();
        assert_eq!(args.profile_path, "run.toml");
        assert_eq!(args.addr, "1.2.3.4:9");
        assert_eq!(args.rank_by.as_deref(), Some("return"));
        assert!(args.json);

        // defaults: server-default ranking, default addr
        let args = parse_args(["--profile=run.toml".to_string()].into_iter()).unwrap();
        assert_eq!(args.addr, DEFAULT_ADDR);
        assert_eq!(args.rank_by, None);
        assert!(!args.json);
    }

    #[test]
    fn an_unknown_rank_by_is_a_clean_error() {
        let err =
            parse_args(["--profile", "r.toml", "--rank-by", "bogus"].map(String::from).into_iter())
                .unwrap_err();
        assert!(err.contains("--rank-by"), "{err}");
    }

    /// The renderer reads the SERVER's per-row stats verbatim and keeps the server's row ORDER —
    /// no client-side ranking, no recomputed metric.
    #[test]
    fn renders_server_rows_in_server_order_with_server_stats() {
        let report: Value = serde_json::from_str(SWEEP_JSON).unwrap();
        let rows = report["rows"].as_array().unwrap();
        assert_eq!(fmt_overrides(rows[0].get("overrides")), "fast=5, slow=20");
        assert_eq!(num(&rows[0]["report"], "sharpe"), 1.8);
        assert_eq!(num(&rows[0]["report"], "max_drawdown"), 0.03);
        // a failed point has no report — the table prints its error instead
        assert!(rows[1].get("report").is_some_and(Value::is_null));
        assert_eq!(rows[1]["error"].as_str(), Some("boom"));
        // and the whole render does not panic
        print_sweep(&report);
    }

    /// A missing/`null` numeric field degrades to `0.0` rather than breaking the table — a NaN
    /// Sharpe over a flat curve serializes as JSON `null`.
    #[test]
    fn a_null_metric_reads_as_zero() {
        let r: Value = serde_json::from_str(r#"{"sharpe": null}"#).unwrap();
        assert_eq!(num(&r, "sharpe"), 0.0);
        assert_eq!(num(&r, "not_a_field"), 0.0);
    }

    #[test]
    fn an_empty_grid_renders_without_panicking() {
        let report: Value = serde_json::from_str(r#"{"rows": [], "rank_by": "sharpe"}"#).unwrap();
        print_sweep(&report);
    }
}
