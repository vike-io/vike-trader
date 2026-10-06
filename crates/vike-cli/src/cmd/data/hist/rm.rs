//! `rm`: the one verb that reaches EITHER store, the engine's on this machine or a datahub's.
use std::io::IsTerminal;
use std::path::Path;

use vike_node_proto::auth::{NodeKeys, Scope};

use super::engine::ENGINE_DATA_VERB;
use super::{Args, RmArgs, Sub};
use crate::cmd::data::shared::connect;
use crate::cmd::engine;
use crate::exit::{CliError, CmdResult};

/// `data hist rm` — route to the datahub or to the engine, having first refused the one shape neither
/// may be asked to handle.
///
/// # ⚠ The TTY refusal happens HERE, before a socket or a process
///
/// No `--yes`, no `--dry-run` and no terminal on stdin is a REFUSAL, on the usage rung. It is not a
/// pre-flight for the far side's own check — both routes refuse it too — it is the rung: nothing
/// was attempted and re-running unchanged cannot succeed, which is exactly what `Exit::Usage`
/// promises and what a wrapper needs to hear before it retries. Reading a confirmation from a PIPE
/// is the failure this exists to prevent (`yes | vike-cli data hist rm …`), and a pipe is
/// indistinguishable from a person once you have decided to read one.
///
/// # ⚠ ONE OPERATION, THREE PROCEDURES — what agrees, what was made to agree, and what still does not
///
/// The deletion CORE is one implementation and always was: `vike_data::store::removal`'s `plan_removal` /
/// `execute_removal` over `SeriesSelector`, with `DataFusionHist::delete_series_checked` re-asserting
/// under the series lock. The wire uses the SAME TYPES —
/// `vike_datahub_client::proto` re-exports `SeriesSelector`, `RemovalPlan`, `RemovalOutcome` and
/// `describe_id` rather than restating them — so selector semantics and plan RENDERING cannot
/// diverge between routes at all.
///
/// What is written three times is the PROCEDURE around it: here, in
/// `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series` (the engine, which the local route
/// spawns), and in `crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb`. Every divergence
/// below lives in that layer.
///
/// **Made to agree by this PR**, both client-side:
///
/// * **The provenance VERDICT.** The engine checks `RemovalPlan::verdict` immediately after
///   printing the plan and before the dry-run branch; [`execute_rm_remote`] checked it NOWHERE, so
///   a refused assertion still prompted a human to type `delete N series` and still spent a second
///   destructive round trip. It is checked at the same point on both routes now.
/// * **A blank `--produced-by`** — refused with the reason that is true of a provenance prefix
///   rather than the grouped-series-sentinel sentence it had inherited, and refused on BOTH routes
///   before anything is dialled or spawned. [`refuse_a_blank_produced_by`].
/// * **A producer PATH under `--addr`** — refused by name, because at the time neither side of that
///   wire resolved one. ⚠ The SERVER half landed on 2026-09-11 and
///   [`refuse_a_producer_path_on_the_remote_route`] became a compatibility guard for a datahub that
///   has not been redeployed; its doc carries why removing it would hand the defect back.
///
/// **Deliberately NOT closed here, because each needs a change outside this crate.** Recorded
/// rather than tolerated silently, and `crates/vike-cli/tests/data_cli.rs` pins the behaviour that
/// survives:
///
/// * **The EXIT LADDER differs by route.** The engine answers `2` for everything — a shape error, a
///   provenance refusal, a partial failure — while `crate::cmd::engine`'s `fold_status`
///   deliberately maps every non-zero child code onto `Exit::Failed` (`1`), because the engine's `2`
///   also means "the venue was geoblocked" and folding that onto the usage rung would tell a
///   wrapper a retry cannot help. So a shape error is `2` typed at the engine and `1` through
///   `data rm --store`, while the same error typed at `data hist rm` is `2` (this side refused it first).
///   Closing it needs the ENGINE to distinguish a pre-flight code from a runtime one; `fold_status`
///   carries that as its own "what would reopen this".
/// * **The no-TTY refusal fires at a different MOMENT.** This function refuses at the door, before
///   a socket or a process, on the usage rung — the engine's `confirm_removal` is reached only
///   after the plan and only when `matched() > 0`, so a no-match cleanup in CI succeeds there and
///   is refused here. THIS ordering is the one that survives, and deliberately: a confirmation must
///   bind to a COUNT, the remote route needs a round trip to learn one, and refusing before the
///   round trip is strictly better than after it. `USAGE`'s "Matching nothing is a SUCCESS" is
///   about the PLAN, not about this pre-flight.
/// * **THREE `--json` documents for one verb** — the engine's (`rm_series_json`, the only one that
///   can carry the resolved store ROOT and the rung that chose it), the local route's
///   ([`rm_json_local`], which nests it), and the remote route's ([`rm_json_remote`], the only one
///   carrying `addr` and `plan`). A machine consumer branches on `route`. Folding them needs a
///   shape neither side can produce alone.
/// * **The SERVER does not resolve `--produced-by` and does not consult `STORE_KINDS` at all.** The
///   clean fix is a `resolve_produced_by` re-export in `vike_datahub_client::proto`, beside the
///   re-exports that exist for exactly this reason — a change to a crate this branch does not own.
///   Until then the refusal above is what stands in for it.
pub(crate) fn execute_rm(
    args: &Args,
    project_root: Option<&Path>,
    keys: Option<&NodeKeys>,
) -> CmdResult<()> {
    let rm = args.rm.as_ref().expect("`parse` builds an RmArgs for every Sub::Rm");
    if !rm.yes && !rm.dry_run && !std::io::stdin().is_terminal() {
        return Err(CliError::usage(
            "refusing to delete without --yes: stdin is not a terminal, so there is nobody to \
             confirm. Pass --yes (a deliberate, greppable token in shell history), or --dry-run to \
             see the plan and stop.",
        ));
    }
    if args.addr_given {
        execute_rm_remote(args, rm, keys)
    } else {
        execute_rm_local(args, rm, project_root)
    }
}

