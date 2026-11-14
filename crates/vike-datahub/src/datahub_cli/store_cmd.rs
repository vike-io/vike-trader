//! `vike-backend datahub store <verb>` (`vike-datahub store <verb>`): the local operator tools over a
//! hist store on THIS box, with no daemon in front of them.
//!
//! They live in the crate that owns the store because only the datahub's code may open it
//! (`docs/decisions/0084-only-the-datahub-touches-the-store.md`). They stay LOCAL rather than wire
//! verbs because each one has a case the wire cannot serve: `repair` is for a store the daemon may be
//! unable to open, a key-less datahub serves no delete (decision 0050, which names `rm` here as the
//! answer), and `fetch-starter` is the first run, before any datahub exists. `export`
//! is the exception and the reason this is not called "the writers": it READS over the wire like every
//! reader, and only its Parquet encoding needs this crate.
//!
//! This is a route of its own in [`super::run`], decided before the daemon's boot: nothing here binds a
//! socket or starts a lane. Being in `vike-datahub` does NOT make it the daemon, and
//! it takes no lock against a running one: the series lock is the only arbiter, as it was when these
//! verbs lived on the backtest engine.
//!
//! ⚠ The shape of a command line (an unknown option, a boolean given a value, a valued flag given none,
//! a positional where the subcommand takes none) is `clap`'s refusal, over the types in `args.rs`, and
//! it runs BEFORE any arm, so a refused command line opens (which means CREATES: every
//! `HistoryStoreDataFusion::open` `create_dir_all`s its root) nothing. What stays here is everything that is
//! not the SHAPE.

use std::path::Path;
use std::process::ExitCode;

use vike_model::run_request::flag_vocab::store_flag_removed;
use vike_store::HistoryStoreDataFusion;

use super::args::{StoreFlags, StoreSub, usage_text};
use super::boot::boot_root;

mod export;
#[cfg(feature = "fetch-starter")]
mod fetch_starter;
mod repair_series;
mod root;

use self::export::run_export;
#[cfg(feature = "fetch-starter")]
use self::fetch_starter::run_fetch_starter;
use self::repair_series::run_repair_series;
use self::root::store_root_resolved;

/// The [`StoreSub`]s that READ history rather than write a store — and therefore refuse `--store`.
///
/// ⚠ **One row, and it is the verb decision 0084's 2026-09-25 amendment named as NOT closed.** The
/// ruling closed the local READ door on every history reader; `export` was left open because it
/// lives among the WRITERS while what it does is read a series out to a file. It reads over the wire
/// now ([`run_export`]) and refuses `--store` by name with
/// `vike_model::run_request::flag_vocab::store_flag_removed`, the one sentence every reader prints.
///
/// ⚠ **`rm --dry-run` and `repair`'s default rehearsal are NOT rows here**, although each run only
/// reads. They are rehearsals OF a write — the plan a deletion or a rebuild would execute against the
/// store the operator named — and a rehearsal that read a different store than the write it rehearses
/// would be a plan for somebody else's data. The writer half of the verdict is unchanged, so they
/// keep `--store` with the verb they rehearse.
///
/// A table rather than a `sub == "export"` so the next reader-shaped verb joins by adding a row, and
/// so `every_store_read_is_a_real_store_subcommand` can hold it against the `clap` command.
const STORE_READS: &[&str] = &["export"];

/// Refuse `--store` on a [`STORE_READS`] verb, BY NAME and with its replacement named — `None` when
/// the verb writes or the flag is absent.
///
/// PURE and checked BEFORE any arm. `--store` is a declared flag of every `store` subcommand so that
/// this sentence is what the operator reads (`--store=DIR` and `--store DIR` alike), a trailing
/// value-less `--store` included.
fn refuse_a_store_on_a_read(sub: &str, flags: &StoreFlags) -> Option<String> {
    (STORE_READS.contains(&sub) && flags.store.is_some())
        .then(|| store_flag_removed(&format!("vike-datahub store {sub}")))
}

/// The prefix the rolling file of these verbs takes: not the daemon's own, because
/// `crates/vike-log/src/lib.rs`'s retention prunes one prefix at a time and a one-shot verb must not
/// prune the daemon's log.
const LOG_PREFIX: &str = "datahub-store";

