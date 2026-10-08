//! `backtest data <sub>`: the retired-flag refusals, the argv triage, the router and its arms.

use std::path::PathBuf;
use std::process::ExitCode;

use vike_analytics::binutil::{arg, has_flag};
use vike_data::DataFusionHist;
use vike_data::demo as demo_tape;
use vike_datahub_client::flag_vocab::store_flag_removed;

use super::DATA_SUBCOMMAND;
use super::args::flag_given;
use super::usage::DATA_USAGE;
use crate::binutil::store_root;

mod export;
#[cfg(feature = "venue-fetch")]
mod fetch_starter;
mod repair_series;

use self::export::run_export;
#[cfg(feature = "venue-fetch")]
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
/// computed as `flag.trim_start_matches("--")`, which is the [`DATA_SUBS`] name for four of the
/// five rows and `rm-series` for the fifth — a subcommand [`triage_data_argv`] refuses. So the
/// refusal for the retired DELETE flag sent an engine-only operator to `backtest data rm-series`,
/// which answers "unknown `data` subcommand". A `contains` test could not see it either, because
/// `"rm-series"` contains `"rm"`; `the_retired_sub_is_a_real_data_subcommand` compares against
/// [`DATA_SUBS`] instead, which is the table that decides.
///
/// A table rather than five hand-written `if`s so [`refuse_a_retired_data_flag`] and this file's own
/// `every_retired_data_flag_names_its_replacement` iterate the same rows.
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

/// Refuse one of the five retired spellings by name, echoing what was written.
///
/// `None` when argv carries none of them, so [`run`] falls through unchanged. Checked in
/// [`RETIRED_DATA_FLAGS`] order, so a line carrying two of them names the first — which is enough:
/// the message tells the operator the whole family moved.
///
/// ⚠ [`flag_given`], not `has_flag`: `--fetch=binance:BTCUSDT:1h` is a spelling `arg` accepts and
/// `has_flag` never sees, and a retirement that missed the inline form would let exactly the
/// scripted invocations through — the ones nobody is watching.
///
/// ⚠ **The adjacent-prefix landmine, checked**: `flag_given(args, "--fetch")` is NOT tripped by
/// `--fetch-starter` (`has_flag` is exact-token and `arg` is anchored on `--fetch=`), which is why
/// the two can be separate rows answering with separate replacements rather than one row that
/// misnames half the traffic. [`flag_given`]'s own doc carries the same check for `--seed`.
pub(super) fn refuse_a_retired_data_flag(args: &[String]) -> Option<ExitCode> {
    let (flag, sub, replacement, tail) =
        RETIRED_DATA_FLAGS.iter().find(|(f, _, _, _)| flag_given(args, f))?;
    let inline = format!("{flag}=");
    let written = args
        .iter()
        .find(|a| a.as_str() == *flag || a.starts_with(&inline))
        .cloned()
        .unwrap_or_else(|| (*flag).to_string());
    eprintln!(
        "backtest: {flag} is retired — data management moved to `vike-cli data` (ruling 12 of \
         docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md). You wrote \
         {written:?}; write `{replacement}` instead, or `backtest {DATA_SUBCOMMAND} {sub}` on a \
         box with only this engine.{tail}"
    );
    Some(ExitCode::from(2))
}

/// Every flag `backtest data` accepts, and whether it takes a VALUE.
///
/// ⚠ It exists so [`triage_data_argv`] can refuse an unknown option and an inline value on a
/// BOOLEAN — and the second half is a live defect being closed, not tidiness.
/// `vike_analytics::binutil::has_flag` is exact-token by design and its own doc declares the
/// residual: "`--json=1`, `--yes=true` and `--dry-run=yes` are still silently ignored on every bin
/// in this family". On THIS verb that residual DELETES: `--rm-series --yes --dry-run=1` ran the
/// removal, because `--dry-run=1` is not the token `has_flag` looks for while `--yes` is. Ruling 12
/// moves the operator-facing surface onto the parser that already refuses both spellings
/// (`crates/vike-cli/src/cmd/args.rs`'s `no_value`), and this table is what gives the engine's own
/// door the same strength rather than leaving it open behind the CLI's.
const DATA_FLAGS: &[(&str, bool)] = &[
    ("--store", true),
    ("--json", false),
    ("--days", true),
    ("--from", true),
    ("--to", true),
    ("--out", true),
    ("--kind", true),
    ("--venue", true),
    ("--symbol", true),
    ("--group", true),
    ("--interval", true),
    ("--produced-by", true),
    ("--dry-run", false),
    ("--yes", false),
];

