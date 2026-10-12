//! `run` executed: rewrite the profile, route it, then dial the daemon or spawn the engine.

use std::path::Path;

use serde_json::Value;
use vike_datahub_client::DatahubClient;
use vike_node_proto::auth::{NodeKeys, Scope};

use crate::exit::{CliError, CmdResult};

use super::profile::{
    StagedProfile, build_profile_toml, inject_script_src, merge_preset_params, render_effective,
};
use super::render::print_paramscan;
use super::route::{Route, WALKFORWARD_HAS_NO_LOCAL_ARM, refuse_a_walkforward_flag, route_of};
use super::{Args, resolve_addr};

/// Read the profile, ship it to the datahub server, and print the report — pretty by default, raw
/// under `--json`. Every failure path funnels into ONE [`CliError`] the caller prints to stderr and
/// exits on; the ones that are not explicitly classified arrive through `From<String>` on the
/// pre-existing rung, which is what let this file be converted without re-judging every `?`.
pub(super) fn execute(
    args: &Args,
    project_root: Option<&Path>,
    user_data_dir: Option<&Path>,
    configured_addr: Option<&str>,
    keys: Option<&NodeKeys>,
) -> CmdResult<()> {
    // THE INVERSION (spec §5.3). `--profile` is now a BASE rather than the whole input: the file's
    // text, when there is one, is the document every override is applied onto. Everything below
    // this point sees only the resulting TEXT, which is what keeps `--local` a byte-identical
    // rehearsal of `--addr`.
    let base = match args.profile_path.as_deref() {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read profile {path}: {e}"))?,
        ),
        None => None,
    };
    let mut profile_toml =
        build_profile_toml(base.as_deref(), &args.overrides).map_err(CliError::usage)?;

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

    // ⚠ BELOW both rewrites, deliberately: what `--write-profile` writes and what `--show-effective`
    // prints must be the text that would ACTUALLY have been run, `--preset`'s merge and
    // `--script`'s injected source included. Writing the pre-rewrite document would produce a file
    // that runs differently from the command line that produced it, which is the one thing spec
    // §15.4's round-trip gate exists to prevent.
    if let Some(path) = &args.write_profile {
        if std::path::Path::new(path).exists() {
            return Err(CliError::usage(format!(
                "--write-profile {path} already exists, and this command will not overwrite a \
                 profile — delete it, or name a different path"
            )));
        }
        std::fs::write(path, &profile_toml)
            .map_err(|e| format!("cannot write the profile to {path}: {e}"))?;
    }
    if args.show_effective {
        print!("{}", render_effective(&profile_toml, &args.overrides));
        return Ok(());
    }

    // ⚠ THE PROFILE ROUTES, not a flag — ruling 7's two axes, folded in ONE function so the
    // carrier choice and the fall-throughs cannot come to disagree. Computed on the REWRITTEN
    // text, for the reason the `--local` branch below is placed this far down: a local run and a
    // remote run are given byte-identical profile text, so they must route identically too. It is
    // INFALLIBLE (ruling 5 withdrew its only refusal), so there is nothing to lift onto an exit
    // rung here.
    let route = route_of(&profile_toml, args.search_requested());

    if route == Route::Walkforward {
        // Refused BEFORE the `--local` branch, so a walk-forward never reaches
        // `crate::cmd::engine`'s search for a binary that could not run it anyway.
        if args.local {
            return Err(CliError::usage(WALKFORWARD_HAS_NO_LOCAL_ARM));
        }
        refuse_a_walkforward_flag(args).map_err(CliError::usage)?;
    }

    // ⚠ `--local` DIVERGES HERE, and everything above it is deliberately shared: the profile has
    // been read and both client-side rewrites have been applied, so a local run and a remote run
    // are given byte-identical profile TEXT. That is the whole point of putting the branch this
    // far down — `--preset` and `--script` are resolved against THIS filesystem in both modes, and
    // a local run that quietly resolved them differently would answer differently from the remote
    // run it is supposed to be a rehearsal for.
    if args.local {
        return execute_local(
            args,
            project_root,
            user_data_dir,
            args.profile_path.as_deref(),
            &profile_toml,
        );
    }

    // ⚠ THE one site on this path that is worth its own rung: the server was not there. Same
    // sentence as before, on the CONNECT rung — the caller that should back off and retry (or go
    // open its SSH tunnel) can now tell this apart from a profile it typed wrong.
    // ⚠ Scope::Write, not Observe: `vike_datahub_client::proto`'s `required_scope` groups every
    // profile-running verb with the WRITE and DESTRUCTIVE ones, because each COMPILES
    // client-supplied Rhai on the server. `None` keeps the unauthenticated dial a key-less
    // server has always answered.
    // ⚠ THE LADDER IS FOLDED HERE, not in the parser: `config.backtest_addr` is resolved by the
    // composition root and handed in, because a `src/cmd/` file reads no settings of its own.
    let addr = resolve_addr(args.addr.as_deref(), configured_addr);
    let mut client = match keys {
        Some(k) => DatahubClient::connect_authed(&addr, k, Scope::Write),
        None => DatahubClient::connect(&addr),
    }
    .map_err(|e| CliError::connect(format!("cannot connect to the backtest daemon at {addr}: {e} (start it with `vike-backend backtest --addr`)")))?;

    // ⚠ **THE PROFILE ROUTES, not a flag** — and this is where `vike-cli sweep` went (ruling 13:
    // there is no second verb, `--optimizer` is where the word "optimize" is spelled) and, since
    // decision 3, where `vike-cli walkforward` went too. [`route_of`] is the whole map.
    //
    // ⚠ It also closes a divergence that predates the merge. The ENGINE has always branched on
    // the grid predicate, so `--local` on a grid profile ran the grid — while this arm called
    // `run_backtest` unconditionally, and the server's `run_backtest` runs `harness::run_backtest`,
    // which ignores the `[sweep]` table and reports ONE point. Same profile, same verb, same
    // flags: a grid here and a single backtest there, with nothing in the output saying which had
    // happened. See [`declares_a_paramscan_grid`]. A `[walkforward]` table was the same defect wearing
    // a second section name, and [`route_of`] closes both.
    let report_json = match route {
        Route::Walkforward => client.run_walkforward_profile(&profile_toml)?,
        // A transport failure and a server-side `Response::Error` both arrive as `Err(String)`
        // and stay on the pre-existing rung: once the connection is open, a failure is the run's.
        Route::Search => client.run_paramscan_profile(
            &profile_toml,
            args.rank_by.as_deref(),
            args.wire_search().as_ref(),
        )?,
        Route::Single => client.run_backtest(&profile_toml)?,
    };

    if args.json {
        // Verbatim: exactly the JSON the server emitted. THREE different documents, one per
        // carrier — a `BacktestReport`, a `ParamscanReport` for a search, a `WalkForwardReport` for a
        // window — and they always were different; the route is what says which one arrived.
        println!("{report_json}");
        return Ok(());
    }
    // Re-parse to a generic `Value` so we do not need `BacktestReport` to derive `Deserialize` (it
    // does not yet — see the proto doc); a parse failure means the server sent something that is
    // not the report JSON, which is worth surfacing.
    let value: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server report was not valid JSON: {e}"))?;
    match route {
        // The stitched OOS table — one row per window, plus the `chosen` column an OPTIMIZING walk
        // earns and a fixed one does not print at all. Rendered by
        // `crate::cmd::walkforward::print_walkforward`, which stayed where it was.
        Route::Walkforward => crate::cmd::walkforward::print_walkforward(&value),
        // The ranked table, rendered from the SERVER's own per-row statistics in the SERVER's own
        // order. Absorbed from the deleted `sweep` verb unchanged — see [`print_paramscan`].
        Route::Search => print_paramscan(&value),
        Route::Single => {
            let pretty = serde_json::to_string_pretty(&value)
                .map_err(|e| format!("cannot pretty-print report: {e}"))?;
            println!("{pretty}");
        }
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
/// `--profile` and `--json` go to the child as themselves (`--store` did too, until it was refused on
/// both arms on 2026-09-25). `--preset` and `--script` do
/// NOT: they are CLIENT-SIDE rewrites (`merge_preset_params`, `inject_script_src`) that the engine
/// has no flags for, and asking it to grow them would put a second copy of the merge rules in the
/// tree. The rewritten profile is staged under `<project>/tmp` instead — through
/// `vike_model::scratch::ScratchDir`, which owns the directory and removes it on drop, including on
/// the panic path — and the child is handed THAT path.
///
/// So the profile the local engine parses is byte-identical to the text a remote datahub would have
/// been shipped, which is the property that makes `--local` a rehearsal for `--addr` rather than a
/// second, subtly different run mode.
///
/// # What the child is TOLD beside its argv
///
/// `user_data_dir` — the directory the reading verbs join their runs root onto — goes to the child
/// in its environment, so the run it saves is the run `backtest ls` lists. The child's own answer
/// comes from a walk that does not read `$VIKE_SETTINGS_DIR`; [`run`]'s doc carries the incident.
pub(super) fn execute_local(
    args: &Args,
    project_root: Option<&Path>,
    user_data_dir: Option<&Path>,
    profile_path: Option<&str>,
    profile_toml: &str,
) -> CmdResult<()> {
    // ⚠ STAGE WHENEVER THE CHILD CANNOT BE HANDED THE OPERATOR'S OWN FILE — which, since stage 2 of
    // the backtest-CLI-surface design, is any run whose profile was BUILT rather than read, and any
    // run whose flags changed it.
    //
    // ⚠ The comparison is on TEXT, and the builder NORMALIZES (`toml::to_string` over a re-parsed
    // `toml::Value` drops comments and reorders keys). So a commented profile always compares
    // unequal and is always staged. That is correct rather than unfortunate: the child must receive
    // the same bytes the remote arm would, which is the whole reason `execute`'s `--local` branch
    // sits BELOW every rewrite. Do NOT "optimise" this by skipping the build when there are no
    // overrides — that would stop `--local` being a byte-identical rehearsal of `--addr`.
    //
    // ⚠ Held for the whole call: `ScratchDir`'s `Drop` is what removes the staged profile, so
    // binding it to `_` (rather than to a name) would delete the file before the child reads it.
    let staged;
    let profile_arg: &Path = match profile_path {
        Some(p) if std::fs::read_to_string(p).is_ok_and(|t| t == profile_toml) => Path::new(p),
        _ => {
            let root = crate::cmd::engine::scratch_root(project_root).ok_or_else(|| {
                CliError::failed(format!(
                    "--local runs the engine on this machine from a profile FILE, and there is no \
                     project above the working directory to stage one in.\n{}\nRun inside your \
                     project (or set $VIKE_SETTINGS_DIR), pass an already-merged profile to \
                     --profile, or write one first with --write-profile <path>",
                    match profile_path {
                        Some(p) => format!(
                            "The flags on this command line changed {p}, so the child cannot be \
                             handed it unchanged."
                        ),
                        None => "This run's profile was built from flags, so there is no file to \
                                 hand the child."
                            .to_string(),
                    }
                ))
            })?;
            staged = StagedProfile::write(&root, profile_toml)?;
            staged.path()
        }
    };

    let mut argv: Vec<std::ffi::OsString> = vec!["--profile".into(), profile_arg.into()];
    // ⚠ The five SEARCH flags go to the child as THEMSELVES, spelled identically — they are the
    // engine's own flags (#1750's `parse_search_flags`), so this is forwarding rather than
    // translation. Nothing here decides whether a search HAPPENS: the profile's `[sweep]` table
    // does, on both sides, which is what makes `--local` a rehearsal for `--addr`.
    //
    // ⚠ The two SELECTORS carry their CANONICAL spelling rather than the operator's typing, for
    // the reason `flag_vocab::accept_value`'s doc gives: the child gets `tpe` for a typed `TPE`,
    // which is the same byte sequence the `--addr` arm puts in a `WireSearch`. That is what keeps
    // the two routes a rehearsal of each other on a search rather than two spellings of one.
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
    // ⚠ TOLD where `user_data` is, rather than left to find it: the child's own resolution is a walk
    // from its working directory that does not read `$VIKE_SETTINGS_DIR`, so without this a run
    // could land in a project `backtest ls` never looks in. See [`run`]'s doc and
    // `crate::cmd::engine`'s `Engine::with_user_data_dir`.
    let program = crate::cmd::engine::locate(args.engine.as_deref(), project_root)
        .with_user_data_dir(user_data_dir);
    crate::cmd::engine::run(&program, &argv, "backtest")
}
