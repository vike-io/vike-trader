//! `backtest data <sub>`: the retired-flag refusals, the router and its arms.
//!
//! ⚠ The argv triage that stood here (`DATA_FLAGS`, `DATA_SUBS`, `triage_data_argv`) is DELETED
//! (decision 0155): the shape of a `data` command line — an unknown option, a boolean given a
//! value, a valued flag given none, a positional where the subcommand takes none or missing where
//! it needs one — is `clap`'s refusal now, over the types in `args.rs`, and it still runs BEFORE
//! any arm, so a refused command line opens (which means CREATES — every `DataFusionHist::open`
//! `create_dir_all`s its root) nothing. What stays here is everything that is not the SHAPE.

use std::process::ExitCode;

use vike_data::demo as demo_tape;
use vike_model::run_request::flag_vocab::store_flag_removed;
use vike_store::DataFusionHist;

use super::DATA_SUBCOMMAND;
use super::args::{DataFlags, DataSub, RunArgs, usage_text};
use crate::binutil::store_root;

mod export;
mod fetch_starter;
mod repair_series;

use self::export::run_export;
use self::fetch_starter::run_fetch_starter;
use self::repair_series::run_repair_series;

// ─── `backtest data`: the five operations ruling 12 moved off the flag surface ───────────────────

/// The five data-management flags ruling 12 retired, and what each became: `(flag, engine_sub,
/// replacement, tail)`. `tail` is the one sentence that is not shared.
///
/// ⚠ **`--rm-series`' tail says NOTHING WAS DELETED in as many words, and that is the reason this
/// is a table of four columns rather than three.** An operator whose cleanup script now exits 2 must
/// not be left reading the refusal as "it may have partially run" — a delete verb's refusal owes
/// that sentence in a way a fetch's does not.
///
/// ⚠ **`engine_sub` is a COLUMN because deriving it was wrong on the one row that deletes.** It was
/// computed as `flag.trim_start_matches("--")`, which is the `data` subcommand's name for four of
/// the five rows and `rm-series` for the fifth — a subcommand `clap` refuses. So the refusal for
/// the retired DELETE flag sent an engine-only operator to `backtest data rm-series`, which
/// answers "unknown `data` subcommand". A `contains` test could not see it either, because
/// `"rm-series"` contains `"rm"`; `every_retired_data_flag_names_its_replacement_and_a_real_subcommand` compares against the
/// `clap` command's own subcommands instead, which is what decides.
///
/// A table rather than five hand-written `if`s so [`refuse_a_retired_data_flag`] and this file's own
/// `every_retired_data_flag_names_its_replacement_and_a_real_subcommand` iterate the same rows.
pub(super) const RETIRED_DATA_FLAGS: &[(&str, &str, &str, &str)] = &[
    ("--fetch", "fetch", "vike-cli data hist fetch VENUE:SYMBOL:INTERVAL --days N", ""),
    ("--fetch-starter", "fetch-starter", "vike-cli data hist fetch --source starter", ""),
    ("--seed-demo", "seed-demo", "vike-cli data hist fetch --source demo", ""),
    ("--export", "export", "vike-cli data hist export VENUE:SYMBOL:INTERVAL --out FILE", ""),
    (
        "--rm-series",
        "rm",
        "vike-cli data hist rm --kind K --venue V …",
        " NOTHING WAS DELETED — this refusal happened before a store was opened.",
    ),
];

/// What the command line wrote for a retired flag: `Some(written)` when `flag` was given, with its
/// value if it carried one (`--fetch binance:BTCUSDT:1h`), `None` when it was not.
///
/// ⚠ A function over the DECLARED fields rather than a scan of the argv, and the match is on the
/// exact long name: `--fetch` and `--fetch-starter` are two fields, so neither can answer for the
/// other (the adjacent-prefix landmine the exact-token scanner was anchored against).
pub(super) fn retired_written(run: &RunArgs, flag: &str) -> Option<String> {
    let given = match flag {
        "--fetch" => &run.retired.fetch,
        "--fetch-starter" => &run.retired.fetch_starter,
        "--seed-demo" => &run.retired.seed_demo,
        "--export" => &run.retired.export,
        "--rm-series" => &run.retired.rm_series,
        _ => return None,
    };
    given.as_ref().map(|value| match value {
        Some(v) => format!("{flag} {v}"),
        None => flag.to_string(),
    })
}

