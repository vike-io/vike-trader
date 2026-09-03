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
//! A feature nobody can see is a feature nobody uses: an empty `strategies/rhai/` reads as setup
//! somebody abandoned, and there is no way to discover that a folder would accept a script by
//! looking at it. So every folder arrives populated — see [`content`]'s module doc for the one
//! deliberate exception (`logs/`, where a sample would be fiction).
//!
//! # This command writes ONLY under the resolved `user_data/`
//!
//! It creates no file in `settings/`, and it reads no credential. The two directories are siblings
//! precisely so that a tool inviting people to drop files somewhere never operates in the directory
//! holding live venue keys — `crates/vike-model/src/state_path.rs`'s `PROJECT_USER_DATA_DIR` argues
//! the split.

pub mod content;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cmd::args::{exit_for_parse_error, help_requested, no_value, Flags};

const USAGE: &str = "\
usage: vike-cli init [options]

Create <project>/user_data — strategies, run profiles, results, notebooks — with runnable
examples in every folder, and print the map.

options:
  --dir PATH      scaffold here instead of the resolved <project>/user_data
  --reset         restore the SHIPPED examples that are missing or edited (never touches
                  a file you added — only the samples this command ships)
  --dry-run       print what would change, write nothing
  -h, --help      this message

re-running is safe: without --reset, a file that already exists is left exactly as it is.
$VIKE_USER_DATA_DIR names the directory outright and skips the project walk.";

/// The parsed `init` command line.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    /// `--dir`: scaffold here instead of the resolved project directory.
    dir: Option<PathBuf>,
    reset: bool,
    dry_run: bool,
}

/// Parse `init`'s own argv tail. PURE — no I/O, so the whole grammar is unit-tested below.
fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut dir = None;
    let mut reset = false;
    let mut dry_run = false;
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--dir" => dir = Some(PathBuf::from(flags.value(&flag, inline)?)),
            "--reset" => {
                no_value(&flag, inline)?;
                reset = true;
            }
            "--dry-run" => {
                no_value(&flag, inline)?;
                dry_run = true;
            }
            "-h" | "--help" => return help_requested(),
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    Ok(Args { dir, reset, dry_run })
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

/// Every file this command ships, as `(path relative to user_data/, content)`.
///
/// The ONE table `--reset` compares against, so it is also the exhaustive list of what `--reset`
/// can possibly overwrite. Built at call time rather than as a `const` because three entries are
/// COMPOSED: `README.md` embeds [`content::MAP`] so the printed map and the written one cannot
/// drift, and the two profiles embed `content::SMA_CROSS_RHAI` so a fresh scaffold cannot ship a
/// sweep whose inlined script differs from the strategy folder's copy.
fn files() -> Vec<(&'static str, String)> {
    vec![
        ("README.md", content::readme()),
        // strategies/rhai — three example strategies, one per SHAPE (reversal / mean-reversion /
        // long-only filter), never one per indicator: the host binds the whole catalog, so three
        // files cannot sample it. See `content`'s doc.
        ("strategies/rhai/README.md", content::RHAI_README.to_string()),
        ("strategies/rhai/sma_cross/sma_cross.rhai", content::SMA_CROSS_RHAI.to_string()),
        ("strategies/rhai/sma_cross/fast.toml", content::PRESET_SMA_FAST.to_string()),
        ("strategies/rhai/sma_cross/slow.toml", content::PRESET_SMA_SLOW.to_string()),
        ("strategies/rhai/rsi_meanrev/rsi_meanrev.rhai", content::RSI_MEANREV_RHAI.to_string()),
        ("strategies/rhai/rsi_meanrev/default.toml", content::PRESET_RSI_DEFAULT.to_string()),
        ("strategies/rhai/ema_trend/ema_trend.rhai", content::EMA_TREND_RHAI.to_string()),
        ("strategies/rhai/ema_trend/default.toml", content::PRESET_EMA_DEFAULT.to_string()),
        // strategies/rust — a template to copy. The shipped reference strategies land here as
        // sibling folders later; nothing in this table assumes it is the only one.
        ("strategies/rust/README.md", content::RUST_README.to_string()),
        ("strategies/rust/my_experiment/my_experiment.rs", content::MY_EXPERIMENT_RS.to_string()),
        ("indicators/README.md", content::INDICATORS_README.to_string()),
        ("indicators/donchian_high.rhai", content::DONCHIAN_HIGH_RHAI.to_string()),
        ("indicators/streak.rhai", content::STREAK_RHAI.to_string()),
        ("profiles/README.md", content::PROFILES_README.to_string()),
        ("profiles/backtest.toml", content::PROFILE_BACKTEST.to_string()),
        ("profiles/sweep.toml", content::profile_sweep()),
        ("profiles/walkforward.toml", content::profile_walkforward()),
        ("backtest_results/README.md", content::RESULTS_README.to_string()),
        ("backtest_results/sample_sma_cross.json", content::SAMPLE_RESULT_JSON.to_string()),
        ("notebooks/README.md", content::NOTEBOOKS_README.to_string()),
        ("notebooks/backtest_report.ipynb", content::NOTEBOOK_IPYNB.to_string()),
        // logs/ ships a README and NO sample — see content's module doc.
        ("logs/README.md", content::LOGS_README.to_string()),
    ]
}

