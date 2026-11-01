//! The third driver: the locally-installed **Claude Code CLI**, driving the same MCP surface as the
//! other two — so a run bills the operator's SUBSCRIPTION rather than API credits.
//!
//! # Why this exists
//!
//! [`crate::drivers::anthropic::Anthropic`] is the honest measurement and it costs money per model turn.
//! An operator who holds a Claude subscription already pays for a model; this driver spends that
//! instead. Nothing else about the evaluation changes: the same cases, the same real `vike-cli mcp`
//! server, the same real paper node, the same [`crate::eval::grade`] checks over the same transcript.
//!
//! # THE DESIGN DECISION: the tool calls come back through the harness's own [`ToolChannel`]
//!
//! `claude -p` is not an API: it is an agent that executes its own tools. There is no flag in this
//! CLI (2.1.x — every flag below was read off the real `claude --help` on the box that runs this
//! lane, never from memory) that hands a `tool_use` block to the host and waits for a result the
//! host computes. So the naive port of the Messages-API loop — parse `tool_use` out of
//! `--output-format stream-json` and feed a `tool_result` back — cannot be written against this
//! version, and pretending otherwise would produce a driver that silently measured nothing.
//!
//! The tempting alternative is to let the CLI open its own MCP connection to a SECOND
//! `vike-cli mcp` process and grade a record that server keeps. That was rejected: two servers
//! against one node means the grader reads a transcript the harness did not record, `Actor::Agent`
//! and `Actor::Harness` stop being separable, and a case could then pass on work the harness did
//! for it — the one property `crates/vike-agent-eval/src/world/mcp.rs`'s `Actor` exists to protect.
//!
//! What this module does instead is make **the harness itself the MCP server the CLI talks to**:
//!
//! ```text
//!   harness ──spawn──> claude -p --mcp-config …
//!      │                   │
//!      │                   └──spawn──> vike-agent-eval mcp-bridge <descriptor>
//!      │                                    │  (a byte pump: the CLI's stdio <-> a loopback socket)
//!      │<────────────── loopback TCP ───────┘
//!      │
//!      └── serve_rpc() ──> ToolChannel::call ──> the ONE `vike-cli mcp` the harness owns
//! ```
//!
//! `--mcp-config` can only name a command to spawn or a URL to fetch, and this crate may not add an
//! HTTP server (`deny.toml`'s `[bans]`, and the root manifest's one-transport-stack rule), so the
//! bridge is the seam: a mode of this harness's own binary that forwards newline-delimited
//! JSON-RPC between the CLI and a loopback socket the harness is listening on. Every `tools/call`
//! the model makes therefore arrives in [`serve_rpc`] and is executed through the SAME
//! [`ToolChannel`] the scripted and API drivers use, recorded in the SAME transcript as
//! `Actor::Agent`. The grader's input is not merely the same shape — it is the same object.
//!
//! ⚠ **The bridge's `initialize` FORWARDS the real server's `instructions`**, and that is the one
//! respect in which the bridge may not be a server of its own invention. This driver is the only
//! one whose client is a REAL MCP client, so this is the only route by which the shipped
//! `instructions` field reaches a model the way an operator's client would deliver it. The API
//! driver renders the same string into its system prompt instead
//! ([`crate::drivers::anthropic::system_for`]) — the two drivers are told the same CONTENT by the mechanism
//! each one's client uses, which is the invariant that matters; a single mechanism was never
//! available, because these two drivers do not have the same client.
//!
//! ⚠ **DECLARED RESIDUAL: whether the CLI RENDERS the field is the CLI's business, not ours.**
//! Nothing here can assert that the model was shown it — `serve_rpc`'s answer is testable and is
//! tested (`crates/vike-agent-eval/tests/claude_cli.rs`'s
//! `the_bridge_forwards_the_servers_instructions`), what the client then does with it is measured
//! only by a case's outcome. If a future CLI version drops server instructions, this lane's four
//! elsewhere-cases fail again and the API lane's do not, and THAT divergence is the finding.
//!
//! # What a subscription-driven run is NOT comparable to
//!
//! ⚠ **DECLARED RESIDUAL: this driver's model may be taught more than the shipped tool
//! descriptions.** The API driver's whole claim is a clean room — [`crate::drivers::anthropic::SYSTEM`] and
//! the roster, nothing else — and this driver passes the same string as `--system-prompt`, adds
//! `--setting-sources ""` (no user, project or local settings), `--strict-mcp-config` (no MCP
//! server but ours), `--disable-slash-commands` and `--tools ""` (no built-in tool at all, so the
//! agent cannot read a file or run a command around the surface under test). What it CANNOT
//! suppress in this CLI version is Claude Code's own memory discovery: a `CLAUDE.md` above the
//! child's working directory, and the user-level `~/.claude/CLAUDE.md`, are still injected.
//! Running the suite with `--work-dir` inside this repository therefore hands the model this
//! workspace's own instructions. `--bare` and `--safe-mode` both suppress memory and both were
//! measured to be unusable here — `--bare` forces `ANTHROPIC_API_KEY` auth (which is the cost this
//! driver exists to avoid) and `--safe-mode` drops the `--mcp-config` server entirely, leaving the
//! agent with no tools at all. So the mitigation is operational rather than structural: point
//! `--work-dir` outside any tree carrying a `CLAUDE.md`, which is what
//! `.github/workflows/agent-eval.yml` does. `docs/ops/agent-eval.md` states it for the operator.
//!
//! # The token
//!
//! ⚠ The subscription token is read in `main.rs` and nowhere else, exactly as the API key is. It is
//! held here, redacted by a manual `Debug`, formatted by no error path, and **set explicitly on the
//! CLI child's environment** rather than left to inheritance — because inheritance is invisible and
//! a reader cannot tell "this child needs it" from "this child happened to get it". It is also the
//! one child of this harness that MUST have it: `main.rs`'s `SCRUB_FROM_CHILDREN` removes the token
//! from the `vike-cli` and `vike-tradehub` children (a child inherits the whole environment, and
//! `vike-cli` sweeps `std::env::vars()` on every invocation), and those children are spawned by the
//! harness with a scrub list the driver is never given.
//!
//! # Bounded, everywhere
//!
//! Two children exist per case here rather than one, so every wait carries a deadline: the accept
//! is polled rather than blocked on, each socket read carries a read timeout, and the whole run
//! carries [`RUN_TIMEOUT`] after which the CLI is KILLED with a message naming the case. A hang
//! would hold a paper node forever and produce no verdict at all.

