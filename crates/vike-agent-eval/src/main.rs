//! `vike-agent-eval` — drive the shipped agent surface with a real model and grade the result
//! deterministically.
//!
//! ```text
//! vike-agent-eval run [--case NAME]... [--driver api|claude-cli|scripted] [--scripted]
//!                     [--model M] [--max-steps N] [--claude PATH]
//!                     [--report PATH.json] [--work-dir DIR] [--keep-work]
//!                     [--vike-cli PATH] [--vike-tradehub PATH]
//! vike-agent-eval list
//! vike-agent-eval mcp-bridge PATH        # internal: see `claude_cli::run_bridge`
//! ```
//!
//! ⚠ THIS BINARY OWNS EVERY ENVIRONMENT READ IN THE CRATE. Everything below `main` takes what it
//! needs as a parameter — the rule `crates/vike-config/src/settings.rs` states and
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs` gates: libraries take configuration as parameters,
//! only binaries read the process environment. There are exactly two variables, one per model
//! driver — [`ANTHROPIC_API_KEY_ENV`] and [`CLAUDE_CODE_OAUTH_TOKEN_ENV`] — and each is read here,
//! handed to its driver, and never printed, logged, passed as an argument or written into the
//! report.
//!
//! ⚠ AN ABSENT CREDENTIAL IS A FAILURE, NOT A SKIP, when a real model was asked for. A run that
//! quietly measured nothing and exited 0 is the failure `.github/workflows/live-smokes.yml`'s
//! skip-honesty step exists to prevent on the venue side: an empty lane can never read green. Both
//! refusals below are deliberately the same words and the same shape.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_agent_eval::drivers::anthropic::{Anthropic, DEFAULT_MODEL};
use vike_agent_eval::drivers::claude_cli;
use vike_agent_eval::drivers::claude_cli::{ClaudeCli, DEFAULT_BINARY};
use vike_agent_eval::drivers::{ModelDriver, Scripted};
use vike_agent_eval::eval::cases::{CASES, Case, by_name};
use vike_agent_eval::eval::harness;
use vike_agent_eval::eval::harness::{Binaries, run_case};
use vike_agent_eval::eval::report::Report;
use vike_agent_eval::{locate_binary, refuse_blank_credential};

/// The Anthropic API key, for `--driver api`. Read HERE and nowhere else.
const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// The Claude Code subscription token, for `--driver claude-cli`. Read HERE and nowhere else.
///
/// It is what `claude setup-token` mints, and the CLI reads it from the environment variable of the
/// same name — so this harness's whole job with it is to move it from this process into that one
/// child, naming it on the way.
///
/// ⚠ The library's own copy of this spelling (`vike_agent_eval::drivers::claude_cli::OAUTH_TOKEN_ENV`) is a
/// DELIBERATE duplicate rather than an import: `crates/vike-model/src/scan.rs` resolves a
/// `Naming::Konst` against the constants of the FILE the read is in, so an imported constant scans
/// as a dynamic read and the registry gate could no longer see this variable at all.
const CLAUDE_CODE_OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Variables removed from the environment of every child this harness spawns THROUGH
/// `crates/vike-agent-eval/src/world/mcp.rs`'s `apply_case_env` — the `vike-cli` MCP server and the
/// `vike-tradehub` paper node.
///
/// ⚠ A spawned child inherits the whole environment, and `crates/vike-cli/src/dispatch.rs`'s `run`
/// sweeps `std::env::vars()` into a map on every invocation — so without this the model credential
/// this process holds would sit inside the two binaries the harness spawns per case, for no reason
/// at all. The API key belongs in one HTTP header and nowhere else.
///
/// ⚠ THE CLAUDE CODE CHILD IS THE ONE EXCEPTION, and it is an exception BY CONSTRUCTION rather than
/// by an entry in a list: this scrub list is threaded to `run_case`, which applies it to the two
/// binaries the HARNESS spawns, while the CLI is spawned by
/// `crates/vike-agent-eval/src/drivers/claude_cli/command.rs`'s `ClaudeCli::command`, which is handed no scrub list
/// and instead SETS `CLAUDE_CODE_OAUTH_TOKEN` on the child outright. That child is the only one
/// that must have the token — it is the one talking to Anthropic — and every other child must not.
/// `crates/vike-agent-eval/tests/claude_cli.rs` pins both halves.
const SCRUB_FROM_CHILDREN: &[&str] = &[ANTHROPIC_API_KEY_ENV, CLAUDE_CODE_OAUTH_TOKEN_ENV];

/// The default model-turn budget per case. Every case in the suite is a handful of tool calls; a
/// model still working past this has not answered the operator, and the case has no answer to grade.
const DEFAULT_MAX_STEPS: usize = 12;

/// The command line, read by `clap` directly (decision 0155; this crate names no `vike-*` crate, so not through `vike_util::cli`): `--flag
/// value` and `--flag=value`, an unknown flag refused with a suggestion, a flag given twice refused
/// (`--case` repeats), `-h`/`--help`/`help` and `-V`/`--version` on stdout with exit 0, any other
/// usage error — including no verb at all — on stderr with exit 2.
#[derive(clap::Parser, Debug)]
#[command(
    name = "vike-agent-eval",
    version,
    about = "Drive the shipped agent surface with a real model and grade the result deterministically",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    verb: Verb,
}

