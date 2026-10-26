//! `vike-agent-run` — one unattended agent session against the operator's OWN node, on a schedule
//! the operating system owns.
//!
//! ```text
//! vike-agent-run run --record-dir DIR (--task NAME | --prompt TEXT | --prompt-file PATH)
//!                    [--node HOST:PORT] [--datahub HOST:PORT] [--settings-dir DIR]
//!                    [--driver claude-cli|api|scripted] [--model M] [--claude PATH]
//!                    [--max-steps N] [--budget SECONDS] [--allow-writes]
//!                    [--vike-cli PATH]
//! vike-agent-run tasks
//! ```
//!
//! The shape, the read-only argument, the record and the delegation of scheduling are all argued in
//! `crates/vike-agent-eval/src/unattended.rs`'s module doc, which is the authority; this file is the
//! composition root and carries only what a root owns.
//!
//! ⚠ THIS BINARY OWNS EVERY ENVIRONMENT READ IT MAKES — the rule `crates/vike-config/src/settings.rs`
//! states and `crates/vike-ops/tests/settings_secrets/settings_registry.rs` gates. There are exactly two variables,
//! one per model driver, each read here, handed to its driver, and never printed, logged, passed as
//! an argument or written into the record. They are the SAME two `crates/vike-agent-eval/src/main.rs`
//! reads, so the registry rows are shared: a row is keyed on `(name, crate)` and both binaries are
//! this crate.
//!
//! ⚠ AN ABSENT CREDENTIAL IS A FAILURE, NOT A SKIP, when a real model was asked for — the same
//! refusal, in the same words, through the same `crate::refuse_blank_credential`. A scheduled run
//! that quietly measured nothing and exited 0 is worse here than in the harness: nobody is watching,
//! so the silence lasts until somebody wonders why the record directory is empty.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use vike_agent_eval::drivers::anthropic::{Anthropic, DEFAULT_MODEL};
use vike_agent_eval::drivers::claude_cli::{ClaudeCli, DEFAULT_BINARY};
use vike_agent_eval::drivers::{ModelDriver, Scripted};
use vike_agent_eval::refuse_blank_credential;
use vike_agent_eval::unattended::{
    DEFAULT_BUDGET_SECS, DEFAULT_MAX_STEPS, Outcome, RunSpec, TASKS, run, scripted_probe,
    task_by_name, write_summary,
};

/// The Anthropic API key, for `--driver api`.
///
/// ⚠ A DELIBERATE duplicate of `crates/vike-agent-eval/src/main.rs`'s constant of the same name
/// rather than an import, for the reason that file states: `crates/vike-model/src/scan.rs` resolves a
/// `Naming::Konst` against the constants of the FILE the read is in, so an imported constant scans
/// as a dynamic read and the registry gate could no longer see this variable at all.
const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// The Claude Code subscription token, for `--driver claude-cli`. Same duplication rule as above.
const CLAUDE_CODE_OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Removed from the environment of the `vike-cli mcp` server this binary spawns.
///
/// A spawned child inherits the whole environment and `crates/vike-cli/src/dispatch.rs`'s `run` sweeps
/// `std::env::vars()` into a map on every invocation, so without this the model credential would sit
/// inside the process holding the operator's node connection, for no reason at all.
///
/// ⚠ The Claude Code child is the exception, BY CONSTRUCTION rather than by an entry in a list: this
/// scrub list reaches the MCP server, while the CLI is spawned by
/// `crates/vike-agent-eval/src/drivers/claude_cli/command.rs`'s `ClaudeCli`, which is handed no scrub list and
/// instead SETS the token on its child outright. That child is the one talking to Anthropic; nothing
/// else must have it.
const SCRUB_FROM_CHILDREN: &[&str] = &[ANTHROPIC_API_KEY_ENV, CLAUDE_CODE_OAUTH_TOKEN_ENV];

/// The command line, read by `clap` directly (decision 0155; this crate names no `vike-*` crate, so not through `vike_util::cli`): `--flag
/// value` and `--flag=value`, an unknown flag refused with a suggestion, a flag given twice refused,
/// `-h`/`--help`/`help` and `-V`/`--version` on stdout with exit 0, any other usage error —
/// including no verb at all — on stderr with exit 2.
#[derive(clap::Parser, Debug)]
#[command(
    name = "vike-agent-run",
    version,
    about = "One unattended agent session against the operator's OWN node, on a schedule the \
             operating system owns",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    verb: Verb,
}

