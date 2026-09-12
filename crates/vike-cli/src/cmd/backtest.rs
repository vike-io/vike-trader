//! `vike-cli backtest` — the thin REMOTE backtest command (headless two-layer plan, Layer 1, PR-1).
//!
//! Run a real backtest against a remote `vike-datahub` server (typically the CI box, next to the
//! 1.17B-row tape) from a laptop that has NO DataFusion in its build graph. This is the
//! compute-to-data proof-of-value: ship a small profile (its TOML text), get back the compact
//! report — the heavy history never crosses the wire. Built on ONLY what #719/#725 shipped
//! ([`DatahubClient::run_backtest`] over the existing wire proto): no new protocol, no new
//! dependency.
//!
//! # Usage
//!
//! ```text
//! vike-cli backtest --profile <run.toml> [--preset <p.toml>] [--script <s.rhai>] [--addr 127.0.0.1:7880] [--json]
//! ```
//!
//! - `--profile <path>` (required): a backtest profile `.toml` — its text is read locally and
//!   shipped verbatim; the SERVER parses+validates it with `BacktestProfile::from_toml_str`.
//! - `--preset <path>`: a preset `.toml` — a flat table of a strategy's knobs, merged into the
//!   shipped profile's `[strategy.params]` (see [`merge_preset_params`]).
//! - `--addr <host:port>` (default `127.0.0.1:7880`): the COMPUTE daemon's address — `vike-backend
//!   backtest --addr`, NOT the datahub, since ruling 7 split the served surface. It binds
//!   localhost only; reach a remote one over `ssh -L 7880:localhost:7880 the CI box` (see the runbook,
//!   `docs/ops/datahub-the CI box.md`).
//! - `--json`: print the report JSON verbatim (as the server emitted it). Without it, the JSON is
//!   pretty-printed for a human — or, for a parameter search, rendered as the ranked table.
//!
//! # ⚠ A parameter SEARCH is this verb too, and the PROFILE is what selects one (ruling 13)
//!
//! ```text
//! vike-cli backtest --profile <sweep.toml> --rank-by sharpe [--addr …]
//! vike-cli backtest --local --profile <sweep.toml> --optimizer tpe --trials 128 --seed 7
//! ```
//!
//! There is no `sweep` verb. It existed until ruling 13 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`, which deleted it: the
//! word "optimize" lives in the FLAG (`--optimizer <method>`), a second verb would be a second
//! name for one operation, and *searching a parameter space IS backtesting* — it is backtesting
//! many times and ranking the results. `vike-cli sweep` now fails naming this command.
//!
//! **A profile with a non-empty `[sweep]` table runs a search; everything else runs one backtest.**
//! That predicate is `BacktestProfile::is_sweep`, which the engine and the compute server BOTH
//! branch on, and [`declares_a_sweep_grid`] is this side's presence check over the same key — so
//! all three answer the same question the same way. ⚠ It closes a divergence that predates the
//! merge and is worth stating: the remote arm used to call `run_backtest` unconditionally, whose
//! server-side runner ignores `[sweep]` and reports ONE point, while `--local` handed the same
//! profile to an engine that ran the whole grid. Same command line, two different computations,
//! nothing in the output saying which.
//!
//! ⚠ **What `--local` can do and `--addr` cannot, refused by name rather than downgraded.**
//! `--optimizer euler|tpe`, `--euler-depth`, `--trials`, `--seed` and `--rank-by multi` are
//! LOCAL-ONLY, because `vike_datahub_client::proto`'s `Request::RunSweepProfile` carries the
//! profile TOML and a ranking metric and NO method selector — there is nowhere on that wire to say
//! "tpe". Widening it is a protocol change, not a client one. A silent fallback to the grid is the
//! exact defect #1750 ended when it retired `--search`, so each of these is a named refusal:
//! [`refuse_a_search_knob_the_wire_cannot_carry`].
//!
//! ⚠ `--rank-by` names how to ORDER results, not what work to do, so a VALID value is IGNORED —
//! not an error — on a profile with no `[sweep]` table. That is the engine's documented behaviour
//! and this side does not second-guess it; an INVALID one is refused here, before a spawn or a
//! round trip.
//!
//! ⚠ **One guard was deliberately NOT carried over from `sweep`.** That verb refused a profile
//! with no `[sweep]` table before spawning, because it PROMISED a grid and the engine handed one
//! back a single backtest at exit 0. This verb promises no such thing — the profile decides — so
//! the same input is now one backtest on both routes, which is the correct answer rather than a
//! lost check. What remains covered is the case that DOES ask for work that cannot happen:
//! `--optimizer` on a profile with no grid, which the engine refuses through
//! `SearchFlags::requested` (#1750), and remotely through the refusal above.
//!
//! # `--local`: the same run, on THIS machine, with no server at all
//!
//! ```text
//! vike-cli backtest --local --profile <run.toml> [--store DIR] [--engine PATH] [--json]
//! ```
//!
//! The remote path above is the compute-to-data offload and needs a `vike-datahub` to talk to.
//! `--local` is for the box that already HAS the tape: it drives the standalone `backtest` engine
//! — **spawned, never linked**, because this crate's identity is being DataFusion-free
//! ([`crate::cmd::engine`] carries the whole argument and the search order). `--store` names the
//! hist-store root for that run and `--engine` names the engine outright; both are refused without
//! `--local`, where they would describe the wrong machine.
//!
//! ⚠ A release attaches that engine beside `vike-cli` on **Linux only** — no published manifest
//! carries a `backtest.exe` — so on Windows this arm needs an engine the user built or fetched
//! themselves, and the missing-engine message says so. [`crate::cmd::engine`]'s module doc is the
//! authority on that asymmetry and on what would close it; nothing here restates the release's
//! asset list.
//!
//! ⚠ **`--preset` and `--script` behave identically in both modes**, which is the property that
//! makes a local run a rehearsal for a remote one: they are client-side rewrites of the profile
//! TEXT, applied here, and the local arm hands the engine the rewritten text through a staged file
//! rather than asking it to grow flags it does not have. See [`execute_local`].
//!
//! Exit code: `0` when the server returns a report, and otherwise a rung of [`crate::exit`] — `2`
//! for a bad command line, `3` when the datahub could not be reached, `1` for everything else (an
//! unreadable profile or preset, a server-side `Response::Error`). The failure message goes to
//! stderr on every rung.
//!
//! # ⚠ Why the preset is resolved HERE and not named in the profile
//!
//! Only the profile TEXT crosses the wire, and the server is typically a different machine (the CI box,
//! next to the tape). A `[strategy] preset = "fast"` FIELD would therefore be resolved against the
//! SERVER's `user_data/`, not the author's — the presets a person is editing would be invisible to
//! the run they just started. So a preset is a LOCAL file, read and merged before the request, the
//! same shape `--script` already has and for the same reason.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;
use vike_datahub_client::{DatahubClient, NodeKeys, Scope};

