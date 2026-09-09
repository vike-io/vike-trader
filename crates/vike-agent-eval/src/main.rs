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
//! needs as a parameter — the rule `crates/vike-ops/src/settings.rs` states and
//! `crates/vike-ops/tests/settings_registry.rs` gates: libraries take configuration as parameters,
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

use vike_agent_eval::anthropic::{Anthropic, DEFAULT_MODEL};
use vike_agent_eval::cases::{CASES, Case, by_name};
use vike_agent_eval::claude_cli::{ClaudeCli, DEFAULT_BINARY};
use vike_agent_eval::driver::{ModelDriver, Scripted};
use vike_agent_eval::harness::{Binaries, run_case};
use vike_agent_eval::report::Report;
use vike_agent_eval::{claude_cli, harness, locate_binary, refuse_blank_credential};

/// The Anthropic API key, for `--driver api`. Read HERE and nowhere else.
const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// The Claude Code subscription token, for `--driver claude-cli`. Read HERE and nowhere else.
///
/// It is what `claude setup-token` mints, and the CLI reads it from the environment variable of the
/// same name — so this harness's whole job with it is to move it from this process into that one
/// child, naming it on the way.
///
/// ⚠ The library's own copy of this spelling (`vike_agent_eval::claude_cli::OAUTH_TOKEN_ENV`) is a
/// DELIBERATE duplicate rather than an import: `crates/vike-ops/src/scan.rs` resolves a
/// `Naming::Konst` against the constants of the FILE the read is in, so an imported constant scans
/// as a dynamic read and the registry gate could no longer see this variable at all.
const CLAUDE_CODE_OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Variables removed from the environment of every child this harness spawns THROUGH
/// `crates/vike-agent-eval/src/mcp.rs`'s `apply_case_env` — the `vike-cli` MCP server and the
/// `vike-tradehub` paper node.
///
/// ⚠ A spawned child inherits the whole environment, and `crates/vike-cli/src/lib.rs`'s `run`
/// sweeps `std::env::vars()` into a map on every invocation — so without this the model credential
/// this process holds would sit inside the two binaries the harness spawns per case, for no reason
/// at all. The API key belongs in one HTTP header and nowhere else.
///
/// ⚠ THE CLAUDE CODE CHILD IS THE ONE EXCEPTION, and it is an exception BY CONSTRUCTION rather than
/// by an entry in a list: this scrub list is threaded to `run_case`, which applies it to the two
/// binaries the HARNESS spawns, while the CLI is spawned by
/// `crates/vike-agent-eval/src/claude_cli.rs`'s `ClaudeCli::command`, which is handed no scrub list
/// and instead SETS `CLAUDE_CODE_OAUTH_TOKEN` on the child outright. That child is the only one
/// that must have the token — it is the one talking to Anthropic — and every other child must not.
/// `crates/vike-agent-eval/tests/claude_cli.rs` pins both halves.
const SCRUB_FROM_CHILDREN: &[&str] = &[ANTHROPIC_API_KEY_ENV, CLAUDE_CODE_OAUTH_TOKEN_ENV];

/// The default model-turn budget per case. Every case in the suite is a handful of tool calls; a
/// model still working past this has not answered the operator, and the case has no answer to grade.
const DEFAULT_MAX_STEPS: usize = 12;

const USAGE: &str = "usage: vike-agent-eval run [--case NAME]... [--driver api|claude-cli|scripted] \
                     [--scripted] [--model M] [--max-steps N] [--claude PATH] \
                     [--report PATH.json] [--work-dir DIR] [--keep-work] \
                     [--vike-cli PATH] [--vike-tradehub PATH]\n       vike-agent-eval list\n       \
                     vike-agent-eval mcp-bridge PATH";

/// Which model, if any, drives the suite.
///
/// ⚠ `Api` is the DEFAULT, so `--scripted` and a bare `run` behave exactly as they did before this
/// enum existed. A new default would have silently changed what an existing command measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriverKind {
    /// The Anthropic Messages API. Costs API credits.
    Api,
    /// The locally-installed Claude Code CLI. Costs the operator's subscription.
    ClaudeCli,
    /// No model at all.
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

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
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

