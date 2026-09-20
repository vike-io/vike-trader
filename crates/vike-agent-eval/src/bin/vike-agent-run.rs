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
//! ⚠ THIS BINARY OWNS EVERY ENVIRONMENT READ IT MAKES — the rule `crates/vike-ops/src/settings.rs`
//! states and `crates/vike-ops/tests/settings_registry.rs` gates. There are exactly two variables,
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

use vike_agent_eval::anthropic::{Anthropic, DEFAULT_MODEL};
use vike_agent_eval::claude_cli::{ClaudeCli, DEFAULT_BINARY};
use vike_agent_eval::driver::{ModelDriver, Scripted};
use vike_agent_eval::refuse_blank_credential;
use vike_agent_eval::unattended::{
    DEFAULT_BUDGET_SECS, DEFAULT_MAX_STEPS, Outcome, RunSpec, TASKS, run, scripted_probe,
    task_by_name, write_summary,
};

/// The Anthropic API key, for `--driver api`.
///
/// ⚠ A DELIBERATE duplicate of `crates/vike-agent-eval/src/main.rs`'s constant of the same name
/// rather than an import, for the reason that file states: `crates/vike-ops/src/scan.rs` resolves a
/// `Naming::Konst` against the constants of the FILE the read is in, so an imported constant scans
/// as a dynamic read and the registry gate could no longer see this variable at all.
const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// The Claude Code subscription token, for `--driver claude-cli`. Same duplication rule as above.
const CLAUDE_CODE_OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Removed from the environment of the `vike-cli mcp` server this binary spawns.
///
/// A spawned child inherits the whole environment and `crates/vike-cli/src/lib.rs`'s `run` sweeps
/// `std::env::vars()` into a map on every invocation, so without this the model credential would sit
/// inside the process holding the operator's node connection, for no reason at all.
///
/// ⚠ The Claude Code child is the exception, BY CONSTRUCTION rather than by an entry in a list: this
/// scrub list reaches the MCP server, while the CLI is spawned by
/// `crates/vike-agent-eval/src/claude_cli.rs`'s `ClaudeCli`, which is handed no scrub list and
/// instead SETS the token on its child outright. That child is the one talking to Anthropic; nothing
/// else must have it.
const SCRUB_FROM_CHILDREN: &[&str] = &[ANTHROPIC_API_KEY_ENV, CLAUDE_CODE_OAUTH_TOKEN_ENV];

const USAGE: &str = "usage: vike-agent-run run --record-dir DIR \
                     (--task NAME | --prompt TEXT | --prompt-file PATH) \
                     [--node HOST:PORT] [--datahub HOST:PORT] [--settings-dir DIR] \
                     [--driver claude-cli|api|scripted] [--model M] [--claude PATH] \
                     [--max-steps N] [--budget SECONDS] [--allow-writes] [--vike-cli PATH]\n       \
                     vike-agent-run tasks";

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriverKind {
    Api,
    ClaudeCli,
    Scripted,
}

impl DriverKind {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "api" => Ok(Self::Api),
            "claude-cli" => Ok(Self::ClaudeCli),
            "scripted" => Ok(Self::Scripted),
            other => {
                Err(format!("--driver {other:?} is not one of api|claude-cli|scripted\n{USAGE}"))
            }
        }
    }
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
    match dispatch(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("vike-agent-run: {e}");
            ExitCode::from(2)
        }
    }
}

