//! The agent-driving crate: the MODEL-IN-THE-LOOP half of the evaluation harness, and the
//! UNATTENDED runner that shares its machine.
//!
//! Two binaries, one library. `vike-agent-eval` (`src/main.rs`) grades a suite of cases against a
//! world it builds itself; `vike-agent-run` ([`unattended`], `src/bin/vike-agent-run.rs`) runs ONE
//! session against the OPERATOR's own node and grades nothing, because there is no expected answer
//! to "is anything wrong this morning". They share the spawned server, the recorded transport and
//! the three model drivers verbatim — which is the whole argument for their living together, and
//! [`unattended`]'s module doc is where it is made.
//!
//! Everything below describes the EVALUATION half.
//!
//! # The contract
//!
//! A DRIVER is handed one user prompt and the tool roster the real `vike-cli mcp` server
//! advertises, and drives that server over its real stdio transport until it stops. Everything it
//! did is recorded — every JSON-RPC request and every response, in order — and the outcome is then
//! graded by [`grade::grade`] from that transcript plus the node's own state, read back through the
//! same MCP server. **No model ever judges a result here.** A model-graded eval measures two
//! nondeterministic things at once and cannot say which one moved.
//!
//! Three drivers implement [`driver::ModelDriver`], and the first two differ only in WHO IS BILLED:
//!
//!   * [`anthropic::Anthropic`] — the Messages API in an ordinary agent loop: `tools/list`
//!     translated into the API's tool schema, `tool_use` blocks executed against the MCP server,
//!     `tool_result` blocks fed back, until the model stops or the step cap is reached. It spends
//!     API credits.
//!   * [`claude_cli::ClaudeCli`] — the locally-installed **Claude Code CLI** in headless mode,
//!     spending the operator's SUBSCRIPTION instead. That CLI executes its own tools, so the
//!     harness makes itself the MCP server the CLI talks to — every tool call still arrives through
//!     the same [`driver::ToolChannel`] and is recorded in the same transcript. The whole argument,
//!     and the one thing it cannot suppress, is in that module's own doc.
//!   * [`driver::Scripted`] — a canned sequence of tool calls per case. It is what this crate's own
//!     tests and CI run: the SAME MCP server, the SAME paper node, the SAME grader, with the model
//!     removed. That is what makes the pipeline itself gateable while the model half never is.
//!
//! # What this proves, and what it cannot
//!
//! It proves that an agent reading only the shipped tool descriptions and the shipped skills
//! reaches the operator's intended outcome through the mandatory preview gate — and, in the refusal
//! and node-down cases, that it declines to invent a capability or a number. It cannot prove a
//! model will do that NEXT time: a model is nondeterministic, so this harness is a MEASUREMENT and
//! never a merge gate. Its deterministic twin — the write gate driven over the same transport with
//! scripted requests — is `crates/vike-cli/tests/mcp_transcript.rs`, and that one IS a gate.
//!
//! # Safety
//!
//! Nothing this harness runs can reach a venue, a real credential, or the box's own settings. Each
//! case runs inside its own throwaway project root with `VIKE_SETTINGS_DIR` pointing into it; every
//! venue mount is PAPER (the daemon's live gate is never armed); the two node HMAC keys are
//! throwaway `DUMMY-` strings this crate mints; and the credential store it writes is the one it
//! created a moment earlier. The model credential — the API key, or the Claude Code subscription
//! token — is read in `main.rs` alone and never reaches a log line, an argv, a transcript or the
//! report.
//!
//! # Bounded, everywhere
//!
//! Every wait in this crate carries a deadline, and that is a rule rather than a habit: this
//! harness spawns two child processes per case (three under
//! [`claude_cli::ClaudeCli`], which adds the CLI itself) and drives one of them over a pipe, so a
//! wait without a bound is a run that hangs forever holding a paper node — the worst outcome
//! available to it, because a hang produces no verdict at all and looks like work in progress.

use std::path::{Path, PathBuf};

pub mod anthropic;
pub mod cases;
pub mod claude_cli;
pub mod driver;
pub mod grade;
pub mod harness;
pub mod mcp;
pub mod node;
pub mod report;
pub mod unattended;

/// The refusal a binary prints when the credential a model driver needs is unset or blank.
///
/// ⚠ ONE function for both drivers, rather than two sentences that happen to agree today. The
/// property being kept is not the prose but the CLAIM: **a run that measured nothing must never
/// exit 0**, and both refusals must say so in words an operator recognises as the same mistake —
/// the skip-honesty rule `docs/ops/live-smoke-lane.md` states per venue. Two copies drift, and the
/// drift is invisible until somebody hits the arm nobody re-read.
///
/// `remedy` is the ONLY part that differs, because getting an API key and minting a subscription
/// token are different errands. It is a sentence fragment continuing "…measured. ", so it starts
/// capitalised and carries no trailing punctuation.
///
/// ⚠ It takes the variable NAME rather than reading anything: this is a library, and only a binary
/// reads the environment (`crates/vike-ops/tests/settings_registry.rs`). The VALUE is never a
/// parameter here and must never become one — a credential that reaches a formatter is a credential
/// that reaches a log.
pub fn refuse_blank_credential(variable: &str, remedy: &str) -> String {
    format!(
        "{variable} is unset or blank, so no model could be driven and NOTHING would have been \
         measured. {remedy}, or run `--scripted` — which drives the same server, node and grader \
         with a canned plan and no model."
    )
}

/// Locate a workspace binary this harness SPAWNS.
///
/// ⚠ `env!("CARGO_BIN_EXE_…")` is not available here and could not be: it resolves only for a bin
/// the CURRENT package produced, and every binary this harness drives belongs to another package.
/// So the lookup is a runtime one, in the order a caller can reason about:
///
///   1. `explicit` — what `--vike-cli` / `--vike-tradehub` named. An operator's answer wins outright.
///   2. beside the running executable — the ordinary case, where cargo put every workspace binary in
///      one directory;
///   3. the PARENT of that directory — where a test binary lives, since cargo puts integration-test
///      executables under `target/<profile>/deps/` and the bins one level up;
///   4. `target/debug/<name>` under the current working directory — the last resort, for a harness
///      invoked from a checkout root against a build it did not produce.
///
/// ⚠ Deliberately NOT a compile-time path. `crates/vike-ops/tests/compile_time_path_gate.rs` states
/// the rule this obeys: a path baked in by the compiler names the tree the binary was BUILT in, and
/// is right on the developer's box and wrong everywhere else.
pub fn locate_binary(name: &str, explicit: Option<&Path>) -> Result<PathBuf, String> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    if let Some(p) = explicit {
        return if p.is_file() {
            Ok(p.to_path_buf())
        } else {
            Err(format!("{}: no such file (named explicitly for {name})", p.display()))
        };
    }
    let mut tried = Vec::new();
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join(&file));
        if let Some(up) = dir.parent() {
            candidates.push(up.join(&file));
        }
    }
    candidates.push(PathBuf::from("target").join("debug").join(&file));
    for c in candidates {
        if c.is_file() {
            return Ok(c);
        }
        tried.push(c.display().to_string());
    }
    Err(format!(
        "cannot find the `{name}` binary. Looked in: {}. Build it (`cargo build -p {name}`) or name \
         it outright with --{name}.",
        tried.join(", ")
    ))
}
