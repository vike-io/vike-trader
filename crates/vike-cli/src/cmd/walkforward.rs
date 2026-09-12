//! `vike-cli walkforward` — the out-of-sample walk-forward sibling of the `backtest` command.
//!
//! Ship a backtest profile's TOML **verbatim** to a remote `vike-datahub` server, walk it forward
//! there (next to the data), and print the stitched out-of-sample result — the compute-to-data
//! offload, with **no DataFusion in this side's build graph**.
//!
//! # Usage
//!
//! ```text
//! vike-cli walkforward --profile <run.toml> [--addr 127.0.0.1:7880] [--json]
//! ```
//!
//! - `--profile <path>` (required): a backtest profile `.toml`, shipped verbatim. Its
//!   `[walkforward]` table supplies `n_splits` (the number of OOS windows) — a first-class profile
//!   section like `[sweep]`, so ONE file describes the whole run. A profile with no `[walkforward]`
//!   table comes back as a clean server-side error.
//! - `--addr <host:port>` (default `127.0.0.1:7880`): the COMPUTE daemon's address (`vike-backend backtest
//!   --addr`, NOT the datahub, since ruling 7). Reach a remote one
//!   over `ssh -L 7880:localhost:7880 the CI box`.
//! - `--json`: print the server's `WalkForwardReport` JSON.
//!
//! # ⚠ There are TWO walk-forward protocols, and this command has no flag for either
//!
//! `[walkforward].n_splits` alone is the FIXED-parameter walk: every window trades the profile's
//! own `[strategy.params]` and asks whether those settings were stable out of sample. Adding
//! `search = "sweep"` to that table asks the other question — each window re-searches the profile's
//! `[sweep]` grid on its OWN training half and trades only that window's winner, so what is being
//! validated is the PROCEDURE of fit-then-trade rather than one parameter set. `mode`
//! (`anchored` | `rolling`) picks the training shape and `rank_by` picks how a window scores its
//! candidates; `crates/vike-backtest/src/harness/profile.rs`'s `WalkforwardCfg` is the authority
//! for every spelling, and an unrecognized one fails server-side at load naming the valid set.
//!
//! ⚠ `mode` is inert without `search`, and that is a property of the protocol rather than a wiring
//! gap: the two modes differ ONLY in where a window's TRAINING half starts, and the fixed walk
//! discards the training half. Setting it alone parses, runs and returns the same numbers
//! (`crates/vike-backtest/src/harness/walkforward.rs`'s
//! `both_walk_modes_leave_the_fixed_walk_report_identical` pins that), so do not read a `rolling`
//! line in a profile as evidence that anything trained differently.
//!
//! **None of that is a flag here, deliberately.** This command parses no TOML at all — it reads the
//! file as text and ships it, and the server runs the one `BacktestProfile::from_toml_str` — so a
//! `--optimize` flag would be a second place to say what `[walkforward].search` already says, and
//! the two could then disagree with nothing to arbitrate. `crates/vike-datahub/src/server.rs`'s
//! `run_walkforward_profile` picks the driver from the profile and from nothing else.
//!
//! The consequence for reading the output: this side cannot say which protocol ran by inspecting
//! what it sent, and does not try to. It learns it from the ANSWER — a window that searched carries
//! the parameters it chose, which the table below renders in a `chosen` column that a fixed-
//! parameter walk does not print at all. That column's absence is the honest signal that no search
//! happened; it is not a rendering option.
//!
//! # ⚠ There is deliberately NO `--local`, unlike the `backtest` sibling
//!
//! That one grew one because the standalone engine can do what it asks: it runs a profile, and a
//! profile with a `[sweep]` table is the same run with its grid expanded. **The engine has no
//! walk-forward mode at all** — `crates/vike-backtest/src/backtest_cli.rs` has one profile path,
//! which branches on `BacktestProfile::is_sweep` and nothing else, so NEITHER driver
//! (`vike_backtest::harness::run_walkforward` or its optimizing sibling) is reachable from any
//! binary; both run from the datahub server alone. A `--local` here would have nothing to spawn.
//!
//! So `--local` is an unknown argument on this verb, which exits on the usage rung with the flag
//! named. That is a smaller lie than a flag that exists and always fails, and this paragraph is
//! where the absence is a decision rather than an oversight. What would change it: a walk-forward
//! entry point on the standalone engine — at which point this verb grows the same three flags its
//! siblings have (`--local`, `--store`, `--engine`) through `crate::cmd::engine`.
//!
//! Like the parameter-search sibling — the flags `crate::cmd::backtest` absorbed when ruling 13
//! deleted the `sweep` verb — this replaced a client-side profile→DTO mapping whose
//! `WireSlice`/`WireEngineParams` pair could not carry `[engine].fee` and the rest of the
//! `[engine]` surface. The server now parses the profile with the ONE
//! `BacktestProfile::from_toml_str` parser, and the walk-forward honors the whole `[engine]`. The
//! Studio's DTO-shaped `RunWalkforward` wire verb is untouched and still serves the GUI.
//!
//! Server-side this is BAR mode over ONE series (the splitter divides a single bar series by index);
//! a tick or multi-symbol profile is a clean error, never a silent first-symbol fallback.