/// The `data` subcommands, and whether each takes a `VENUE:SYMBOL:INTERVAL` positional.
///
/// ⚠ **`fetch` is RETIRED and is still a row here, deliberately.** Its dispatcher arm now only
/// refuses, naming `vike-cli data hist fetch` — and for that refusal to be REACHED, the verb has to
/// parse like any other: [`triage_data_argv`] validates argv against this table before `run_data`
/// dispatches, so deleting the row would make `backtest data fetch binance:BTCUSDT:1h --days 180`
/// fail on its positional instead of on its retirement, which teaches the operator nothing.
/// [`RETIRED_DATA_FLAGS`]'s `--fetch` row also names `"fetch"` as its sub, and
/// `the_retired_sub_is_a_real_data_subcommand` compares the two — so both rows stay or both go.
pub(super) const DATA_SUBS: &[(&str, bool)] = &[
    ("fetch", true),
    ("fetch-starter", false),
    ("seed-demo", false),
    ("export", true),
    ("rm", false),
    // ⚠ `false` for the same reason `rm` is: a series is (kind, venue, symbol-or-group, interval?)
    // and a colon-string can express neither a GROUPED series (whose symbol is empty) nor a kind.
    // `repair` needs to name one exactly — including one the store cannot ENUMERATE, which is the
    // whole point of it — so it selects with the same named flags `rm` does.
    ("repair", false),
];

/// The [`DATA_SUBS`] that READ history rather than write a store — and therefore refuse `--store`.
///
/// ⚠ **One row, and it is the verb decision 0084's 2026-09-25 amendment named as NOT closed.** The
/// ruling closed the local READ door on every history reader; `data export` was left open because
/// it lives among the WRITERS while what it does is read a series out to a file. It reads over the
/// wire now ([`run_export`]) and refuses `--store` by name with
/// `vike_datahub_client::flag_vocab::store_flag_removed`, the one sentence every reader prints.
///
/// ⚠ **`rm --dry-run` and `repair`'s default rehearsal are NOT rows here**, although each run
/// only reads. They are rehearsals OF a write — the plan a deletion or a rebuild would execute
/// against the store the operator named — and a rehearsal that read a different store than the
/// write it rehearses would be a plan for somebody else's data. The writer half of the verdict is
/// unchanged, so they keep `--store` with the verb they rehearse.
///
/// A table rather than a `sub == "export"` so the next reader-shaped verb joins by adding a row, and
/// so `every_data_read_is_a_real_data_subcommand` can hold it against [`DATA_SUBS`].
pub(super) const DATA_READS: &[&str] = &["export"];

/// Refuse `--store` on a [`DATA_READS`] verb, BY NAME and with its replacement named — `None` when
/// the verb writes or the flag is absent.
///
/// PURE and checked BEFORE [`triage_data_argv`]: a trailing value-less `--store` would otherwise be
/// answered "requires a value", which tells somebody to supply a directory that would then be
/// refused anyway. [`flag_given`] rather than `has_flag`, so `--store=DIR` is caught too — the
/// spelling a script uses, and the one nobody watches.
pub(super) fn refuse_a_store_on_a_data_read(sub: &str, args: &[String]) -> Option<String> {
    (DATA_READS.contains(&sub) && flag_given(args, "--store"))
        .then(|| store_flag_removed(&format!("backtest {DATA_SUBCOMMAND} {sub}")))
}

