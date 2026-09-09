//! **THE UNATTENDED RUNNER** — one agent session against the OPERATOR's own node, on the operator's
//! own box, on the operating system's own schedule, leaving a record.
//!
//! ```text
//! vike-agent-run --record-dir DIR --task node-review --node 127.0.0.1:7900
//! ```
//!
//! # What this is the local-first answer to
//!
//! The hosted platforms answer *"run my agent while I sleep"* with a cloud: an account, a hosted
//! agent, a task, a deployment, a routine, and a conversation log you read back through their web
//! UI. Every one of those is a place your credentials and your positions live that is not your box.
//! This workspace has ruled a cloud out, so the equivalent has to be assembled from what is already
//! here — and it already all is:
//!
//!   * the surface (`vike-cli mcp`, a stdio MCP server with a `--profile` ring and a mandatory
//!     preview gate on every write);
//!   * the per-call record (`crates/vike-cli/src/cmd/mcp_trace.rs`'s `McpTrace`, one appended JSONL
//!     line per `tools/call` including every REFUSAL, redacted by name);
//!   * the model drivers and the MCP client this crate already owns for the evaluation harness
//!     (`crates/vike-agent-eval/src/driver.rs`'s `ModelDriver`,
//!     `crates/vike-agent-eval/src/mcp.rs`'s `McpServer`).
//!
//! What was missing is the thing that runs one session with none of the above wired to a human: a
//! single process that stands the server up, hands one prompt to one model, bounds the whole thing,
//! and writes down what happened. That is this module. The SCHEDULE is not here and never will be —
//! `deploy/vike-agent-run@.service` and its timer hand that job to systemd, which already solves
//! missed firings, jitter, persistence across reboots and failure notification better than anything
//! that could be written in this crate.
//!
//! # Why it lives in THIS crate, beside the evaluation harness
//!
//! The two are different products and they share one machine. `crates/vike-agent-eval/src/harness.rs`'s
//! `run_case` builds a WORLD — a throwaway project folder, a paper `vike-tradehub` node, minted
//! `DUMMY-` keys — and grades what the agent did against declared expectations. This module builds
//! NOTHING: it is handed the operator's node address and lets `vike-cli` find the operator's own
//! credential store, and it grades nothing at all, because there is no expected answer to a
//! question like *"is anything wrong with my node this morning"*.
//!
//! But underneath that split they are the same three pieces — the spawned server, the recorded
//! JSON-RPC transport, and the [`ModelDriver`] seam with its three implementations. A separate crate
//! would have to either duplicate those (the 900-line Claude Code driver above all) or take a
//! `vike-*` dependency edge onto this one, and this crate's whole declared property is that it has
//! NO such edge in either direction: it reaches the system the way an operator does, by spawning the
//! shipped binaries, so it can never compile against an internal API the shipped surface does not
//! expose. A second `[[bin]]` costs a file; a second crate costs a manifest, a CI member and that
//! property — and `docs/decisions/0004-crate-splits-do-not-shrink-ci.md` already settled that a
//! split is not free.
//!
//! The consequence worth stating: this crate's name now under-describes it. It is the
//! AGENT-DRIVING crate, of which evaluation is one consumer and the unattended runner is the other.
//!
//! # ⚠ READ-ONLY BY DEFAULT, AND THE DEFAULT IS NOT A PARSED VALUE
//!
//! A scheduled agent that can place an order is a different product from a scheduled agent that can
//! read. This one is the second, and three separate things hold it there:
//!
//!   1. **There is no `--profile` flag on this binary.** The scope is a BOOLEAN — [`RunSpec`]'s
//!      `allow_writes` — and [`profile_for`] turns it into `read-only` or `full`. A flag that parses
//!      a NAME has a wrong-value branch, and a wrong-value branch that falls back rather than
//!      refusing is how a ring silently widens; `crates/vike-cli/src/cmd/mcp.rs`'s `Profile::parse`
//!      gets that right for the server and there is no reason to hand the same mistake a second
//!      place to be made. A boolean has no unknown value.
//!   2. **The profile is always passed EXPLICITLY.** The server's own default is `full`
//!      (deliberately — an absent flag there is byte-identical to the surface that shipped before
//!      profiles existed), so a runner that merely *omitted* the flag would arm every write tool.
//!      [`mcp_argv`] therefore emits `--profile` on every launch, and
//!      `the_default_launch_asks_for_the_read_only_ring` pins the string.
//!   3. **The roster the server actually advertised is CROSS-CHECKED before the model sees it.**
//!      [`refuse_write_tools`] reads each advertised tool's own `annotations.destructiveHint` — the
//!      declaration `crates/vike-cli/src/cmd/mcp.rs`'s `is_write_tool` roster is already pinned
//!      equal to — and ABORTS the run before the prompt is issued if a write tool is on it. That is
//!      the difference between asking for a ring and having one: an argument this process typed is
//!      a request, and the roster that came back is the answer.
//!
//! Widening is one flag, `--allow-writes`, and it is loud: the binary prints a banner naming the
//! node it is about to be able to trade on, and the record stamps `allow_writes` and the profile so
//! that a run which COULD have written is distinguishable forever from one that could not.
//!
//! The mandatory preview gate is untouched underneath all of this. Even under `--allow-writes` a
//! write executes only against a `preview_token` the same server minted for the same command, so an
//! unattended agent that decides to trade must still make two calls and both are in the transcript.
//!
//! # The record
//!
//! Two halves, in ONE directory, and only the second is new:
//!
//!   * **Per CALL** — `mcp-YYYY-MM.jsonl`, written by the server itself. This module does not
//!     re-implement it; it passes `--trace-dir` and gets `crates/vike-cli/src/cmd/mcp_trace.rs`'s
//!     writer, its caps, its lock and its redaction table for free. That writer is OFF unless asked
//!     for (`docs/decisions/0039-the-agent-transcript-is-opt-in-and-argument-redacted.md`), and an
//!     unattended run is exactly the case that record exists for, so this runner always asks.
//!   * **Per RUN** — `run-<UTC stamp>-<pid>.json`, written here: when, which task and prompt, which
//!     driver and model, how many model turns and tool calls, what the agent concluded, and the
//!     outcome. It is the header a per-call stream cannot carry.
//!
//! ⚠ **A FILE PER RUN, not a line appended to a shared one — and the reason is the lock.**
//! `McpTrace` appends per tool call from sessions that genuinely share a file, so it has to take an
//! exclusive advisory lock around every append; the measurement that forced that (a host bind mount
//! silently losing most of 100 concurrent appends) is in its own module doc. This writer has no such
//! problem to solve and buys nothing by inheriting its solution: one record is produced at the END
//! of one run, and giving it its own file — opened `create_new`, so it can never overwrite and never
//! interleave — makes the atomicity structural instead of bought. The directory is still
//! append-only in the sense that matters: nothing here ever opens an existing record for writing,
//! rewrites one, or deletes one.
//!
//! ⚠ **No credential can reach the record, and it is enforced rather than asserted.**
//! [`write_summary`] is handed the model credential VALUES this process holds and walks every string
//! of the serialized document replacing them, so a conclusion in which a model echoed its own token
//! back lands as [`REDACTED`]. That is a narrow guarantee stated narrowly: it covers the secrets
//! this process actually has. A bare high-entropy string a model invented is not detectable here any
//! more than it is in `McpTrace` — the same residual, named in the same terms.
//!
//! # Bounded, everywhere
//!
//! Same rule as the rest of the crate, and here it is the operator's whole safety net: this runs at
//! 04:00 with nobody watching, so a wait without a bound is a process holding a connection to a live
//! trading node until somebody notices. Three bounds compose, and none of them is this module's own
//! invention:
//!
//!   * every JSON-RPC request is bounded by `crates/vike-agent-eval/src/mcp.rs`'s `RESPONSE_TIMEOUT`;
//!   * every model turn is bounded by its driver — `crates/vike-agent-eval/src/anthropic.rs`'s
//!     `REQUEST_TIMEOUT` for the API loop, `crates/vike-agent-eval/src/claude_cli.rs`'s `RUN_TIMEOUT`
//!     for the CLI (which this module OVERRIDES to the run budget, so the driver's own kill lands
//!     at the same instant the budget does);
//!   * the whole run is bounded by [`RunSpec::budget`], enforced at every tool call by
//!     [`BudgetedChannel`] and re-checked when the driver returns.
//!
//! ⚠ **The overshoot is stated, not assumed away.** The budget is checked BETWEEN waits, never
//! inside one, so a run can exceed it by at most one bounded wait — a model turn already in flight.
//! For `--driver api` that is `REQUEST_TIMEOUT`; for `--driver claude-cli` it is zero, because that
//! driver's own deadline is set to the budget. What can NOT happen is an unbounded wait, and what is
//! recorded either way is a [`Outcome::Timeout`], never a success and never an absence.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::driver::{DriveContext, ModelDriver, Step, ToolChannel};
use crate::locate_binary;
use crate::mcp::{Actor, McpServer, ToolResult};