/// Run one `store` verb, from the top: the argv refusals, the boot, the log subscriber, then the arm.
///
/// The file level is read before the subscriber because the subscriber is built once, and it is the
/// one row these verbs read before they know which store they are on.
pub(super) fn run_store(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&Path>,
    sub: StoreSub,
) -> ExitCode {
    // ⚠ BEFORE any arm and before any I/O — see [`refuse_a_store_on_a_read`]. Exit 2, the usage rung
    // every other argv refusal on this verb takes: nothing was tried.
    let (name, flags) = match &sub {
        StoreSub::FetchStarter(flags) => ("fetch-starter", flags),
        StoreSub::Export(spec) => ("export", &spec.flags),
        StoreSub::Rm(rm) => ("rm", &rm.flags),
        StoreSub::Repair(flags) => ("repair", flags),
    };
    if let Some(why) = refuse_a_store_on_a_read(name, flags) {
        eprintln!("{why}");
        return ExitCode::from(2);
    }
    // A WRITER keeps its `--store`, and a value-less one is a usage error for it.
    if matches!(flags.store, Some(None)) {
        eprintln!(
            "vike-datahub store {name}: --store requires a value\n\n{}",
            usage_text(&["store", name])
        );
        return ExitCode::from(2);
    }

    // The process's ONE walk: the log home and `fetch-starter`'s scratch come off this boot's answer.
    // `export` dials a datahub, and a seed naming no store has the `config.store_root` row to answer to
    // (`root.rs`'s module doc), so both take the daemon's whole boot; the rest read no setting.
    let seeds = matches!(sub, StoreSub::FetchStarter(_));
    let boot = if matches!(sub, StoreSub::Export(_)) || (seeds && flags.store_path().is_none()) {
        boot_root(vars, cwd)
    } else {
        boot_local_verb(vars, cwd)
    };
    let booted = match boot {
        Ok(b) => b,
        Err(e) => {
            eprintln!("vike-datahub store {name}: {e}");
            return ExitCode::from(2);
        }
    };

    let read = vike_config::log_file_level(vars, cwd);
    if let Some(line) = read.unread_line(&vike_log::LogConfig::default().file_level) {
        eprintln!("vike-datahub: {line}");
    }
    let mut log = vike_log::LogConfig {
        file_prefix: LOG_PREFIX.to_string(),
        project_dir: booted.log_home.clone(),
        ..Default::default()
    };
    if let Some(level) = read.level {
        log.file_level = level;
    }
    let _log_guards = vike_log::init(log);

    match sub {
        StoreSub::Export(spec) => run_export(&booted, &spec.flags, &spec.spec),
        StoreSub::Rm(rm) => run_rm_series(vars, cwd, &rm.flags, rm.source),
        StoreSub::Repair(flags) => run_repair_series(vars, cwd, &flags),
        #[cfg(feature = "fetch-starter")]
        StoreSub::FetchStarter(flags) => run_fetch_starter(&booted, vars, cwd, &flags),
        // A build without the HTTPS client still describes the verb and says what it lacks, the shape
        // `--record` has on a build without `record`.
        #[cfg(not(feature = "fetch-starter"))]
        StoreSub::FetchStarter(_) => {
            eprintln!(
                "vike-datahub store fetch-starter: this binary was built without the \
                 `fetch-starter` feature, so it carries no HTTPS client. Rebuild with: cargo build \
                 -p vike-datahub --features fetch-starter"
            );
            ExitCode::from(2)
        }
    }
}

/// The boot of a verb that loads nothing: the narrowest slice of the sequence, the walk alone.
fn boot_local_verb(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&Path>,
) -> Result<vike_boot::Booted, String> {
    vike_boot::boot(&vike_boot::BootSpec {
        env: vars,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Ignore(
            "a local store verb reads no ceiling and can place no order, and a box already in trouble \
             must keep its repair and its delete",
        ),
        settings: vike_boot::SettingsLoad::Skip(
            "`--store` names the store outright and a broken settings tree must not stop a repair; \
             the one row these verbs read is the log level, best-effort",
        ),
        credentials: vike_boot::Credentials::DeferredNoVenueKeys(
            "these verbs read no credential at all; the handle is never used",
        ),
        log_home: vike_boot::LogHome::UnderSettings,
        disclosure: vike_boot::Disclosure::Skip("a one-shot verb, not a daemon starting"),
    })
}

// ─── `store rm`: emptying the store ────────────────────────────────────────────────────────────

