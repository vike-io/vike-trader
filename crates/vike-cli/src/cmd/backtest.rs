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
//! vike-cli backtest --profile <run.toml> [--preset <p.toml>] [--script <s.rhai>] [--addr 127.0.0.1:7878] [--json]
//! ```
//!
//! - `--profile <path>` (required): a backtest profile `.toml` — its text is read locally and
//!   shipped verbatim; the SERVER parses+validates it with `BacktestProfile::from_toml_str`.
//! - `--preset <path>`: a preset `.toml` — a flat table of a strategy's knobs, merged into the
//!   shipped profile's `[strategy.params]` (see [`merge_preset_params`]).
//! - `--addr <host:port>` (default `127.0.0.1:7878`): the datahub server address. The server binds
//!   localhost only; reach a remote one over `ssh -L 7878:localhost:7878 the CI box` (see the runbook,
//!   `docs/ops/datahub-the CI box.md`).
//! - `--json`: print the report JSON verbatim (as the server emitted it). Without it, the JSON is
//!   pretty-printed for a human.
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

use vike_datahub_client::{DatahubClient, NodeKeys, Scope};

use crate::cmd::args::{self, Flags};
use crate::exit::{CliError, CmdResult};

/// The default datahub listen address (mirrors `VIKE_DATAHUB_ADDR`'s default in the server bin).
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

/// The `[strategy.params]` key carrying a Rhai strategy's SOURCE — what `--script` sets, and the one
/// key a `--preset` file may not define. `crates/vike-backtest/src/harness/registry.rs`'s
/// `rhai_overrides` reserves the same name on the server side.
const SRC_KEY: &str = "src";

/// The container key a preset must NOT wrap its knobs in — see [`merge_preset_params`].
const PARAMS_WRAPPER_KEY: &str = "params";

/// The command's own usage roster. `pub(crate)` so `crate::cmd::mcp`'s
/// `the_instructions_name_only_real_commands` can hold the MCP `instructions` text to the
/// subcommands and flags THIS module actually accepts, rather than to a copy of them.
pub(crate) const USAGE: &str = "usage: vike-cli backtest --profile <run.toml> [--preset <p.toml>] [--script <s.rhai>] [--addr 127.0.0.1:7878] [--json]\n       vike-cli backtest --local --profile <run.toml> [--preset <p.toml>] [--script <s.rhai>] [--store DIR] [--engine PATH] [--json]\n       vike-cli backtest --list-params --script <s.rhai>";

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

    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--profile" => profile_path = Some(flags.value(&flag, inline)?),
            "--preset" => preset_path = Some(flags.value(&flag, inline)?),
            "--script" => script_path = Some(flags.value(&flag, inline)?),
            "--addr" => addr = Some(flags.value(&flag, inline)?),
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
        profile_path,
        preset_path,
        script_path,
        list_params,
        addr: addr.unwrap_or_else(|| DEFAULT_ADDR.to_string()),
        json,
        local,
        store,
        engine,
    })
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
    // ⚠ Scope::Control, not Observe: `vike_datahub::server`'s `required_scope` groups every
    // profile-running verb with the WRITE and DESTRUCTIVE ones, because each COMPILES
    // client-supplied Rhai on the server. `None` keeps the unauthenticated dial a key-less
    // server has always answered.
    let mut client = match keys {
        Some(k) => DatahubClient::connect_authed(&args.addr, k, Scope::Control),
        None => DatahubClient::connect(&args.addr),
    }
    .map_err(|e| CliError::connect(format!("cannot connect to datahub at {}: {e}", args.addr)))?;

    // A transport failure and a server-side `Response::Error` both arrive as `Err(String)` here,
    // and both stay on the pre-existing rung: once the connection is open, a failure is the run's,
    // not the connection's.
    let report_json = client.run_backtest(&profile_toml)?;

    if args.json {
        // Verbatim: exactly the JSON the server emitted (the `backtest --json` shape).
        println!("{report_json}");
    } else {
        // Pretty-print for a human. Re-parse to a generic `Value` so we do not need `BacktestReport`
        // to derive `Deserialize` (it does not yet — see the proto doc); a parse failure means the
        // server sent something that is not the report JSON, which is worth surfacing.
        let value: serde_json::Value = serde_json::from_str(&report_json)
            .map_err(|e| format!("server report was not valid JSON: {e}"))?;
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
