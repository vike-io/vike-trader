//! One session: the budgeted tool channel, the summary it produces, and `run` with its teardown.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{
    Outcome, RECORD_KIND, RECORD_SCHEMA, RunSpec, cap_text, mcp_argv, now_ms, profile_for,
    refuse_write_tools, run_stem, utc_stamp, write_tools_in,
};
use crate::drivers::{DriveContext, ModelDriver, ToolChannel};
use crate::locate_binary;
use crate::world::mcp::{Actor, McpServer, ToolResult};

/// The deadline, wrapped around the one thing a driver is allowed to do.
///
/// ⚠ This is where the budget is actually ENFORCED, and it is deliberately the narrow seam rather
/// than a watchdog that kills a process: a driver whose next tool call errors STOPS and hands back
/// the error, which is a run that ends with a recorded outcome. A killed process ends with no record
/// at all, which is the outcome this module exists to prevent.
pub(super) struct BudgetedChannel<'a> {
    server: &'a mut McpServer,
    deadline: Instant,
}

impl ToolChannel for BudgetedChannel<'_> {
    fn call(&mut self, name: &str, args: &Value) -> Result<ToolResult, String> {
        if Instant::now() >= self.deadline {
            return Err(BUDGET_EXCEEDED.to_string());
        }
        self.server.call_tool(Actor::Agent, name, args)
    }
}

/// The message a budget refusal carries. A constant because the binary's outcome classification and
/// this refusal must not be two independent opinions about the same event.
pub const BUDGET_EXCEEDED: &str =
    "the run's wall-clock budget was reached before this tool call, so the session was ended";

/// What one run produced. Everything the record holds, plus what the binary prints.
pub struct Summary {
    pub started_ms: i64,
    pub finished_ms: i64,
    pub task: String,
    pub prompt: String,
    pub driver: String,
    pub profile: &'static str,
    pub allow_writes: bool,
    pub node: Option<String>,
    pub max_steps: usize,
    pub budget: Duration,
    /// Model turns (or scripted steps) spent. Zero when nothing was driven.
    pub steps: usize,
    /// How many tools the server advertised, and how many of them declared themselves destructive.
    /// Both are recorded because the SECOND is the evidence the ring held — see the module doc.
    pub roster_size: usize,
    pub write_tools_offered: usize,
    /// `tools/call`s the AGENT made, read back off the transcript rather than counted as it went:
    /// the transcript is the one record and a second count could disagree with it.
    pub tool_calls: usize,
    pub write_tools_called: usize,
    pub outcome: Outcome,
    /// The failure's message, or `None`.
    pub detail: Option<String>,
    /// What the agent would have told the operator.
    pub conclusion: String,
    pub record_dir: PathBuf,
}

impl Summary {
    /// The record, as it goes to disk. Nulls are written rather than omitted, for the reason
    /// `McpTrace::record` gives: a filter over a year of records should not have to branch on a
    /// field's PRESENCE as well as on its value.
    pub fn to_json(&self) -> Value {
        json!({
            "kind": RECORD_KIND,
            "schema": RECORD_SCHEMA,
            "started_ms": self.started_ms,
            "started_utc": utc_stamp(self.started_ms),
            "finished_utc": utc_stamp(self.finished_ms),
            "duration_ms": (self.finished_ms - self.started_ms).max(0),
            "task": self.task,
            "prompt": cap_text(&self.prompt),
            "driver": self.driver,
            "profile": self.profile,
            "allow_writes": self.allow_writes,
            "node": self.node,
            "max_steps": self.max_steps,
            "budget_ms": u64::try_from(self.budget.as_millis()).unwrap_or(u64::MAX),
            "steps": self.steps,
            "roster_size": self.roster_size,
            "write_tools_offered": self.write_tools_offered,
            "tool_calls": self.tool_calls,
            "write_tools_called": self.write_tools_called,
            "outcome": self.outcome.as_str(),
            "detail": self.detail.as_deref().map(cap_text),
            "conclusion": cap_text(&self.conclusion),
            "record_dir": self.record_dir.display().to_string(),
            "pid": std::process::id(),
            "runner_version": env!("CARGO_PKG_VERSION"),
        })
    }

