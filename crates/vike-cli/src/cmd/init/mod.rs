//! `vike-cli init` — scaffold `<project>/user_data`, the user-content directory.
//!
//! ```text
//! vike-cli init [--dir PATH] [--reset] [--dry-run]
//! ```
//!
//! Creates the strategy / profile / result / notebook / log tree described by
//! [`content::MAP`], fills every folder with runnable examples, and PRINTS the map — so the
//! answer to "what is all this?" arrives before the user opens a file manager, not after they go
//! looking for a README.
//!
//! # Safe by default; `--reset` is the loaded gun, and it is still narrow
//!
//! Re-running is the ordinary case (a new version ships new examples), so the DEFAULT writes only
//! what is ABSENT. An edited file is never touched, never diffed and never backed up, because the
//! command has no business having an opinion about it.
//!
//! `--reset` restores the SHIPPED samples — the Freqtrade `create-userdir --reset` behaviour — and
//! is deliberately not a wipe: it overwrites the files in [`files`] and nothing else. A strategy the
//! user WROTE is not in that table, so no combination of flags here can delete it. What `--reset`
//! can destroy is edits to a shipped example, which is exactly what it is for.
//!
//! `--dry-run` prints the same report with nothing written, because a command that can overwrite
//! should be able to answer "what would you do?" first.
//!
//! # Why the examples ship instead of being created on demand
//!
//! A feature nobody can see is a feature nobody uses: an empty `strategies/` reads as setup
//! somebody abandoned, and there is no way to discover that a folder would accept a script by
//! looking at it. So every folder arrives populated — see [`content`]'s module doc for the three
//! deliberate exceptions (`logs/`, `profiles/`, `backtest_results/`: README only, because a sample
//! log, run profile or result would be fiction). The three starter strategy
//! files go to `<project>/strategies/` ([`strategy_files`]), CREATE-ONLY like the shipped ones:
//! `--reset` restores user_data's samples and never a strategy file.
//!
//! # The release's shipped strategy files
//!
//! Every run also copies the release's shipped strategy files from
//! `<project>/vendor/vike-strategies` into `<project>/strategies/` through
//! `vike_strategy::seed_shipped_strategies`, the ONE seed every installer calls: a file already
//! there, edited or not, is never touched, and `--reset` does not reach them (they are the user's
//! once copied). `--strategies-only` does that and nothing else; it is what `deploy/docker/vikectl`,
//! `deploy/sbin/vike-trader-ci-deploy` and `deploy/docker/entrypoint.sh` run, with
//! `VIKE_SETTINGS_DIR` naming the project. A box with no shipped tree seeds nothing and says so.
//!
//! # This command writes ONLY under `user_data/` and `strategies/`
//!
//! It creates no file in `settings/`, and it reads no credential. The directories are siblings
//! precisely so that a tool inviting people to drop files somewhere never operates in the directory
//! holding live venue keys — `crates/vike-paths/src/state_path.rs`'s `PROJECT_USER_DATA_DIR` argues
//! the split.

pub mod content;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cmd::args::{self, Stop};

/// The program name this verb is read under.
const BIN: &str = "vike-cli init";

/// The parsed `init` command line.
#[derive(clap::Parser, Debug, PartialEq, Eq)]
#[command(
    about = "Create <project>/user_data — presets, indicators, notebooks, and folders for your \
             run profiles and results — plus the starter strategy files, and print the map",
    after_help = "re-running is safe: without --reset, a file that already exists is left exactly \
                  as it is.\n$VIKE_USER_DATA_DIR names the directory outright and skips the \
                  project walk."
)]
struct Args {
    /// `--dir`: scaffold here instead of the resolved project directory.
    #[arg(
        long,
        value_name = "PATH",
        help = "scaffold here instead of the resolved <project>/user_data"
    )]
    dir: Option<PathBuf>,
    #[arg(
        long,
        help = "restore the SHIPPED examples that are missing or edited (never touches a file you \
                added — only the samples this command ships)"
    )]
    reset: bool,
    #[arg(long, help = "print what would change, write nothing")]
    dry_run: bool,
    #[arg(
        long,
        conflicts_with = "reset",
        help = "only copy the release's shipped strategy files from <project>/vendor/vike-strategies \
                into <project>/strategies, creating what is absent (a file already there is never \
                touched); scaffold nothing in user_data"
    )]
    strategies_only: bool,
    /// `--json`: the same report as ONE machine-readable object instead of the map + the change
    /// list + the next-step prose. See [`report_json`].
    #[arg(
        long,
        help = "the same report as one JSON object — the directory, every shipped file and what \
                happened (or would happen) to it — instead of the map and the next-step notes"
    )]
    json: bool,
}