/// The directories created even when they hold no file of their own.
///
/// Every path in [`files`] gets its parents created on write, so this list exists for the folders
/// whose EMPTINESS is the normal state. It is spelled out rather than derived because a folder that
/// silently stops being created is exactly the kind of regression a derived list hides.
const DIRS: &[&str] = &[
    "strategies",
    "strategies/rhai",
    "strategies/rust",
    "indicators",
    "profiles",
    "backtest_results",
    "notebooks",
    "logs",
];

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
fn scaffold(
    root: &Path,
    reset: bool,
    dry_run: bool,
) -> Result<Vec<(&'static str, Action)>, String> {
    if !dry_run {
        for dir in DIRS {
            let path = resolve(root, dir);
            std::fs::create_dir_all(&path)
                .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        }
    }
    let mut report = Vec::new();
    for (rel, body) in files() {
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

/// Run the subcommand. `user_data_dir` is `<project>/user_data`, resolved once by the dispatcher —
/// see [`target`].
pub fn run(args: impl Iterator<Item = String>, user_data_dir: Option<&Path>) -> ExitCode {
    let args = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("init", USAGE, &msg),
    };
    let root = match target(&args, user_data_dir) {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("vike-cli init: {msg}");
            return ExitCode::FAILURE;
        }
    };
    match scaffold(&root, args.reset, args.dry_run) {
        Ok(report) => {
            print_report(&root, &report, &args);
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli init: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Print the location, the MAP, and what changed.
///
/// The map is printed on EVERY run, including one that changed nothing: this command is also how a
/// user asks "what is in here again?", and an answer that only appears the first time is an answer
/// nobody sees twice. Unchanged files are counted rather than listed — a wall of `kept` lines
/// buries the one line that matters on the run where something DID change.
fn print_report(root: &Path, report: &[(&str, Action)], args: &Args) {
    let verb = if args.dry_run { "would be" } else { "" };
    println!("user_data: {}", root.display());
    println!();
    print!("{}", content::MAP);
    println!();

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
    println!();
    if args.dry_run {
        println!("--dry-run: nothing was written");
    } else if !args.reset {
        println!(
            "re-run any time; your edits are never overwritten (--reset restores the samples)"
        );
    }
    println!("next: read {}", resolve(root, "strategies/rhai/README.md").display());
    // ⚠ The three lines below are the whole answer to "I installed it and there is nothing here".
    // `init` writes strategies and profiles; it writes NO market data, and the shipped profile
    // therefore names a slice an empty store does not hold — a run that completes with no trades
    // and no explanation. Naming the seeding command HERE, where a new user already is, is what
    // turns that into a sequence. The store is not seeded from this command on purpose: writing a
    // hist store needs DataFusion and this binary is DataFusion-free by construction, so `init`
    // points at the tool that owns the store rather than growing a dependency to reach past it.
    println!();
    println!("no market data yet? the shipped profile runs on a SYNTHETIC demo tape:");
    println!("  backtest --seed-demo                       # write it (safe to re-run)");
    // Named here too because the demo tape answers "the tools are empty" and this answers "I want
    // REAL data" — and the second question arrives about ninety seconds after the first. Three
    // routes, in the order a user should try them: synthetic (always works), published (works
    // without a venue), venue-direct (works when the venue is reachable).
    println!();
    println!("...or real market data:");
    println!("  backtest --fetch-starter                   # published dataset, no venue needed");
    println!("  backtest --fetch binance:BTCUSDT:1h --days 180   # straight from the venue");
    println!("  backtest --profile {} \\", resolve(root, "profiles/backtest.toml").display());
    println!(
        "      --script {}",
        resolve(root, "strategies/rhai/sma_cross/sma_cross.rhai").display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_of(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn the_default_is_no_flags_at_all() {
        assert_eq!(parse_of(&[]).unwrap(), Args { dir: None, reset: false, dry_run: false });
    }

    #[test]
    fn options_parse_in_both_flag_forms() {
        assert_eq!(parse_of(&["--dir", "/tmp/ud"]).unwrap().dir, Some(PathBuf::from("/tmp/ud")));
        assert_eq!(parse_of(&["--dir=/tmp/ud"]).unwrap().dir, Some(PathBuf::from("/tmp/ud")));
        assert!(parse_of(&["--reset"]).unwrap().reset);
        assert!(parse_of(&["--dry-run"]).unwrap().dry_run);
    }

    #[test]
    fn usage_errors_are_clean() {
        assert!(parse_of(&["--nope"]).unwrap_err().contains("unknown option"));
        assert!(parse_of(&["--dir"]).unwrap_err().contains("requires a value"));
        // A bare boolean must reject an inline value rather than silently reading it as true.
        assert!(parse_of(&["--reset=yes"]).unwrap_err().contains("takes no value"));
    }

    #[test]
    fn help_short_circuits() {
        assert_eq!(parse_of(&["--help"]).unwrap_err(), "help requested");
        assert_eq!(parse_of(&["-h"]).unwrap_err(), "help requested");
    }

    /// `--dir` outranks the dispatcher's answer; with neither, this is an ERROR rather than a
    /// silent scaffold into the working directory (see [`target`]).
    #[test]
    fn the_target_is_the_flag_then_the_dispatchers_answer_then_an_error() {
        let resolved = Path::new("/p/user_data");
        let no_flag = Args { dir: None, reset: false, dry_run: false };
        assert_eq!(target(&no_flag, Some(resolved)).unwrap(), resolved);

        let with_flag = Args { dir: Some(PathBuf::from("/tmp/x")), ..no_flag };
        assert_eq!(target(&with_flag, Some(resolved)).unwrap(), PathBuf::from("/tmp/x"));
        assert_eq!(target(&with_flag, None).unwrap(), PathBuf::from("/tmp/x"));

        let err = target(&no_flag, None).unwrap_err();
        assert!(err.contains("--dir"), "the error must name the way out: {err}");
    }

    /// Table paths are `/`-separated because the table is also documentation. On Windows a literal
    /// join would produce ONE component containing slashes, and the tree would be a single
    /// oddly-named file.
    #[test]
    fn table_paths_become_real_nested_paths() {
        let got = resolve(Path::new("root"), "strategies/rhai/sma_cross/sma_cross.rhai");
        assert_eq!(
            got,
            Path::new("root")
                .join("strategies")
                .join("rhai")
                .join("sma_cross")
                .join("sma_cross.rhai")
        );
    }

    /// ⚠ The map the command PRINTS and the map it WRITES are one string. A user reads the printed
    /// tree once, at scaffold time, and the README from then on; if those two could disagree the
    /// printed one would be the lie, because nothing ever re-prints it.
    #[test]
    fn map_is_embedded_verbatim_in_the_readme() {
        assert!(
            content::readme().contains(content::MAP),
            "user_data/README.md must embed the printed MAP verbatim"
        );
    }

    /// Every folder in the map is a folder the command actually creates, and every folder it
    /// creates is in the map. A map naming a folder that never appears is a worse map than none.
    #[test]
    fn the_map_and_the_created_tree_agree() {
        for dir in DIRS {
            assert!(
                content::MAP.contains(&format!("{dir}/")),
                "{dir}/ is created but absent from the printed map"
            );
        }
        // ...and the other direction, for the folders the map names with their own line.
        for line in content::MAP.lines().skip(1) {
            // `split_whitespace` skips leading whitespace itself, so trimming first is redundant
            // (clippy::trim_split_whitespace). The map indents its rows; this still reads the
            // first token of each.
            let folder = line.split_whitespace().next().unwrap_or_default();
            let folder = folder.trim_end_matches('/');
            assert!(DIRS.contains(&folder), "the map names {folder}/ but DIRS does not create it");
        }
    }

    /// Each folder ships at least one example — the decision this scaffold exists to implement.
    /// `logs/` is the ONE exception and is asserted as such, so removing its README fails here
    /// rather than quietly leaving a folder with nothing in it.
    #[test]
    fn every_folder_ships_content() {
        let shipped = files();
        for dir in DIRS {
            if *dir == "strategies" {
                continue; // a pure parent: its two children carry the examples
            }
            let n = shipped.iter().filter(|(p, _)| p.starts_with(&format!("{dir}/"))).count();
            assert!(n > 0, "{dir}/ ships no file at all");
            if *dir == "logs" {
                // ⚠ A committed sample log would be invented timestamps describing a run that
                // never happened, and compile.log is rewritten on the next start regardless.
                assert_eq!(n, 1, "logs/ must ship its README and nothing else");
                continue;
            }
            assert!(
                shipped
                    .iter()
                    .any(|(p, _)| p.starts_with(&format!("{dir}/")) && !p.ends_with("README.md")),
                "{dir}/ ships only a README — an empty folder reads as unfinished setup"
            );
        }
    }

    /// No shipped file may be empty, and none may be listed twice — a duplicate row would make
    /// `--reset`'s "restore the sample" ambiguous about which sample.
    #[test]
    fn the_table_is_well_formed() {
        let shipped = files();
        let mut paths: Vec<&str> = shipped.iter().map(|(p, _)| *p).collect();
        paths.sort_unstable();
        let before = paths.len();
        paths.dedup();
        assert_eq!(before, paths.len(), "a path is listed twice in files()");
        for (path, body) in &shipped {
            assert!(!body.trim().is_empty(), "{path} ships empty content");
        }
    }
}
