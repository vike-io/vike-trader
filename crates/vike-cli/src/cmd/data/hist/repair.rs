//! `repair`: rebuild one series' manifest through the engine, on the box the store is on.
use std::path::Path;

use super::engine::ENGINE_DATA_VERB;
use super::{Args, RepairArgs, Sub};
use crate::cmd::engine;
use crate::exit::CmdResult;

/// `data hist repair` — rebuild ONE series' manifest from its parts, through the engine.
///
/// # ⚠ It REHEARSES by default, and that is a different rule from `rm`'s
///
/// [`execute_rm`] refuses without `--yes` because somebody typing `rm` means to delete and the
/// danger is doing it unseen. This verb prints the plan and exits 0 without `--yes`, because the
/// danger here is the opposite: a rebuild can SUCCEED and still cost the series its idempotency
/// log — `RebuildReport::parts_without_keys` — after which re-running a backfill re-admits an
/// already-applied append and DUPLICATES rows. The fact an operator needs is therefore the
/// VERDICT, and the verdict is knowable only by running the pass the write runs. The rehearsal IS
/// that pass: lock-free, writing nothing, costing a live writer nothing.
///
/// So there is no TTY refusal here and deliberately none. `rm`'s exists because a confirmation read
/// from a PIPE is not a confirmation; this verb asks for no confirmation at all, and a line without
/// `--yes` is already the safe one. Adding a refusal would make the SAFE spelling the one that
/// fails in CI.
///
/// # ⚠ ONE route, and `--addr` is refused at [`parse`]
///
/// [`refuse_the_remote_route_on_repair`] carries the three-part argument. The short form: a
/// datahub can only name series it ENUMERATED, and the series this repairs is by definition one no
/// enumeration shows.
///
/// # What this side does NOT do
///
/// It resolves no store root, opens nothing and judges no series. The whole of the work — the plan,
/// the lock, the rebuild and the verdict — is
/// `crates/vike-backtest/src/backtest_cli/data_cmd/repair_series.rs`'s `run_repair_series`, for
/// [`execute_engine`]'s reason: opening a hist store needs DataFusion and this binary links none.
/// The engine's non-zero exit for a LOSSY rebuild folds onto [`crate::exit::Exit::Failed`] through
/// `crate::cmd::engine`'s `fold_status`, exactly as `rm`'s partial-failure rung does.
pub(super) fn execute_repair(args: &Args, project_root: Option<&Path>) -> CmdResult<()> {
    let repair = args.repair.as_ref().expect("`parse` builds a RepairArgs for every Sub::Repair");
    let program = engine::locate(args.engine.as_deref(), project_root);
    let argv = repair_engine_argv(args, repair);
    // ⚠ NO stdin is passed through, unlike [`execute_rm_local`]: the engine's `data hist repair` arm
    // reads no confirmation, so a child holding this process's terminal would be a door nothing
    // opens.
    if !args.json {
        return engine::run(&program, &argv, "data");
    }
    let report = engine::run_capturing_stdout(&program, &argv, "data")?;
    println!("{}", repair_json_local(args, repair, &program, &argv, &report));
    Ok(())
}

/// The engine argv `repair`'s arguments become — PURE, so the translation is unit-tested rather
/// than only observed through a spawn. `backtest data repair …`; see [`engine_argv`].
///
/// ⚠ `--dry-run` is forwarded when the operator wrote it AND when they wrote neither flag, so the
/// engine is told in one word what this side decided. The alternative — forwarding nothing and
/// letting the engine's own default rehearse — makes the child's behaviour depend on a default
/// this side is also documenting, which is two places for one rule.
pub(super) fn repair_engine_argv(args: &Args, repair: &RepairArgs) -> Vec<String> {
    let mut argv = vec![
        ENGINE_DATA_VERB.to_string(),
        Sub::Repair.as_str().to_string(),
        "--kind".to_string(),
        repair.kind.clone(),
        "--venue".to_string(),
        repair.venue.clone(),
    ];
    for (flag, value) in
        [("--symbol", &repair.symbol), ("--group", &repair.group), ("--interval", &repair.interval)]
    {
        if let Some(v) = value {
            argv.push(flag.to_string());
            argv.push(v.clone());
        }
    }
    // ⚠ `--dry-run` WINS over `--yes` on BOTH sides, and it is spelled here as well as there so a
    // reader of either argv can see which one was decided. `repair.dry_run || !repair.yes` is the
    // rehearsal condition — the exact complement of the engine's `repair_writes`.
    if repair.dry_run || !repair.yes {
        argv.push("--dry-run".to_string());
    } else {
        argv.push("--yes".to_string());
    }
    if args.json {
        // FORWARDED, for [`rm_engine_argv`]'s reason: the engine's `data hist repair` emits a machine
        // document carrying the RESOLVED store root, the rung that chose it, and the plan — none
        // of which this side can know, because the engine resolves them in another process.
        argv.push("--json".to_string());
    }
    if let Some(store) = &args.store {
        argv.push("--store".to_string());
        argv.push(store.clone());
    }
    argv
}

/// `repair`'s `--json` document: what was asked for, what ran, and the ENGINE's own document
/// nested whole.
///
/// The shape and the reasoning are [`rm_json_local`]'s unchanged — the child prints a JSON document
/// it owns, so nesting it hands a caller the structure rather than a string to re-parse, and a
/// document that does not parse degrades to the raw lines.
fn repair_json_local(
    args: &Args,
    repair: &RepairArgs,
    program: &engine::Engine,
    argv: &[String],
    report: &[String],
) -> String {
    let joined = report.join("\n");
    let parsed = serde_json::from_str::<serde_json::Value>(&joined).ok();
    let doc = serde_json::json!({
        "subcommand": args.sub.as_str(),
        // Spelled even though there is only one, because a caller branching on `route` across the
        // `data` verbs should not have to special-case the one that omits the field.
        "route": "engine",
        "store": args.store.clone(),
        "series": {
            "kind": repair.kind,
            "venue": repair.venue,
            "symbol": repair.symbol,
            "group": repair.group,
            "interval": repair.interval,
        },
        // What this side DECIDED, which is what the child was told — not what was typed. A caller
        // reading `false` here knows no write was attempted without having to re-derive the
        // dry-run-wins rule.
        "writes": !(repair.dry_run || !repair.yes),
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