#[expect(
    clippy::large_enum_variant,
    reason = "`Verb` is built once per process; the gap between `Run(RunArgs)` and the unit verbs costs nothing"
)]
#[derive(clap::Subcommand, Debug)]
enum Verb {
    /// Run one session and leave a record
    Run(RunArgs),
    /// Print the built-in tasks
    Tasks,
}

/// `run`'s flags. ⚠ `--record-dir` is REQUIRED and has no default, deliberately: this binary writes
/// a permanent record of what an agent did against a live node, and every default available to it
/// would be a project WALK — a second resolution that is `$VIKE_SETTINGS_DIR`-blind and can answer
/// with a different project than the credentials came from (the rule
/// `crates/vike-boot/src/spec.rs`'s `Booted` exists to enforce). One line in a unit file, one
/// argument at a prompt. The work is named by exactly ONE of `--task`, `--prompt`, `--prompt-file`
/// (naming two used to let the last win in silence).
#[derive(clap::Args, Debug)]
#[command(group = clap::ArgGroup::new("work").required(true).multiple(false)
    .args(["task", "prompt", "prompt_file"]))]
struct RunArgs {
    /// Where the permanent record of the session is written (required)
    #[arg(long, value_name = "DIR", value_parser = non_blank_path)]
    record_dir: PathBuf,
    /// A built-in task by name (`vike-agent-run tasks` prints them)
    #[arg(long, value_name = "NAME", value_parser = task_name)]
    task: Option<String>,
    /// The prompt, verbatim
    #[arg(long, value_name = "TEXT", value_parser = non_blank)]
    prompt: Option<String>,
    /// A file holding the prompt
    #[arg(long, value_name = "PATH", value_parser = non_blank_path)]
    prompt_file: Option<PathBuf>,
    /// The vike-tradehub observe address of the node to review
    #[arg(long, value_name = "HOST:PORT", value_parser = non_blank)]
    node: Option<String>,
    /// The vike-datahub address to give the agent
    #[arg(long, value_name = "HOST:PORT", value_parser = non_blank)]
    datahub: Option<String>,
    /// The settings directory the vike-cli server reads
    #[arg(long, value_name = "DIR", value_parser = non_blank_path)]
    settings_dir: Option<PathBuf>,
    /// Which model drives the session
    #[arg(long, value_enum, default_value = "api")]
    driver: DriverKind,
    /// The model id (default: the driver's own)
    #[arg(long, value_name = "M", value_parser = non_blank)]
    model: Option<String>,
    /// The Claude Code binary for --driver claude-cli
    #[arg(long, value_name = "PATH", value_parser = non_blank_path, default_value = DEFAULT_BINARY)]
    claude: PathBuf,
    /// The model-turn budget
    #[arg(long, value_name = "N", default_value_t = DEFAULT_MAX_STEPS)]
    max_steps: usize,
    /// The wall-clock budget of the whole session
    #[arg(long, value_name = "SECONDS", default_value_t = DEFAULT_BUDGET_SECS)]
    budget: u64,
    // ⚠ THE ONLY WIDENING, and it is a boolean rather than a `--profile NAME` on purpose —
    // `crates/vike-agent-eval/src/unattended.rs`'s module doc, point 1. A boolean has no unknown
    // value to fall back from.
    /// Give the session the FULL tool ring (submit, cancel, modify, flatten, halt); read-only by default
    #[arg(long)]
    allow_writes: bool,
    /// The vike-cli binary (default: a sibling of this executable, then target/debug)
    #[arg(long, value_name = "PATH", value_parser = non_blank_path)]
    vike_cli: Option<PathBuf>,
}

/// `--task`'s value parser: the name must be one of the built-in tasks.
fn task_name(raw: &str) -> Result<String, String> {
    task_by_name(raw)
        .map(|task| task.name.to_string())
        .ok_or_else(|| "unknown task; `vike-agent-run tasks` prints them".to_string())
}

/// Which model, if any, drives the session.
///
/// ⚠ `Api` is the DEFAULT, the SAME default `crates/vike-agent-eval/src/main.rs` has. Two binaries
/// in one crate that disagreed about what an unnamed `--driver` means would be a trap with no
/// upside, and the local-first claim this runner makes is not about which endpoint answers: the box
/// is the operator's, the node is the operator's, the record is on the operator's disk, and none of
/// that changes with the driver. `--driver claude-cli` spends a subscription instead of credits and
/// is the better choice on a workstation; it also needs the operator's HOME directory and a JIT
/// runtime, which is why the shipped unit names a driver rather than taking this default —
/// `deploy/vike-agent-run@.service`'s header carries the sandbox lines that differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum DriverKind {
    Api,
    ClaudeCli,
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