#[derive(clap::Subcommand, Debug)]
enum Verb {
    /// Run the evaluation suite (every case, or the named ones) and print the report
    Run(RunArgs),
    /// Print every case with the checks it declares
    List,
    /// INTERNAL, not an operator verb: the byte pump `--driver claude-cli` re-enters this binary as
    #[command(name = "mcp-bridge")]
    McpBridge {
        /// The bridge descriptor the harness wrote
        #[arg(value_name = "PATH", value_parser = non_blank_path)]
        descriptor: PathBuf,
    },
}

/// `run`'s flags.
#[derive(clap::Args, Debug)]
struct RunArgs {
    /// A case to run (repeatable; default: the whole suite)
    #[arg(long = "case", value_name = "NAME", value_parser = case_name)]
    cases: Vec<String>,
    /// Which model drives the suite (default: api)
    #[arg(long, value_enum)]
    driver: Option<DriverKind>,
    /// Same as --driver scripted
    #[arg(long)]
    scripted: bool,
    /// The model id (default: the driver's own)
    #[arg(long, value_name = "M", value_parser = non_blank)]
    model: Option<String>,
    /// The model-turn budget per case
    #[arg(long, value_name = "N", default_value_t = DEFAULT_MAX_STEPS)]
    max_steps: usize,
    /// The Claude Code binary for --driver claude-cli
    #[arg(long, value_name = "PATH", value_parser = non_blank_path, default_value = DEFAULT_BINARY)]
    claude: PathBuf,
    /// Write the JSON report here (and the transcript of every failing case beside it)
    #[arg(long, value_name = "PATH.json", value_parser = non_blank_path)]
    report: Option<PathBuf>,
    // Under the CURRENT DIRECTORY, never the operating system's temp directory: the rule
    // `crates/vike-ops/tests/hygiene/system_temp_gate.rs` states — a production path into the OS temp
    // directory resolves to something else inside a container and is emptied by something the
    // operator does not control.
    /// Where each run's scratch directory goes (relative to the current directory by default)
    #[arg(long, value_name = "DIR", value_parser = non_blank_path, default_value = "agent-eval-work")]
    work_dir: PathBuf,
    /// Keep the work directory on the success path
    #[arg(long)]
    keep_work: bool,
    /// The vike-cli binary (default: a sibling of this executable, then target/debug)
    #[arg(long, value_name = "PATH", value_parser = non_blank_path)]
    vike_cli: Option<PathBuf>,
    /// The vike-tradehub binary (default: a sibling of this executable, then target/debug)
    #[arg(long, value_name = "PATH", value_parser = non_blank_path)]
    vike_tradehub: Option<PathBuf>,
}

/// `--case`'s value parser: the name must be one of the suite's.
fn case_name(raw: &str) -> Result<String, String> {
    by_name(raw)
        .map(|case| case.name.to_string())
        .ok_or_else(|| "unknown case; `vike-agent-eval list` prints the suite".to_string())
}

