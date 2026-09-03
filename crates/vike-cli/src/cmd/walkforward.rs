//! `vike-cli walkforward` — the anchored out-of-sample walk-forward sibling of the `backtest`
//! command.
//!
//! Ship a backtest profile's TOML **verbatim** to a remote `vike-datahub` server, run the anchored
//! walk-forward there (the EXISTING `vike_backtest::harness::run_walkforward`, next to the data),
//! and print the stitched out-of-sample result — the compute-to-data offload, with **no DataFusion
//! in this side's build graph**.
//!
//! # Usage
//!
//! ```text
//! vike-cli walkforward --profile <run.toml> [--addr 127.0.0.1:7878] [--json]
//! ```
//!
//! - `--profile <path>` (required): a backtest profile `.toml`, shipped verbatim. Its
//!   `[walkforward]` table supplies `n_splits` (the number of anchored OOS windows) — a first-class
//!   profile section like `[sweep]`, so ONE file describes the whole run. A profile with no
//!   `[walkforward]` table comes back as a clean server-side error.
//! - `--addr <host:port>` (default `127.0.0.1:7878`): the datahub server address (reach a remote one
//!   over `ssh -L 7878:localhost:7878 the CI box`).
//! - `--json`: print the server's `WalkForwardReport` JSON.
//!
//! Like the `sweep` sibling this replaced a client-side profile→DTO mapping whose
//! `WireSlice`/`WireEngineParams` pair could not carry `[engine].fee` and the rest of the `[engine]`
//! surface (see [`crate::cmd::sweep`]'s module doc). The server now parses the profile with the ONE
//! `BacktestProfile::from_toml_str` parser, and the walk-forward honors the whole `[engine]`. The
//! Studio's DTO-shaped `RunWalkforward` wire verb is untouched and still serves the GUI.
//!
//! Server-side this is BAR mode over ONE series (the splitter divides a single bar series by index);
//! a tick or multi-symbol profile is a clean error, never a silent first-symbol fallback.

use std::process::ExitCode;

use serde_json::Value;
use vike_datahub_client::DatahubClient;

use crate::cmd::args::{self, Flags};

/// The default datahub listen address (mirrors the `backtest`/`sweep` commands + the deploy runbook).
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

const USAGE: &str =
    "usage: vike-cli walkforward --profile <run.toml> [--addr 127.0.0.1:7878] [--json]";

/// The parsed `walkforward` command line.
#[derive(Debug)]
struct Args {
    profile_path: String,
    addr: String,
    json: bool,
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `walkforward` subcommand.
pub fn run(args: impl Iterator<Item = String>) -> ExitCode {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("walkforward", USAGE, &msg),
    };
    match execute(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("vike-cli walkforward: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Hand-rolled arg parser (mirrors the `backtest`/`sweep` commands, over the shared
/// [`crate::cmd::args`] glue): `--flag value` and `--flag=value`.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut profile_path: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut json = false;

    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--profile" => profile_path = Some(flags.value(&flag, inline)?),
            "--addr" => addr = Some(flags.value(&flag, inline)?),
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
        json,
    })
}

/// Read the profile, ship its TOML to the datahub server, and print the stitched OOS result. Every
/// failure path funnels into ONE `Err(String)` the caller prints.
fn execute(args: &Args) -> Result<(), String> {
    let profile_toml = std::fs::read_to_string(&args.profile_path)
        .map_err(|e| format!("cannot read profile {}: {e}", args.profile_path))?;

    let mut client = DatahubClient::connect(&args.addr)
        .map_err(|e| format!("cannot connect to datahub at {}: {e}", args.addr))?;
    let report_json = client.run_walkforward_profile(&profile_toml)?;

    if args.json {
        // Verbatim: exactly the JSON the server emitted.
        println!("{report_json}");
        return Ok(());
    }
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server walk-forward report was not valid JSON: {e}"))?;
    print_walkforward(&report);
    Ok(())
}

/// Print the server's `WalkForwardReport` as a human table: one row per OOS window plus the summary
/// line. Every number is the server's own — nothing is recomputed here.
fn print_walkforward(report: &Value) {
    let windows = report.get("windows").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    println!("walk-forward: {} out-of-sample window(s)", windows.len());
    println!("{:>6}  {:<20}  {:>11}", "window", "test_range", "oos_return");
    for (i, w) in windows.iter().enumerate() {
        let range = w.get("test_range").and_then(Value::as_array);
        let bound = |k: usize| {
            range.and_then(|r| r.get(k)).and_then(Value::as_i64).unwrap_or_default().to_string()
        };
        println!(
            "{:>6}  {:<20}  {:>+10.2}%",
            i + 1,
            format!("[{}, {})", bound(0), bound(1)),
            num(w, "oos_return") * 100.0
        );
    }
    println!();
    println!(
        "oos_return = {:+.2}%   oos_sharpe = {:.3}   wf_consistency = {:.1}%",
        num(report, "oos_return") * 100.0,
        num(report, "oos_sharpe"),
        num(report, "wf_consistency") * 100.0
    );
}

/// One numeric field of the server report; `0.0` when absent or `null` (serde_json's encoding of a
/// non-finite f64), so a degenerate run prints instead of breaking the table.
fn num(v: &Value, field: &str) -> f64 {
    v.get(field).and_then(Value::as_f64).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server `WalkForwardReport` JSON as the wire delivers it.
    const WF_JSON: &str = r#"{
      "windows": [{"test_range": [0, 60], "oos_return": 0.05},
                  {"test_range": [60, 120], "oos_return": -0.02}],
      "oos_equity_curve": [1000.0, 1050.0, 1029.0],
      "oos_return": 0.029,
      "oos_sharpe": 0.81,
      "wf_consistency": 0.5
    }"#;

    #[test]
    fn arg_parsing_requires_a_profile() {
        assert!(parse_args(["--json".to_string()].into_iter()).unwrap_err().contains("--profile"));

        let args = parse_args(
            ["--profile", "run.toml", "--addr", "1.2.3.4:9", "--json"]
                .map(String::from)
                .into_iter(),
        )
        .unwrap();
        assert_eq!(args.profile_path, "run.toml");
        assert_eq!(args.addr, "1.2.3.4:9");
        assert!(args.json);

        let args = parse_args(["--profile=run.toml".to_string()].into_iter()).unwrap();
        assert_eq!(args.addr, DEFAULT_ADDR);
        assert!(!args.json);
    }

    /// The renderer reads the SERVER's window rows + summary scalars verbatim.
    #[test]
    fn renders_the_server_windows_and_summary() {
        let report: Value = serde_json::from_str(WF_JSON).unwrap();
        assert_eq!(report["windows"].as_array().unwrap().len(), 2);
        assert_eq!(num(&report, "oos_sharpe"), 0.81);
        assert_eq!(num(&report["windows"][1], "oos_return"), -0.02);
        print_walkforward(&report);
    }

    /// A degenerate report (no windows, `null` scalars) renders rather than panicking.
    #[test]
    fn a_degenerate_report_renders() {
        let report: Value = serde_json::from_str(r#"{"windows": [], "oos_sharpe": null}"#).unwrap();
        assert_eq!(num(&report, "oos_sharpe"), 0.0);
        print_walkforward(&report);
    }
}
