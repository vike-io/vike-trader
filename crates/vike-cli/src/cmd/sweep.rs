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
//! # `--local`: the same grid, on THIS machine
//!
//! ```text
//! vike-cli sweep --local --profile <run.toml> [--store DIR] [--engine PATH] [--rank-by …] [--json]
//! ```
//!
//! Drives the standalone `backtest` engine — SPAWNED, never linked ([`crate::cmd::engine`] carries
//! the argument) — which expands the profile's `[sweep]` table itself. The engine's own `--rank-by`
//! takes the name this parser has already spelling-checked, so a typo is still caught before a
//! process is started.
//!
//! ⚠ **"The same grid" is a claim with one guard behind it**, because the engine on its own would
//! not have made it true: handed a profile with no `[sweep]` table it runs a SINGLE backtest and
//! exits 0, where the server REFUSES. [`refuse_a_profile_with_no_grid`] closes that, with the
//! server's own sentence, before anything is spawned. ⚠ An engine installed on this box is a
//! prerequisite of `--local` and is a Linux/container release asset — [`crate::cmd::engine`]'s
//! module doc carries that asymmetry.
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

use std::path::Path;
use std::process::ExitCode;

use serde_json::Value;
use vike_datahub_client::{DatahubClient, NodeKeys, Scope};

use crate::cmd::args::{self, Flags};
use crate::cmd::engine;
use crate::exit::{CliError, CmdResult};

/// The default datahub listen address (mirrors the `backtest` command + the deploy runbook).
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

const USAGE: &str = "usage: vike-cli sweep --profile <run.toml> [--addr 127.0.0.1:7878] [--rank-by sharpe|return|max_dd|equity] [--json]\n       vike-cli sweep --local --profile <run.toml> [--store DIR] [--engine PATH] [--rank-by …] [--json]";

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
    /// `--local`: run the grid on THIS machine by driving the standalone engine — the same flag,
    /// the same argument and the same spawn as `backtest --local`. See [`execute_local`].
    local: bool,
    /// `--store DIR`: the hist-store root the LOCAL engine reads. `--local` only.
    store: Option<String>,
    /// `--engine PATH`: name the standalone engine outright. `--local` only.
    engine: Option<String>,
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `sweep` subcommand;
/// `project_root` is `<project>`, resolved once by [`crate::run`] and needed only by `--local`.
pub fn run(
    args: impl Iterator<Item = String>,
    project_root: Option<&Path>,
    keys: Option<&NodeKeys>,
) -> ExitCode {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("sweep", USAGE, &msg),
    };
    match execute(&args, project_root, keys) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli sweep: {}", e.msg);
            e.exit.into()
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
    let mut local = false;
    let mut store: Option<String> = None;
    let mut engine: Option<String> = None;

    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--profile" => profile_path = Some(flags.value(&flag, inline)?),
            "--addr" => addr = Some(flags.value(&flag, inline)?),
            "--rank-by" => rank_by = Some(parse_rank_by(&flags.value(&flag, inline)?)?),
            "--store" => store = Some(flags.value(&flag, inline)?),
            "--engine" => engine = Some(flags.value(&flag, inline)?),
            "--local" => {
                args::no_value(&flag, inline)?;
                local = true;
            }
            "--json" => {
                args::no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    // The same mode exclusivity the `backtest` sibling enforces, spelled the same way and for the
    // same reason: a flag that describes the wrong machine is refused, never quietly dropped.
    if local {
        if addr.is_some() {
            return Err("--addr names a remote datahub, so it cannot be combined with --local\n\
                        drop one: --local runs the engine on this machine, --addr ships the \
                        profile to a server"
                .to_string());
        }
    } else {
        for (flag, present) in [("--store", store.is_some()), ("--engine", engine.is_some())] {
            if present {
                return Err(format!(
                    "{flag} applies to --local only — a remote run reads the SERVER's store"
                ));
            }
        }
    }

    Ok(Args {
        profile_path: profile_path.ok_or("missing required --profile <run.toml>")?,
        addr: addr.unwrap_or_else(|| DEFAULT_ADDR.to_string()),
        rank_by,
        json,
        local,
        store,
        engine,
    })
}