use std::process::ExitCode;

use serde_json::Value;
use vike_datahub_client::{DatahubClient, NodeKeys, Scope};

use crate::cmd::args::{self, Flags};
use crate::exit::{CliError, CmdResult};

/// The default COMPUTE-daemon address this command dials — see the `backtest` command's own
/// constant for why it is the backtest daemon rather than the datahub since ruling 7.
const DEFAULT_ADDR: &str = vike_config::DEFAULT_BACKTEST_ADDR;

const USAGE: &str =
    "usage: vike-cli walkforward --profile <run.toml> [--addr 127.0.0.1:7880] [--json]";

/// The parsed `walkforward` command line.
#[derive(Debug)]
struct Args {
    profile_path: String,
    addr: String,
    json: bool,
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `walkforward` subcommand.
pub fn run(args: impl Iterator<Item = String>, keys: Option<&NodeKeys>) -> ExitCode {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("walkforward", USAGE, &msg),
    };
    match execute(&args, keys) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli walkforward: {}", e.msg);
            e.exit.into()
        }
    }
}

/// Hand-rolled arg parser (mirrors the `backtest` command, over the shared
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
/// failure path funnels into ONE [`CliError`] the caller prints and exits on — the connect
/// classified explicitly, everything else through `From<String>` on the pre-existing rung.
fn execute(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let profile_toml = std::fs::read_to_string(&args.profile_path)
        .map_err(|e| format!("cannot read profile {}: {e}", args.profile_path))?;

    // The CONNECT rung — the same classification the `backtest` sibling makes.
    // ⚠ Scope::Control, not Observe: `vike_datahub_client::proto`'s `required_scope` groups every
    // profile-running verb with the WRITE and DESTRUCTIVE ones, because each COMPILES
    // client-supplied Rhai on the server. `None` keeps the unauthenticated dial a key-less
    // server has always answered.
    let mut client = match keys {
        Some(k) => DatahubClient::connect_authed(&args.addr, k, Scope::Control),
        None => DatahubClient::connect(&args.addr),
    }
    .map_err(|e| CliError::connect(format!("cannot connect to the backtest daemon at {}: {e} (start it with `vike-backend backtest --addr`)", args.addr)))?;
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
///
/// The `chosen` column appears only when at least one window carries `chosen_params`, i.e. only for
/// a walk whose `[walkforward].search` actually searched. That is not a tidiness rule: the fixed
/// walk and the optimizing walk return the SAME report type, this side parses no TOML and so cannot
/// know which one it asked for, and the per-window winners are the only thing in the answer that
/// distinguishes them. Printing an empty column on the fixed walk would make the two look like one
/// protocol with a blank field; omitting it keeps the two outputs visibly different, and keeps the
/// fixed walk's table byte-identical to what this command has always printed.
fn print_walkforward(report: &Value) {
    let windows = report.get("windows").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    println!("walk-forward: {} out-of-sample window(s)", windows.len());
    // `chosen_params` is `skip_serializing_if = "Option::is_none"` on the server's `WfWindow`, so
    // the FIELD's presence — not its emptiness — is what says a window searched.
    let searched = windows.iter().any(|w| w.get("chosen_params").is_some());
    if searched {
        println!("{:>6}  {:<20}  {:>11}  chosen", "window", "test_range", "oos_return");
    } else {
        println!("{:>6}  {:<20}  {:>11}", "window", "test_range", "oos_return");
    }
    for (i, w) in windows.iter().enumerate() {
        let range = w.get("test_range").and_then(Value::as_array);
        let bound = |k: usize| {
            range.and_then(|r| r.get(k)).and_then(Value::as_i64).unwrap_or_default().to_string()
        };
        let row = format!(
            "{:>6}  {:<20}  {:>+10.2}%",
            i + 1,
            format!("[{}, {})", bound(0), bound(1)),
            num(w, "oos_return") * 100.0
        );
        if searched {
            println!("{row}  {}", chosen(w));
        } else {
            println!("{row}");
        }
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

/// One window's SELECTED parameters as `key=value` pairs — what that window's own search chose on
/// its training half, and the whole diagnostic value of an optimizing walk: the stitched number
/// says what the procedure earned, these say whether it kept finding the same answer or wandered.
///
/// The pairs are rendered in the order they arrive and are not re-sorted here — they need no
/// sorting, because `crates/vike-backtest/src/harness/sweep.rs`'s `expand_sweep` sorts the grid's
/// keys before it builds a point, so two windows' cells already line up term by term even when the
/// chosen values differ. Rendering is JSON `Display` — compact, the
/// server's own bytes — except that a string value loses its quotes, because this is the human
/// table and `--json` is the exact answer for anyone who must tell `5` from `"5"`.
///
/// `-` for a window that recorded no choice. Reachable only in a MIXED report, which today's server
/// cannot produce (a window whose every candidate failed is a hard error, not a blank row) — so
/// this is a rendering that cannot be reached rather than a case that is handled, and it exists
/// because the alternative is an empty cell that reads as a missing column instead of a missing
/// choice.
fn chosen(w: &Value) -> String {
    let pairs = w.get("chosen_params").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    if pairs.is_empty() {
        return "-".to_string();
    }
    pairs
        .iter()
        .map(|pair| {
            let kv = pair.as_array().map(Vec::as_slice).unwrap_or(&[]);
            let key = kv.first().and_then(Value::as_str).unwrap_or("?");
            match kv.get(1) {
                Some(Value::String(s)) => format!("{key}={s}"),
                Some(v) => format!("{key}={v}"),
                None => format!("{key}=?"),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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

    /// An OPTIMIZED walk's window carries what it chose, and the renderer states it in the
    /// profile's own vocabulary — `key=value`, one term per swept axis, key-sorted as sent.
    ///
    /// The four value shapes are the four a `[sweep]` axis can hold, and each is pinned because
    /// each is a different `serde_json::Value` arm: the string one is the case worth watching,
    /// since it is the only one whose rendering DIFFERS from `Value`'s own `Display` (quotes off).
    #[test]
    fn a_searched_window_renders_the_parameters_it_chose() {
        let w: Value = serde_json::from_str(
            r#"{"test_range": [0, 60], "oos_return": 0.05,
                "chosen_params": [["flag", true], ["label", "fast-lane"],
                                  ["size", 1.5], ["slow", 30]]}"#,
        )
        .unwrap();
        assert_eq!(chosen(&w), "flag=true label=fast-lane size=1.5 slow=30");
    }

    /// A FIXED-parameter window renders `-`, and so does an empty choice list. This is the whole
    /// signal an operator has for "which protocol did the server actually run", so it is asserted
    /// rather than left to the eye: the server omits `chosen_params` entirely on the fixed walk
    /// (`skip_serializing_if`), which is the FIRST case here, and the second is the same answer
    /// reached from a shape today's server does not emit.
    #[test]
    fn a_fixed_parameter_window_renders_no_choice() {
        let fixed: Value = serde_json::from_str(WF_JSON).unwrap();
        assert_eq!(chosen(&fixed["windows"][0]), "-");
        let empty: Value = serde_json::from_str(r#"{"chosen_params": []}"#).unwrap();
        assert_eq!(chosen(&empty), "-");
    }

    /// Both table shapes RENDER — the fixed walk with three columns, the optimized walk with four.
    ///
    /// ⚠ What this proves is only that neither path panics: `print_walkforward` writes to stdout
    /// and this test does not capture it, so the COLUMN suppression itself is unasserted here. The
    /// per-window assertion above is what actually pins the cell contents; this is the smoke test
    /// its two neighbours already are, extended to the branch that did not exist before.
    #[test]
    fn both_table_shapes_render() {
        print_walkforward(&serde_json::from_str(WF_JSON).unwrap());
        let optimized: Value = serde_json::from_str(
            r#"{"windows": [{"test_range": [0, 60], "oos_return": 0.05,
                             "chosen_params": [["fast", 5]]}],
                "oos_return": 0.05, "oos_sharpe": 1.1, "wf_consistency": 1.0}"#,
        )
        .unwrap();
        print_walkforward(&optimized);
    }
}