use crate::cmd::args::{self, Flags};
use crate::exit::{CliError, CmdResult};

/// The default COMPUTE-daemon address this command dials.
///
/// ⚠ It is the BACKTEST daemon now, not the datahub (ruling 7 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`): the `Run*` verbs left
/// `vike-backend datahub` for `vike-backend backtest --addr`. Only the ADDRESS changed — the verb,
/// the protocol, the handshake and this command's name are all unchanged (ruling 16). Spelled
/// through `vike_config::DEFAULT_BACKTEST_ADDR` so the client and the daemon cannot answer
/// differently.
const DEFAULT_ADDR: &str = vike_config::DEFAULT_BACKTEST_ADDR;

/// The `[strategy.params]` key carrying a Rhai strategy's SOURCE — what `--script` sets, and the one
/// key a `--preset` file may not define. `crates/vike-backtest/src/harness/registry.rs`'s
/// `rhai_overrides` reserves the same name on the server side.
const SRC_KEY: &str = "src";

/// The container key a preset must NOT wrap its knobs in — see [`merge_preset_params`].
const PARAMS_WRAPPER_KEY: &str = "params";

/// The `--rank-by` names this verb accepts, validated HERE so a typo is a local usage error
/// instead of a wasted round trip. It is a SPELLING check, never a second implementation: the
/// metric itself is computed by `vike_backtest::harness::RankMetric` on whichever side runs.
///
/// ⚠ `multi` is LOCAL-ONLY and that is a WIRE fact, not a preference — see
/// [`refuse_a_search_knob_the_wire_cannot_carry`]. The engine's own `--rank-by` takes it (it picks
/// the composite `vike_backtest::objective` score); the datahub's `RunSweepProfile` resolves
/// `rank_by` through `harness::RankMetric::from_str_ci`, which has four arms and no `multi`, so a
/// remote run carrying it would come back as a server-side error naming a set the CLI advertised.
const RANK_METRICS: [&str; 5] = ["sharpe", "return", "max_dd", "equity", "multi"];

/// The `--optimizer` METHOD names, likewise a spelling check against
/// `crates/vike-backtest/src/backtest_cli.rs`'s `parse_search_flags`.
///
/// ⚠ Only `grid` has a remote route. `vike_datahub_client::proto`'s `Request::RunSweepProfile`
/// carries `profile_toml` and `rank_by` and NO method selector, so there is nowhere on that wire
/// to say "euler" — see [`refuse_a_search_knob_the_wire_cannot_carry`], which refuses rather than
/// silently running the grid. A silent fallback is precisely the `--search`/`--optimizer`
/// precedence defect #1750 was written to end.
const OPTIMIZERS: [&str; 3] = ["grid", "euler", "tpe"];

/// The default `--optimizer`, and the one method the remote route can carry.
const DEFAULT_OPTIMIZER: &str = "grid";

/// The command's own usage roster. `pub(crate)` so `crate::cmd::mcp`'s
/// `the_instructions_name_only_real_commands` can hold the MCP `instructions` text to the
/// subcommands and flags THIS module actually accepts, rather than to a copy of them.
pub(crate) const USAGE: &str = "usage: vike-cli backtest --profile <run.toml> [--preset <p.toml>] [--script <s.rhai>] [--addr 127.0.0.1:7880] [--json]\n       vike-cli backtest --local --profile <run.toml> [--preset <p.toml>] [--script <s.rhai>] [--store DIR] [--engine PATH] [--json]\n       vike-cli backtest --list-params --script <s.rhai>";

/// The parsed `backtest` command line.
#[derive(Debug)]
struct Args {
    /// The backtest profile `.toml`. Required for a run; unused (and optional) for `--list-params`.
    profile_path: Option<String>,
    /// A preset `.toml` whose keys are merged into the shipped profile's `[strategy.params]`.
    preset_path: Option<String>,
    /// An authored Rhai script. With `--list-params` it is the discovery target; otherwise its
    /// source is injected into the shipped profile's `[strategy.params].src`.
    script_path: Option<String>,
    /// Print a Rhai script's tunable `param(name, default)` knobs and exit — OFFLINE, no server.
    list_params: bool,
    addr: String,
    json: bool,
    /// `--local`: run on THIS machine by driving the standalone engine, instead of shipping the
    /// profile to a datahub server. See [`execute_local`].
    local: bool,
    /// `--store DIR`: the hist-store root the LOCAL engine reads. Meaningless remotely — the store
    /// is the server's — so it is refused together with the rest of the remote-only surface.
    store: Option<String>,
    /// `--engine PATH`: name the standalone engine outright instead of searching for it. See
    /// [`crate::cmd::engine`]'s search order, and why this flag exists rather than a variable.
    engine: Option<String>,
    /// `--rank-by`: which metric orders a parameter search's rows. `None` = whichever side runs it
    /// applies its own default (annualized Sharpe).
    ///
    /// ⚠ It names how to ORDER results, not what work to do, so it is IGNORED — not an error — on
    /// a profile with no `[sweep]` table. That is documented engine behaviour
    /// (`crates/vike-backtest/src/backtest_cli.rs`'s `parse_search_flags`) and this side does not
    /// second-guess it.
    rank_by: Option<String>,
    /// `--optimizer`: the search METHOD. `None` = the engine's default (`grid`).
    ///
    /// ⚠ Unlike `--rank-by` this says what WORK to do, which is why a profile with no `[sweep]`
    /// table refuses it engine-side rather than ignoring it.
    optimizer: Option<String>,
    /// The three per-method knobs, forwarded VERBATIM and judged by the engine.
    ///
    /// ⚠ Deliberately not validated here beyond presence. The engine owns the ownership rule (a
    /// `--trials` under `--optimizer euler` is REFUSED, by a table), the ranges and the caps, and
    /// each of its refusals names the method that owns the knob — which is a better message than
    /// anything a second copy of that table could produce. The module doc's "what is validated
    /// here" rule, applied to a fourth family.
    euler_depth: Option<String>,
    trials: Option<String>,
    seed: Option<String>,
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `backtest` subcommand;
/// `project_root` is `<project>`, resolved once by [`crate::run`] — `--local` needs it to find the
/// standalone engine under `bin/` and to stage a rewritten profile under `tmp/`, and it arrives as
/// a PARAMETER because a `src/cmd/` file may not read the environment for itself.
pub fn run(
    args: impl Iterator<Item = String>,
    project_root: Option<&Path>,
    keys: Option<&NodeKeys>,
) -> ExitCode {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("backtest", USAGE, &msg),
    };
    match execute(&args, project_root, keys) {
        Ok(()) => ExitCode::SUCCESS,
        // The message is printed exactly as it always was; only the NUMBER beside it is new. See
        // [`crate::exit`] for what each rung licenses a caller to do about it.
        Err(e) => {
            eprintln!("vike-cli backtest: {}", e.msg);
            e.exit.into()
        }
    }
}