use std::time::Duration;

#[cfg(doc)]
use crate::drivers::ToolChannel;

mod bridge;
mod command;
mod drive;
mod stream;

pub use bridge::{
    Bridge, Served, allowed_tool_names, mcp_config, run_bridge, serve_rpc, tool_envelope,
};
pub use command::{ClaudeCli, Invocation, argv};
pub use stream::{StreamSummary, outcome_of, parse_stream};

/// The CLI this driver spawns when `--claude` names nothing. Resolved through `PATH` — the CI box
/// installs it system-wide at `/usr/bin/claude`, and the lane ASSERTS that rather than installing
/// it, because a lane that installs a model client is a lane that can silently change which client
/// answered.
pub const DEFAULT_BINARY: &str = "claude";

/// The long-lived subscription token the CLI authenticates with, minted by `claude setup-token`.
///
/// ⚠ This constant is the CHILD-SIDE spelling. The READ of the same name lives in `main.rs`, which
/// declares its own constant beside it — deliberately, because
/// `crates/vike-model/src/scan.rs` resolves a `Naming::Konst` against the constants of the FILE the
/// read is in, so a shared constant imported across modules would scan as a dynamic read.
pub const OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// The name the harness's MCP server is registered under in the CLI's `--mcp-config`.
///
/// ⚠ It is part of the wire: Claude Code exposes an MCP tool to the model as
/// `mcp__<server>__<tool>`, which is the spelling `--allowedTools` must use. Measured against the
/// real CLI rather than assumed — a run with this config reports
/// `"tools":["mcp__vike__node_snapshot", …]` in its `system`/`init` line.
pub const MCP_SERVER_NAME: &str = "vike";

/// The MCP protocol version this bridge answers `initialize` with. The real CLI's client accepts
/// it (measured: a server answering exactly this reports `"status":"connected"`).
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// What the bridge calls itself in `initialize`.
pub const SERVER_INFO_NAME: &str = "vike-agent-eval-bridge";

/// The whole-run ceiling for one case. Generous — a subscription run queues behind rate limits an
/// API key does not — and it is a CEILING rather than an expectation: the CLI is killed at it.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(900);

/// How long the bridge waits for the harness's socket before giving up. The harness is already
/// listening when the CLI is spawned, so this only has to outlast the CLI's own startup.
const BRIDGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

/// One poll of the accept/read loop. Also the socket read timeout, so a read never blocks past it.
const POLL: Duration = Duration::from_millis(50);

/// How long the CLI's remaining stdout may take to arrive after the process has exited.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// What a tool call gets once the case's budget is spent. It is an MCP tool ERROR rather than a
/// transport failure, so the model reads a sentence and can stop; the driver still fails the case,
/// because a run that ran out of budget has not answered the operator.
pub const BUDGET_EXHAUSTED: &str = "this evaluation harness's tool-call budget for the case is exhausted (--max-steps); no further \
     tool call will be executed";