    /// The lines the binary prints. Short on purpose: this runs under a service manager, so its
    /// stdout is a journal entry somebody skims, and the document on disk is the detail.
    pub fn render(&self) -> String {
        let mut out = format!(
            "vike-agent-run: {} — task {} · driver {} · profile {} · {} step(s), {} tool call(s)\n",
            self.outcome.as_str(),
            self.task,
            self.driver,
            self.profile,
            self.steps,
            self.tool_calls,
        );
        if let Some(d) = &self.detail {
            out.push_str(&format!("  detail: {d}\n"));
        }
        if !self.conclusion.trim().is_empty() {
            out.push_str(&format!("  answer: {}\n", self.conclusion.trim()));
        }
        out
    }
}

/// Run one unattended session and return what happened.
///
/// ⚠ **This never returns `Err`.** Every failure — a missing binary, a server that would not
/// handshake, a roster the ring forbids, a model that errored, a budget that ran out — is folded
/// into the [`Summary`] with its own [`Outcome`], because the caller's next action is the same in
/// all of them: write the record. A `Result` here would give the binary a path on which a run
/// happened and nothing was written down, which is the one outcome an accountability record cannot
/// have.
pub fn run(spec: &RunSpec<'_>, driver: &mut dyn ModelDriver) -> Summary {
    let started_ms = now_ms();
    let deadline = Instant::now() + spec.budget;
    let mut summary = Summary {
        started_ms,
        finished_ms: started_ms,
        task: spec.task.to_string(),
        prompt: spec.prompt.to_string(),
        driver: driver.name(),
        profile: profile_for(spec.allow_writes),
        allow_writes: spec.allow_writes,
        node: spec.node.map(str::to_string),
        max_steps: spec.max_steps,
        budget: spec.budget,
        steps: 0,
        roster_size: 0,
        write_tools_offered: 0,
        tool_calls: 0,
        write_tools_called: 0,
        outcome: Outcome::Failed,
        detail: None,
        conclusion: String::new(),
        record_dir: spec.record_dir.to_path_buf(),
    };

    match drive(spec, driver, deadline, &mut summary) {
        Ok(()) => {}
        Err(Stopped::Refused(why)) => {
            summary.outcome = Outcome::Refused;
            summary.detail = Some(why);
        }
        Err(Stopped::Failed(why)) => {
            // ⚠ The budget is re-checked HERE rather than only inside the channel. A driver can fail
            // for a reason of its own AFTER the deadline has passed (the Claude Code driver's own
            // kill is set to the same instant), and reporting that as an ordinary failure would hide
            // the fact that the run simply ran out of time.
            summary.outcome =
                if Instant::now() >= deadline { Outcome::Timeout } else { Outcome::Failed };
            summary.detail = Some(why);
        }
    }
    summary.finished_ms = now_ms();
    summary
}

/// How a run stopped short. Private: it exists only to keep [`run`]'s two failure dispositions from
/// being told apart by string matching.
enum Stopped {
    Refused(String),
    Failed(String),
}

