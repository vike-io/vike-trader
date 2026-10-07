//! The ENGINE route: the `backtest data <sub>` child the local verbs spawn, and its `--json` report.
use std::path::Path;

use super::{Args, Sub, Window};
use crate::cmd::data::shared::Source;
use crate::cmd::engine;
use crate::exit::CmdResult;

/// The engine SUBCOMMAND every engine route spawns — `backtest data <sub> …`: the writers
/// (`fetch --source starter|demo`, a local `rm`, `repair`) and `export`'s Parquet route, which is a
/// READ since 2026-09-26 (its engine asks a datahub for the bars and only encodes the file) and
/// spawns the engine all the same.
///
/// Spelled once here because [`engine_argv`] and [`rm_engine_argv`] both build it, and it is the
/// name of a surface in ANOTHER crate
/// (`crates/vike-backtest/src/backtest_cli.rs`'s `DATA_SUBCOMMAND`): two literals would be two
/// places to notice a rename, and the failure mode is a child process refusing an argv this side
/// believes it built correctly.
pub(super) const ENGINE_DATA_VERB: &str = "data";

/// Build the child's argv and hand it to [`crate::cmd::engine`]. Nothing else happens on this
/// path: the store is opened, written and reported on by the engine, whose streams this process
/// inherits.
///
/// ⚠ Under `--json` the child's STDOUT is read back instead of inherited, and this process emits
/// [`report_json`] there instead — because stdout under `--json` is the document and nothing else,
/// the same rule `crate::cmd::secrets`'s `list` and `crate::cmd::init` follow. The engine's report
/// is not lost: [`engine::run_capturing_stdout`] echoes every line to stderr as it arrives, and the
/// document carries the same lines verbatim.
pub(super) fn execute_engine(args: &Args, project_root: Option<&Path>) -> CmdResult<()> {
    let program = engine::locate(args.engine.as_deref(), project_root);
    let argv = engine_argv(args);
    if !args.json {
        return engine::run(&program, &argv, "data");
    }
    let report = engine::run_capturing_stdout(&program, &argv, "data")?;
    println!("{}", report_json(args, &program, &argv, &report));
    Ok(())
}