/// The LOCAL route: drive the engine's `data hist rm` against a store on this machine.
///
/// ⚠ **The confirmation is the ENGINE's, and the child gets this process's stdin so it can read
/// one.** The alternative — confirming here — needs the matched COUNT the typed line is bound to,
/// which only the plan knows, which means spawning once for a plan and once to act, plus parsing a
/// machine document on the side of the fence that deliberately cannot open a store. Passing the
/// terminal through costs an enum ([`engine::Stdin`]) and keeps one confirmation, in the process
/// that computed the number it names.
fn execute_rm_local(args: &Args, rm: &RmArgs, project_root: Option<&Path>) -> CmdResult<()> {
    let program = engine::locate(args.engine.as_deref(), project_root);
    let argv = rm_engine_argv(args, rm);
    // The child may need to read a typed confirmation — unless `--yes` or `--dry-run` already
    // settled it, in which case it reads nothing and the default applies.
    let stdin = if rm.yes || rm.dry_run { engine::Stdin::Null } else { engine::Stdin::Inherit };
    if !args.json {
        return engine::run_with_stdin(&program, &argv, "data", stdin);
    }
    let report = engine::run_capturing_stdout_with_stdin(&program, &argv, "data", stdin)?;
    println!("{}", rm_json_local(args, rm, &program, &argv, &report));
    Ok(())
}