/// ARGV TRIAGE for `backtest data <sub>`, returning the one positional the subcommand takes.
///
/// PURE — no store, no profile, no environment, no clock — and it runs BEFORE any arm, so a refused
/// command line opens (which means CREATES — every `DataFusionHist::open` `create_dir_all`s its
/// root) nothing. That is the rule `crates/vike-backtest/tests/optimizer_cli/flags.rs`'s
/// `a_refused_flag_never_opens_the_store` pins, extended to this verb.
///
/// What it judges and what it deliberately does not: it judges the SHAPE of the command line — an
/// unknown option, a boolean given a value, a valued flag given none, a positional where the
/// subcommand takes none (or missing where it does). It judges no VALUE: which venues exist, which
/// kinds the store partitions by and what a window means are the arms' own, and a second roster
/// here would be a second list to keep in step. That is the split
/// `crates/vike-cli/src/cmd/data.rs`'s module doc already draws between shape and roster.
pub(super) fn triage_data_argv(sub: &str, rest: &[String]) -> Result<Option<String>, String> {
    let (_, takes_spec) = DATA_SUBS.iter().find(|(name, _)| *name == sub).ok_or_else(|| {
        format!(
            "unknown `data` subcommand '{sub}' (expected {})",
            DATA_SUBS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" | ")
        )
    })?;

    let mut spec: Option<String> = None;
    let mut it = rest.iter();
    while let Some(token) = it.next() {
        if token.starts_with("--") {
            let (name, inline) = match token.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v)),
                None => (token.clone(), None),
            };
            let Some((_, valued)) = DATA_FLAGS.iter().find(|(f, _)| *f == name) else {
                return Err(format!("unknown option '{token}' on `data {sub}`"));
            };
            if !*valued {
                // ⚠ The `--dry-run=1` hole, closed. See [`DATA_FLAGS`].
                if inline.is_some() {
                    return Err(format!(
                        "{name} takes no value, and {token:?} was SILENTLY IGNORED before — which \
                         on `data rm` meant a run written as a rehearsal performed the deletion"
                    ));
                }
                continue;
            }
            if inline.is_some() {
                continue;
            }
            match it.next() {
                Some(v) if v.starts_with("--") => {
                    return Err(format!(
                        "{name} requires a value, but the next argument is another flag ({v})"
                    ));
                }
                Some(_) => continue,
                None => return Err(format!("{name} requires a value")),
            }
        }
        match &spec {
            None => spec = Some(token.clone()),
            Some(already) => {
                return Err(format!(
                    "unexpected extra argument '{token}' (the spec is already '{already}')"
                ));
            }
        }
    }

    match (*takes_spec, &spec) {
        (true, None) => Err(format!(
            "`data {sub}` needs a VENUE:SYMBOL:INTERVAL spec, e.g. `backtest data {sub} \
             binance:BTCUSDT:1h …`"
        )),
        (false, Some(extra)) => {
            Err(format!("'{extra}': `data {sub}` takes no VENUE:SYMBOL:INTERVAL spec"))
        }
        _ => Ok(spec),
    }
}

