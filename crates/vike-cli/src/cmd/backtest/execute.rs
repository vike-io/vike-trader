//! `run` executed: rewrite the profile, route it, then dial the daemon or spawn the engine.

use std::path::Path;

use serde_json::Value;
use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::client::ParamscanProfileAnswer;
use vike_client::resolved::backtesthub_dial_address;
use vike_hub_protocol::auth::{HubKeys, Scope};

use crate::exit::{CliError, CommandResult};
use crate::refusals::FlagRefusal;

use super::Args;
use super::profile::{StagedProfile, build_profile_toml, merge_preset_params, render_effective};
use super::render::{print_paramscan, print_paramscan_summary, print_permutation, print_summary};
use super::route::{
    Route, asks_for_a_plan, refuse_a_permutation_flag, refuse_a_walkforward_flag, route_of,
};

/// Read the profile, ship it to the backtesthub, and print the answer — a single backtest's or a
/// search's summary unless `--whole` asks for its report, pretty by default, raw under `--json`. Every failure path
/// funnels into ONE [`CliError`] the caller prints to stderr and
/// exits on; the ones that are not explicitly classified arrive through `From<String>` on the
/// pre-existing rung, which is what let this file be converted without re-judging every `?`.
pub(super) fn execute(
    args: &Args,
    project_root: Option<&Path>,
    user_data_dir: Option<&Path>,
    configured_address: Option<&str>,
    keys: Option<&HubKeys>,
) -> CommandResult<()> {
    // THE INVERSION (spec §5.3). `--profile` is now a BASE rather than the whole input: the file's
    // text, when there is one, is the document every override is applied onto. Everything below
    // this point sees only the resulting TEXT, which is what keeps `--local` a byte-identical
    // rehearsal of `--addr`.
    let base = match args.profile_path.as_deref() {
        Some(path) => Some(std::fs::read_to_string(path).map_err(|e| {
            FlagRefusal::ProfileUnreadable { path: path.to_string(), e: e.to_string() }.rendered()
        })?),
        None => None,
    };
    let mut profile_toml =
        build_profile_toml(base.as_deref(), &args.overrides).map_err(CliError::usage)?;

    // --preset: merge the preset file's knobs into `[strategy.params]` (a preset carrying `src` at all
    // is refused outright — see `merge_preset_params`).
    if let Some(preset_path) = &args.preset_path {
        let preset = std::fs::read_to_string(preset_path).map_err(|e| {
            FlagRefusal::PresetUnreadable { preset_path: preset_path.clone(), e: e.to_string() }
                .rendered()
        })?;
        profile_toml = merge_preset_params(&profile_toml, &preset).map_err(|why| {
            FlagRefusal::PresetRefused { preset_path: preset_path.clone(), why }.rendered()
        })?;
    }

    // ⚠ BELOW the rewrite, deliberately: what `--write-profile` writes and what `--show-effective`
    // prints must be the text that would ACTUALLY have been run, `--preset`'s merge included. Writing the pre-rewrite document would produce a file
    // that runs differently from the command line that produced it, which is the one thing spec
    // §15.4's round-trip gate exists to prevent.
    if let Some(path) = &args.write_profile {
        if std::path::Path::new(path).exists() {
            return Err(CliError::usage(
                FlagRefusal::WriteProfileExists { path: path.clone() }.rendered(),
            ));
        }
        std::fs::write(path, &profile_toml).map_err(|e| {
            FlagRefusal::WriteProfileFailed { path: path.clone(), e: e.to_string() }.rendered()
        })?;
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

    if matches!(route, Route::Walkforward | Route::CombinatorialWalkforward) {
        // Refused BEFORE the `--local` branch, so a walk-forward never reaches
        // `crate::cmd::engine`'s search for a binary that could not run it anyway.
        if args.local {
            return Err(CliError::usage(FlagRefusal::LocalHasNoWalkforward.rendered()));
        }
        refuse_a_walkforward_flag(args).map_err(CliError::usage)?;
    }
    if route == Route::Permutation {
        // The same two refusals, for the same reasons: no engine arm, and no flag the verb carries.
        if args.local {
            return Err(CliError::usage(FlagRefusal::LocalHasNoPermutation.rendered()));
        }
        refuse_a_permutation_flag(args).map_err(CliError::usage)?;
    }

    // ⚠ `--local` DIVERGES HERE, and everything above it is deliberately shared: the profile has
    // been read and the client-side rewrite has been applied, so a local run and a remote run
    // are given byte-identical profile TEXT. That is the whole point of putting the branch this
    // far down — `--preset` is resolved against THIS filesystem in both modes, and
    // a local run that quietly resolved it differently would answer differently from the remote
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
    let addr = backtesthub_dial_address(args.addr.as_deref(), configured_address);
    let mut client = BacktesthubClient::dial(&addr, keys, Scope::Write)?;
    client.set_fresh(args.fresh);

    // ⚠ **THE PROFILE ROUTES, not a flag** — and this is where `vike-cli sweep` went (ruling 13:
    // there is no second verb, `--optimizer` is where the word "optimize" is spelled) and, since
    // decision 3, where `vike-cli walkforward` went too. [`route_of`] is the whole map.
    //
    // ⚠ It also closes a divergence that predates the merge. The ENGINE has always branched on
    // the grid predicate, so `--local` on a grid profile ran the grid — while this arm called
    // `run_backtest` unconditionally, and the server's `run_backtest` runs `run::run_backtest`,
    // which ignores the `[sweep]` table and reports ONE point. Same profile, same verb, same
    // flags: a grid here and a single backtest there, with nothing in the output saying which had
    // happened. See [`declares_a_paramscan_grid`]. A `[walkforward]` table was the same defect wearing
    // a second section name, and [`route_of`] closes both.
    //
    // ⚠ **A single backtest answers its SUMMARY unless `--whole` asks for the report** (decision 0078's
    // amendment of 2026-11-18): `Request::Summarized` around the same `RunBacktest`, the summarizer a
    // Studio Run gets, so the answer is a few tens of kilobytes whatever the window, and the trades and
    // the whole equity curve stay on the backtesthub. A daemon without `FEATURE_RUN_CURVE` is refused in
    // words naming `--whole`, never sent the verb. A data plan is its own whole answer.
    if route == Route::Single && !args.whole && !asks_for_a_plan(&profile_toml) {
        let summary = client.run_backtest_summary(&profile_toml)?;
        print_summary(&summary, args.json)?;
        return Ok(());
    }
    let report_json = match route {
        Route::Walkforward => client.run_walkforward_profile(&profile_toml)?,
        Route::CombinatorialWalkforward => {
            client.run_combinatorial_walkforward_profile(&profile_toml)?
        }
        Route::Permutation => client.run_permutation_profile(&profile_toml)?,
        // A transport failure and a server-side `Response::Error` both arrive as `Err(String)`
        // and stay on the pre-existing rung: once the connection is open, a failure is the run's.
        //
        // ⚠ **A search answers its SUMMARY where the backtesthub has one** (decision 0078's amendment of
        // 2026-11-19): each point's overrides and metric table, paged, the whole report in the run the
        // backtesthub saved, so a search of any size answers. `--whole`, or a backtesthub without
        // `FEATURE_PARAMSCAN_PROFILE_SUMMARY`, answers the whole report as before (the client's one fallback).
        Route::Search => match client.run_paramscan_profile_answer(
            &profile_toml,
            args.rank_by.as_deref(),
            args.wire_search().as_ref(),
            args.whole,
        )? {
            ParamscanProfileAnswer::Summary(summary) => {
                print_paramscan_summary(&summary, args.json)?;
                return Ok(());
            }
            ParamscanProfileAnswer::Whole(json) => json,
        },
        Route::Single => client.run_backtest(&profile_toml)?,
    };

    if args.json {
        // Verbatim: exactly the JSON the server emitted. THREE different documents, one per
        // carrier — a `TradingResults`, a `ParamscanReport` for a search, a `WalkforwardResults` for a
        // window — and they always were different; the route is what says which one arrived.
        println!("{report_json}");
        return Ok(());
    }
    // Re-parse to a generic `Value` so we do not need `TradingResults` to derive `Deserialize` (it
    // does not yet — see the proto doc); a parse failure means the server sent something that is
    // not the report JSON, which is worth surfacing.
    let value: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server report was not valid JSON: {e}"))?;
    if let Some(id) = value.get("answered_from_saved_run").and_then(Value::as_str) {
        println!("answered from saved run {id} (an identical run; `--fresh` runs it again)");
    }
    match route {
        // The stitched OOS table — one row per window, plus the `chosen` column an OPTIMIZING walk
        // earns and a fixed one does not print at all. Rendered by
        // `super::render::walkforward::print_walkforward`.
        Route::Walkforward => super::render::walkforward::print_walkforward(&value),
        // One summary row per path, the combinatorial walk's whole answer.
        Route::CombinatorialWalkforward => {
            super::render::walkforward::print_combinatorial_walkforward(&value)
        }
        Route::Permutation => print_permutation(&value),
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
/// opens a `HistoryStoreDataFusion`), the search order, and the fold from the child's exit status onto this
/// crate's ladder.
///
/// # What is forwarded, and the one thing that is NOT
///
/// `--profile` and `--json` go to the child as themselves (`--store` did too, until it was refused on
/// both arms on 2026-09-25). `--preset` does
/// NOT: it is a CLIENT-SIDE rewrite (`merge_preset_params`) that the engine
/// has no flag for, and asking it to grow one would put a second copy of the merge rules in the
/// tree. The rewritten profile is staged under `<project>/tmp` instead — through
/// `vike_paths::scratch::ScratchDir`, which owns the directory and removes it on drop, including on
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
) -> CommandResult<()> {
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
                let staging_reason = match profile_path {
                    Some(p) => format!(
                        "The flags on this command line changed {p}, so the child cannot be \
                         handed it unchanged."
                    ),
                    None => "This run's profile was built from flags, so there is no file to hand \
                             the child."
                        .to_string(),
                };
                CliError::failed(FlagRefusal::NoProjectToStage { staging_reason }.rendered())
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
    // which is the same byte sequence the `--addr` arm puts in a `SearchSelectionWire`. That is what keeps
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