/// The tool ring a run gets unless `--allow-writes` was passed. The spelling is
/// `crates/vike-cli/src/cmd/mcp.rs`'s `PROFILES`, and it is passed EXPLICITLY on every launch —
/// see the module doc's point 2.
pub const READ_ONLY_PROFILE: &str = "read-only";
/// The ring `--allow-writes` asks for: every tool the server implements, writes included. Still
/// behind the mandatory two-call preview gate.
pub const FULL_PROFILE: &str = "full";

/// The model-turn budget per run, when `--max-steps` names none.
///
/// Larger than the evaluation harness's, and for the opposite reason: a case is a handful of tool
/// calls with a known answer, while an unattended review of a live node is open-ended and a model
/// that spent one extra turn reading a second venue's positions is doing the job, not failing at it.
pub const DEFAULT_MAX_STEPS: usize = 24;

/// The wall-clock budget for one run when `--budget` names none, in seconds.
///
/// Chosen against the OTHER bound rather than against a model's speed: `--driver claude-cli` kills
/// its own child at `crates/vike-agent-eval/src/claude_cli.rs`'s `RUN_TIMEOUT`, and a default budget
/// longer than that would be a default nothing enforces.
pub const DEFAULT_BUDGET_SECS: u64 = 900;

/// What a redacted value is replaced by — the same word `crates/vike-cli/src/cmd/mcp_trace.rs`'s
/// `REDACTED` uses, so one grep over the whole record directory finds every suppression.
pub const REDACTED: &str = "<redacted>";