/// Hand-rolled tiny arg parser (no `clap` — PR-1 adds no dependency), over the shared
/// [`crate::cmd::args`] glue: both `--flag value` and `--flag=value`. `--profile` is required;
/// `--addr` defaults to [`DEFAULT_ADDR`]; `--json`/`--list-params` are bare booleans. A
/// `--help`/`-h` short-circuits out through the `Err` channel; [`args::exit_for_parse_error`] is
/// what turns that back into a SUCCESS with the usage on stdout.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut profile_path: Option<String> = None;
    let mut preset_path: Option<String> = None;
    let mut script_path: Option<String> = None;
    let mut list_params = false;
    let mut addr: Option<String> = None;
    let mut json = false;
    let mut local = false;
    let mut store: Option<String> = None;
    let mut engine: Option<String> = None;
    let mut rank_by: Option<String> = None;
    let mut optimizer: Option<String> = None;
    let mut euler_depth: Option<String> = None;
    let mut trials: Option<String> = None;
    let mut seed: Option<String> = None;

    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--profile" => profile_path = Some(flags.value(&flag, inline)?),
            "--preset" => preset_path = Some(flags.value(&flag, inline)?),
            "--script" => script_path = Some(flags.value(&flag, inline)?),
            "--addr" => addr = Some(flags.value(&flag, inline)?),
            "--store" => store = Some(flags.value(&flag, inline)?),
            "--engine" => engine = Some(flags.value(&flag, inline)?),
            // The five SEARCH flags, absorbed from the deleted `vike-cli sweep` (ruling 13). The
            // two SELECTORS are spelling-checked here so a typo costs no round trip and no spawn;
            // the three method KNOBS are forwarded verbatim — see [`Args::euler_depth`].
            "--rank-by" => {
                rank_by = Some(one_of(&flag, &flags.value(&flag, inline)?, &RANK_METRICS)?)
            }
            "--optimizer" => {
                optimizer = Some(one_of(&flag, &flags.value(&flag, inline)?, &OPTIMIZERS)?)
            }
            "--euler-depth" => euler_depth = Some(flags.value(&flag, inline)?),
            "--trials" => trials = Some(flags.value(&flag, inline)?),
            "--seed" => seed = Some(flags.value(&flag, inline)?),
            // ⚠ `--search` is RETIRED on the engine and refused there by name
            // (`crates/vike-backtest/src/backtest_cli.rs`'s `parse_search_flags` carries the
            // argument for why an alias could not be made safe). It never existed on this verb, so
            // it falls into the unknown-argument arm below — which names it, which is the same
            // product.
            "--local" => {
                args::no_value(&flag, inline)?;
                local = true;
            }
            "--json" => {
                args::no_value(&flag, inline)?;
                json = true;
            }
            "--list-params" => {
                args::no_value(&flag, inline)?;
                list_params = true;
            }
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    // `--list-params` is the OFFLINE discovery mode: it needs a script but no profile/server. A run
    // needs a profile. Enforce the two shapes here so `execute` can trust its inputs.
    if list_params {
        if script_path.is_none() {
            return Err("--list-params requires --script <s.rhai>".to_string());
        }
        // ⚠ …and it is a THIRD mode, so it owes the same debt the two run modes owe each other
        // below: a flag it cannot honour is REFUSED, never dropped. `execute` returns from this
        // arm before any of these is looked at — it reads a file and lists the knobs it declares,
        // opening no socket, locating no engine and touching no store — so accepting one would tell
        // an operator their discovery ran against a host, a store or an engine that was never
        // consulted. `--profile` is deliberately NOT in this list: it is documented as unused-and-
        // optional here, so a run and a discovery over the same command line stay spellable.
        for (flag, present) in [
            ("--local", local),
            ("--store", store.is_some()),
            ("--engine", engine.is_some()),
            ("--addr", addr.is_some()),
            ("--json", json),
            // The five SEARCH flags join the same list for the same reason: a discovery runs no
            // backtest at all, so a method or a ranking it accepted would have configured nothing.
            ("--rank-by", rank_by.is_some()),
            ("--optimizer", optimizer.is_some()),
            ("--euler-depth", euler_depth.is_some()),
            ("--trials", trials.is_some()),
            ("--seed", seed.is_some()),
        ] {
            if present {
                return Err(format!(
                    "{flag} does not apply to --list-params, which reads the script on this \
                     machine and lists its knobs — no server, no store, no engine"
                ));
            }
        }
    } else if profile_path.is_none() {
        return Err("missing required --profile <run.toml>".to_string());
    }

    // ⚠ The two RUN MODES are exclusive, and each refuses the other's exclusive flags rather than
    // ignoring them. A `--store` that reached a remote run would name a directory on the wrong
    // machine; an `--addr` typed beside `--local` says the operator believes they are talking to a
    // server. Silently dropping either is how somebody comes to believe a run used a store, or a
    // host, that it never touched.
    if local {
        if addr.is_some() {
            return Err(
                "--addr names a remote backtest daemon, so it cannot be combined with --local\n\
                        drop one: --local runs the engine on this machine, --addr ships the \
                        profile to a server"
                    .to_string(),
            );
        }
    } else {
        for (flag, present) in [("--store", store.is_some()), ("--engine", engine.is_some())] {
            if present {
                return Err(format!(
                    "{flag} applies to --local only — a remote run reads the SERVER's store"
                ));
            }
        }
        // ⚠ …and the search knobs the WIRE cannot carry. Refused by name, never silently downgraded
        // to the grid — see [`refuse_a_search_knob_the_wire_cannot_carry`].
        refuse_a_search_knob_the_wire_cannot_carry(
            optimizer.as_deref(),
            rank_by.as_deref(),
            euler_depth.is_some(),
            trials.is_some(),
            seed.is_some(),
        )?;
    }

    Ok(Args {
        profile_path,
        preset_path,
        script_path,
        list_params,
        addr: addr.unwrap_or_else(|| DEFAULT_ADDR.to_string()),
        json,
        local,
        store,
        engine,
        rank_by,
        optimizer,
        euler_depth,
        trials,
        seed,
    })
}

