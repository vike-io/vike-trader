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
//!   * the per-call record (`crates/vike-cli/src/cmd/mcp/trace.rs`'s `McpTrace`, one appended JSONL
//!     line per `tools/call` including every REFUSAL, redacted by name);
//!   * the model drivers and the MCP client this crate already owns for the evaluation harness
//!     (`crates/vike-agent-eval/src/drivers/mod.rs`'s `ModelDriver`,
//!     `crates/vike-agent-eval/src/world/mcp.rs`'s `McpServer`).
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
//! The two are different products and they share one machine. `crates/vike-agent-eval/src/eval/harness.rs`'s
//! `run_case` builds a WORLD — a throwaway project folder, a paper `vike-tradehub` node, node keys
//! minted into that project's store — and grades what the agent did against declared expectations. This module builds
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
//! read. This one is the second, and three separate things hold it there — plus a fourth that holds
//! even where `--allow-writes` widens the ring:
//!
//!   1. **There is no `--profile` flag on this binary.** The scope is a BOOLEAN — [`RunSpec`]'s
//!      `allow_writes` — and [`profile_for`] turns it into `read-only` or `full`. A flag that parses
//!      a NAME has a wrong-value branch, and a wrong-value branch that falls back rather than
//!      refusing is how a ring silently widens; `crates/vike-cli/src/cmd/mcp/scope.rs`'s `Profile::parse`
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
//!   4. **No setting is changed by ANY run, `--allow-writes` included.** The owner ruled on
//!      decision 0040 (`policy.*` on 2026-09-28, every other key on 2026-09-29): an ATTENDED agent
//!      changes a live setting only after the owner said yes in chat (`docs/decisions/0086`
//!      point 6), and a session nobody attends has no chat — so [`mcp_argv`] launches every server
//!      with `--unattended`, and that server refuses every `set_setting` before a preview or a
//!      token exists (`crates/vike-cli/src/cmd/mcp/node_writes.rs`'s `unattended_refusal`). The operator
//!      changes settings with `vike-cli config set` or the GUI. It is asserted for both rings by
//!      `every_launch_declares_the_session_unattended`, and against the shipped server by
//!      `crates/vike-cli/tests/mcp_unattended_cli.rs`.
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
//!     re-implement it; it passes `--trace-dir` and gets `crates/vike-cli/src/cmd/mcp/trace.rs`'s
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
//!   * every JSON-RPC request is bounded by `crates/vike-agent-eval/src/world/mcp.rs`'s `RESPONSE_TIMEOUT`;
//!   * every model turn is bounded by its driver — `crates/vike-agent-eval/src/drivers/anthropic.rs`'s
//!     `REQUEST_TIMEOUT` for the API loop, `crates/vike-agent-eval/src/drivers/claude_cli.rs`'s `RUN_TIMEOUT`
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

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

#[cfg(doc)]
use crate::drivers::ModelDriver;
use crate::drivers::Step;

mod record;
mod run;
#[cfg(test)]
mod tests;

pub use record::{
    cap_text, compact_stamp, now_ms, redact_in_place, run_stem, utc_stamp, write_summary,
};
#[cfg(doc)]
use run::BudgetedChannel;
pub use run::{BUDGET_EXCEEDED, Summary, run};

/// The tool ring a run gets unless `--allow-writes` was passed. The spelling is
/// `crates/vike-cli/src/cmd/mcp/scope.rs`'s `PROFILES`, and it is passed EXPLICITLY on every launch —
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
/// its own child at `crates/vike-agent-eval/src/drivers/claude_cli.rs`'s `RUN_TIMEOUT`, and a default budget
/// longer than that would be a default nothing enforces.
pub const DEFAULT_BUDGET_SECS: u64 = 900;

/// What a redacted value is replaced by — the same word `crates/vike-cli/src/cmd/mcp/trace.rs`'s
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
        // Nobody attends this session, so the server refuses every `set_setting` (decision 0040,
        // the owner's rulings of 2026-09-28 and 2026-09-29) — UNCONDITIONALLY, for both rings: the
        // module doc's point 4. A server too old to know the flag refuses its argv and the run
        // ends as `Failed` before a prompt is issued, which is the fail-closed direction.
        "--unattended".to_string(),
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
/// `crates/vike-cli/src/cmd/mcp/tool_schemas.rs`'s `tools_spec` pins its `destructiveHint` annotations equal to
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
/// `crates/vike-agent-eval/src/drivers/mod.rs`'s `Scripted` makes for the evaluation half. It is also a
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