/// This verb as a `clap` command, for the published reference (`crate::surface`).
pub(crate) fn command() -> clap::Command {
    args::command::<Args>(BIN)
}

/// Read `init`'s own argv tail. PURE — no I/O, so the whole grammar is unit-tested below.
fn parse(argv: impl Iterator<Item = String>) -> Result<Args, Stop> {
    args::parse(BIN, argv)
}

/// What happened to one shipped file. Reported per file so the output is a record of what the
/// command DID, never a claim about what it intended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Was absent; written.
    Created,
    /// Existed with different content and `--reset` was given; overwritten with the shipped copy.
    Restored,
    /// Left exactly as it was — either identical to the shipped copy, or edited and protected.
    Kept,
}

impl Action {
    /// The name a MACHINE reads, in [`report_json`]. Deliberately not the human column label:
    /// `print_report` prints imperatives (`create`, `restore`) beside a `would be` when nothing was
    /// written, and a caller parsing JSON reads the `dry_run` field for that instead of a tense.
    fn name(self) -> &'static str {
        match self {
            Action::Created => "created",
            Action::Restored => "restored",
            Action::Kept => "kept",
        }
    }
}

/// Every file this command ships, as `(path relative to user_data/, content)`.
///
/// The ONE table `--reset` compares against, so it is also the exhaustive list of what `--reset`
/// can possibly overwrite. Built at call time rather than as a `const` because `README.md` is
/// COMPOSED: it embeds [`content::MAP`] so the printed map and the written one cannot drift.
fn files() -> Vec<(&'static str, String)> {
    vec![
        ("README.md", content::readme()),
        // presets/ — param tables for the starter strategies, which live in ../strategies/.
        ("presets/sma_cross/fast.toml", content::PRESET_SMA_FAST.to_string()),
        ("presets/sma_cross/slow.toml", content::PRESET_SMA_SLOW.to_string()),
        ("presets/rsi_meanrev/default.toml", content::PRESET_RSI_DEFAULT.to_string()),
        ("presets/ema_trend/default.toml", content::PRESET_EMA_DEFAULT.to_string()),
        ("indicators/README.md", content::INDICATORS_README.to_string()),
        ("indicators/donchian_high.rhai", content::DONCHIAN_HIGH_RHAI.to_string()),
        ("indicators/streak.rhai", content::STREAK_RHAI.to_string()),
        // profiles/, backtest_results/ and logs/ ship a README and NOTHING else — no run profile,
        // no result, no log (content's module doc; the owner's rulings of 2026-11-13).
        ("profiles/README.md", content::PROFILES_README.to_string()),
        ("backtest_results/README.md", content::RESULTS_README.to_string()),
        ("notebooks/README.md", content::NOTEBOOKS_README.to_string()),
        ("notebooks/backtest_report.ipynb", content::NOTEBOOK_IPYNB.to_string()),
        ("logs/README.md", content::LOGS_README.to_string()),
    ]
}

/// The starter strategy FILES, as `(path relative to <project>/strategies/, content)`: three Rhai
/// files, one per SHAPE (reversal / mean-reversion / long-only filter), never one per indicator: the
/// host binds the whole catalog, so three files cannot sample it (see `content`'s doc). Written
/// CREATE-ONLY, like the release's shipped files: once there a file is the user's, and `--reset`
/// does not reach it. The Rust starter is the release's shipped `other/example.rs`.
fn strategy_files() -> Vec<(&'static str, String)> {
    vec![
        ("README.md", content::RHAI_README.to_string()),
        ("trend/sma_cross.rhai", content::SMA_CROSS_RHAI.to_string()),
        ("mean_reversion/rsi_meanrev.rhai", content::RSI_MEANREV_RHAI.to_string()),
        ("trend/ema_trend.rhai", content::EMA_TREND_RHAI.to_string()),
    ]
}

/// The directories created even when they hold no file of their own.
///
/// Every path in [`files`] gets its parents created on write, so this list exists for the folders
/// whose EMPTINESS is the normal state. It is spelled out rather than derived because a folder that
/// silently stops being created is exactly the kind of regression a derived list hides.
const DIRS: &[&str] =
    &["presets", "indicators", "profiles", "backtest_results", "notebooks", "logs"];