/// `store rm` — DELETE stored series, irreversibly.
///
/// # Why this verb is local
///
/// Emptying a store needs `HistoryStoreDataFusion`, and `vike-cli` is DataFusion-FREE by construction (its
/// manifest argues every edge; CI's `light-consumers` lane asserts it). `vike-cli data hist rm` is a
/// route to this arm, exactly as `vike-cli data hist fetch --source starter` is a route to `store
/// fetch-starter`. It is a first-class verb here rather than only a spawn target because an operator ON
/// the box — which is where a cleanup happens, and where the 2026-09-07 the CI box cleanup DID happen —
/// should not need a datahub to empty their own store, and a key-less datahub serves no delete at all
/// (`docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`).
///
/// # ⚠ The plan LEADS with the store, and it leads on STDOUT
///
/// `store_root` already logs the resolved root and the rung that chose it, through
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
/// ⚠ **No `--yes` and no terminal is a REFUSAL, never a read.** `yes | vike-datahub store rm …` is
/// the exact failure this exists to prevent, and a pipe is indistinguishable from a person once you
/// have decided to read one.
fn run_rm_series(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&Path>,
    flags: &StoreFlags,
    source: Option<String>,
) -> ExitCode {
    use std::io::IsTerminal;
    use vike_data::store::removal::SeriesSelector;

    // ⚠ FIRST, before the store is opened: the root AND the rung that chose it, on stdout.
    let resolved = store_root_resolved(flags.store_path(), None, vars, cwd);
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
            eprintln!("vike-datahub store rm: {e}\n\n{}", usage_text(&["store", "rm"]));
            return ExitCode::from(2);
        }
    };
    let selector = SeriesSelector {
        kind,
        venue: flags.venue.clone().unwrap_or_default(),
        symbol: flags.symbol.clone(),
        group: flags.group.clone(),
        interval: flags.interval.clone(),
        source,
    };
    if let Err(e) = selector.validate_shape() {
        eprintln!("vike-datahub store rm: {e}\n\n{}", usage_text(&["store", "rm"]));
        return ExitCode::from(2);
    }
    // Resolved BEFORE the store is opened: a `--produced-by` spelling is a fact about the argument,
    // and refusing a typo without touching a store is the cheaper failure.
    let produced_by =
        match flags.produced_by.as_deref().map(vike_data::store::store_kind::resolve_produced_by) {
            Some(Ok(p)) => Some(p),
            Some(Err(e)) => {
                eprintln!("vike-datahub store rm: {e}");
                return ExitCode::from(2);
            }
            None => None,
        };
    // ⚠ The SWEEP rule, and it is this arm's as much as the CLI's: this arm is reachable directly.
    if selector.is_sweep() && produced_by.is_none() {
        eprintln!(
            "vike-datahub store rm: `{}` matches more than one series, so --produced-by is \
             REQUIRED. Deleting a whole sweep by name alone is what that assertion exists to \
             replace; name every dimension instead, or pass the commit-key prefix the rows carry.",
            selector.describe()
        );
        return ExitCode::from(2);
    }
    // ⚠ BEFORE the open, which `create_dir_all`s its root: a mistyped path used to be created, match
    // nothing and exit 0, so the operator read "nothing to delete" about a store never looked at.
    // An existing EMPTY store still answers "matched 0", a success.
    if !resolved.root.is_dir() {
        eprintln!(
            "vike-datahub store rm: no store at {}: nothing was created or deleted. Check the path, \
             or name the store with --store DIR: this verb reads no settings.",
            resolved.root.display()
        );
        return ExitCode::from(2);
    }

    let store = match HistoryStoreDataFusion::open(&resolved.root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "vike-datahub: cannot open the hist store at {}: {e}",
                resolved.root.display()
            );
            return ExitCode::from(2);
        }
    };
    let plan =
        match vike_data::store::removal::plan_removal(&store, &selector, produced_by.as_deref()) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("vike-datahub store rm: {e}");
                return ExitCode::from(2);
            }
        };

    let dry_run = flags.dry_run;
    // ⚠ Under `--json` the plan goes to STDERR, not nowhere. Stdout is the document and nothing
    // else, but a run that is about to ask a human to type `delete N series` must have SHOWN them
    // what N is made of — and without this the operator was prompted having seen no plan at all.
    // Same stream every diagnostic in this workspace uses, and the same shape
    // `crates/vike-cli/src/cmd/engine.rs`'s `run_capturing_stdout` already gives this tool's lines.
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
        eprintln!("vike-datahub store rm: provenance REFUSED — nothing was deleted");
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
        eprintln!("vike-datahub store rm: {e}");
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
            eprintln!("vike-datahub store rm: {e}");
            ExitCode::from(2)
        }
    }
}

/// The confirmation gate — PURE over its two inputs, so both branches are unit-tested rather than
/// only reachable from a terminal.
///
/// `stdin_is_terminal` is a PARAMETER for that reason; the caller reads the real one.
fn confirm_removal(
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
             `yes | vike-datahub store rm …` is exactly what this refuses.",
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

/// The `--json` document for `store rm`.
///
/// ⚠ It carries the RESOLVED store root and its RUNG, and that is the one fact that cannot survive
/// a prose round trip: `vike-cli data hist rm --json` wraps this document rather than re-deriving the
/// root, because the CLI resolves nothing (this tool runs in another process) and a path it
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

#[path = "store_cmd/rm_series_tests.rs"]
#[cfg(test)]
mod rm_series_tests;