/// The cap on one recorded free-text field, in BYTES.
///
/// Bytes rather than characters for the reason `McpTrace` gives: one character is up to four bytes
/// and more after escaping, so a character cap bounds nothing about the file. Truncation lands on a
/// character boundary and says so in the text it leaves behind — a silently shortened conclusion
/// reads like a model that stopped early.
pub const MAX_TEXT_BYTES: usize = 8192;

/// The `kind` discriminator every run record carries, so a reader who finds this file beside the
/// per-call stream can tell them apart on a prefix match — `McpTrace`'s `RECORD_KIND` idiom.
pub const RECORD_KIND: &str = "agent_run";

/// Bumped when a field's MEANING changes, never for an addition. A reader that branches on it can
/// then treat an unknown value as "newer than me" rather than guessing.
pub const RECORD_SCHEMA: u64 = 1;

/// Every run record file starts with this and ends with `.json`.
pub const RUN_FILE_PREFIX: &str = "run-";

/// A named prompt an operator can put in a systemd unit without embedding a paragraph of prose in
/// it.
///
/// ⚠ Every task here is READ-SHAPED, and that is not a coincidence to be relied on: the ring is what
/// enforces it (module doc, point 3), and a task's wording is only what makes the agent's job
/// legible. A task that asked for a trade would simply be refused by the server it is pointed at.
pub struct Task {
    /// The `--task` selector, and the systemd instance name (`vike-agent-run@node-review`).
    pub name: &'static str,
    /// One line for the `tasks` verb.
    pub what: &'static str,
    /// The whole prompt the model is handed. No procedural help beyond it — the tool descriptions
    /// the server advertises are the rest of what the agent is taught, exactly as in the evaluation
    /// harness.
    pub prompt: &'static str,
}