/// Read the profile, ship its TOML to the datahub server, and print the ranked result. Every
/// failure path funnels into ONE [`CliError`] the caller prints to stderr and exits on — the
/// connect classified explicitly, everything else through `From<String>` on the pre-existing rung.
fn execute(args: &Args, project_root: Option<&Path>, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // ⚠ `--local` diverges BEFORE the profile is SHIPPED, unlike the `backtest` sibling's arm — and
    // the difference is that this command performs no client-side rewrite at all. There is nothing
    // to apply and nothing to stage, so the engine is handed the operator's own file and does its
    // own parsing: one profile parser, which is the property this command's module doc exists to
    // defend. `[sweep]` in that profile is what makes the engine run a grid rather than one point.
    if args.local {
        refuse_a_profile_with_no_grid(&args.profile_path)?;
        let mut argv: Vec<std::ffi::OsString> =
            vec!["--profile".into(), args.profile_path.as_str().into()];
        if let Some(store) = &args.store {
            argv.push("--store".into());
            argv.push(store.into());
        }
        if let Some(rank_by) = &args.rank_by {
            argv.push("--rank-by".into());
            argv.push(rank_by.into());
        }
        if args.json {
            argv.push("--json".into());
        }
        let program = engine::locate(args.engine.as_deref(), project_root);
        return engine::run(&program, &argv, "sweep");
    }

    let profile_toml = std::fs::read_to_string(&args.profile_path)
        .map_err(|e| format!("cannot read profile {}: {e}", args.profile_path))?;

    // The CONNECT rung — the same classification the `backtest` sibling makes at the same site.
    // ⚠ Scope::Control, not Observe: `vike_datahub::server`'s `required_scope` groups every
    // profile-running verb with the WRITE and DESTRUCTIVE ones, because each COMPILES
    // client-supplied Rhai on the server. `None` keeps the unauthenticated dial a key-less
    // server has always answered.
    let mut client = match keys {
        Some(k) => DatahubClient::connect_authed(&args.addr, k, Scope::Control),
        None => DatahubClient::connect(&args.addr),
    }
    .map_err(|e| CliError::connect(format!("cannot connect to datahub at {}: {e}", args.addr)))?;
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

/// The one thing `--local` must check before spawning: **that this profile is actually a sweep.**
///
/// ⚠ The two modes did not agree without it, and the silent one was the local one. The remote path
/// is guarded server-side — `crates/vike-datahub/src/server.rs` answers a profile with no `[sweep]`
/// table with a `Response::Error` this command surfaces as a failure — while the engine has ONE
/// profile path that branches on `BacktestProfile::is_sweep` and, when it is false, runs a SINGLE
/// backtest, prints a single-run table, persists a run and exits 0. So an operator who reused a
/// backtest profile (or dropped the `[sweep]` table by mistake) got a clean refusal from a server
/// and a successful-looking report from their own machine, with `--rank-by` silently inert. That is
/// not "the same grid on this machine", which is exactly what this module's doc promises.
///
/// The SENTENCE is the server's own, verbatim, because the two modes must answer the same question
/// the same way; the RUNG is the pre-existing catch-all, which is what the remote path exits on for
/// the same input.
///
/// ⚠ It refuses only on POSITIVE evidence. An unreadable file, text that is not valid TOML, and a
/// `sweep` key that is present but not a table all fall through to the engine untouched — that is
/// the one profile parser's diagnosis to make, and a second parser guessing at it here is precisely
/// the duplication this command's module doc was written to end. This check answers one question
/// (is there a non-empty `[sweep]` table) and defers everything else.
fn refuse_a_profile_with_no_grid(profile_path: &str) -> CmdResult<()> {
    let Ok(text) = std::fs::read_to_string(profile_path) else { return Ok(()) };
    if declares_a_sweep_grid(&text) == Some(false) {
        return Err(CliError::failed(
            "profile has no [sweep] table — a sweep needs a parameter grid, e.g. \
             `[sweep]\nfast = [5, 10, 15]`",
        ));
    }
    Ok(())
}

/// Whether `profile_toml` carries the non-empty `[sweep]` table a grid needs — `None` when this
/// side cannot tell and the engine's own parser must answer.
///
/// Mirrors `vike_backtest::harness::profile::BacktestProfile::is_sweep` (present AND non-empty),
/// which is the predicate both the engine and the datahub server branch on. It is a PRESENCE check
/// over one key, not a profile parser: nothing here validates a slice, a strategy or a window.
fn declares_a_sweep_grid(profile_toml: &str) -> Option<bool> {
    let doc: toml::Value = toml::from_str(profile_toml).ok()?;
    match doc.get("sweep") {
        None => Some(false),
        Some(toml::Value::Table(t)) => Some(!t.is_empty()),
        // Present and the wrong SHAPE. The engine refuses it with a type error naming the field,
        // which is a better message than anything this side could produce.
        Some(_) => None,
    }
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

    /// The `--local` guard, over the predicate it is built on: a non-empty `[sweep]` table is a
    /// grid, an absent or empty one is NOT (matching `BacktestProfile::is_sweep`), and everything
    /// this side cannot judge is deferred to the engine's own parser rather than guessed at.
    #[test]
    fn a_sweep_grid_is_recognised_and_nothing_else_is_judged() {
        assert_eq!(declares_a_sweep_grid("[sweep]\nfast = [5, 10]\n"), Some(true));
        assert_eq!(declares_a_sweep_grid("[strategy]\nname = \"buy_hold\"\n"), Some(false));
        assert_eq!(declares_a_sweep_grid("[sweep]\n"), Some(false), "an EMPTY table is not a grid");
        assert_eq!(
            declares_a_sweep_grid("not = = toml"),
            None,
            "unparseable is the engine's to say"
        );
        assert_eq!(declares_a_sweep_grid("sweep = 3\n"), None, "…and so is the wrong shape");
    }

    /// …and the refusal itself: the server's own sentence, on the rung the remote path exits with
    /// for the same profile, so the two modes answer one question one way.
    #[test]
    fn a_non_sweep_profile_is_refused_before_anything_is_spawned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profile = dir.path().join("run.toml");
        std::fs::write(&profile, "[strategy]\nname = \"buy_hold\"\n").expect("write profile");
        let path = profile.to_str().expect("utf-8 temp path");

        let err = refuse_a_profile_with_no_grid(path).unwrap_err();
        assert_eq!(err.exit, crate::exit::Exit::Failed);
        assert!(err.msg.contains("no [sweep] table"), "{}", err.msg);
        assert!(err.msg.contains("parameter grid"), "{}", err.msg);

        // A profile that IS a sweep passes, and so does one this side cannot read at all.
        std::fs::write(&profile, "[sweep]\nfast = [5, 10]\n").expect("rewrite profile");
        assert!(refuse_a_profile_with_no_grid(path).is_ok());
        assert!(
            refuse_a_profile_with_no_grid(dir.path().join("absent.toml").to_str().unwrap()).is_ok(),
            "an unreadable profile is the ENGINE's to diagnose, not this check's"
        );
    }

    #[test]
    fn an_empty_grid_renders_without_panicking() {
        let report: Value = serde_json::from_str(r#"{"rows": [], "rank_by": "sharpe"}"#).unwrap();
        print_sweep(&report);
    }
}