/// Route `backtest data <sub>`. `rest` is everything after the `data` word.
///
/// Each arm is the SAME function the retired flag called, handed the sub-slice — so this is a
/// rename of the door, never a second implementation. The `venue-fetch` refusals move with their
/// arms and name the new spelling.
pub(super) fn run_data(
    vars: &std::collections::HashMap<String, String>,
    rest: &[String],
    now_unix_secs: &dyn Fn() -> i64,
) -> ExitCode {
    if rest.first().is_none_or(|a| a == "-h" || a == "--help" || a == "help") {
        // stdout + exit 0, the rule `--help` already obeys above: help is output a user pipes into
        // a pager, not a diagnostic. A bare `backtest data` is the same question with no verb.
        println!("{DATA_USAGE}");
        return ExitCode::SUCCESS;
    }
    let sub = rest[0].clone();
    let args = &rest[1..];
    // ⚠ BEFORE the triage and before any I/O — see [`refuse_a_store_on_a_data_read`]. Exit 2, the
    // usage rung every other argv refusal on this verb takes: nothing was tried.
    if let Some(why) = refuse_a_store_on_a_data_read(&sub, args) {
        eprintln!("{why}");
        return ExitCode::from(2);
    }
    let spec = match triage_data_argv(&sub, args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest data: {e}\n\n{DATA_USAGE}");
            return ExitCode::from(2);
        }
    };
    let _ = (&spec, now_unix_secs);

    match sub.as_str() {
        "seed-demo" => run_seed_demo(vars, args),
        "rm" => run_rm_series(vars, args),
        "repair" => run_repair_series(vars, args),
        // ⚠ **RETIRED, and answered BY NAME rather than falling through to the catch-all.** This
        // arm used to call `run_fetch`, which called `crate::fetch::fetch_into`, which called
        // `vike_binance::data::fetch_klines_range` — the one venue call in the compute plane. It is
        // deleted: history is fetched by the DATA plane, once, into the store.
        //
        // The refusal is spelled out rather than left to `other =>`'s "unknown `data` subcommand"
        // for the same reason [`RETIRED_DATA_FLAGS`] exists: this is a verb operators have typed,
        // it is installed on a box where `vike-backtest.service` runs, and a bare "unknown"
        // teaches nothing. ⚠ The replacement is NOT a drop-in — it needs a reachable datahub,
        // which the direct call did not — so the message says so rather than implying a rename.
        "fetch" => {
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
        "fetch-starter" => {
            #[cfg(feature = "venue-fetch")]
            {
                run_fetch_starter(vars, args)
            }
            #[cfg(not(feature = "venue-fetch"))]
            {
                eprintln!(
                    "backtest data fetch-starter: this build has no network fetch — it was \
                     compiled without the `venue-fetch` feature. The shipped release binary and \
                     the container image both have it. `backtest data seed-demo` needs no network \
                     and works in every build."
                );
                ExitCode::from(2)
            }
        }
        // ⚠ **UNGATED now, and it should never have been gated on `venue-fetch`.** This arm carried
        // a `#[cfg(feature = "venue-fetch")]` and a refusal blaming that feature for "carrying the
        // Parquet writer's caller". Measured, that was simply false: `export` reaches no VENUE at
        // all, and `vike_data::write_bars_parquet` has no `#[cfg]` of its own beyond
        // `hist-datafusion`, which `datafusion-store` already forwards. (It read the local store
        // until 2026-09-26; it asks a DATAHUB for the bars now — [`run_export`] — which is a
        // socket, not a venue, and needs no feature either.)
        //
        // There is no `#[cfg(feature = "datafusion-store")]` in its place either, because that
        // would be a tautology: `crates/vike-backtest/src/lib.rs` compiles this whole module only
        // under that feature. A build that can reach this line can already export.
        "export" => run_export(vars, args, spec.as_deref().unwrap_or_default()),
        // Unreachable: [`triage_data_argv`] refused every other spelling above. Spelled as a
        // refusal rather than `unreachable!()` so a new row in [`DATA_SUBS`] with no arm here is a
        // message instead of a panic in a binary an operator is running against their own store.
        other => {
            eprintln!("backtest data: '{other}' has no arm in this build\n\n{DATA_USAGE}");
            ExitCode::from(2)
        }
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
fn run_seed_demo(vars: &std::collections::HashMap<String, String>, args: &[String]) -> ExitCode {
    let root = store_root(arg(args, "--store").map(PathBuf::from), vars);
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
fn run_rm_series(vars: &std::collections::HashMap<String, String>, args: &[String]) -> ExitCode {
    use std::io::IsTerminal;
    use vike_data::store::removal::SeriesSelector;

    // ⚠ FIRST, before the store is opened: the root AND the rung that chose it, on stdout.
    let resolved =
        crate::binutil::store_root_resolved(arg(args, "--store").map(PathBuf::from), vars);
    let json = has_flag(args, "--json");
    // The store LEADS, on stdout for a human and on stderr under `--json` (where stdout is the
    // document and the same fact rides `store_root`/`store_rung` inside it).
    if json {
        eprintln!("store: {resolved}");
    } else {
        println!("store: {resolved}");
    }

    let selector = SeriesSelector {
        kind: arg(args, "--kind").unwrap_or_default(),
        venue: arg(args, "--venue").unwrap_or_default(),
        symbol: arg(args, "--symbol"),
        group: arg(args, "--group"),
        interval: arg(args, "--interval"),
    };
    if let Err(e) = selector.validate_shape() {
        eprintln!("backtest data rm: {e}\n\n{DATA_USAGE}");
        return ExitCode::from(2);
    }
    // Resolved BEFORE the store is opened: a `--produced-by` spelling is a fact about the argument,
    // and refusing a typo without touching a store is the cheaper failure.
    let produced_by = match arg(args, "--produced-by")
        .map(|s| vike_data::store::store_kind::resolve_produced_by(&s))
    {
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

    let dry_run = has_flag(args, "--dry-run");
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
    if let Err(e) = confirm_removal(&plan, has_flag(args, "--yes"), std::io::stdin().is_terminal())
    {
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
    resolved: &vike_model::paths::store_path::StoreRoot,
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