/// The built-in tasks. Deliberately short: `--prompt` and `--prompt-file` cover everything else, and
/// a long list of canned prompts is a thing that rots without anybody noticing, because nothing can
/// gate whether a prompt is still a good one.
pub const TASKS: &[Task] = &[
    Task {
        name: "node-review",
        what: "read the node's state and report anything that looks wrong",
        prompt: "Review the state of the trading node. Report which venues are connected, what \
                 positions and working orders exist, and anything that looks wrong or unexpected. \
                 Use only the tools you have been given; if a tool refuses or the node is \
                 unreachable, say so plainly rather than guessing. Finish with a short summary for \
                 the operator.",
    },
    Task {
        name: "risk-review",
        what: "review open exposure and working orders, and say what you would change",
        prompt: "Review the open exposure and the working orders on the trading node against what \
                 the node itself reports about its limits and trading state. Say plainly what you \
                 would change and why. Do not attempt to change anything — describe it. If a tool \
                 refuses or the node is unreachable, say so rather than estimating. Finish with a \
                 short summary for the operator.",
    },
];

/// Look one up by `--task` name.
pub fn task_by_name(name: &str) -> Option<&'static Task> {
    TASKS.iter().find(|t| t.name == name)
}

/// How a run ended. Four outcomes, and the two that are NOT `Ok` are told apart deliberately:
/// "the agent could not finish in time" and "the agent finished and something went wrong" are
/// different mornings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The agent answered within its budget.
    Ok,
    /// The run reached its wall-clock budget. Recorded as this, never as a success and never as an
    /// absence — the module doc's last section is why.
    Timeout,
    /// The run could not be stood up or the driver failed: no server, no roster, a model error.
    Failed,
    /// A PRE-FLIGHT refusal. Nothing was driven — the server answered with a roster the requested
    /// ring forbids, so the prompt was never issued. Distinct from [`Outcome::Failed`] because it
    /// says the surface behaved differently from what was asked for, which is a finding about the
    /// build rather than about the run.
    Refused,
}

impl Outcome {
    /// The wire word. A `jq` filter's vocabulary, so it is spelled once, here.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Timeout => "timeout",
            Outcome::Failed => "failed",
            Outcome::Refused => "refused",
        }
    }

    /// The process exit status. Every non-zero value makes a systemd `Type=oneshot` unit enter the
    /// FAILED state, which is what an `OnFailure=` notifier keys on; the distinct codes are so a
    /// human reading a journal can tell a timeout from a failure without opening the record.
    pub fn exit_code(self) -> u8 {
        match self {
            Outcome::Ok => 0,
            Outcome::Failed => 1,
            // 2 is the usage-error code the binaries in this crate already use, so it is skipped.
            Outcome::Timeout => 3,
            Outcome::Refused => 4,
        }
    }
}