struct Options {
    record_dir: PathBuf,
    task: String,
    prompt: String,
    node: Option<String>,
    datahub: Option<String>,
    settings_dir: Option<PathBuf>,
    driver: DriverKind,
    model: Option<String>,
    claude: PathBuf,
    max_steps: usize,
    budget: Duration,
    allow_writes: bool,
    vike_cli: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli: Cli = <Cli as clap::Parser>::parse();
    match dispatch(cli.verb) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("vike-agent-run: {e}");
            ExitCode::from(2)
        }
    }
}

fn dispatch(verb: Verb) -> Result<ExitCode, String> {
    match verb {
        Verb::Tasks => {
            for task in TASKS {
                println!("{:<16} {}", task.name, task.what);
            }
            Ok(ExitCode::SUCCESS)
        }
        Verb::Run(args) => execute(options(args)?),
    }
}

/// `run`'s flags as the options the session takes. What `clap` cannot know stays here: the prompt
/// file is READ (a failure names the path), and an empty prompt or a zero budget of turns would
/// ask or drive nothing.
fn options(args: RunArgs) -> Result<Options, String> {
    let (task, prompt) = match (args.task, args.prompt, args.prompt_file) {
        (Some(name), _, _) => {
            // Validated by `task_name` already.
            let found = task_by_name(&name).ok_or_else(|| format!("unknown task {name:?}"))?;
            (found.name.to_string(), found.prompt.to_string())
        }
        (None, Some(text), _) => ("<prompt>".to_string(), text),
        (None, None, Some(path)) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            (format!("<file:{}>", path.display()), text)
        }
        // The `work` group is required, so `clap` refused this before it got here.
        (None, None, None) => {
            return Err("name the work: --task NAME, --prompt TEXT or --prompt-file PATH".into());
        }
    };
    if prompt.trim().is_empty() {
        return Err("the prompt is empty, so nothing would be asked".to_string());
    }
    if args.max_steps == 0 {
        return Err("--max-steps 0 would drive nothing".to_string());
    }
    Ok(Options {
        record_dir: args.record_dir,
        task,
        prompt,
        node: args.node,
        datahub: args.datahub,
        settings_dir: args.settings_dir,
        driver: args.driver,
        model: args.model,
        claude: args.claude,
        max_steps: args.max_steps,
        budget: Duration::from_secs(args.budget),
        allow_writes: args.allow_writes,
        vike_cli: args.vike_cli,
    })
}

fn execute(opts: Options) -> Result<ExitCode, String> {
    // ⚠ Read HERE and nowhere else, and held only long enough to hand to a driver. The values are
    // also handed to `write_summary`, which walks the whole record replacing them — so a model that
    // echoed its own credential back cannot land it on disk.
    let mut secrets: Vec<String> = Vec::new();
    let mut driver: Box<dyn ModelDriver> = match opts.driver {
        DriverKind::Scripted => Box::new(Scripted::new(scripted_probe())),
        DriverKind::Api => {
            let key = std::env::var(ANTHROPIC_API_KEY_ENV).unwrap_or_default();
            if key.trim().is_empty() {
                return Err(refuse_blank_credential(ANTHROPIC_API_KEY_ENV, "Set it"));
            }
            secrets.push(key.clone());
            let model = opts.model.clone().unwrap_or_else(|| DEFAULT_MODEL.to_string());
            Box::new(Anthropic::new(key, model, opts.max_steps))
        }
        DriverKind::ClaudeCli => {
            let token = std::env::var(CLAUDE_CODE_OAUTH_TOKEN_ENV).unwrap_or_default();
            if token.trim().is_empty() {
                return Err(refuse_blank_credential(
                    CLAUDE_CODE_OAUTH_TOKEN_ENV,
                    "Mint one with `claude setup-token` (it needs a Claude subscription) and set it",
                ));
            }
            secrets.push(token.clone());
            // ⚠ The driver's OWN kill is moved to this run's budget, so the two deadlines are one
            // instant rather than two numbers that could disagree. Without it a run would be bounded
            // by whichever of them happened to be smaller.
            Box::new(
                ClaudeCli::new(opts.claude.clone(), token, opts.model.clone(), opts.max_steps)
                    .with_timeout(opts.budget),
            )
        }
    };

    if opts.allow_writes {
        announce_write_access(opts.node.as_deref());
    }

    // ABSOLUTE from here down: the record's own `record_dir` field is pasted by a human into a `jq`
    // or an `ls`, and a relative path in it names whatever directory the reader happens to be in.
    let record_dir = std::path::absolute(&opts.record_dir)
        .map_err(|e| format!("resolve {} to an absolute path: {e}", opts.record_dir.display()))?;

    let spec = RunSpec {
        vike_cli: opts.vike_cli.as_deref(),
        task: &opts.task,
        prompt: &opts.prompt,
        node: opts.node.as_deref(),
        datahub: opts.datahub.as_deref(),
        settings_dir: opts.settings_dir.as_deref(),
        record_dir: &record_dir,
        allow_writes: opts.allow_writes,
        max_steps: opts.max_steps,
        budget: opts.budget,
        scrub: SCRUB_FROM_CHILDREN,
    };
    let summary = run(&spec, driver.as_mut());

    // ⚠ The record is written on EVERY path — `run` returns a `Summary` and never an `Err`
    // precisely so that this call has no branch to skip it. A run that happened and left nothing
    // behind is the one outcome an accountability record cannot have.
    let borrowed: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let written = write_summary(&record_dir, &summary, &borrowed);

    print!("{}", summary.render());
    match &written {
        Ok(path) => eprintln!("vike-agent-run: record -> {}", path.display()),
        // The record could not be written, which is worse than the run's own outcome: report it,
        // and fail regardless of what the agent concluded.
        Err(e) => {
            eprintln!("vike-agent-run: the run record could not be written: {e}");
            return Ok(ExitCode::from(Outcome::Failed.exit_code()));
        }
    }
    Ok(ExitCode::from(summary.outcome.exit_code()))
}