/// Refuse one of the five retired spellings by name, echoing what was written.
///
/// `None` when argv carries none of them, so [`super::run`] falls through unchanged. Checked in
/// [`RETIRED_DATA_FLAGS`] order, so a line carrying two of them names the first — which is enough:
/// the message tells the operator the whole family moved.
///
/// ⚠ The inline spelling (`--fetch=binance:BTCUSDT:1h`) is read like the spaced one, and a retired
/// flag written with NO value is read too: a retirement that missed either would let exactly the
/// scripted invocations through — the ones nobody is watching.
pub(super) fn refuse_a_retired_data_flag(run: &RunArgs) -> Option<ExitCode> {
    let (flag, sub, replacement, tail, written) = RETIRED_DATA_FLAGS
        .iter()
        .find_map(|(f, s, r, t)| retired_written(run, f).map(|w| (*f, *s, *r, *t, w)))?;
    eprintln!(
        "backtest: {flag} is retired — data management moved to `vike-cli data` (ruling 12 of \
         docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md). You wrote \
         {written:?}; write `{replacement}` instead, or `backtest {DATA_SUBCOMMAND} {sub}` on a \
         box with only this engine.{tail}"
    );
    Some(ExitCode::from(2))
}

/// The [`DataSub`]s that READ history rather than write a store — and therefore refuse `--store`.
///
/// ⚠ **One row, and it is the verb decision 0084's 2026-09-25 amendment named as NOT closed.** The
/// ruling closed the local READ door on every history reader; `data export` was left open because
/// it lives among the WRITERS while what it does is read a series out to a file. It reads over the
/// wire now ([`run_export`]) and refuses `--store` by name with
/// `vike_model::run_request::flag_vocab::store_flag_removed`, the one sentence every reader prints.
///
/// ⚠ **`rm --dry-run` and `repair`'s default rehearsal are NOT rows here**, although each run
/// only reads. They are rehearsals OF a write — the plan a deletion or a rebuild would execute
/// against the store the operator named — and a rehearsal that read a different store than the
/// write it rehearses would be a plan for somebody else's data. The writer half of the verdict is
/// unchanged, so they keep `--store` with the verb they rehearse.
///
/// A table rather than a `sub == "export"` so the next reader-shaped verb joins by adding a row, and
/// so `every_data_read_is_a_real_data_subcommand` can hold it against the `clap` command.
pub(super) const DATA_READS: &[&str] = &["export"];

/// Refuse `--store` on a [`DATA_READS`] verb, BY NAME and with its replacement named — `None` when
/// the verb writes or the flag is absent.
///
/// PURE and checked BEFORE any arm. `--store` is a declared flag of every `data` subcommand so that
/// this sentence is what the operator reads (`--store=DIR` and `--store DIR` alike); a trailing
/// value-less `--store` is `clap`'s "a value is required" instead, which is also a refusal.
pub(super) fn refuse_a_store_on_a_data_read(sub: &str, flags: &DataFlags) -> Option<String> {
    (DATA_READS.contains(&sub) && flags.store.is_some())
        .then(|| store_flag_removed(&format!("backtest {DATA_SUBCOMMAND} {sub}")))
}

/// Print the `data` page (`backtest data --help`) to STDOUT and succeed: help is output a user
/// pipes into a pager, not a diagnostic. A bare `backtest data` is the same question with no verb.
fn data_help() -> ExitCode {
    let version = super::args::version_line();
    let mut command = super::args::program(&version).command::<super::args::BacktestArgs>();
    command.build();
    if let Some(data) = command.find_subcommand_mut(DATA_SUBCOMMAND) {
        println!("{}", data.render_long_help());
    }
    ExitCode::SUCCESS
}