/// Everything one unattended run needs. Owned by the binary; nothing below reads the environment.
pub struct RunSpec<'a> {
    /// `--vike-cli PATH`, or `None` to look beside this executable
    /// (`crate::locate_binary`'s ladder).
    pub vike_cli: Option<&'a Path>,
    /// The `--task` name, or `"<prompt>"` for an inline one. Recorded verbatim.
    pub task: &'a str,
    /// The prompt handed to the model.
    pub prompt: &'a str,
    /// `--node HOST:PORT` — the OPERATOR's running `vike-tradehub`. `None` runs the create/backtest
    /// half of the surface with no node at all, which is a legitimate unattended job and is why this
    /// is an `Option` rather than a requirement.
    pub node: Option<&'a str>,
    /// `--datahub HOST:PORT`, or `None` to leave the server on its own default.
    pub datahub: Option<&'a str>,
    /// `--settings-dir DIR`, passed to the child as `VIKE_SETTINGS_DIR`. `None` lets the child
    /// perform its own project walk — the shape a deployed unit already gets from its
    /// `Environment=` line.
    pub settings_dir: Option<&'a Path>,
    /// Where both halves of the record go: this module's per-run document and, through
    /// `--trace-dir`, the server's own per-call JSONL.
    pub record_dir: &'a Path,
    /// See the module doc. `false` is `read-only`.
    pub allow_writes: bool,
    pub max_steps: usize,
    pub budget: Duration,
    /// Variables removed from the spawned server's environment — this process's own model
    /// credential. A PARAMETER because only the binary knows what it read.
    pub scrub: &'a [&'a str],
}

/// The `vike-cli` argument list one run launches, in a pure function so the ring can be pinned
/// without spawning anything.
///
/// ⚠ `--profile` is unconditional. See the module doc's point 2: the server's own default is `full`,
/// so an omitted flag is the widest ring rather than no opinion.
pub fn mcp_argv(spec: &RunSpec<'_>) -> Vec<String> {
    let mut args = vec![
        "mcp".to_string(),
        "--profile".to_string(),
        profile_for(spec.allow_writes).to_string(),
        // The per-call half of the record. Named OUTRIGHT rather than as a bare `--trace`, so the
        // server writes beside this module's own document instead of resolving a project of its own
        // — the "one walk decides" rule, obeyed by not walking twice.
        "--trace-dir".to_string(),
        spec.record_dir.display().to_string(),
    ];
    if let Some(node) = spec.node {
        args.push("--node".to_string());
        args.push(node.to_string());
    }
    if let Some(addr) = spec.datahub {
        args.push("--addr".to_string());
        args.push(addr.to_string());
    }
    args
}

/// Which ring a run asks for. The whole mapping, in one place.
pub fn profile_for(allow_writes: bool) -> &'static str {
    if allow_writes { FULL_PROFILE } else { READ_ONLY_PROFILE }
}

/// The tools an advertised roster declares DESTRUCTIVE — the server's own annotation, not a list
/// spelled here.
///
/// `crates/vike-cli/src/cmd/mcp.rs`'s `tools_spec` pins its `destructiveHint` annotations equal to
/// the `is_write_tool` roster the preview gate routes on, so an eighth write tool joins this check
/// the moment it joins that array. A tool that declares nothing is not counted: the absence of a
/// hint is not a claim, and the ring cross-check below is about tools that say they write.
pub fn write_tools_in(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter(|t| t["annotations"]["destructiveHint"] == json!(true))
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect()
}

/// The pre-flight ring cross-check: did the server we just launched actually withhold what we asked
/// it to withhold?
///
/// ⚠ POSITIVE in both directions. An EMPTY roster is refused too — a run against a server offering
/// no tools at all would "pass" this check while being able to do nothing, which is the vacuous
/// green this whole crate exists to make impossible.
pub fn refuse_write_tools(tools: &[Value], allow_writes: bool) -> Result<(), String> {
    if tools.is_empty() {
        return Err(
            "the mcp server advertised an EMPTY tool roster, so this run could do nothing \
                    at all — refusing rather than recording a session that was never possible"
                .to_string(),
        );
    }
    if allow_writes {
        return Ok(());
    }
    let writes = write_tools_in(tools);
    if writes.is_empty() {
        return Ok(());
    }
    Err(format!(
        "this run asked for the `{READ_ONLY_PROFILE}` tool ring and the server advertised {} \
         destructive tool(s) anyway: {}. Nothing was driven. An unattended agent must not be handed \
         a write tool it was not meant to have, so the disagreement between the requested profile \
         and the served roster is a refusal rather than something to proceed through.",
        writes.len(),
        writes.join(", ")
    ))
}