/// The `--json` document: what was asked for, what ran, and what the engine said about it.
///
/// # What is in it, and what each field is honest about
///
/// * `store` is what `--store` NAMED, or `null` when the flag was absent. It is not a resolved
///   path, and must not be printed as one: on a WRITER with no `--store` the ENGINE resolves the
///   root (its `--store` > `$VIKE_HIST_STORE` > default chain), in a different process, and this
///   side would be guessing. `null` hands a caller the absence rather than a sentence to
///   pattern-match — the same shape `crate::cmd::secrets`'s `list --json` gives an absent store.
///   ⚠ On `export` it is `null` ALWAYS, since 2026-09-26: `--store` is refused on that verb and
///   the engine reads no store at all — it asks a DATAHUB for the bars (decision 0084's amendment).
///   **There is no field naming that datahub, deliberately.** The engine resolves the address
///   itself (`config.datahub_addr`, with `VIKE_DATAHUB_ADDR` above it, loopback by default), and
///   a field this side filled in would be a second resolution of one fact that could disagree with
///   the first. The provenance is in `report` instead: the engine's success line ends
///   `(read from <datahub>)`, naming the hub it actually dialled.
/// * `series` and `window` are the parsed request, split into fields so a caller need not re-split
///   `VENUE:SYMBOL:INTERVAL` or guess which window form was used. Both are `null` for `seed-demo`,
///   which takes neither (the parser REFUSES a spec or a window there rather than ignoring one).
/// * `engine` and `engine_argv` are the command this process actually ran. The verb's whole product
///   is that argv — `crates/vike-cli/tests/data_cli/rm_repair.rs`'s
///   `fetch_and_seed_demo_reach_the_engine_as_its_own_flags` says so for the human path — so a
///   machine reader gets it rather than having to infer it.
/// * `report` is the engine's own stdout, line by line, VERBATIM.
///
/// ⚠ **`report` is where the counts are, and they are not parsed into fields.** `data hist fetch`
/// prints `N bars returned … M rows written` and `data seed-demo` prints a line per slice; neither has
/// a `--json` mode of its own, so the only way to field those numbers would be to read them out of
/// the sentences — a second implementation of another crate's output format, in a crate that cannot
/// see it change, which would start reporting a WRONG count rather than failing on the day a word
/// moves. Giving a caller the lines is honest; claiming to have understood them would not be. The
/// day the engine grows a machine report for these two paths, this field becomes structured and
/// the change is one function.
pub(crate) fn report_json(
    args: &Args,
    program: &engine::Engine,
    argv: &[String],
    report: &[String],
) -> String {
    let doc = serde_json::json!({
        // [`Sub::as_str`], not a second table: the refusals in `parse` name a subcommand by that
        // one spelling, and a document that named it differently would be the same verb under two
        // names in one session.
        "subcommand": args.sub.as_str(),
        "store": args.store.clone(),
        "series": args.spec.as_deref().map(|spec| {
            // Three non-empty parts by construction: `check_spec` refused anything else before a
            // process was started, so this split cannot be partial.
            let mut parts = spec.splitn(3, ':');
            serde_json::json!({
                "venue": parts.next().unwrap_or_default(),
                "symbol": parts.next().unwrap_or_default(),
                "interval": parts.next().unwrap_or_default(),
                // The spelling the operator typed, kept beside the split so a caller echoing the
                // request back does not have to reassemble it.
                "spec": spec,
            })
        }),
        // ⚠ TWO shapes and a null, because two subcommands bound a range and they bound
        // DIFFERENT things: `fetch`'s [`Window`] is one form or the other and is REQUIRED;
        // `export`'s [`ExportRange`] is two independent optional bounds over what the store
        // already holds. Rendering them as one shape would make a caller unable to tell an absent
        // bound from an absent window.
        "window": match (&args.window, &args.export_range) {
            (Some(Window::Days(d)), _) => serde_json::json!({ "days": d }),
            (Some(Window::Range { from, to }), _) => serde_json::json!({ "from": from, "to": to }),
            (None, Some(range)) => serde_json::json!({ "from": range.from, "to": range.to }),
            (None, None) => serde_json::Value::Null,
        },
        // `export`'s destination, and `null` everywhere else — the one fact a caller driving an
        // export needs back and cannot re-derive from the report prose.
        "out": args.out.clone(),
        // ⚠ THE AXIS, and it is in the document because the collapse would otherwise LOSE
        // information a caller already had. Before it, `subcommand` alone distinguished
        // `seed-demo` from `fetch-starter`; now both are `fetch` and only this field separates
        // them. `null` for a venue fetch and for every other verb — the flag names a choice only
        // `fetch` makes, so a value elsewhere would assert one nobody took.
        "source": match (args.sub, args.source) {
            (Sub::Fetch, Source::Starter) => serde_json::json!("starter"),
            (Sub::Fetch, Source::Demo) => serde_json::json!("demo"),
            _ => serde_json::Value::Null,
        },
        "engine": program.display(),
        "engine_argv": argv,
        "report": report,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, arrays and nulls; serialization is total")
}

/// The engine argv this verb's arguments become — PURE, so the translation is unit-tested rather
/// than only observed through a spawn.
///
/// ⚠ **It is `backtest data <sub> …` since ruling 12, and the translation is now a RENAME rather
/// than a re-shape.** The engine used to carry `--fetch`/`--seed-demo`/`--export`/`--fetch-starter`
/// as flags on the backtest verb; it carries a `data` SUBCOMMAND now
/// (`crates/vike-backtest/src/backtest_cli/data_cmd.rs`'s `run_data`), spelled with the same words
/// as this side. So each line below reads as the command the operator typed, which is the property
/// that makes an argv assertion in `crates/vike-cli/tests/data_cli.rs` legible as a contract rather
/// than as a mapping table.
///
/// ⚠ **The engine flags did NOT retire from the SPAWN — only from the engine's own help.** This
/// argv is what makes `vike-cli data` work at all: opening a hist store needs DataFusion and this
/// crate links none. Deleting the engine's `data` subcommand deletes these verbs with it.
pub(super) fn engine_argv(args: &Args) -> Vec<String> {
    // ⚠ The CHILD's verb, which is NOT always ours. `fetch --source starter` spawns the engine's
    // own `fetch-starter`: that binary has its own surface and its own compatibility story, and
    // collapsing our three verbs into one must not collapse its. [`Source::engine_verb`] is the
    // seam, and a test asserts the child argv rather than trusting this line.
    let child_verb = args.source.engine_verb().unwrap_or_else(|| args.sub.as_str());
    let mut argv = vec![ENGINE_DATA_VERB.to_string(), child_verb.to_string()];
    match args.sub {
        Sub::Export => {
            // Both present by construction: `parse` refuses an `export` without a spec or `--out`.
            argv.push(args.spec.clone().unwrap_or_default());
            argv.push("--out".to_string());
            argv.push(args.out.clone().unwrap_or_default());
            // ⚠ INDEPENDENTLY, unlike `fetch`'s window — see [`ExportRange`] for why an export's
            // two bounds are neither paired nor required.
            if let Some(range) = &args.export_range {
                for (flag, value) in [("--from", &range.from), ("--to", &range.to)] {
                    if let Some(v) = value {
                        argv.push(flag.to_string());
                        argv.push(v.clone());
                    }
                }
            }
        }
        // ⚠ THE ONLY OTHER REACHABLE ARM, and it carries no argument at all. `fetch` reaches this
        // function ONLY under `--source starter|demo` ([`execute`] sends `Source::Venue` to a
        // datahub instead), and those two sources take no spec and no window — the whole of their
        // argv is the child verb plus `--store`. [`Source::engine_verb`] has already chosen that
        // child verb above, which is why one arm serves both.
        //
        // ⚠ This used to push a spec and a window, back when `fetch` drove the engine against a
        // local store. It does not any more, and the flags did not move here — they went to
        // `Request::Backfill`. An `if let Some(spec)` left behind would hand the child a starter
        // pull with a series name on it.
        Sub::Fetch => {}
        // ⚠ UNREACHABLE: [`execute`] routes the read verbs to their own arms, and `rm`/`repair`
        // build their argv in [`rm_engine_argv`] / [`repair_engine_argv`] — each selector has flags
        // of its own and folding them in here would make one function answer for three grammars.
        // Named rather than folded into a `_`, deliberately: a wildcard would silently absorb a
        // future subcommand that DOES need an engine flag and hand the child an argv with the flag
        // missing instead of failing to compile — which is exactly what it would have done to
        // `Sub::Gaps`.
        Sub::Running
        | Sub::Cancel
        | Sub::Import
        | Sub::Get
        | Sub::List
        | Sub::Gaps
        | Sub::Coverage
        | Sub::TapeHealth
        | Sub::Universe
        | Sub::Gate
        | Sub::Rm
        | Sub::Repair => {}
    }
    if let Some(store) = &args.store {
        argv.push("--store".to_string());
        argv.push(store.clone());
    }
    argv
}

// ─── the read half: two verbs over a running datahub ────────────────────────────────────────────