/// Validate one flag's value against a roster, returning it owned.
///
/// A SPELLING check, never a second implementation: what a metric or a method MEANS is computed by
/// `vike_backtest::harness`, on whichever side runs. Catching a typo here is what makes it a local
/// usage error instead of a wasted round trip or a wasted spawn.
fn one_of(flag: &str, value: &str, roster: &[&str]) -> Result<String, String> {
    if roster.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(format!("{flag} must be {}, got {value:?}", roster.join("|")))
    }
}

/// ⚠ **The search knobs the REMOTE route cannot carry, refused BY NAME rather than downgraded.**
///
/// `vike_datahub_client::proto`'s `Request::RunSweepProfile` carries exactly two fields —
/// `profile_toml` and `rank_by` — and no method selector. There is nowhere on that wire to say
/// "euler", "tpe", a halving depth, a trial budget or a seed; and the server resolves `rank_by`
/// through `vike_backtest::harness::RankMetric::from_str_ci`, whose four arms do not include
/// `multi`.
///
/// So a remote run carrying any of them would either run the GRID while the operator believes they
/// asked for a Bayesian search, or come back as a server-side error naming a metric set this CLI
/// had advertised. Both are the same defect — a selector silently overruled — and it is precisely
/// the one #1750 was written to end when it retired `--search`
/// (`crates/vike-backtest/src/backtest_cli.rs`'s `parse_search_flags` carries that argument).
///
/// ⚠ **This is a NARROWING of the merged verb and it is stated in `--help`, not discovered.**
/// `vike-cli sweep` never carried these knobs either — it took `--rank-by` alone — so nothing
/// regresses; what is new is that `vike-cli backtest --local` can now do everything the engine can,
/// and the remote arm cannot. Widening it means widening the wire, which belongs to the crate that
/// owns the protocol rather than to this one.
fn refuse_a_search_knob_the_wire_cannot_carry(
    optimizer: Option<&str>,
    rank_by: Option<&str>,
    euler_depth: bool,
    trials: bool,
    seed: bool,
) -> Result<(), String> {
    if let Some(method) = optimizer
        && method != DEFAULT_OPTIMIZER
    {
        return Err(format!(
            "--optimizer {method:?} applies to --local only: the wire verb a remote search runs \
             over carries the profile and a ranking metric and NO method selector, so there is \
             nowhere to say {method:?}. Run it with --local, or drop the flag to search the grid \
             on the server."
        ));
    }
    for (flag, present) in [("--euler-depth", euler_depth), ("--trials", trials), ("--seed", seed)]
    {
        if present {
            return Err(format!(
                "{flag} configures a search METHOD, and selecting one is --local only — see \
                 --optimizer. A remote run searches the grid."
            ));
        }
    }
    if rank_by == Some("multi") {
        return Err(
            "--rank-by multi applies to --local only: it selects the composite objective the \
             ENGINE computes, and the wire's ranking field resolves to one of \
             sharpe|return|max_dd|equity. Run it with --local, or rank remotely by one of those."
                .to_string(),
        );
    }
    Ok(())
}

/// Whether `profile_toml` carries the non-empty `[sweep]` table a parameter search needs — `None`
/// when this side cannot tell and the engine's (or the server's) own parser must answer.
///
/// Mirrors `vike_backtest::harness::profile::BacktestProfile::is_sweep` (present AND non-empty),
/// which is the predicate BOTH the engine and the compute server branch on. It is a PRESENCE check
/// over one key, not a profile parser: nothing here validates a slice, a strategy or a window.
///
/// ⚠ **It ROUTES the remote arm, and that closes a live divergence between the two modes.** The
/// engine has always branched on this predicate — `backtest sweep.toml` runs `harness::run_sweep`
/// — so `--local` on a grid profile has always run the grid. The remote arm called
/// `DatahubClient::run_backtest` unconditionally, and the server's `run_backtest` runs
/// `harness::run_backtest`, which IGNORES the `[sweep]` table and reports ONE point. Same profile,
/// same verb, same flags: a grid on this machine and a single backtest on the server, with nothing
/// in the output saying which had happened. Routing here on the same predicate both far sides use
/// is what makes `--local` a rehearsal for `--addr` on a search, which is what this module's doc
/// already promises for `--preset` and `--script`.
///
/// ⚠ It runs on the REWRITTEN text — after `--preset` and `--script` are merged — so a profile
/// whose grid arrives through a preset routes the same way its contents say it should.
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