/// The canned plan `--driver scripted` runs: one read, then an answer.
///
/// It drives the REAL server over the REAL transport with no model and no credential, which is what
/// makes an end-to-end test of this runner possible on every PR — the same argument
/// `crates/vike-agent-eval/src/driver.rs`'s `Scripted` makes for the evaluation half. It is also a
/// genuine operator tool: scheduled with `--driver scripted`, it is a daily proof that the agent
/// surface still stands up against the node, costing no model at all.
pub fn scripted_probe() -> Vec<Step> {
    vec![
        Step::Call { tool: "node_snapshot".to_string(), args: json!({}) },
        Step::Answer(
            "scripted probe: the agent surface was reachable and `node_snapshot` answered. No \
             model was involved, so nothing here is an opinion about the node."
                .to_string(),
        ),
    ]
}

/// The deadline, wrapped around the one thing a driver is allowed to do.
///
/// ⚠ This is where the budget is actually ENFORCED, and it is deliberately the narrow seam rather
/// than a watchdog that kills a process: a driver whose next tool call errors STOPS and hands back
/// the error, which is a run that ends with a recorded outcome. A killed process ends with no record
/// at all, which is the outcome this module exists to prevent.
struct BudgetedChannel<'a> {
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
    // the answer to report the tidying-up. `crates/vike-agent-eval/src/harness.rs`'s `run_case_inner`
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
/// ⚠ **Deliberately NOT `crates/vike-agent-eval/src/mcp.rs`'s `apply_case_env`, and the difference is
/// the whole product.** That function scrubs `VIKE_HIST_STORE`, `RUST_LOG`, `VIKE_MAX_ORDER_QTY` and
/// the rest because a CASE is measuring the surface and the box's settings are noise. Here the box's
/// settings ARE the subject: this is the operator's own node, configured the way the operator
/// configured it, and a runner that quietly stripped their knobs would report on a system that does
/// not exist. Under `--allow-writes` it would also strip a guardrail on the way past.
///
/// So exactly three things are done, each with a reason that survives that argument:
///
///   * the model credential this process holds is REMOVED. A spawned child inherits the whole
///     environment and `crates/vike-cli/src/lib.rs`'s `run` sweeps it into a map on every
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
/// a new row on `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` ratchet, bought for a
/// convenience the deployment already expresses.
fn apply_child_env(cmd: &mut Command, spec: &RunSpec<'_>) {
    for name in spec.scrub {
        cmd.env_remove(name);
    }
    if let Some(dir) = spec.settings_dir {
        cmd.env("VIKE_SETTINGS_DIR", dir);
    }
}

/// Write the run record, and return the file it landed in.
///
/// ⚠ `secrets` is the VALUES this process holds, and every string in the document is walked for
/// them. It is a parameter for the same reason the scrub list is: only the binary knows what it
/// read, and a library that fetched a credential in order to redact it would be a library that holds
/// one.
///
/// ⚠ `create_new`: the file is never opened if it exists. Two runs starting in the same second under
/// the same pid is not a shape that occurs, but "never overwrite a record" is not a property to rest
/// on arithmetic, so a collision takes the next free suffix rather than replacing anything.
pub fn write_summary(dir: &Path, summary: &Summary, secrets: &[&str]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let mut record = summary.to_json();
    redact_in_place(&mut record, secrets);
    let body =
        serde_json::to_string_pretty(&record).map_err(|e| format!("encode the run record: {e}"))?;

    let stem = run_stem(summary.started_ms);
    for attempt in 0..64u32 {
        let name =
            if attempt == 0 { format!("{stem}.json") } else { format!("{stem}-{attempt}.json") };
        let path = dir.join(&name);
        match std::fs::File::create_new(&path) {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(body.as_bytes())
                    .map_err(|e| format!("write {}: {e}", path.display()))?;
                f.sync_all().map_err(|e| format!("sync {}: {e}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create {}: {e}", path.display())),
        }
    }
    Err(format!("64 run-record names under {} were already taken", dir.display()))
}