/// Route `backtest data <sub>`. `sub` is what `clap` read after the `data` word (`None` for a bare
/// `backtest data`).
///
/// Each arm is the SAME function the retired flag called, handed the parsed flags — so this is a
/// rename of the door, never a second implementation. The refusals move with their
/// arms and name the new spelling.
pub(super) fn run_data(
    vars: &std::collections::HashMap<String, String>,
    sub: Option<DataSub>,
) -> ExitCode {
    let Some(sub) = sub else {
        return data_help();
    };
    // ⚠ BEFORE any arm and before any I/O — see [`refuse_a_store_on_a_data_read`]. Exit 2, the
    // usage rung every other argv refusal on this verb takes: nothing was tried.
    let (name, flags) = match &sub {
        DataSub::Fetch(spec) => ("fetch", &spec.flags),
        DataSub::FetchStarter(flags) => ("fetch-starter", flags),
        DataSub::SeedDemo(flags) => ("seed-demo", flags),
        DataSub::Export(spec) => ("export", &spec.flags),
        DataSub::Rm(flags) => ("rm", flags),
        DataSub::Repair(flags) => ("repair", flags),
        DataSub::Help { .. } => return data_help(),
    };
    if let Some(why) = refuse_a_store_on_a_data_read(name, flags) {
        eprintln!("{why}");
        return ExitCode::from(2);
    }
    // A WRITER keeps its `--store`, and a value-less one is a usage error for it.
    if matches!(flags.store, Some(None)) {
        eprintln!(
            "backtest {DATA_SUBCOMMAND} {name}: --store requires a value\n\n{}",
            usage_text(&[DATA_SUBCOMMAND, name])
        );
        return ExitCode::from(2);
    }

    match sub {
        DataSub::SeedDemo(flags) => run_seed_demo(vars, &flags),
        DataSub::Rm(flags) => run_rm_series(vars, &flags),
        DataSub::Repair(flags) => run_repair_series(vars, &flags),
        DataSub::Help { .. } => data_help(),
        // ⚠ **RETIRED, and answered BY NAME rather than falling through to a catch-all.** This
        // arm used to call `run_fetch`, which called `crate::fetch::fetch_into`, which called
        // `vike_binance::data::fetch_klines_range` — the one venue call in the compute plane. It is
        // deleted: history is fetched by the DATA plane, once, into the store.
        //
        // The refusal is spelled out rather than left to a bare "unknown" for the same reason
        // [`RETIRED_DATA_FLAGS`] exists: this is a verb operators have typed, it is installed on a
        // box where `vike-backtest.service` runs, and a bare "unknown" teaches nothing. ⚠ The
        // replacement is NOT a drop-in — it needs a reachable datahub, which the direct call did
        // not — so the message says so rather than implying a rename.
        //
        // `fetch` is still a SUBCOMMAND (with its positional) so that this refusal is REACHED:
        // deleting it would make `backtest data fetch binance:BTCUSDT:1h --days 180` fail on an
        // unknown subcommand instead of on its retirement, which teaches the operator nothing.
        DataSub::Fetch(_) => {
            eprintln!(
                "backtest data fetch: RETIRED — this binary no longer reaches a venue at all.\n\
                 \x20 use: vike-cli data hist fetch VENUE:SYMBOL:INTERVAL --days N [--addr HOST:PORT]\n\
                 \n\
                 That asks a DATAHUB to fetch the window into the store, which is where a fetch \
                 belongs: the datahub collects SIX venues where this path reached one, and it \
                 drops a still-forming candle, which this path never did.\n\
                 \x20 ⚠ It needs a reachable datahub (default 127.0.0.1:7878). This command needed \
                 no server, so that is a real precondition and not a renamed flag.\n\
                 \x20 With no datahub and no network: `backtest data fetch-starter` downloads the \
                 published dataset, and `backtest data seed-demo` needs neither."
            );
            ExitCode::from(2)
        }
        DataSub::FetchStarter(flags) => run_fetch_starter(vars, &flags),
        // ⚠ **`export` was gated on a `venue-fetch` feature once (the feature is DELETED now), and it
        // should never have been.** That arm carried a cfg and a refusal blaming the feature for
        // "carrying the Parquet writer's caller". Measured, that was simply false: `export` reaches
        // no VENUE at all, and `vike_store::write_bars_parquet` has no `#[cfg]` of its own beyond the
        // `vike-store` dependency. (It read the local store until 2026-09-26; it asks a DATAHUB for
        // the bars now — [`run_export`] — which is a socket, not a venue, and needs no feature
        // either.) Every build of this crate can export.
        DataSub::Export(spec) => run_export(vars, &spec.flags, &spec.spec),
    }
}