fn run(args: Vec<String>) -> Result<ExitCode, String> {
    let Some(verb) = args.first().map(String::as_str) else {
        println!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    match verb {
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        "list" => {
            for case in CASES {
                println!("{:<28} skill: {:<28} {:?}", case.name, case.skill, case.node);
                for check in harness::checks_of(case) {
                    println!("      {check}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        "run" => execute(parse(&args[1..])?),
        // ⚠ NOT an operator verb. `--driver claude-cli` writes an MCP client configuration naming
        // THIS binary and this verb, and the Claude Code CLI then spawns it; a person running it by
        // hand gets a connect failure against a descriptor nothing wrote. It is a verb rather than a
        // second binary because it must be the same executable the running harness is, which
        // `std::env::current_exe` answers and a `[[bin]]` name does not.
        "mcp-bridge" => {
            let descriptor = args.get(1).ok_or_else(|| {
                format!("mcp-bridge needs the path of the bridge descriptor\n{USAGE}")
            })?;
            claude_cli::run_bridge(Path::new(descriptor))?;
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command {other:?}\n{USAGE}")),
    }
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut opts = Options {
        cases: Vec::new(),
        driver: DriverKind::Api,
        model: None,
        max_steps: DEFAULT_MAX_STEPS,
        claude: PathBuf::from(DEFAULT_BINARY),
        report: None,
        // Under the CURRENT DIRECTORY, never the operating system's temp directory: the rule
        // `crates/vike-ops/tests/system_temp_gate.rs` states — a production path into the OS temp
        // directory resolves to something else inside a container and is emptied by something the
        // operator does not control.
        work_dir: PathBuf::from("agent-eval-work"),
        keep_work: false,
        vike_cli: None,
        vike_tradehub: None,
    };
    let value = |i: usize, name: &str| -> Result<String, String> {
        args.get(i + 1).cloned().ok_or_else(|| format!("{name} needs a value\n{USAGE}"))
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        // Every valued option advances the cursor by two; the flags below by one.
        let mut consumed = 2;
        match arg {
            "--case" => {
                let name = value(i, "--case")?;
                let case = by_name(&name).ok_or_else(|| {
                    format!("unknown case {name:?}; `vike-agent-eval list` prints the suite")
                })?;
                opts.cases.push(case);
            }
            // ⚠ KEPT, and it is not an alias that could drift: it SETS `--driver scripted`, so the
            // documented spelling and the new one cannot mean different things.
            "--scripted" => {
                opts.driver = DriverKind::Scripted;
                consumed = 1;
            }
            "--driver" => opts.driver = DriverKind::parse(&value(i, "--driver")?)?,
            "--keep-work" => {
                opts.keep_work = true;
                consumed = 1;
            }
            "--claude" => opts.claude = PathBuf::from(value(i, "--claude")?),
            "--model" => opts.model = Some(value(i, "--model")?),
            "--max-steps" => {
                let raw = value(i, "--max-steps")?;
                opts.max_steps =
                    raw.parse().map_err(|e| format!("--max-steps {raw:?}: {e}\n{USAGE}"))?;
            }
            "--report" => opts.report = Some(PathBuf::from(value(i, "--report")?)),
            "--work-dir" => opts.work_dir = PathBuf::from(value(i, "--work-dir")?),
            "--vike-cli" => opts.vike_cli = Some(PathBuf::from(value(i, "--vike-cli")?)),
            "--vike-tradehub" => {
                opts.vike_tradehub = Some(PathBuf::from(value(i, "--vike-tradehub")?));
            }
            other => return Err(format!("unknown option {other:?}\n{USAGE}")),
        }
        i += consumed;
    }
    if opts.cases.is_empty() {
        opts.cases = CASES.iter().collect();
    }
    if opts.max_steps == 0 {
        return Err("--max-steps 0 would evaluate nothing".to_string());
    }
    Ok(opts)
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
    // directory (`crates/vike-agent-eval/src/node.rs`'s `PaperNode::spawn`). `Project::create` is
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