fn drive(
    spec: &RunSpec<'_>,
    driver: &mut dyn ModelDriver,
    deadline: Instant,
    summary: &mut Summary,
) -> Result<(), Stopped> {
    std::fs::create_dir_all(spec.record_dir)
        .map_err(|e| Stopped::Failed(format!("create {}: {e}", spec.record_dir.display())))?;

    let binary = locate_binary("vike-cli", spec.vike_cli).map_err(Stopped::Failed)?;
    let mut cmd = Command::new(&binary);
    cmd.args(mcp_argv(spec));
    apply_child_env(&mut cmd, spec);

    // The server's OWN diagnostic channel, named as a SIBLING of the record this run is about to
    // write so the pairing is visible in a directory listing. It is not part of the record — it is
    // what `McpServer`'s error messages quote when the server dies, and an unattended run is exactly
    // the case where "it died and nobody saw why" is the failure to avoid.
    let stderr_path = spec.record_dir.join(format!("{}.mcp.err", run_stem(summary.started_ms)));
    let mut server = McpServer::launch(cmd, &stderr_path).map_err(Stopped::Failed)?;
    let (tools, instructions) = server.initialize().map_err(Stopped::Failed)?;

    summary.roster_size = tools.len();
    summary.write_tools_offered = write_tools_in(&tools).len();
    refuse_write_tools(&tools, spec.allow_writes).map_err(Stopped::Refused)?;

    let outcome = {
        let mut channel = BudgetedChannel { server: &mut server, deadline };
        let ctx = DriveContext {
            case: spec.task,
            work_dir: spec.record_dir,
            instructions: &instructions,
        };
        driver.drive(&ctx, spec.prompt, &tools, &mut channel)
    };

    // Read off the transcript BEFORE the disposition below can return: the calls happened whether
    // the driver finished or not, and a record that dropped them on the failure path would lose
    // exactly the evidence an operator wants after a bad night.
    let calls = server.transcript.agent_calls();
    let writes = write_tools_in(&tools);
    summary.tool_calls = calls.len();
    summary.write_tools_called = calls.iter().filter(|c| writes.contains(&c.tool)).count();

    let result = match outcome {
        Ok(o) => {
            summary.steps = o.steps;
            summary.conclusion = o.final_text;
            summary.outcome = Outcome::Ok;
            Ok(())
        }
        Err(e) => Err(Stopped::Failed(e)),
    };

    // Teardown is FOLDED IN, never propagated: a server that outlives its own EOF is a finding about
    // this run, and letting it discard a conclusion that has already been computed would throw away
    // the answer to report the tidying-up. `crates/vike-agent-eval/src/eval/harness.rs`'s `run_case_inner`
    // makes the same call for the same reason.
    if let Err(e) = server.shutdown()
        && result.is_ok()
    {
        summary.outcome = Outcome::Failed;
        summary.detail = Some(e);
    }
    result
}

/// The environment the spawned `vike-cli mcp` server runs under.
///
/// ⚠ **Deliberately NOT `crates/vike-agent-eval/src/world/mcp.rs`'s `apply_case_env`, and the difference is
/// the whole product.** That function scrubs `VIKE_HIST_STORE`, `RUST_LOG`, `VIKE_MAX_ORDER_QTY` and
/// the rest because a CASE is measuring the surface and the box's settings are noise. Here the box's
/// settings ARE the subject: this is the operator's own node, configured the way the operator
/// configured it, and a runner that quietly stripped their knobs would report on a system that does
/// not exist. Under `--allow-writes` it would also strip a guardrail on the way past.
///
/// So exactly three things are done, each with a reason that survives that argument:
///
///   * the model credential this process holds is REMOVED. A spawned child inherits the whole
///     environment and `crates/vike-cli/src/dispatch.rs`'s `run` sweeps it into a map on every
///     invocation, so without this the key would sit inside the server for no reason at all. The
///     names are the caller's — only a binary knows what it read.
///   * `VIKE_SETTINGS_DIR` is set only when `--settings-dir` named one. Unset, the child performs
///     its own walk, which is what a deployed unit's own `Environment=` line already answers.
///
/// ⚠ **And nothing else — the file log level in particular is NOT touched here.** A nightly run is
/// exactly the shape that once wrote 341 GB of trace-level JSON, so turning that down matters; it is
/// the UNIT's `Environment=` line that does it, the way every other shipped unit does, rather than
/// this process forming an opinion. The mechanical half of that choice is that reading the variable
/// in order to decide whether to override it would be a direct environment read in a LIBRARY file —
/// a new row on `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` ratchet, bought for a
/// convenience the deployment already expresses.
fn apply_child_env(cmd: &mut Command, spec: &RunSpec<'_>) {
    for name in spec.scrub {
        cmd.env_remove(name);
    }
    if let Some(dir) = spec.settings_dir {
        cmd.env("VIKE_SETTINGS_DIR", dir);
    }
}