/// The banner `--allow-writes` prints. Loud, on stderr (which every service manager captures), and
/// naming the node — an operator reading a journal must be able to see that a scheduled agent COULD
/// have traded, without opening the record.
fn announce_write_access(node: Option<&str>) {
    let where_ = node.unwrap_or("no node (--node was not given)");
    eprintln!(
        "vike-agent-run: ⚠ --allow-writes: this unattended session is being given the FULL tool \
         ring against {where_}, so it can submit, cancel, modify, flatten and halt. Every write \
         still goes through the server's mandatory two-call preview gate and lands in the node's \
         audit trail, and this run's record stamps that the ring was open. It can change NO \
         policy.* setting: the server is told nobody attends it, and limits are the operator's \
         (`vike-cli config set` or the GUI). Remove --allow-writes for the read-only default."
    );
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

    /// `run <line>` as the options the session would take.
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
    /// the `clap` migration): the deployed unit's `ExecStart=` line (`deploy/vike-agent-run@.service`)
    /// and `docs/ops/agent-schedule.md`'s example.
    #[test]
    fn the_deployed_line_is_accepted_with_its_meaning() {
        // run --task %i --driver api --record-dir D --node ADDR --budget 900
        let o = opts(&[
            "--task",
            "node-review",
            "--driver",
            "api",
            "--record-dir",
            "/srv/vike-<unit>/settings/state/agent",
            "--node",
            "127.0.0.1:7879",
            "--budget",
            "900",
        ]);
        assert_eq!(o.task, "node-review");
        assert!(!o.prompt.trim().is_empty(), "the task supplies its prompt");
        assert_eq!(o.driver, DriverKind::Api);
        assert_eq!(o.record_dir, PathBuf::from("/srv/vike-<unit>/settings/state/agent"));
        assert_eq!(o.node.as_deref(), Some("127.0.0.1:7879"));
        assert_eq!(o.budget, Duration::from_secs(900));
        assert!(!o.allow_writes);
        assert_eq!(o.max_steps, DEFAULT_MAX_STEPS);
        // docs/ops/agent-schedule.md: the same without `--driver`.
        let o = opts(&[
            "--task",
            "node-review",
            "--node",
            "127.0.0.1:7879",
            "--record-dir",
            "d",
            "--budget",
            "900",
        ]);
        assert_eq!(o.driver, DriverKind::Api);
    }

    #[test]
    fn every_flag_is_accepted_in_a_free_order() {
        let o = opts(&[
            "--allow-writes",
            "--vike-cli",
            "/b/vike-cli",
            "--claude",
            "/usr/bin/claude",
            "--model",
            "m1",
            "--max-steps",
            "5",
            "--settings-dir",
            "/s",
            "--datahub",
            "127.0.0.1:7878",
            "--prompt",
            "look at the node",
            "--driver",
            "claude-cli",
            "--record-dir",
            "rec",
        ]);
        assert!(o.allow_writes);
        assert_eq!(o.vike_cli, Some(PathBuf::from("/b/vike-cli")));
        assert_eq!(o.claude, PathBuf::from("/usr/bin/claude"));
        assert_eq!(o.model.as_deref(), Some("m1"));
        assert_eq!(o.max_steps, 5);
        assert_eq!(o.settings_dir, Some(PathBuf::from("/s")));
        assert_eq!(o.datahub.as_deref(), Some("127.0.0.1:7878"));
        assert_eq!(o.task, "<prompt>");
        assert_eq!(o.prompt, "look at the node");
        assert_eq!(o.driver, DriverKind::ClaudeCli);
        assert_eq!(o.budget, Duration::from_secs(DEFAULT_BUDGET_SECS));
    }

    #[test]
    fn a_prompt_file_is_read_and_names_the_task() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("p.txt");
        std::fs::write(&file, "check the book").unwrap();
        let path = file.display().to_string();
        let o = opts(&["--record-dir", "rec", "--prompt-file", &path]);
        assert_eq!(o.prompt, "check the book");
        assert_eq!(o.task, format!("<file:{path}>"));
    }

    #[test]
    fn the_required_record_dir_and_the_work_are_enforced() {
        assert!(refused(&["run", "--task", "node-review"]).contains("--record-dir"));
        let text = refused(&["run", "--record-dir", "rec"]);
        assert!(text.contains("--task") && text.contains("--prompt"), "{text}");
        // Exactly ONE of the three names the work.
        refused(&["run", "--record-dir", "rec", "--task", "node-review", "--prompt", "x"]);
    }

    /// `--flag=value` is read like `--flag value`.
    #[test]
    fn the_equals_spelling_is_read() {
        let o =
            opts(&["--task=node-review", "--record-dir=rec", "--budget=60", "--driver=scripted"]);
        assert_eq!(o.task, "node-review");
        assert_eq!(o.budget, Duration::from_secs(60));
        assert_eq!(o.driver, DriverKind::Scripted);
    }

    /// An unknown task, driver, flag or verb and a malformed number are usage errors; no verb at all
    /// prints the help page to stderr with exit 2.
    #[test]
    fn typos_and_a_bare_command_are_usage_errors() {
        assert!(refused(&["run", "--record-dir", "r", "--task", "nope"]).contains("--task"));
        assert!(
            refused(&["run", "--record-dir", "r", "--task", "node-review", "--driver", "x"])
                .contains("--driver")
        );
        assert!(
            refused(&["run", "--record-dir", "r", "--task", "node-review", "--budget", "soon"])
                .contains("--budget")
        );
        refused(&["run", "--record-dir", "r", "--task", "node-review", "--nope"]);
        refused(&["frobnicate"]);
        refused(&[]);
    }

    /// The `tasks` verb, the `help` word and `-h`/`-V` are read; the help word is a success.
    #[test]
    fn the_tasks_verb_the_help_word_and_version_are_read() {
        assert!(matches!(try_cli(&["tasks"]).unwrap().verb, Verb::Tasks));
        for line in [&["help"][..], &["-h"], &["--help"], &["run", "--help"]] {
            let exit = try_cli(line).expect_err("help is an exit");
            assert_eq!((exit.code, exit.to_stdout), (0, true), "{line:?}: {exit:?}");
        }
        let exit = try_cli(&["-V"]).expect_err("version is an exit");
        assert_eq!(exit.text, concat!("vike-agent-run ", env!("CARGO_PKG_VERSION"), "\n"));
    }

    /// An empty prompt (a blank file) and a zero budget of turns are refused after the parse.
    #[test]
    fn an_empty_prompt_file_and_zero_steps_are_refused_after_the_parse() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("blank.txt");
        std::fs::write(&file, "  \n").unwrap();
        let path = file.display().to_string();
        let Ok(Cli { verb: Verb::Run(args) }) =
            try_cli(&["run", "--record-dir", "r", "--prompt-file", &path])
        else {
            panic!("the command line itself parses");
        };
        assert!(options(args).is_err());
        let Ok(Cli { verb: Verb::Run(args) }) =
            try_cli(&["run", "--record-dir", "r", "--task", "node-review", "--max-steps", "0"])
        else {
            panic!("the command line itself parses");
        };
        assert!(options(args).is_err());
    }
}