/// Print a server `SweepReport` as a human table — one row per grid point, in the order the SERVER
/// ranked them. Every number printed is read straight out of that row's `BacktestReport`; nothing
/// is recomputed here, which is the workspace rule that no metric is reimplemented outside
/// `vike-backtest`.
fn print_sweep(report: &Value) {
    let rows = report.get("rows").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    if rows.is_empty() {
        println!("(the search returned no grid points)");
        return;
    }
    let rank_by = report.get("rank_by").and_then(Value::as_str).unwrap_or("sharpe");
    println!("parameter search — {} point(s), ranked server-side by {rank_by}", rows.len());
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
            // A per-point failure never fails the whole search server-side — it comes back as this
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

/// Read the profile, ship it to the datahub server, and print the report — pretty by default, raw
/// under `--json`. Every failure path funnels into ONE [`CliError`] the caller prints to stderr and
/// exits on; the ones that are not explicitly classified arrive through `From<String>` on the
/// pre-existing rung, which is what let this file be converted without re-judging every `?`.
fn execute(args: &Args, project_root: Option<&Path>, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // --list-params: OFFLINE — read the script, print the tunable knobs it declares, done. No
    // profile, no server (an author inspects a script's `param(name, default)` surface locally).
    if args.list_params {
        // Unreachable in practice — `parse_args` refuses the shape — but it is a USAGE error if it
        // ever is reached, and saying so here keeps the two statements of the same rule in step.
        let path = args
            .script_path
            .as_deref()
            .ok_or_else(|| CliError::usage("--list-params requires --script <s.rhai>"))?;
        let src =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read script {path}: {e}"))?;
        let params =
            vike_script::discover_params(&src).map_err(|e| format!("rhai compile error: {e}"))?;
        if params.is_empty() {
            println!("(no tunable params — the script declares no param(name, default) calls)");
        } else {
            for (name, default) in params {
                println!("{name} = {default}");
            }
        }
        return Ok(());
    }

    let profile_path = args
        .profile_path
        .as_deref()
        .ok_or_else(|| CliError::usage("missing required --profile <run.toml>"))?;
    let mut profile_toml = std::fs::read_to_string(profile_path)
        .map_err(|e| format!("cannot read profile {profile_path}: {e}"))?;

    // --preset: merge the preset file's knobs into `[strategy.params]`. BEFORE `--script`, so the
    // script's own `src` can never be shadowed by anything a params file carries (and a preset
    // carrying `src` at all is refused outright — see `merge_preset_params`).
    if let Some(preset_path) = &args.preset_path {
        let preset = std::fs::read_to_string(preset_path)
            .map_err(|e| format!("cannot read preset {preset_path}: {e}"))?;
        profile_toml = merge_preset_params(&profile_toml, &preset)
            .map_err(|e| format!("preset {preset_path}: {e}"))?;
    }

    // --script: inject the .rhai file's SOURCE into the profile's `[strategy.params].src`, so an
    // authored strategy ships self-contained — the (possibly remote) datahub server has no access
    // to this client's filesystem, only the profile text.
    if let Some(script_path) = &args.script_path {
        let src = std::fs::read_to_string(script_path)
            .map_err(|e| format!("cannot read script {script_path}: {e}"))?;
        profile_toml = inject_script_src(&profile_toml, &src)?;
    }

    // ⚠ `--local` DIVERGES HERE, and everything above it is deliberately shared: the profile has
    // been read and both client-side rewrites have been applied, so a local run and a remote run
    // are given byte-identical profile TEXT. That is the whole point of putting the branch this
    // far down — `--preset` and `--script` are resolved against THIS filesystem in both modes, and
    // a local run that quietly resolved them differently would answer differently from the remote
    // run it is supposed to be a rehearsal for.
    if args.local {
        return execute_local(args, project_root, profile_path, &profile_toml);
    }

    // ⚠ THE one site on this path that is worth its own rung: the server was not there. Same
    // sentence as before, on the CONNECT rung — the caller that should back off and retry (or go
    // open its SSH tunnel) can now tell this apart from a profile it typed wrong.
    // ⚠ Scope::Control, not Observe: `vike_datahub_client::proto`'s `required_scope` groups every
    // profile-running verb with the WRITE and DESTRUCTIVE ones, because each COMPILES
    // client-supplied Rhai on the server. `None` keeps the unauthenticated dial a key-less
    // server has always answered.
    let mut client = match keys {
        Some(k) => DatahubClient::connect_authed(&args.addr, k, Scope::Control),
        None => DatahubClient::connect(&args.addr),
    }
    .map_err(|e| CliError::connect(format!("cannot connect to the backtest daemon at {}: {e} (start it with `vike-backend backtest --addr`)", args.addr)))?;

    // ⚠ **THE PROFILE ROUTES, not a flag** — and this is where `vike-cli sweep` went (ruling 13:
    // there is no second verb, `--optimizer` is where the word "optimize" is spelled). A profile
    // declaring a non-empty `[sweep]` grid is a parameter SEARCH and goes over `RunSweepProfile`;
    // everything else is one backtest over `RunBacktest`.
    //
    // ⚠ It also closes a divergence that predates the merge. The ENGINE has always branched on
    // this same predicate, so `--local` on a grid profile ran the grid — while this arm called
    // `run_backtest` unconditionally, and the server's `run_backtest` runs `harness::run_backtest`,
    // which ignores the `[sweep]` table and reports ONE point. Same profile, same verb, same
    // flags: a grid here and a single backtest there, with nothing in the output saying which had
    // happened. See [`declares_a_sweep_grid`].
    //
    // `None` — a profile this side cannot parse, or a `sweep` key of the wrong shape — routes to
    // `RunBacktest`, whose `BacktestProfile::from_toml_str` produces the type error naming the
    // field. One profile parser; this side guesses at nothing.
    let searching = declares_a_sweep_grid(&profile_toml) == Some(true);
    let report_json = if searching {
        // A transport failure and a server-side `Response::Error` both arrive as `Err(String)`
        // and stay on the pre-existing rung: once the connection is open, a failure is the run's.
        client.run_sweep_profile(&profile_toml, args.rank_by.as_deref())?
    } else {
        client.run_backtest(&profile_toml)?
    };

    if args.json {
        // Verbatim: exactly the JSON the server emitted (the `backtest --json` shape, or the
        // `SweepReport` shape for a search — the two are different documents and always were).
        println!("{report_json}");
        return Ok(());
    }
    // Re-parse to a generic `Value` so we do not need `BacktestReport` to derive `Deserialize` (it
    // does not yet — see the proto doc); a parse failure means the server sent something that is
    // not the report JSON, which is worth surfacing.
    let value: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server report was not valid JSON: {e}"))?;
    if searching {
        // The ranked table, rendered from the SERVER's own per-row statistics in the SERVER's own
        // order. Absorbed from the deleted `sweep` verb unchanged — see [`print_sweep`].
        print_sweep(&value);
    } else {
        let pretty = serde_json::to_string_pretty(&value)
            .map_err(|e| format!("cannot pretty-print report: {e}"))?;
        println!("{pretty}");
    }
    Ok(())
}

/// The `--local` arm: run the backtest on THIS machine by driving the standalone engine.
///
/// ⚠ **It spawns rather than links, and that is not a shortcut** — `crates/vike-cli/src/cmd/
/// engine.rs` carries the whole argument (this crate's identity is DataFusion-free, and the engine
/// opens a `DataFusionHist`), the search order, and the fold from the child's exit status onto this
/// crate's ladder.
///
/// # What is forwarded, and the one thing that is NOT
///
/// `--profile`, `--store` and `--json` go to the child as themselves. `--preset` and `--script` do
/// NOT: they are CLIENT-SIDE rewrites (`merge_preset_params`, `inject_script_src`) that the engine
/// has no flags for, and asking it to grow them would put a second copy of the merge rules in the
/// tree. The rewritten profile is staged under `<project>/tmp` instead — through
/// `vike_model::scratch::ScratchDir`, which owns the directory and removes it on drop, including on
/// the panic path — and the child is handed THAT path.
///
/// So the profile the local engine parses is byte-identical to the text a remote datahub would have
/// been shipped, which is the property that makes `--local` a rehearsal for `--addr` rather than a
/// second, subtly different run mode.
fn execute_local(
    args: &Args,
    project_root: Option<&Path>,
    profile_path: &str,
    profile_toml: &str,
) -> CmdResult<()> {
    // Nothing to rewrite ⇒ nothing to stage: hand the child the operator's own file. This is the
    // ordinary case, and keeping it scratch-free is what lets `--local` work in a checkout that
    // has no project above it at all.
    let rewritten = args.preset_path.is_some() || args.script_path.is_some();
    // ⚠ Held for the whole call: `ScratchDir`'s `Drop` is what removes the staged profile, so
    // binding it to `_` (rather than to a name) would delete the file before the child reads it.
    let staged;
    let profile_arg: &Path = if rewritten {
        let root = crate::cmd::engine::scratch_root(project_root).ok_or_else(|| {
            CliError::failed(
                "--local with --preset/--script rewrites the profile before running it, and there \
                 is no project above the working directory to stage the rewrite in.\nRun inside \
                 your project (or set $VIKE_SETTINGS_DIR), or pass an already-merged profile to \
                 --profile",
            )
        })?;
        staged = StagedProfile::write(&root, profile_toml)?;
        staged.path()
    } else {
        Path::new(profile_path)
    };

    let mut argv: Vec<std::ffi::OsString> = vec!["--profile".into(), profile_arg.into()];
    if let Some(store) = &args.store {
        argv.push("--store".into());
        argv.push(store.into());
    }
    // ⚠ The five SEARCH flags go to the child as THEMSELVES, spelled identically — they are the
    // engine's own flags (#1750's `parse_search_flags`), so this is forwarding rather than
    // translation. Nothing here decides whether a search HAPPENS: the profile's `[sweep]` table
    // does, on both sides, which is what makes `--local` a rehearsal for `--addr`.
    //
    // ⚠ The three method KNOBS are forwarded unvalidated on purpose. The engine owns the ownership
    // rule (`--trials` under `--optimizer euler` is REFUSED, from a table), the ranges and the
    // caps, and each refusal names the method that owns the knob. A second copy of that table here
    // would be one more thing to keep in step and could only ever produce a worse message.
    for (flag, value) in [
        ("--rank-by", &args.rank_by),
        ("--optimizer", &args.optimizer),
        ("--euler-depth", &args.euler_depth),
        ("--trials", &args.trials),
        ("--seed", &args.seed),
    ] {
        if let Some(v) = value {
            argv.push(flag.into());
            argv.push(v.into());
        }
    }
    if args.json {
        argv.push("--json".into());
    }
    let program = crate::cmd::engine::locate(args.engine.as_deref(), project_root);
    crate::cmd::engine::run(&program, &argv, "backtest")
}

/// A profile written into `<project>/tmp` for a child process to read, removed when this value is
/// dropped.
///
/// A thin wrapper rather than a bare path because the OWNERSHIP is the point: the directory guard
/// has to outlive the child, and a function returning a `PathBuf` out of a dropped `ScratchDir`
/// would compile and then hand the engine a file that is already gone.
struct StagedProfile {
    /// The guard. Never read — its `Drop` is the whole job — and named rather than `_` so it is
    /// obvious that dropping it early is what breaks this.
    _dir: vike_model::scratch::ScratchDir,
    path: PathBuf,
}

impl StagedProfile {
    fn write(scratch_root: &Path, profile_toml: &str) -> CmdResult<Self> {
        let dir = vike_model::scratch::ScratchDir::create_in(scratch_root, "vike-cli-local")
            .map_err(|e| {
                CliError::failed(format!(
                    "cannot create a scratch directory under {}: {e}",
                    scratch_root.display()
                ))
            })?;
        let path = dir.path().join("profile.toml");
        std::fs::write(&path, profile_toml).map_err(|e| {
            CliError::failed(format!(
                "cannot stage the rewritten profile at {}: {e}",
                path.display()
            ))
        })?;
        Ok(Self { _dir: dir, path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

/// Inject an authored Rhai script's `src` into a profile's `[strategy.params].src`, returning the
/// re-serialized profile TOML to ship. Parses the profile as a `toml::Value` (a bad profile is a
/// clean error, not a panic), creates `[strategy]`/`[strategy.params]` if absent, and sets/overwrites
/// `src`. Existing params (the numeric knobs a `[sweep]` varies) are preserved. Any pre-existing
/// inline `src` is overwritten — the `--script` file wins.
pub(crate) fn inject_script_src(profile_toml: &str, script_src: &str) -> Result<String, String> {
    let mut doc: toml::Value =
        toml::from_str(profile_toml).map_err(|e| format!("profile is not valid TOML: {e}"))?;
    strategy_params_mut(&mut doc)?
        .insert(SRC_KEY.to_string(), toml::Value::String(script_src.to_string()));
    toml::to_string(&doc)
        .map_err(|e| format!("cannot re-serialize profile after --script inject: {e}"))
}

/// Merge a PRESET's knobs into a profile's `[strategy.params]`, returning the re-serialized profile
/// TOML to ship — the `--preset` half of the same client-side rewrite [`inject_script_src`] does.
///
/// A preset IS the params table: a FLAT table of a strategy's knobs, whose keys land in
/// `[strategy.params]` one for one, last-wins over anything the profile already set there. Every
/// other key of the profile is untouched.
///
/// # Two shapes are REFUSED, both because accepting them would do nothing visible
///
/// * **A `[params]` wrapper.** Merged as-is it would give the strategy one parameter called
///   `params` that no `from_params` reader looks at, while every knob silently kept its default —
///   the worst available outcome, because the user did everything else right. Silently UNWRAPPING it
///   instead would make two shapes legal and pick between them by an invisible rule.
/// * **A `src` key**, which is the strategy's SOURCE rather than a knob. Allowing it would let a
///   params file smuggle a whole script past `--script`, and two overwrite rules interacting is
///   exactly how a silent surprise gets built.
///
/// ⚠ **This rule is stated in two crates and that is deliberate.**
/// `crates/vike-studio-core/src/user_strategies/load.rs`'s `check_preset_shape` is the authority and
/// carries the full argument; this CLI cannot call it, because `vike-studio-core` depends on
/// `vike-data/hist-datafusion` and this crate's whole identity is being DataFusion-free on the fast
/// lane (see the `[dependencies]` rationale in `crates/vike-cli/Cargo.toml`). The rule is ten lines
/// and the alternative is dragging Arrow into a laptop binary.
pub(crate) fn merge_preset_params(profile_toml: &str, preset_toml: &str) -> Result<String, String> {
    let preset: toml::Value =
        toml::from_str(preset_toml).map_err(|e| format!("not valid TOML: {e}"))?;
    let knobs = preset.as_table().ok_or("a preset must be a table of parameters")?;
    if knobs.len() == 1 && knobs.get(PARAMS_WRAPPER_KEY).is_some_and(toml::Value::is_table) {
        return Err(format!(
            "it wraps its knobs in a [{PARAMS_WRAPPER_KEY}] table, so the strategy would receive \
             one parameter called '{PARAMS_WRAPPER_KEY}' that nothing reads and every knob would \
             keep its default. A preset IS the params table: delete the [{PARAMS_WRAPPER_KEY}] \
             header and leave the keys at the top level"
        ));
    }
    if knobs.contains_key(SRC_KEY) {
        return Err(format!(
            "it defines '{SRC_KEY}', which is the strategy's SOURCE rather than one of its knobs — \
             that is what `--script` is for. Delete the '{SRC_KEY}' key"
        ));
    }

    let mut doc: toml::Value =
        toml::from_str(profile_toml).map_err(|e| format!("profile is not valid TOML: {e}"))?;
    let params = strategy_params_mut(&mut doc)?;
    for (key, value) in knobs {
        params.insert(key.clone(), value.clone());
    }
    toml::to_string(&doc)
        .map_err(|e| format!("cannot re-serialize profile after --preset merge: {e}"))
}

/// The profile's `[strategy.params]` table, CREATING `[strategy]` and `[strategy.params]` when
/// absent — the one place both client-side rewrites above reach into a profile, so they cannot
/// disagree about where params live or about what a non-table there means.
fn strategy_params_mut(
    doc: &mut toml::Value,
) -> Result<&mut toml::map::Map<String, toml::Value>, String> {
    let root = doc.as_table_mut().ok_or("profile root is not a TOML table")?;
    let strategy = root
        .entry("strategy")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or("[strategy] is not a table")?;
    strategy
        .entry("params")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| "[strategy.params] is not a table".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_sets_src_and_preserves_existing_params() {
        let profile = "[strategy]\nname = \"rhai\"\n[strategy.params]\nqty = 2.0\n\n[data]\nvenue = \"sim\"\n";
        let out = inject_script_src(profile, "fn on_bar() { buy(1.0); }").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["strategy"]["params"]["src"].as_str(), Some("fn on_bar() { buy(1.0); }"));
        assert_eq!(v["strategy"]["params"]["qty"].as_float(), Some(2.0)); // knob preserved
        assert_eq!(v["strategy"]["name"].as_str(), Some("rhai")); // rest of the profile intact
        assert_eq!(v["data"]["venue"].as_str(), Some("sim"));
    }

    #[test]
    fn inject_creates_strategy_params_when_absent() {
        let out = inject_script_src("[data]\nvenue = \"sim\"\n", "fn on_bar() {}").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["strategy"]["params"]["src"].as_str(), Some("fn on_bar() {}"));
    }

    #[test]
    fn inject_overwrites_a_prior_inline_src() {
        let profile = "[strategy]\nname = \"rhai\"\n[strategy.params]\nsrc = \"OLD\"\n";
        let out = inject_script_src(profile, "NEW").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["strategy"]["params"]["src"].as_str(), Some("NEW"));
    }

    #[test]
    fn inject_rejects_malformed_profile_toml() {
        assert!(inject_script_src("this is [not valid", "fn on_bar() {}").is_err());
    }

    #[test]
    fn list_params_requires_a_script() {
        let err = parse_args(["--list-params".to_string()].into_iter()).unwrap_err();
        assert!(err.contains("--script"), "names the missing flag: {err}");
    }

    /// `--list-params` is a THIRD mode and refuses what it cannot honour, exactly as the two run
    /// modes refuse each other's flags. `execute` returns from that arm before a socket, an engine
    /// or a store is looked at, so accepting one of these would leave an operator believing their
    /// discovery consulted something it never touched.
    #[test]
    fn list_params_refuses_the_flags_it_would_otherwise_drop() {
        for extra in [
            vec!["--local"],
            vec!["--store", "/data"],
            vec!["--engine", "/opt/backtest"],
            vec!["--addr", "1.2.3.4:9"],
            vec!["--json"],
        ] {
            let mut argv = vec!["--list-params".to_string(), "--script".to_string()];
            argv.push("s.rhai".to_string());
            argv.extend(extra.iter().map(|s| s.to_string()));
            let Err(err) = parse_args(argv.into_iter()) else {
                panic!("{extra:?} must be refused under --list-params, not silently dropped");
            };
            assert!(err.contains(extra[0]), "the message names the flag: {err}");
            assert!(err.contains("--list-params"), "…and the mode that refused it: {err}");
        }
    }

    #[test]
    fn a_run_still_requires_a_profile() {
        let err = parse_args(["--json".to_string()].into_iter()).unwrap_err();
        assert!(err.contains("--profile"), "names the missing flag: {err}");
    }

    #[test]
    fn script_and_profile_parse_together() {
        let args =
            parse_args(["--profile", "p.toml", "--script", "s.rhai"].map(String::from).into_iter())
                .unwrap();
        assert_eq!(args.profile_path.as_deref(), Some("p.toml"));
        assert_eq!(args.script_path.as_deref(), Some("s.rhai"));
        assert!(!args.list_params);
    }

    // ---- --preset ------------------------------------------------------------------------------

    #[test]
    fn preset_parses_beside_the_profile_and_the_script() {
        let args = parse_args(
            ["--profile", "p.toml", "--preset", "fast.toml", "--script", "s.rhai"]
                .map(String::from)
                .into_iter(),
        )
        .unwrap();
        assert_eq!(args.preset_path.as_deref(), Some("fast.toml"));
        assert_eq!(args.script_path.as_deref(), Some("s.rhai"));
    }

    /// THE merge: a preset's keys land in `[strategy.params]`, keeping their TOML types, without
    /// disturbing anything else the profile said.
    #[test]
    fn merge_lands_every_knob_in_strategy_params_and_leaves_the_rest_alone() {
        let profile = "[strategy]\nname = \"buy_hold\"\n[strategy.params]\nqty = 1.0\n\n[data]\nvenue = \"sim\"\n";
        let out = merge_preset_params(profile, "size = 3\nsymbol = \"BTCUSDT\"\n").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["strategy"]["params"]["size"].as_integer(), Some(3));
        assert_eq!(v["strategy"]["params"]["symbol"].as_str(), Some("BTCUSDT"));
        assert_eq!(v["strategy"]["params"]["qty"].as_float(), Some(1.0), "un-preset knob survives");
        assert_eq!(v["strategy"]["name"].as_str(), Some("buy_hold"));
        assert_eq!(v["data"]["venue"].as_str(), Some("sim"));
    }

    /// The preset WINS over a value the profile already set — it is the more specific instruction,
    /// typed on the command line for this run.
    #[test]
    fn a_preset_key_overrides_the_profiles_own_value() {
        let out = merge_preset_params("[strategy.params]\nsize = 1\n", "size = 9\n").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["strategy"]["params"]["size"].as_integer(), Some(9));
    }

    #[test]
    fn merge_creates_strategy_params_when_absent() {
        let out = merge_preset_params("[data]\nvenue = \"sim\"\n", "size = 2\n").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["strategy"]["params"]["size"].as_integer(), Some(2));
    }

    /// A nested knob table is a legitimate preset (`funding_carry` reads `[venues]` straight out of
    /// `[strategy.params]`), so only the EXACT lone-`[params]` wrapper is refused.
    #[test]
    fn a_nested_knob_table_merges_intact() {
        let out = merge_preset_params(
            "[strategy]\nname = \"funding_carry\"\n",
            "cooldown_ms = 500\n[venues]\nBTCUSDT = \"binance\"\n",
        )
        .unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["strategy"]["params"]["venues"]["BTCUSDT"].as_str(), Some("binance"));
        assert_eq!(v["strategy"]["params"]["cooldown_ms"].as_integer(), Some(500));
    }

    /// ⚠ The `[params]`-wrapped shape is REFUSED with the fix in the message. Merged as-is it would
    /// give the strategy one key nothing reads while every knob silently kept its default.
    #[test]
    fn a_params_wrapped_preset_is_refused_naming_the_fix() {
        let err = merge_preset_params("[strategy]\nname = \"buy_hold\"\n", "[params]\nsize = 3\n")
            .unwrap_err();
        assert!(err.contains("[params]"), "names the offending header: {err}");
        assert!(err.contains("top level"), "names the fix: {err}");
    }

    /// …and a preset that legitimately has ONE knob does not trip that rule just by being small.
    #[test]
    fn a_single_scalar_preset_is_not_mistaken_for_a_wrapper() {
        let out = merge_preset_params("[strategy.params]\n", "params = 3\n").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(
            v["strategy"]["params"]["params"].as_integer(),
            Some(3),
            "only a lone `params` TABLE is the wrapper shape"
        );
    }

    /// A preset may not carry `src`: that would smuggle a whole script past `--script`.
    #[test]
    fn a_preset_that_defines_src_is_refused() {
        let err =
            merge_preset_params("[strategy.params]\n", "src = \"fn on_bar() {}\"\n").unwrap_err();
        assert!(err.contains("src"), "names the offending key: {err}");
        assert!(err.contains("--script"), "names what to use instead: {err}");
    }

    #[test]
    fn merge_rejects_malformed_preset_and_profile_toml() {
        assert!(merge_preset_params("[strategy.params]\n", "size = = 3").is_err());
        assert!(merge_preset_params("this is [not valid", "size = 3").is_err());
    }

    /// ORDER: preset first, then `--script`, so a stale `src` in the profile cannot survive and the
    /// script file always wins. (A preset carrying `src` is refused before either runs.)
    #[test]
    fn the_script_wins_over_whatever_the_preset_left_behind() {
        let profile = "[strategy]\nname = \"rhai\"\n[strategy.params]\nsrc = \"OLD\"\n";
        let merged = merge_preset_params(profile, "fast = 5\n").unwrap();
        let out = inject_script_src(&merged, "NEW").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["strategy"]["params"]["src"].as_str(), Some("NEW"));
        assert_eq!(v["strategy"]["params"]["fast"].as_integer(), Some(5));
    }
}