/// Which model, if any, drives the suite.
///
/// ⚠ `Api` is the DEFAULT, so `--scripted` and a bare `run` behave exactly as they did before this
/// enum existed. A new default would have silently changed what an existing command measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum DriverKind {
    /// The Anthropic Messages API. Costs API credits.
    Api,
    /// The locally-installed Claude Code CLI. Costs the operator's subscription.
    ClaudeCli,
    /// No model at all.
    Scripted,
}

/// A value parser for a text flag: a blank value (empty or whitespace only) is refused, and
/// anything else is kept VERBATIM. This is the one rule of `vike_util::cli` this binary needs (the
/// blank-value row of decision 0155); the crate reads `clap` directly, not `vike_util`, to stay
/// free of every `vike-*` dependency.
fn non_blank(raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        Err("needs a non-empty value".to_string())
    } else {
        Ok(raw.to_string())
    }
}

/// [`non_blank`] for a path flag.
fn non_blank_path(raw: &str) -> Result<PathBuf, String> {
    non_blank(raw).map(PathBuf::from)
}

fn main() -> ExitCode {
    let cli: Cli = <Cli as clap::Parser>::parse();
    match run(cli.verb) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("vike-agent-eval: {e}");
            ExitCode::from(2)
        }
    }
}

struct Options {
    cases: Vec<&'static Case>,
    driver: DriverKind,
    /// ⚠ `Option`, not a defaulted `String`: for `--driver claude-cli` an ABSENT `--model` means
    /// "leave the flag off and let the subscription's own default answer", which is a different
    /// instruction from naming that default. The API driver falls back to `DEFAULT_MODEL` at the
    /// one place it needs a concrete id.
    model: Option<String>,
    max_steps: usize,
    claude: PathBuf,
    report: Option<PathBuf>,
    work_dir: PathBuf,
    keep_work: bool,
    vike_cli: Option<PathBuf>,
    vike_tradehub: Option<PathBuf>,
}