/// The engine argv `rm`'s arguments become — PURE, so the translation is unit-tested rather than
/// only observed through a spawn. `backtest data rm …` since ruling 12; see [`engine_argv`].
///
/// ⚠ `--produced-by` is forwarded VERBATIM, never resolved here. A producer-path spelling resolves
/// against `STORE_KINDS`, and that table lives in `vike-data` — the tree this crate exists not to
/// link. Resolving it here would be a second copy of the roster; the engine's message names what it
/// could not resolve.
///
/// ⚠ That forwarding used to make the LOCAL route the ONLY one that resolves a producer path. Since
/// 2026-09-11 the datahub resolves one too, through the `vike_datahub_client::proto` re-export of
/// the same function — so the asymmetry is closed in the code and survives only as a MIXED-FLEET
/// question, which [`refuse_a_producer_path_on_the_remote_route`] is what still answers.
pub(super) fn rm_engine_argv(args: &Args, rm: &RmArgs) -> Vec<String> {
    let mut argv = vec![
        ENGINE_DATA_VERB.to_string(),
        Sub::Rm.as_str().to_string(),
        "--kind".to_string(),
        rm.kind.clone(),
        "--venue".to_string(),
        rm.venue.clone(),
    ];
    for (flag, value) in [
        ("--symbol", &rm.symbol),
        ("--group", &rm.group),
        ("--interval", &rm.interval),
        ("--produced-by", &rm.produced_by),
    ] {
        if let Some(v) = value {
            argv.push(flag.to_string());
            argv.push(v.clone());
        }
    }
    if rm.dry_run {
        argv.push("--dry-run".to_string());
    }
    if rm.yes {
        argv.push("--yes".to_string());
    }
    if args.json {
        // ⚠ FORWARDED, unlike `fetch`'s `--json` (which this module CONSUMES —
        // `json_parses_on_both_subcommands_and_is_not_forwarded` pins that). The divergence is
        // deliberate and it is about ONE fact: the engine's `data hist rm` emits a machine document
        // carrying the RESOLVED store root and the rung that chose it, and this side cannot know
        // either — the engine resolves them in another process. `fetch` has no such document, so
        // its counts stay prose rather than becoming a second implementation of another crate's
        // sentences.
        argv.push("--json".to_string());
    }
    if let Some(store) = &args.store {
        argv.push("--store".to_string());
        argv.push(store.clone());
    }
    argv
}