/// The name every artefact of one run shares: `run-20260906T031700Z-1234`.
///
/// ONE function, because the record and the server's stderr file must be recognisable as a pair in a
/// directory listing, and two independent renderings of "which run is this" would drift the first
/// time either changed.
pub fn run_stem(started_ms: i64) -> String {
    format!("{RUN_FILE_PREFIX}{}-{}", compact_stamp(started_ms), std::process::id())
}

/// Replace every occurrence of every secret, in every string of the document.
///
/// A whole-document walk rather than a per-field one: the fields are added to over time and a
/// per-field list is a second roster that rots. Empty and whitespace-only entries are skipped —
/// replacing the empty string would rewrite the document into nothing.
pub fn redact_in_place(value: &mut Value, secrets: &[&str]) {
    match value {
        Value::String(s) => {
            for secret in secrets {
                if secret.trim().is_empty() {
                    continue;
                }
                if s.contains(secret) {
                    *s = s.replace(secret, REDACTED);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_in_place(item, secrets);
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                redact_in_place(v, secrets);
            }
        }
        _ => {}
    }
}

/// One recorded free-text field, capped at [`MAX_TEXT_BYTES`] on a character boundary and SAYING it
/// was capped.
pub fn cap_text(text: &str) -> String {
    if text.len() <= MAX_TEXT_BYTES {
        return text.to_string();
    }
    let mut end = MAX_TEXT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… <truncated at {MAX_TEXT_BYTES} bytes>", &text[..end])
}

/// Milliseconds since the Unix epoch. A clock read, in a crate the determinism ratchet
/// (`crates/vike-ops/tests/clock_pin.rs`) deliberately does not scope: a record of WHEN a run
/// happened is the wall clock by definition.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// `1970-01-01T00:00:00Z`, for the record's human-readable fields.
pub fn utc_stamp(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// The same instant with no separators, for a FILE NAME: `19700101T000000Z`. Windows cannot hold a
/// `:` in a path, and the compact form still sorts in calendar order, which is the whole reason the
/// stamp rather than the epoch millisecond is what a reader sees in a directory listing.
pub fn compact_stamp(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(ms);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// Split an epoch-millisecond instant into UTC calendar parts.
///
/// ⚠ Hand-rolled rather than reached for: this crate carries NO `vike-*` dependency by design (see
/// the module doc), so `vike_model`'s calendar helpers are unavailable to it, and adding a date
/// crate for six fields would be a new dependency in a tree whose manifest argues every one. The
/// algorithm is Howard Hinnant's `civil_from_days`, which is exact for the whole proleptic Gregorian
/// range; `utc_stamp_matches_known_instants` pins it against four independently known instants
/// rather than against itself.
fn utc_parts(ms: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = ms.div_euclid(86_400_000);
    let rem_ms = ms.rem_euclid(86_400_000);
    let secs = rem_ms / 1_000;
    let (h, mi, s) = (secs / 3_600, (secs % 3_600) / 60, secs % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    (if mo <= 2 { y + 1 } else { y }, mo, d, h, mi, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The calendar arithmetic, against instants whose UTC rendering is known independently of this
    /// code. Without these the helper would be pinned only against itself.
    #[test]
    fn utc_stamp_matches_known_instants() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(86_400_000), "1970-01-02T00:00:00Z");
        // Unix time 1_000_000_000 and 1_600_000_000, two widely-published instants.
        assert_eq!(utc_stamp(1_000_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(utc_stamp(1_600_000_000_000), "2020-09-13T12:26:40Z");
        // …and a leap day, the case an off-by-one in the era arithmetic lands on.
        assert_eq!(utc_stamp(1_583_020_800_000), "2020-03-01T00:00:00Z");
        assert_eq!(compact_stamp(1_600_000_000_000), "20200913T122640Z");
    }

    #[test]
    fn a_capped_field_says_it_was_capped_and_stays_valid_utf8() {
        let short = "a short answer";
        assert_eq!(cap_text(short), short);
        // ⚠ A THREE-byte character, deliberately: the cap is not a multiple of 3, so the cut lands
        // mid-character and the boundary walk is what stops this from panicking. A two-byte
        // character would divide the cap exactly and prove nothing.
        let long: String = "€".repeat(MAX_TEXT_BYTES);
        assert!(!long.is_char_boundary(MAX_TEXT_BYTES), "this test needs a straddling cut");
        let capped = cap_text(&long);
        assert!(capped.contains("truncated"), "a capped field must say so: {capped}");
        assert!(
            capped.len() <= MAX_TEXT_BYTES + 64,
            "the cap must actually shorten the field: {} bytes",
            capped.len()
        );
        assert!(capped.starts_with('€'), "the surviving prefix must still be the text");
    }

    #[test]
    fn redaction_reaches_every_string_of_the_document() {
        let mut doc = json!({
            "conclusion": "the key is sk-live-SECRET, apparently",
            "nested": { "list": ["sk-live-SECRET", 7] },
            "untouched": 12,
        });
        redact_in_place(&mut doc, &["sk-live-SECRET", "", "   "]);
        let text = doc.to_string();
        assert!(!text.contains("sk-live-SECRET"), "a secret survived redaction: {text}");
        assert_eq!(text.matches(REDACTED).count(), 2, "both occurrences must be replaced: {text}");
        assert_eq!(doc["untouched"], json!(12), "non-strings are untouched");
    }

    #[test]
    fn the_default_launch_asks_for_the_read_only_ring() {
        let dir = PathBuf::from("record");
        let spec = RunSpec {
            vike_cli: None,
            task: "node-review",
            prompt: "p",
            node: Some("127.0.0.1:7900"),
            datahub: None,
            settings_dir: None,
            record_dir: &dir,
            allow_writes: false,
            max_steps: DEFAULT_MAX_STEPS,
            budget: Duration::from_secs(DEFAULT_BUDGET_SECS),
            scrub: &[],
        };
        let args = mcp_argv(&spec);
        let profile = args
            .iter()
            .position(|a| a == "--profile")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str);
        assert_eq!(
            profile,
            Some(READ_ONLY_PROFILE),
            "the default launch must be read-only: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--trace-dir"),
            "the per-call record is always on: {args:?}"
        );

        let wide = RunSpec { allow_writes: true, ..spec };
        let args = mcp_argv(&wide);
        let profile = args
            .iter()
            .position(|a| a == "--profile")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str);
        assert_eq!(profile, Some(FULL_PROFILE), "--allow-writes is the ONLY widening: {args:?}");
    }

    #[test]
    fn every_task_is_selectable_and_unique() {
        assert!(!TASKS.is_empty(), "an empty task table would make `--task` unusable");
        for task in TASKS {
            assert_eq!(task_by_name(task.name).map(|t| t.name), Some(task.name));
            assert!(!task.prompt.trim().is_empty(), "{} has no prompt", task.name);
            assert!(!task.what.trim().is_empty(), "{} has no one-line description", task.name);
        }
        let mut names: Vec<&str> = TASKS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two tasks share a --task name");
    }
}