fn run(verb: Verb) -> Result<ExitCode, String> {
    match verb {
        Verb::List => {
            for case in CASES {
                println!("{:<28} skill: {:<28} {:?}", case.name, case.skill, case.node);
                for check in harness::checks_of(case) {
                    println!("      {check}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Verb::Run(args) => execute(options(args)?),
        // ⚠ NOT an operator verb. `--driver claude-cli` writes an MCP client configuration naming
        // THIS binary and this verb, and the Claude Code CLI then spawns it; a person running it by
        // hand gets a connect failure against a descriptor nothing wrote. It is a verb rather than a
        // second binary because it must be the same executable the running harness is, which
        // `std::env::current_exe` answers and a `[[bin]]` name does not.
        Verb::McpBridge { descriptor } => {
            claude_cli::run_bridge(&descriptor)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// `run`'s flags as the options the harness takes. What `clap` cannot know stays here: the two
/// spellings of the scripted driver must agree, and a zero budget evaluates nothing.
fn options(args: RunArgs) -> Result<Options, String> {
    // ⚠ `--scripted` is KEPT, and it is not an alias that could drift: it SETS `--driver scripted`,
    // so the documented spelling and the new one cannot mean different things. Naming another
    // driver beside it used to let the LAST of the two win; it is now refused.
    let driver = match (args.scripted, args.driver) {
        (true, None | Some(DriverKind::Scripted)) => DriverKind::Scripted,
        (true, Some(_)) => {
            return Err("--scripted means --driver scripted; it cannot be combined with another \
                        --driver"
                .to_string());
        }
        (false, driver) => driver.unwrap_or(DriverKind::Api),
    };
    if args.max_steps == 0 {
        return Err("--max-steps 0 would evaluate nothing".to_string());
    }
    let cases = if args.cases.is_empty() {
        CASES.iter().collect()
    } else {
        // Every name was checked by `case_name` already.
        args.cases.iter().filter_map(|name| by_name(name)).collect()
    };
    Ok(Options {
        cases,
        driver,
        model: args.model,
        max_steps: args.max_steps,
        claude: args.claude,
        report: args.report,
        work_dir: args.work_dir,
        keep_work: args.keep_work,
        vike_cli: args.vike_cli,
        vike_tradehub: args.vike_tradehub,
    })
}

fn execute(opts: Options) -> Result<ExitCode, String> {
    let bins = Binaries {
        vike_cli: locate_binary("vike-cli", opts.vike_cli.as_deref())?,
        vike_tradehub: locate_binary("vike-tradehub", opts.vike_tradehub.as_deref())?,
    };
    // `None` in scripted mode: the canned driver is per-CASE (its plan is the case's own), so it is
    // built inside the loop.
    let mut model: Option<Box<dyn ModelDriver>> = match opts.driver {
        DriverKind::Scripted => None,
        // THE ENVIRONMENT READS, both of them, and neither reachable from the library. An absent or
        // blank credential FAILS the run by name rather than skipping it: a green that measured
        // nothing is the outcome this harness exists to make impossible. The two refusals are
        // deliberately the same words and the same shape, because they are the same mistake.
        DriverKind::Api => {
            let key = std::env::var(ANTHROPIC_API_KEY_ENV).unwrap_or_default();
            if key.trim().is_empty() {
                return Err(refuse_blank_credential(ANTHROPIC_API_KEY_ENV, "Set it"));
            }
            let model = opts.model.clone().unwrap_or_else(|| DEFAULT_MODEL.to_string());
            Some(Box::new(Anthropic::new(key, model, opts.max_steps)))
        }
        DriverKind::ClaudeCli => {
            let token = std::env::var(CLAUDE_CODE_OAUTH_TOKEN_ENV).unwrap_or_default();
            if token.trim().is_empty() {
                return Err(refuse_blank_credential(
                    CLAUDE_CODE_OAUTH_TOKEN_ENV,
                    "Mint one with `claude setup-token` (it needs a Claude subscription) and set it",
                ));
            }
            Some(Box::new(ClaudeCli::new(
                opts.claude.clone(),
                token,
                opts.model.clone(),
                opts.max_steps,
            )))
        }
    };

    // ⚠ ABSOLUTE from here down. The default work directory is the RELATIVE `agent-eval-work`, and
    // every case hands its children paths derived from this one while moving their working
    // directory (`crates/vike-agent-eval/src/world/node.rs`'s `PaperNode::spawn`). `Project::create` is
    // the boundary that guarantees it; this call makes the `--keep-work` line below name a path the
    // operator can paste, and keeps the two shapes identical rather than merely equivalent.
    let run_dir = std::path::absolute(opts.work_dir.join(format!("run-{}", std::process::id())))
        .map_err(|e| format!("resolve {} to an absolute path: {e}", opts.work_dir.display()))?;
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("create {}: {e}", run_dir.display()))?;

    let driver_name = match &model {
        Some(m) => m.name(),
        None => "scripted".to_string(),
    };
    let mut report = Report { driver: driver_name, cases: Vec::new() };
    for case in opts.cases.iter().copied() {
        eprintln!("vike-agent-eval: {} ({})", case.name, case.skill);
        let verdict = match model.as_mut() {
            Some(m) => run_case(case, &bins, m.as_mut(), &run_dir, SCRUB_FROM_CHILDREN),
            None => {
                let mut scripted = Scripted::new((case.script)());
                run_case(case, &bins, &mut scripted, &run_dir, SCRUB_FROM_CHILDREN)
            }
        };
        report.cases.push(verdict);
    }
    // ⚠ Read AGAIN, after the suite. A driver's `name()` may only become concrete once a model has
    // answered — `ClaudeCli` cannot know which model the subscription handed it until the CLI's
    // first `system`/`init` line — and a report that named the REQUEST rather than the answer would
    // be a report of an experiment nobody can reproduce.
    if let Some(m) = model.as_ref() {
        report.driver = m.name();
    }

    print!("{}", report.render());
    if let Some(path) = &opts.report {
        write_report(&report, path)?;
    }
    if !opts.keep_work {
        // Best effort: a work directory left behind is untidy, and failing the run over it would
        // discard a verdict that has already been computed.
        let _ = std::fs::remove_dir_all(&run_dir);
    } else {
        eprintln!("vike-agent-eval: work kept at {}", run_dir.display());
    }
    Ok(if report.failed() == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Write the JSON report, and the full transcript of every FAILING case beside it.
///
/// Only the failures get a transcript file: a passing case's transcript is a large document nobody
/// reads, and a directory of them is how the one that matters gets lost.
fn write_report(report: &Report, path: &Path) -> Result<(), String> {
    let body = serde_json::to_string_pretty(&report.to_json())
        .map_err(|e| format!("encode the report: {e}"))?;
    std::fs::write(path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("."));
    for case in report.cases.iter().filter(|c| !c.pass) {
        let out = dir.join(format!("{stem}.{}.transcript.json", case.name));
        let text = serde_json::to_string_pretty(&case.transcript)
            .map_err(|e| format!("encode the transcript of {}: {e}", case.name))?;
        std::fs::write(&out, text).map_err(|e| format!("write {}: {e}", out.display()))?;
        eprintln!(
            "vike-agent-eval: transcript of the failing case {} -> {}",
            case.name,
            out.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `clap` would have done with a command line that did not yield a `Cli`.
    #[derive(Debug)]
    struct CliExit {
        code: i32,
        to_stdout: bool,
        text: String,
    }

    fn try_cli(line: &[&str]) -> Result<Cli, CliExit> {
        let argv = std::iter::once("bin").chain(line.iter().copied());
        <Cli as clap::Parser>::try_parse_from(argv).map_err(|e| CliExit {
            code: e.exit_code(),
            to_stdout: !e.use_stderr(),
            text: e.to_string(),
        })
    }

    /// `run <line>` as the options the harness would take.
    fn opts(line: &[&str]) -> Options {
        let argv: Vec<&str> = std::iter::once("run").chain(line.iter().copied()).collect();
        match try_cli(&argv) {
            Ok(Cli { verb: Verb::Run(args) }) => match options(args) {
                Ok(o) => o,
                Err(e) => panic!("expected {line:?} to be accepted, got: {e}"),
            },
            Ok(other) => panic!("expected the run verb from {line:?}, got {other:?}"),
            Err(e) => panic!("expected {line:?} to be accepted, got: {e:?}"),
        }
    }

    /// The usage error the test expects (stderr, exit 2), as its text.
    fn refused(line: &[&str]) -> String {
        let exit = try_cli(line).expect_err("this command line must be refused");
        assert_eq!((exit.code, exit.to_stdout), (2, false), "{line:?}: {exit:?}");
        exit.text
    }

    /// POSITIVE argv pins (decision 0155, written against the hand-rolled parser and kept through
    /// the `clap` migration): the lines `.github/workflows/agent-eval.yml` and `docs/ops/agent-eval.md`
    /// write, and every flag in a free order.
    #[test]
    fn the_documented_run_lines_are_accepted_with_their_meaning() {
        // `run --scripted --report scripted-report.json` (the CI scripted lane).
        let o = opts(&["--scripted", "--report", "scripted-report.json"]);
        assert_eq!(o.driver, DriverKind::Scripted);
        assert_eq!(o.report, Some(PathBuf::from("scripted-report.json")));
        assert_eq!(o.cases.len(), CASES.len(), "no --case means the whole suite");
        assert_eq!(o.max_steps, DEFAULT_MAX_STEPS);
        assert_eq!(o.work_dir, PathBuf::from("agent-eval-work"));
        assert!(!o.keep_work);
        // The CI model lane: `run --driver D --report R --max-steps N [--model M] [--case C] --work-dir W`.
        let o = opts(&[
            "--driver",
            "claude-cli",
            "--report",
            "model-report.json",
            "--max-steps",
            "8",
            "--model",
            "m1",
            "--case",
            "halt-trading",
            "--work-dir",
            "/home/ci/agent-eval-work",
        ]);
        assert_eq!(o.driver, DriverKind::ClaudeCli);
        assert_eq!(o.max_steps, 8);
        assert_eq!(o.model.as_deref(), Some("m1"));
        assert_eq!(o.cases.len(), 1);
        assert_eq!(o.cases[0].name, "halt-trading");
        assert_eq!(o.work_dir, PathBuf::from("/home/ci/agent-eval-work"));
        let o = opts(&["--driver", "api", "--case", "halt-trading", "--report", "r.json"]);
        assert_eq!(o.driver, DriverKind::Api);
    }

    #[test]
    fn a_bare_run_and_every_remaining_flag_are_accepted() {
        let o = opts(&[]);
        assert_eq!(o.driver, DriverKind::Api, "the default driver is the API one");
        assert_eq!(o.claude, PathBuf::from(DEFAULT_BINARY));
        assert!(o.model.is_none() && o.report.is_none());
        assert!(o.vike_cli.is_none() && o.vike_tradehub.is_none());
        let o = opts(&[
            "--keep-work",
            "--claude",
            "/usr/bin/claude",
            "--vike-cli",
            "/b/vike-cli",
            "--vike-tradehub",
            "/b/vike-tradehub",
            "--driver",
            "scripted",
        ]);
        assert!(o.keep_work);
        assert_eq!(o.claude, PathBuf::from("/usr/bin/claude"));
        assert_eq!(o.vike_cli, Some(PathBuf::from("/b/vike-cli")));
        assert_eq!(o.vike_tradehub, Some(PathBuf::from("/b/vike-tradehub")));
        assert_eq!(o.driver, DriverKind::Scripted);
    }

    /// `--case` repeats and keeps its order.
    #[test]
    fn case_is_repeatable() {
        let o = opts(&["--case", "halt-trading", "--case", "node-is-down"]);
        let names: Vec<&str> = o.cases.iter().map(|c| c.name).collect();
        assert_eq!(names, ["halt-trading", "node-is-down"]);
    }

    /// `--scripted` alone and `--driver scripted` mean the same thing.
    #[test]
    fn scripted_and_driver_scripted_agree() {
        assert_eq!(opts(&["--scripted"]).driver, opts(&["--driver", "scripted"]).driver);
    }

    /// `--flag=value` is read like `--flag value`.
    #[test]
    fn the_equals_spelling_is_read() {
        let o = opts(&["--driver=scripted", "--max-steps=3", "--case=halt-trading"]);
        assert_eq!(o.driver, DriverKind::Scripted);
        assert_eq!(o.max_steps, 3);
        assert_eq!(o.cases[0].name, "halt-trading");
    }

    /// The verbs `list` and `mcp-bridge PATH` are read; the old `help` word and `-h` print the help
    /// page on stdout with exit 0.
    #[test]
    fn the_other_verbs_and_the_help_word_are_read() {
        assert!(matches!(try_cli(&["list"]).unwrap().verb, Verb::List));
        match try_cli(&["mcp-bridge", "/tmp/descriptor.json"]).unwrap().verb {
            Verb::McpBridge { descriptor } => {
                assert_eq!(descriptor, PathBuf::from("/tmp/descriptor.json"));
            }
            other => panic!("expected mcp-bridge, got {other:?}"),
        }
        for line in [&["help"][..], &["-h"], &["--help"], &["run", "--help"]] {
            let exit = try_cli(line).expect_err("help is an exit");
            assert_eq!((exit.code, exit.to_stdout), (0, true), "{line:?}: {exit:?}");
        }
        let exit = try_cli(&["--version"]).expect_err("version is an exit");
        assert_eq!(exit.text, concat!("vike-agent-eval ", env!("CARGO_PKG_VERSION"), "\n"));
    }

    /// No verb, an unknown verb, an unknown flag, an unknown case, a bad driver and a malformed
    /// number are usage errors; the stray `mcp-bridge` without its descriptor too.
    #[test]
    fn a_bare_command_and_every_typo_are_usage_errors() {
        refused(&[]);
        refused(&["frobnicate"]);
        refused(&["run", "--nope"]);
        assert!(refused(&["run", "--case", "no-such-case"]).contains("--case"));
        assert!(refused(&["run", "--driver", "gpt"]).contains("--driver"));
        assert!(refused(&["run", "--max-steps", "many"]).contains("--max-steps"));
        refused(&["run", "--max-steps"]);
        refused(&["mcp-bridge"]);
    }

    /// A zero budget would evaluate nothing, and the two spellings of the scripted driver may not
    /// disagree (the last of the two used to win silently).
    #[test]
    fn a_zero_budget_and_a_contradictory_driver_are_refused_after_the_parse() {
        let parsed = try_cli(&["run", "--max-steps", "0"]).unwrap();
        let Verb::Run(args) = parsed.verb else { panic!("not the run verb") };
        assert!(options(args).is_err());
        let parsed = try_cli(&["run", "--scripted", "--driver", "api"]).unwrap();
        let Verb::Run(args) = parsed.verb else { panic!("not the run verb") };
        assert!(options(args).is_err());
    }
}