/// Join a `/`-separated table path onto `root`, one component at a time.
///
/// The table is written with `/` because it is also documentation; this is what keeps it a correct
/// path on Windows, where a literal `"a/b"` component would become a file named `a/b`.
fn resolve(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Create the tree under `root` and report what each shipped file did.
///
/// `reset` overwrites a shipped file whose content DIFFERS from the shipped copy; without it an
/// existing file is left alone whatever it contains. `dry_run` performs every comparison and writes
/// nothing, so its report is the report the real run would produce.
///
/// An unreadable existing file is treated as DIFFERENT rather than as a failure: the question being
/// asked is "does this match the sample?", and a file that cannot be read is not the sample. Under
/// `--reset` that restores it; without `--reset` it is kept, which is what happens to every other
/// existing file.
///
/// `table` is [`files`] (under user_data, with [`DIRS`]) or [`strategy_files`] (under the
/// strategies folder, no `dirs`, never reset).
fn scaffold(
    root: &Path,
    table: Vec<(&'static str, String)>,
    dirs: &[&str],
    reset: bool,
    dry_run: bool,
) -> Result<Vec<(&'static str, Action)>, String> {
    if !dry_run {
        for dir in dirs {
            let path = resolve(root, dir);
            std::fs::create_dir_all(&path)
                .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        }
    }
    let mut report = Vec::new();
    for (rel, body) in table {
        let path = resolve(root, rel);
        let action = if !path.exists() {
            Action::Created
        } else if reset && std::fs::read_to_string(&path).map(|s| s != body).unwrap_or(true) {
            Action::Restored
        } else {
            Action::Kept
        };
        if !dry_run && action != Action::Kept {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            std::fs::write(&path, &body)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }
        report.push((rel, action));
    }
    Ok(report)
}

/// The directory this invocation scaffolds: `--dir` if given, else the one the DISPATCHER resolved
/// (`$VIKE_USER_DATA_DIR`, else the project walk).
///
/// PURE — no I/O, so the whole override grammar is unit-tested. `None` from both is an ERROR rather
/// than a fallback to the working directory: scattering a `user_data/` tree into whatever directory
/// somebody happened to be standing in is not a recoverable mistake, and `--dir` says what they
/// meant in one word.
fn target(args: &Args, user_data_dir: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(dir) = &args.dir {
        return Ok(dir.clone());
    }
    user_data_dir.map(Path::to_path_buf).ok_or_else(|| {
        "no project found above the working directory, so there is nowhere to put user_data.\n\
         Run this inside your project, or name the directory: `vike-cli init --dir PATH` \
         (or set $VIKE_USER_DATA_DIR)."
            .to_string()
    })
}

/// The shipped-file seed of `<project>`, or `None` when this box has no shipped tree. Never fails
/// the scaffold silently: an I/O error is returned for the caller to print and exit on.
fn seed(project: &Path, dry_run: bool) -> Result<Option<vike_strategy::SeedReport>, String> {
    let shipped = resolve(project, vike_paths::state_path::SHIPPED_STRATEGIES_DIR);
    let strategies = project.join(vike_paths::state_path::PROJECT_STRATEGIES_DIR);
    vike_strategy::seed_shipped_strategies(&shipped, &strategies, dry_run).map_err(|e| {
        format!("cannot copy the shipped strategy files into {}: {e}", strategies.display())
    })
}

/// Print one seed's report: every copy, the kept files counted.
fn print_seed(project: &Path, report: Option<&vike_strategy::SeedReport>, dry_run: bool) {
    let strategies = project.join(vike_paths::state_path::PROJECT_STRATEGIES_DIR);
    let Some(report) = report else {
        println!(
            "shipped strategies: none on this box ({} is absent), so nothing was copied",
            resolve(project, vike_paths::state_path::SHIPPED_STRATEGIES_DIR).display()
        );
        return;
    };
    let verb = if dry_run { "would copy" } else { "copied" };
    println!("shipped strategies into {}:", strategies.display());
    for relative in &report.copied {
        println!("  {verb} {}", relative.display());
    }
    println!(
        "  ({} copied, {} already there and left untouched)",
        report.copied.len(),
        report.kept.len()
    );
}

/// Run the subcommand. `user_data_dir` is `<project>/user_data` and `project_root` is `<project>`,
/// both resolved once by the dispatcher — see [`target`].
pub fn run(
    argv: impl Iterator<Item = String>,
    user_data_dir: Option<&Path>,
    project_root: Option<&Path>,
) -> ExitCode {
    let args = match parse(argv) {
        Ok(a) => a,
        Err(stop) => return args::finish(&stop),
    };
    if args.strategies_only {
        let Some(project) = project_root else {
            eprintln!(
                "vike-cli init: no project found, so there is no strategies folder to fill. Run this \
                 inside your project, or name its settings: VIKE_SETTINGS_DIR=<project>/settings."
            );
            return ExitCode::FAILURE;
        };
        return match seed(project, args.dry_run) {
            Ok(report) => {
                print_seed(project, report.as_ref(), args.dry_run);
                ExitCode::SUCCESS
            }
            Err(msg) => {
                eprintln!("vike-cli init: {msg}");
                ExitCode::FAILURE
            }
        };
    }
    let root = match target(&args, user_data_dir) {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("vike-cli init: {msg}");
            return ExitCode::FAILURE;
        }
    };
    // The user_data scaffold, the starter strategy files, then the seed. With no project (a bare
    // `--dir` outside one) there is no strategies folder: the last two are skipped, and said.
    let strategies_dir =
        project_root.map(|p| p.join(vike_paths::state_path::PROJECT_STRATEGIES_DIR));
    let outcome = scaffold(&root, files(), DIRS, args.reset, args.dry_run).and_then(|report| {
        let starters = strategies_dir
            .as_deref()
            .map(|dir| scaffold(dir, strategy_files(), &[], false, args.dry_run))
            .transpose()?;
        let seeded = project_root.map(|p| seed(p, args.dry_run)).transpose()?;
        Ok((report, starters, seeded))
    });
    let (report, starters, seeded) = match outcome {
        Ok(done) => done,
        Err(msg) => {
            eprintln!("vike-cli init: {msg}");
            return ExitCode::FAILURE;
        }
    };
    let starters = strategies_dir.as_deref().zip(starters.as_deref());
    // Same report, two renderings — and under `--json` the JSON is the WHOLE of stdout. The map,
    // the change list and the next-step lines are prose for a person; emitting them alongside the
    // document would leave a caller parsing a stream that is not JSON.
    if args.json {
        println!("{}", report_json(&root, &report, starters, &args, seeded.as_ref()));
        return ExitCode::SUCCESS;
    }
    print_report(&root, &report, starters, &args);
    println!();
    match (project_root, &seeded) {
        (Some(project), Some(report)) => print_seed(project, report.as_ref(), args.dry_run),
        _ => println!("no project found, so no strategy files were written or copied"),
    }
    ExitCode::SUCCESS
}

/// The `--json` report: the directory, whether anything was actually written, and one row per
/// shipped file with what happened to it.
///
/// ⚠ **Every file is a row, `kept` included** — which is exactly where this differs from the human
/// rendering, and deliberately. [`print_report`] COUNTS the unchanged ones because a wall of `kept`
/// lines buries the one line a person needs; a machine has the opposite problem, because "the file
/// is not in the list" and "the file is not shipped at all" are indistinguishable from a filtered
/// array. The `dry_run` field is what a caller reads to know whether the actions describe what
/// happened or what would have.
///
/// `strategies` is the starter strategy files (`null` with no project): the folder and one row per
/// file, RELATIVE to it. `shipped_strategies` is the seed: `null` with no project,
/// `{"shipped": false}` on a box with no shipped tree, else the copied and kept paths, relative too.
fn report_json(
    root: &Path,
    report: &[(&str, Action)],
    starters: Option<(&Path, &[(&str, Action)])>,
    args: &Args,
    seeded: Option<&Option<vike_strategy::SeedReport>>,
) -> String {
    let relative = |list: &[PathBuf]| -> Vec<String> {
        list.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect()
    };
    let rows = |report: &[(&str, Action)]| -> Vec<serde_json::Value> {
        report
            .iter()
            .map(|(rel, action)| serde_json::json!({ "path": rel, "action": action.name() }))
            .collect()
    };
    let strategies = match starters {
        None => serde_json::Value::Null,
        Some((dir, report)) => {
            serde_json::json!({ "dir": dir.display().to_string(), "files": rows(report) })
        }
    };
    let shipped = match seeded {
        None => serde_json::Value::Null,
        Some(None) => serde_json::json!({ "shipped": false }),
        Some(Some(s)) => serde_json::json!({
            "shipped": true,
            "copied": relative(&s.copied),
            "kept": relative(&s.kept),
        }),
    };
    let doc = serde_json::json!({
        "shipped_strategies": shipped,
        "strategies": strategies,
        "user_data": root.display().to_string(),
        // The two flags are echoed because they change what every `action` below MEANS: under
        // `--dry-run` nothing was written, and without `--reset` an edited sample reads `kept`
        // because it was protected rather than because it matched.
        "dry_run": args.dry_run,
        "reset": args.reset,
        // RELATIVE to `user_data`, the same string the shipped-file table is keyed on, so a caller
        // can join it onto `user_data` itself rather than parsing a rendered path.
        "files": rows(report),
    });
    serde_json::to_string_pretty(&doc).expect("a tree of strings and bools; serialization is total")
}

/// Print the location, the MAP, and what changed.
///
/// The map is printed on EVERY run, including one that changed nothing: this command is also how a
/// user asks "what is in here again?", and an answer that only appears the first time is an answer
/// nobody sees twice. Unchanged files are counted rather than listed — a wall of `kept` lines
/// buries the one line that matters on the run where something DID change.
fn print_report(
    root: &Path,
    report: &[(&str, Action)],
    starters: Option<(&Path, &[(&str, Action)])>,
    args: &Args,
) {
    let verb = if args.dry_run { "would be" } else { "" };
    println!("user_data: {}", root.display());
    println!();
    print!("{}", content::MAP);
    println!();
    print_changes(report, verb);
    if let Some((dir, starters)) = starters {
        println!();
        println!("strategies: {}", dir.display());
        print_changes(starters, verb);
    }
    println!();
    if args.dry_run {
        println!("--dry-run: nothing was written");
    } else if !args.reset {
        println!(
            "re-run any time; your edits are never overwritten (--reset restores the samples)"
        );
    }
    let first =
        starters.map_or_else(|| resolve(root, "README.md"), |(dir, _)| dir.join("README.md"));
    println!("next: read {}", first.display());
    for step in next_steps() {
        println!();
        println!("{}", step.say);
        for hint in &step.run {
            println!("{}", hint.render());
        }
    }
}

/// One table's change list: every file created or restored, the unchanged ones counted.
fn print_changes(report: &[(&str, Action)], verb: &str) {
    let changed: Vec<_> = report.iter().filter(|(_, a)| *a != Action::Kept).collect();
    let kept = report.len() - changed.len();
    for (rel, action) in &changed {
        let label = match action {
            Action::Created => "create",
            Action::Restored => "restore",
            Action::Kept => unreachable!("filtered out above"),
        };
        println!("  {label:<8}{verb:<7} {rel}");
    }
    if changed.is_empty() {
        println!("  nothing to do — all {kept} example files are already in place");
    } else if kept > 0 {
        println!("  ({kept} already in place, left untouched)");
    }
}

/// One block of what `init` prints after the map: a sentence, then the command lines it introduces.
struct Step {
    say: &'static str,
    run: Vec<Hint>,
}

/// One command line `init` prints — held as the TOKENS a user types, so the line that is printed
/// and the line the tests drive through the real parser are one value.
///
/// ⚠ **Why this is data and not a run of `println!`s.** These lines were hand-typed strings that no
/// parser ever saw, and their spelling moved three times: `backtest --seed-demo` (ruling 12 retired
/// the engine's data flags, #1754), `vike-cli backtest --profile …` (decision 11 made `backtest` a
/// plane that always takes a sub-verb, #1783), and `vike-cli data seed-demo` / `data fetch-starter`
/// / `data fetch` (the group split moved every `data` verb under `hist` and the `--source` axis
/// absorbed the two seeders, #2072/#2074). The first two were swept by the PR that moved them; the
/// third was not, and from 2026-09-21 every new user was told three commands the binary refuses.
/// `every_printed_command_is_one_the_real_parser_accepts` holds these tokens to
/// [`crate::cmd::accepts`], which is the same parser a typed line meets, so the next move reddens
/// here instead of depending on somebody remembering this file.
struct Hint {
    /// The program, then its arguments — exactly what a user types, one token each.
    argv: Vec<String>,
    /// Print a `\` continuation before this token, so a long line wraps where a shell accepts it.
    wrap_at: Option<usize>,
    /// The `# …` note printed after the command, or empty for none.
    note: &'static str,
}

impl Hint {
    fn new(argv: &[&str], note: &'static str) -> Self {
        Hint { argv: argv.iter().map(|s| s.to_string()).collect(), wrap_at: None, note }
    }

    fn render(&self) -> String {
        let (head, tail) = self.argv.split_at(self.wrap_at.unwrap_or(self.argv.len()));
        let mut line = format!("  {}", head.join(" "));
        if !tail.is_empty() {
            line = format!("{line} \\\n      {}", tail.join(" "));
        }
        if self.note.is_empty() { line } else { format!("{line:<46} # {}", self.note) }
    }
}

/// The command lines printed after the map, in the order a new user runs them. Each is an EXAMPLE:
/// `<VENUE>`, `<SYMBOL>`, `<INTERVAL>`, `<FROM>`, `<TO>`, `<CASH>`, `<FILE>`, `<FORMAT>` and `<DATASET>`
/// are placeholders the user fills, never a compiled ticker (the owner's ruling of 2026-11-13, "no
/// hardcoded symbols"). `--from`, `--to` and `--cash` are printed because a run cannot go without
/// them: `ProfileData::from`/`to` and `ProfileEngine::cash` have no default, and the engine refuses
/// a profile built without them ("missing field `from`", measured on a lane 2026-11-13).
///
/// ⚠ **No data ships, so the first thing said is that the store is EMPTY.** `init` writes strategies,
/// presets and READMEs; it writes NO market data and no run profile (the owner's rulings of
/// 2026-11-13: "no demo or fake data", and no example profiles — a profile is the user's own recipe).
/// A run over a slice the store does not hold completes with no trades and no explanation, so the
/// way to load REAL data comes before the run: the released `vike-cli data hist import` of an
/// archive the user downloaded, a venue fetch through a datahub, or the published starter dataset.
///
/// ⚠ **The DATAHUB step comes first.** Since the owner closed the local read door on 2026-09-25
/// (decision 0084's amendment) every history READ goes through a datahub — a `--local` run moves the
/// ENGINE here, never the read — and `import` and a venue fetch ask one too. `vike-backend datahub`
/// with no environment resolves this project's `<project>/market_data/hist` (both sides through
/// `vike_paths::store_path`'s `resolve_store_root_from`), binds loopback, and with no hub keys
/// authenticates nothing.
fn next_steps() -> Vec<Step> {
    let mut run = Hint::new(
        &[
            "vike-cli",
            "backtest",
            "run",
            "--local",
            "--strategy",
            "<FILE>",
            "--venue",
            "<VENUE>",
            "--symbol",
            "<SYMBOL>",
            "--interval",
            "<INTERVAL>",
            "--from",
            "<FROM>",
            "--to",
            "<TO>",
            "--cash",
            "<CASH>",
        ],
        "",
    );
    run.wrap_at = Some(6);
    vec![
        Step {
            say: "no market data ships: the store holds NOTHING until you load real bars. Serve this\n\
                  project's store first — key-less and loopback-only; leave it running in a second \
                  terminal:",
            run: vec![Hint::new(&["vike-backend", "datahub"], "")],
        },
        Step {
            say: "then load real bars: import an archive you downloaded onto this box, or fetch a \
                  venue's history:",
            run: vec![
                Hint::new(
                    &["vike-cli", "data", "hist", "import", "<FORMAT>", "<DATASET>"],
                    "a vendor archive, read on the datahub's box",
                ),
                Hint::new(
                    &[
                        "vike-cli",
                        "data",
                        "hist",
                        "fetch",
                        "<VENUE>:<SYMBOL>:<INTERVAL>",
                        "--days",
                        "180",
                    ],
                    "asks the datahub (--addr names another)",
                ),
            ],
        },
        Step {
            say: "...or the published dataset, with no venue and no server:",
            run: vec![Hint::new(
                &["vike-cli", "data", "hist", "fetch", "--source", "starter"],
                "real bars, over plain HTTPS",
            )],
        },
        Step {
            say: "then run a strategy FILE over the bars you loaded — no profile needed (a profile is \
                  your own recipe: the Studio saves one, or --write-profile keeps what flags built);\n\
                  <FROM> and <TO> are UTC hours (YYYY-MM-DDTHH) inside those bars, <CASH> the \
                  starting balance:",
            run: vec![run],
        },
    ]
}

#[cfg(test)]
mod init_tests;
