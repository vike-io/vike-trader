//! `rename`: move one instrument's stored series under its new name, through the store tool, on the
//! box the store is on.
use std::io::IsTerminal;
use std::path::Path;

use super::engine::ENGINE_DATA_VERB;
use super::{Args, RenameArgs, Sub};
use crate::cmd::engine;
use crate::exit::{CliError, CommandResult};

/// `data history rename --venue V --symbol OLD --to NEW` — every per-symbol series of `V` stored under
/// `OLD` (every kind, interval and `source=` lane unless `--kind`/`--interval`/`--lane` narrow it)
/// moves under `NEW`, merging into a series already filed there; every row, part and commit key is
/// kept and each row's own symbol reads `NEW`. Decision 0290's store migration. The whole of the work
/// is `crates/vike-datahub/src/datahub_cli/store_cmd/rename_series.rs`'s `run_rename_series`, for
/// `rm`'s reason: this binary links no DataFusion. The contract — what survives, the crash story,
/// what a move costs — is `crates/vike-store/src/datafusion_hist/rename.rs`'s module doc.
///
/// # ⚠ Why a VERB, run after the deploy, and not a deploy row step
///
/// `deploy/row-steps` steps run first on each pre-flight probe's COPY of the settings, and the CI box's
/// `config.store_root` row is an ABSOLUTE path to the real store — so a store step would touch the
/// real store before the deploy is decided. The renames run after the deploy instead, beside the
/// running datahub, under each series' lock, and the runbook may re-run them blindly: a re-run after
/// success matches nothing and exits 0 saying so.
///
/// # The TTY refusal, `rm`'s rule
///
/// No `--yes`, no `--dry-run` and no terminal on stdin is refused HERE, on the usage rung, before a
/// process is spawned (`crates/vike-cli/src/cmd/data/hist/rm.rs`'s `execute_rm` says why a pipe is
/// never read as a confirmation). With a terminal the child reads the typed `rename N series` itself,
/// in the process that knows N.
pub(super) fn execute_rename(args: &Args, project_root: Option<&Path>) -> CommandResult<()> {
    let rename = args.rename.as_ref().expect("`parse` builds a RenameArgs for every Sub::Rename");
    if !rename.yes && !rename.dry_run && !std::io::stdin().is_terminal() {
        return Err(CliError::usage(
            "refusing to rename without --yes: stdin is not a terminal, so there is nobody to \
             confirm. Pass --yes, or --dry-run to see the plan and stop.",
        ));
    }
    let program = engine::locate_store_tool(args.engine.as_deref(), project_root);
    let argv = rename_engine_argv(args, rename);
    let stdin =
        if rename.yes || rename.dry_run { engine::Stdin::Null } else { engine::Stdin::Inherit };
    if !args.json {
        return engine::run_with_stdin(&program, &argv, "data", stdin);
    }
    let report = engine::run_capturing_stdout_with_stdin(&program, &argv, "data", stdin)?;
    println!("{}", rename_json_local(args, rename, &program, &argv, &report));
    Ok(())
}

/// The store tool's argv `rename`'s arguments become — PURE, so the translation is unit-tested.
/// `vike-datahub store rename …`; `--json` and `--store` are forwarded for `rm`'s reason (the tool
/// owns the only document that can carry the resolved store root), `--engine` is consumed here.
pub(super) fn rename_engine_argv(args: &Args, rename: &RenameArgs) -> Vec<String> {
    let mut argv: Vec<String> = [
        ENGINE_DATA_VERB,
        Sub::Rename.as_str(),
        "--venue",
        rename.venue.as_str(),
        "--symbol",
        rename.from.as_str(),
        "--to",
        rename.to.as_str(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for (flag, value) in
        [("--kind", &rename.kind), ("--interval", &rename.interval), ("--source", &rename.source)]
    {
        if let Some(v) = value {
            argv.push(flag.to_string());
            argv.push(v.clone());
        }
    }
    for (flag, on) in [("--dry-run", rename.dry_run), ("--yes", rename.yes), ("--json", args.json)]
    {
        if on {
            argv.push(flag.to_string());
        }
    }
    if let Some(store) = &args.store {
        argv.push("--store".to_string());
        argv.push(store.clone());
    }
    argv
}

/// `rename`'s `--json` document: what was asked for, what ran, and the store tool's own document
/// nested whole — `rm`'s local shape (`crates/vike-cli/src/cmd/data/hist/rm.rs`'s `rm_json_local`),
/// degrading to the raw lines when the child's output does not parse.
fn rename_json_local(
    args: &Args,
    rename: &RenameArgs,
    program: &engine::EngineCommand,
    argv: &[String],
    report: &[String],
) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(&report.join("\n")).ok();
    let doc = serde_json::json!({
        "subcommand": args.sub.as_str(),
        "route": "engine",
        "store": args.store.clone(),
        "venue": rename.venue,
        "from": rename.from,
        "to": rename.to,
        "kind": rename.kind,
        "interval": rename.interval,
        "source": rename.source,
        "dry_run": rename.dry_run,
        "engine": program.display(),
        "engine_argv": argv,
        "engine_report": parsed,
        "engine_report_lines": if parsed.is_some() { serde_json::Value::Null } else {
            serde_json::json!(report)
        },
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, arrays and nulls; serialization is total")
}