fn dispatch(args: Vec<String>) -> Result<ExitCode, String> {
    let Some(verb) = args.first().map(String::as_str) else {
        println!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    match verb {
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        "tasks" => {
            for task in TASKS {
                println!("{:<16} {}", task.name, task.what);
            }
            Ok(ExitCode::SUCCESS)
        }
        "run" => execute(parse(&args[1..])?),
        other => Err(format!("unknown command {other:?}\n{USAGE}")),
    }
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut record_dir: Option<PathBuf> = None;
    let mut task: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut opts = Options {
        record_dir: PathBuf::new(),
        task: String::new(),
        prompt: String::new(),
        node: None,
        datahub: None,
        settings_dir: None,
        driver: DriverKind::Api,
        model: None,
        claude: PathBuf::from(DEFAULT_BINARY),
        max_steps: DEFAULT_MAX_STEPS,
        budget: Duration::from_secs(DEFAULT_BUDGET_SECS),
        allow_writes: false,
        vike_cli: None,
    };
    let value = |i: usize, name: &str| -> Result<String, String> {
        args.get(i + 1).cloned().ok_or_else(|| format!("{name} needs a value\n{USAGE}"))
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let mut consumed = 2;
        match arg {
            "--record-dir" => record_dir = Some(PathBuf::from(value(i, "--record-dir")?)),
            "--task" => {
                let name = value(i, "--task")?;
                let found = task_by_name(&name).ok_or_else(|| {
                    format!("unknown task {name:?}; `vike-agent-run tasks` prints them")
                })?;
                task = Some(found.name.to_string());
                prompt = Some(found.prompt.to_string());
            }
            "--prompt" => {
                let text = value(i, "--prompt")?;
                task = Some("<prompt>".to_string());
                prompt = Some(text);
            }
            "--prompt-file" => {
                let path = PathBuf::from(value(i, "--prompt-file")?);
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("read {}: {e}", path.display()))?;
                task = Some(format!("<file:{}>", path.display()));
                prompt = Some(text);
            }
            "--node" => opts.node = Some(value(i, "--node")?),
            "--datahub" => opts.datahub = Some(value(i, "--datahub")?),
            "--settings-dir" => {
                opts.settings_dir = Some(PathBuf::from(value(i, "--settings-dir")?))
            }
            "--driver" => opts.driver = DriverKind::parse(&value(i, "--driver")?)?,
            "--model" => opts.model = Some(value(i, "--model")?),
            "--claude" => opts.claude = PathBuf::from(value(i, "--claude")?),
            "--vike-cli" => opts.vike_cli = Some(PathBuf::from(value(i, "--vike-cli")?)),
            "--max-steps" => {
                let raw = value(i, "--max-steps")?;
                opts.max_steps =
                    raw.parse().map_err(|e| format!("--max-steps {raw:?}: {e}\n{USAGE}"))?;
            }
            "--budget" => {
                let raw = value(i, "--budget")?;
                let secs: u64 =
                    raw.parse().map_err(|e| format!("--budget {raw:?}: {e}\n{USAGE}"))?;
                opts.budget = Duration::from_secs(secs);
            }
            // ⚠ THE ONLY WIDENING, and it is a boolean rather than a `--profile NAME` on purpose —
            // `crates/vike-agent-eval/src/unattended.rs`'s module doc, point 1. A boolean has no
            // unknown value to fall back from.
            "--allow-writes" => {
                opts.allow_writes = true;
                consumed = 1;
            }
            other => return Err(format!("unknown option {other:?}\n{USAGE}")),
        }
        i += consumed;
    }

    // ⚠ `--record-dir` is REQUIRED and has no default, deliberately. This binary writes a permanent
    // record of what an agent did against a live node, and every default available to it would be a
    // project WALK — a second resolution that is `$VIKE_SETTINGS_DIR`-blind and can answer with a
    // different project than the credentials came from (the rule `crates/vike-boot/src/lib.rs`'s
    // `Booted` exists to enforce). One line in a unit file, one argument at a prompt.
    opts.record_dir = record_dir.ok_or_else(|| {
        format!(
            "--record-dir is required: this run leaves a permanent record and there is no safe \
             default for where it goes (a derived one would be a second project walk). Name the \
             directory, e.g. --record-dir <project>/settings/state/agent\n{USAGE}"
        )
    })?;
    opts.task = task.ok_or_else(|| {
        format!("name the work: --task NAME, --prompt TEXT or --prompt-file PATH\n{USAGE}")
    })?;
    opts.prompt = prompt.unwrap_or_default();
    if opts.prompt.trim().is_empty() {
        return Err(format!("the prompt is empty, so nothing would be asked\n{USAGE}"));
    }
    if opts.max_steps == 0 {
        return Err("--max-steps 0 would drive nothing".to_string());
    }
    Ok(opts)
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
         audit trail, and this run's record stamps that the ring was open. Remove --allow-writes \
         for the read-only default."
    );
}