/// The LOCAL route's `--json` document: what was asked for, what ran, and the ENGINE's own document
/// nested whole.
///
/// ⚠ `engine_report` is the child's document PARSED, not its lines. That is the opposite of
/// [`report_json`]'s rule for `fetch`, and the difference is what is on the other end: `fetch`'s
/// engine prints PROSE, so reading numbers out of it would be a second implementation of another
/// crate's sentences; `data hist rm` prints a JSON document it owns, so nesting it hands a caller
/// the structure rather than a string to re-parse. A document that does not parse degrades to the
/// raw lines under `engine_report_lines`, so a caller is never handed a silently-empty object.
fn rm_json_local(
    args: &Args,
    rm: &RmArgs,
    program: &engine::Engine,
    argv: &[String],
    report: &[String],
) -> String {
    let joined = report.join("\n");
    let parsed = serde_json::from_str::<serde_json::Value>(&joined).ok();
    let doc = serde_json::json!({
        "subcommand": args.sub.as_str(),
        "route": "engine",
        // What `--store` NAMED, or null. Never a path this side guessed at — the ENGINE resolves
        // the root, in another process, and its own document carries the answer.
        "store": args.store.clone(),
        "selector": rm_selector_json(rm),
        "produced_by": rm.produced_by.clone(),
        "dry_run": rm.dry_run,
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

/// The selector as this side parsed it — the four dimensions as their own fields, so a caller need
/// not re-split anything and an omitted (wildcarded) dimension is an explicit `null`.
fn rm_selector_json(rm: &RmArgs) -> serde_json::Value {
    serde_json::json!({
        "kind": rm.kind,
        "venue": rm.venue,
        "symbol": rm.symbol,
        "group": rm.group,
        "interval": rm.interval,
    })
}

/// The REMOTE route: ask a running `vike-datahub` to delete from the store IT has open.
///
/// # Two round trips, and the confirmation is bound to the first
///
/// The plan pass (`dry_run: true`) always runs, its lines are printed, and only then is the
/// operator asked. `--dry-run` stops there; `--yes` skips the asking. That is one more round trip
/// than the local route needs — the engine there prints its own plan and reads its own confirmation
/// in one process — and the reason it is worth it is that this side has no other way to know the
/// matched COUNT the typed line names.
///
/// ⚠ **The plan and the delete are two requests, so the store can move between them.** The
/// confirmation binds to what was SHOWN, not to what will be deleted, and nothing on this wire
/// makes those the same set. What DOES hold across the gap is the provenance assertion: the server
/// re-checks it under each series' own lock immediately before the removal, so a key committed in
/// between refuses that series rather than deleting it. Closing the count gap entirely means a
/// server-side token, which is a protocol change rather than a client one.
///
/// # ⚠ This route AUTHENTICATES now, and `keys` is how — it was a declared gap until it did
///
/// The verb is served ONLY by a KEYED server (`vike_datahub_client::proto`'s
/// `FEATURE_DELETE_SERIES` carries why), so until `vike-cli` could resolve
/// `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` this route could not reach a server that
/// serves it: every datahub caller used the plain unauthenticated `DatahubClient::connect`, which a
/// keyed server refuses. The fix could not live inside `src/cmd/` — an `env::var` here would be a
/// new `Layer::Library` row on `crates/vike-ops/tests/settings/settings_registry.rs`'s `LIBRARY_PIN` ratchet,
/// and a credential-store read a new `CREDENTIAL_STORE_PIN` entry, both ratchets that may shrink
/// and never grow — so the keys arrive as a PARAMETER from the dispatcher, exactly as
/// `crate::cmd::mcp`'s `NodeKeyring` already did. `crate::Resolved::datahub_keys` is the field.
///
/// **The two refusals are still there and are still the right ones**, which is why neither was
/// deleted: a KEY-LESS server does not advertise the verb, so `DatahubClient::delete_series`
/// refuses before sending; and a keyed server asked for the wrong SCOPE refuses at the handshake.
/// [`connect`] asks for [`Scope::Write`] here — a delete is a write — so a key that only grants
/// Observe is refused at the socket rather than after a selector has travelled.
/// [`rm_remote_hint`] is the sentence that turns any of these into an instruction.
fn execute_rm_remote(args: &Args, rm: &RmArgs, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // ⚠ NAMED through `vike_datahub_client::proto`'s re-export, never through a `vike_data` edge —
    // this crate takes that dependency for DEV targets only, and the re-export exists so the wire's
    // vocabulary can be CONSTRUCTED here without one. See that re-export's own doc.
    use vike_datahub_client::proto::{SeriesSelector, describe_id};

    // Control: this route DELETES. A keyed server that would grant only Observe refuses here,
    // before a selector is sent — which is the refusal an operator wants to see.
    let mut client = connect(&args.addr, keys, Scope::Write)?;
    let selector = SeriesSelector {
        kind: rm.kind.clone(),
        venue: rm.venue.clone(),
        symbol: rm.symbol.clone(),
        group: rm.group.clone(),
        interval: rm.interval.clone(),
    };
    let planned = client
        .delete_series(&selector, rm.produced_by.as_deref(), true)
        .map_err(|e| CliError::failed(format!("{e}\n{}", rm_remote_hint())))?;
    let plan = planned.plan;

    // ⚠ The plan is SHOWN either way, and under `--json` it goes to STDERR rather than nowhere.
    // Stdout is the document and nothing else, but a run about to ask a human to type
    // `delete N series` must have shown them what N is made of — and the refusal below is only
    // legible beside the lines that carry it. Same split the ENGINE's own arm makes.
    let store_line = format!("store: the one the datahub at {} has open", args.addr);
    if args.json {
        // This side cannot name the store's PATH — that is the server's resolution, in another
        // process on another box — so it names the SERVER rather than guessing at a directory.
        eprintln!("{store_line}");
        for line in plan.lines() {
            eprintln!("{line}");
        }
    } else {
        println!("{store_line}");
        for line in plan.lines() {
            println!("{line}");
        }
    }
    // ⚠ **THE PROVENANCE VERDICT, checked HERE — where the ENGINE checks it, and where this route
    // did not.** `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series` calls
    // `plan.verdict()` immediately after printing the plan and BEFORE the dry-run branch, so a
    // refused assertion exits without asking anybody anything. This route called it nowhere: it
    // printed the plan (which does render "provenance: REFUSED", so the text was on screen),
    // fell through to `confirm_typed`, made the operator type `delete N series` for a deletion
    // that could never happen, sent a second `delete_series`, and only then surfaced the server's
    // `execute_removal` error. Under `--yes` that is a wasted destructive round trip; without it,
    // a human is asked to authorise something already decided against. Under `--json` it was
    // worse still — the plan went nowhere and `rm_json_remote` had no `refused` field, so the
    // refusal reached a machine reader as a bare error string with no document at all.
    //
    // ⚠ **It REPORTS what the server already decided; it is not a client-side gate.** The plan
    // arrived over the wire, so this is a second evaluation of the assertion in a different
    // process from the one that will act. It can only ever make a run FAIL that the server had
    // already rendered as REFUSED in the very lines just printed. The authority is unchanged and
    // is on the far side: `vike_data::store::removal::execute_removal` re-checks, and
    // `DataFusionHist::delete_series_checked` re-checks again under the series lock.
    if let Err(refusals) = plan.verdict() {
        if args.json {
            println!("{}", rm_json_remote(args, rm, &plan, None, Some(&refusals)));
        }
        return Err(CliError::failed(
            "provenance REFUSED — nothing was deleted (the plan above names every series whose \
             commit keys do not carry --produced-by)",
        ));
    }
    if rm.dry_run {
        if args.json {
            println!("{}", rm_json_remote(args, rm, &plan, None, None));
        } else {
            println!("--dry-run: nothing was deleted");
        }
        return Ok(());
    }
    // "Nothing matched" is a SUCCESS, on the same rung as a delete — the server's delete is
    // idempotent and a cleanup that fails on re-run is a cleanup nobody re-runs.
    if plan.matched() == 0 {
        if args.json {
            println!("{}", rm_json_remote(args, rm, &plan, Some(&Default::default()), None));
        }
        return Ok(());
    }
    if !rm.yes {
        confirm_typed(plan.matched())?;
    }
    let done = client
        .delete_series(&selector, rm.produced_by.as_deref(), false)
        .map_err(CliError::failed)?;
    let outcome = done.outcome.unwrap_or_default();
    if args.json {
        println!("{}", rm_json_remote(args, rm, &done.plan, Some(&outcome), None));
    } else {
        for id in &outcome.deleted {
            println!("deleted {}", describe_id(id));
        }
        for (id, why) in &outcome.failed {
            eprintln!("FAILED {}: {why}", describe_id(id));
        }
        println!(
            "{} of {} series deleted",
            outcome.deleted.len(),
            outcome.deleted.len() + outcome.failed.len()
        );
    }
    // One broken series is one SKIPPED series: reported, the rest went, and the rung says look.
    if outcome.is_clean() {
        Ok(())
    } else {
        Err(CliError::failed(format!(
            "{} of {} series could not be deleted (see above); re-running finishes the job",
            outcome.failed.len(),
            outcome.failed.len() + outcome.deleted.len()
        )))
    }
}

/// The sentence appended to every remote-route refusal — what to do about it.
///
/// ⚠ **It named the wrong remedy between #1688 and #1691, which is the worst thing a refusal can
/// do.** It said "this CLI cannot yet authenticate to one" and sent the operator to the box. That
/// was true for three commits; #1691 taught `crates/vike-cli/src/boot.rs`'s `datahub_keyring` to
/// resolve the pair, so the CLI authenticates whenever the box sets the keys — and an operator
/// following the old sentence would have gone to the box rather than setting the two variables that
/// fix it.
///
/// ⚠ And then it named the wrong remedy a SECOND time, for a different reason, which is why this
/// paragraph is worth keeping rather than trimming. That resolver was a field called `datahub_keys`
/// reading the PROCESS ENVIRONMENT alone, while every refusal — this one included — told the
/// operator to put the keys in the CREDENTIAL STORE. Following the advice exactly left them broken.
/// #1701 made it the lazy function named above, env first and store second. Twice in one surface is
/// what a refusal costs when it is written from intent rather than from the resolver.
///
/// The refusal it is appended to has TWO causes and the sentence now separates them, because the
/// remedies differ: this side holding no key (set them here), or the datahub itself holding none
/// (a key-less server advertises no delete verb at all — `docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`),
/// in which case no client-side change helps and the local route is the answer.
fn rm_remote_hint() -> String {
    "The remote delete is served only by a datahub that holds node keys. Either set \
     VIKE_DATAHUB_OBSERVE_KEY and VIKE_DATAHUB_CONTROL_KEY here so this CLI authenticates, or — if \
     the datahub itself holds none, in which case it advertises no delete verb at all — run the \
     delete ON that box: `vike-cli data hist rm --store DIR …`, over `ssh` if it is remote."
        .to_string()
}

/// Read one line and require it to equal `delete N series`.
///
/// ⚠ Binding the confirmation to a FACT OF THE PLAN is what makes a line copied from a previous run
/// against a different plan fail to match — the same property the MCP surface's `preview_token`
/// buys, with no token and no state. The caller has already refused the no-terminal case, so this
/// is only ever reached with a person on the other end.
fn confirm_typed(matched: usize) -> CmdResult<()> {
    confirm_phrase(&format!("delete {matched} series"), "nothing was deleted")
}

/// Read one line and require it to equal `want` — [`confirm_typed`]'s rule for any verb that plans
/// a write and asks a person to bind the confirmation to one of the plan's numbers (`rm`'s
/// `delete N series`, `import`'s `import N days`). `undone` is what the refusal says did NOT happen.
///
/// ⚠ The caller refuses the no-terminal case BEFORE it gets here, on the usage rung: this function
/// would read a pipe as readily as a person, which is the one thing a confirmation must not do.
pub(super) fn confirm_phrase(want: &str, undone: &str) -> CmdResult<()> {
    eprintln!("type `{want}` to confirm, or anything else to abort:");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| CliError::failed(format!("reading the confirmation: {e}")))?;
    if line.trim() == want {
        Ok(())
    } else {
        Err(CliError::failed(format!("not confirmed (expected `{want}`) — {undone}")))
    }
}

/// The REMOTE route's `--json` document. Unlike the local route's, this side BUILDS it: the answer
/// arrives as typed data over the wire rather than as another process's stdout, so there is nothing
/// to nest and nothing to re-parse.
///
/// ⚠ **`refused` is new and it closes a hole, not a gap in symmetry.** The ENGINE's document
/// (`crates/vike-backtest/src/backtest_cli.rs`'s `rm_series_json`) has always carried the
/// per-series provenance refusals; this one had no field for them because this route never
/// consulted `RemovalPlan::verdict` at all. A machine reader driving `--json --addr` therefore saw
/// a provenance refusal as a bare error string with no document — the one failure whose whole
/// value is the LIST of which series objected.
///
/// ⚠ Three `--json` shapes still exist for this one verb (engine, local-nested, remote-built) and
/// this PR does not fold them: the engine's carries the resolved store ROOT and its rung, which
/// this side cannot know, and the remote's carries `addr` and `plan`, which the engine's has no
/// use for. A caller branches on `route`. Recorded rather than silently tolerated.
fn rm_json_remote(
    args: &Args,
    rm: &RmArgs,
    plan: &vike_datahub_client::proto::RemovalPlan,
    outcome: Option<&vike_datahub_client::proto::RemovalOutcome>,
    refused: Option<&[String]>,
) -> String {
    let doc = serde_json::json!({
        "subcommand": args.sub.as_str(),
        "route": "datahub",
        // ⚠ `store` is the ADDRESS, never a path: the resolved root is the SERVER's, and a
        // directory named here would be this side's guess about another box's filesystem — the
        // exact class of confident-wrong answer `report_json`'s `store: null` rule exists for.
        "addr": args.addr,
        "store": serde_json::Value::Null,
        "selector": rm_selector_json(rm),
        "produced_by": rm.produced_by.clone(),
        "dry_run": rm.dry_run,
        "matched": plan.matched(),
        "rows": plan.rows(),
        "bytes": plan.bytes(),
        "series": plan.series,
        "plan": plan.lines(),
        "outcome": outcome,
        // `null` when the assertion held; the per-series refusals otherwise — the same field, with
        // the same meaning, the engine's own document carries.
        "refused": refused,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, arrays and nulls; serialization is total")
}

// ─── `repair`: the one verb that reaches a series the store cannot ENUMERATE ────────────────────