/// `data seed-demo` — the SYNTHETIC tape, and the answer to an empty store that needs no network.
///
/// ⚠ THE EMPTY-STORE ANSWER, and it needs no profile and no strategy registry, only a store root.
///
/// A clean install has an empty hist store, so the shipped example profile — which names a slice —
/// reports a run with no trades, and nothing distinguishes that from a strategy that never fired.
/// `vike_data::demo` writes a tape the shipped profile already names;
/// `crates/vike-cli/tests/demo_tape_profile.rs` holds the two in agreement, so this writes not
/// "some data" but THE data the next command reads.
///
/// Deliberately on this binary rather than in `vike-cli`: writing a store needs `DataFusionHist`,
/// and vike-cli is DataFusion-free BY CONSTRUCTION (the `light-consumers` CI lane asserts it). The
/// tool that CONSUMES hist data is the honest place for the command that creates some — which is
/// why ruling 12 moved the SPELLING and not this function.
fn run_seed_demo(vars: &std::collections::HashMap<String, String>, flags: &DataFlags) -> ExitCode {
    let root = store_root(flags.store_path(), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    let seeded = match demo_tape::seed(&store) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: seeding {} failed: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    // The provenance line FIRST, because a synthetic tape that is not announced as one is the
    // hazard this whole feature carries: a result computed on invented prices reads exactly like a
    // result computed on real ones.
    println!(
        "seeded SYNTHETIC demo bars (venue `{}` — a closed-form curve, NOT market data) into {}",
        demo_tape::DEMO_VENUE,
        root.display()
    );
    let mut written = 0usize;
    for done in &seeded {
        written += done.rows;
        println!(
            "  {}/{} {}  {} bars{}",
            demo_tape::DEMO_VENUE,
            done.slice.symbol,
            done.slice.interval,
            done.slice.len(),
            if done.rows == 0 { "  (already present — nothing written)" } else { "" }
        );
    }
    if written == 0 {
        println!("this store already held the demo tape; nothing was written.");
    }
    ExitCode::SUCCESS
}

// ─── `data rm`: emptying the store ─────────────────────────────────────────────────────────────

/// `backtest data rm` — DELETE stored series, irreversibly.
///
/// # Why this verb is on the ENGINE
///
/// For `data export`'s reason, one step further: emptying a store needs `DataFusionHist`, and
/// `vike-cli` is DataFusion-FREE by construction (its manifest argues every edge; CI's
/// `light-consumers` lane asserts it). `vike-cli data hist rm` is a route to this arm, exactly as
/// `vike-cli data hist fetch --source demo` is a route to `data seed-demo`. It is a first-class
/// verb here rather than only a spawn target because an operator ON the box — which is where a
/// cleanup happens, and where the 2026-09-07 the CI box cleanup DID happen — should not need a datahub
/// to empty their own store.
///
/// # ⚠ The plan LEADS with the store, and it leads on STDOUT
///
/// `binutil::store_root` already logs the resolved root and the rung that chose it, through
/// `tracing::info!` — which `RUST_LOG` silences. "Which store" is the question a destructive verb
/// must answer before "which series", and a store does not MERGE: a resolution that moved is
/// invisible until it has destroyed the wrong tree. So the first line of every run, dry or not,
/// carries `StoreRoot`'s `Display` — the path and the sentence saying why it is that path.
///
/// # The confirmation
///
/// `--yes`, or a TERMINAL on which the operator types `delete N series` for the N this plan
/// matched. Binding the confirmation to a fact of the plan is what makes a line copied from a
/// previous run against a different plan fail to match, with no token and no state.
///
/// ⚠ **No `--yes` and no terminal is a REFUSAL, never a read.** `yes | backtest data rm …` is
/// the exact failure this exists to prevent, and a pipe is indistinguishable from a person once you
/// have decided to read one.
fn run_rm_series(vars: &std::collections::HashMap<String, String>, flags: &DataFlags) -> ExitCode {
    use std::io::IsTerminal;
    use vike_data::store::removal::SeriesSelector;

    // ⚠ FIRST, before the store is opened: the root AND the rung that chose it, on stdout.
    let resolved = crate::binutil::store_root_resolved(flags.store_path(), vars);
    let json = flags.json;
    // The store LEADS, on stdout for a human and on stderr under `--json` (where stdout is the
    // document and the same fact rides `store_root`/`store_rung` inside it).
    if json {
        eprintln!("store: {resolved}");
    } else {
        println!("store: {resolved}");
    }

    let kind = match SeriesSelector::parse_kind(flags.kind.as_deref()) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("backtest data rm: {e}\n\n{}", usage_text(&["data", "rm"]));
            return ExitCode::from(2);
        }
    };
    let selector = SeriesSelector {
        kind,
        venue: flags.venue.clone().unwrap_or_default(),
        symbol: flags.symbol.clone(),
        group: flags.group.clone(),
        interval: flags.interval.clone(),
    };
    if let Err(e) = selector.validate_shape() {
        eprintln!("backtest data rm: {e}\n\n{}", usage_text(&["data", "rm"]));
        return ExitCode::from(2);
    }
    // Resolved BEFORE the store is opened: a `--produced-by` spelling is a fact about the argument,
    // and refusing a typo without touching a store is the cheaper failure.
    let produced_by =
        match flags.produced_by.as_deref().map(vike_data::store::store_kind::resolve_produced_by) {
            Some(Ok(p)) => Some(p),
            Some(Err(e)) => {
                eprintln!("backtest data rm: {e}");
                return ExitCode::from(2);
            }
            None => None,
        };
    // ⚠ The SWEEP rule, and it is the engine's as much as the CLI's: this arm is reachable directly.
    if selector.is_sweep() && produced_by.is_none() {
        eprintln!(
            "backtest data rm: `{}` matches more than one series, so --produced-by is \
             REQUIRED. Deleting a whole sweep by name alone is what that assertion exists to \
             replace; name every dimension instead, or pass the commit-key prefix the rows carry.",
            selector.describe()
        );
        return ExitCode::from(2);
    }

    let store = match DataFusionHist::open(&resolved.root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", resolved.root.display());
            return ExitCode::from(2);
        }
    };
    let plan =
        match vike_data::store::removal::plan_removal(&store, &selector, produced_by.as_deref()) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("backtest data rm: {e}");
                return ExitCode::from(2);
            }
        };

    let dry_run = flags.dry_run;
    // ⚠ Under `--json` the plan goes to STDERR, not nowhere. Stdout is the document and nothing
    // else, but a run that is about to ask a human to type `delete N series` must have SHOWN them
    // what N is made of — and without this the operator was prompted having seen no plan at all.
    // Same stream every diagnostic in this workspace uses, and the same shape
    // `crates/vike-cli/src/cmd/engine.rs`'s `run_capturing_stdout` already gives the engine's own
    // lines.
    for line in plan.lines() {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
    // A provenance refusal is reported and deletes nothing, whether or not this was a dry run.
    if let Err(refusals) = plan.verdict() {
        if json {
            println!("{}", rm_series_json(&resolved, &plan, None, Some(&refusals)));
        }
        eprintln!("backtest data rm: provenance REFUSED — nothing was deleted");
        return ExitCode::from(2);
    }
    // ⚠ `--dry-run` WINS over `--yes`: a rehearsal must not require stripping a flag.
    if dry_run {
        if json {
            println!("{}", rm_series_json(&resolved, &plan, None, None));
        } else {
            println!("--dry-run: nothing was deleted");
        }
        return ExitCode::SUCCESS;
    }
    // "Nothing matched" is a SUCCESS on the same rung as a delete: `delete_series` is idempotent,
    // and a cleanup that fails on re-run is a cleanup nobody re-runs.
    if plan.matched() == 0 {
        if json {
            println!("{}", rm_series_json(&resolved, &plan, Some(&Default::default()), None));
        }
        return ExitCode::SUCCESS;
    }
    if let Err(e) = confirm_removal(&plan, flags.yes, std::io::stdin().is_terminal()) {
        eprintln!("backtest data rm: {e}");
        return ExitCode::from(2);
    }

    match vike_data::store::removal::execute_removal(&store, &plan) {
        Ok(outcome) => {
            if json {
                println!("{}", rm_series_json(&resolved, &plan, Some(&outcome), None));
            } else {
                for id in &outcome.deleted {
                    println!("deleted {}", vike_data::store::removal::describe_id(id));
                }
                for (id, why) in &outcome.failed {
                    eprintln!("FAILED {}: {why}", vike_data::store::removal::describe_id(id));
                }
                println!(
                    "{} of {} series deleted",
                    outcome.deleted.len(),
                    outcome.deleted.len() + outcome.failed.len()
                );
            }
            // One broken series is one SKIPPED series (`run_maintenance`'s rule) — reported, the
            // rest continue, and the exit is non-zero so a wrapper knows to look.
            if outcome.is_clean() { ExitCode::SUCCESS } else { ExitCode::from(2) }
        }
        Err(e) => {
            eprintln!("backtest data rm: {e}");
            ExitCode::from(2)
        }
    }
}

/// The confirmation gate — PURE over its two inputs, so both branches are unit-tested rather than
/// only reachable from a terminal.
///
/// `stdin_is_terminal` is a PARAMETER for that reason; the caller reads the real one.
pub(super) fn confirm_removal(
    plan: &vike_data::store::removal::RemovalPlan,
    yes: bool,
    stdin_is_terminal: bool,
) -> Result<(), String> {
    if yes {
        return Ok(());
    }
    if !stdin_is_terminal {
        return Err(format!(
            "refusing to delete {} series without --yes: stdin is not a terminal, so there is \
             nobody to confirm. A confirmation read from a PIPE is not a confirmation — \
             `yes | backtest data rm …` is exactly what this refuses.",
            plan.matched()
        ));
    }
    let want = format!("delete {} series", plan.matched());
    eprintln!("type `{want}` to confirm, or anything else to abort:");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| format!("reading the confirmation: {e}"))?;
    if line.trim() == want {
        Ok(())
    } else {
        Err(format!("not confirmed (expected `{want}`) — nothing was deleted"))
    }
}

/// The `--json` document for `data rm`.
///
/// ⚠ It carries the RESOLVED store root and its RUNG, and that is the one fact that cannot survive
/// a prose round trip: `vike-cli data hist rm --json` wraps this document rather than re-deriving the
/// root, because the CLI resolves nothing (the engine runs in another process) and a path it
/// guessed at would be the confident-sounding wrong answer.
fn rm_series_json(
    resolved: &vike_paths::store_path::StoreRoot,
    plan: &vike_data::store::removal::RemovalPlan,
    outcome: Option<&vike_data::store::removal::RemovalOutcome>,
    refusals: Option<&[String]>,
) -> String {
    let doc = serde_json::json!({
        "store_root": resolved.root.display().to_string(),
        "store_rung": resolved.rung.as_str(),
        "store_rung_why": resolved.rung.why(),
        "selector": plan.selector,
        "produced_by": plan.produced_by,
        "matched": plan.matched(),
        "rows": plan.rows(),
        "bytes": plan.bytes(),
        "series": plan.series,
        // `null` for a dry run and for a refusal — the two cases where nothing was attempted.
        "outcome": outcome,
        // `null` when the assertion held; the per-series refusals otherwise.
        "refused": refusals,
    });
    serde_json::to_string_pretty(&doc).expect("a tree of plain data; serialization is total")
}
