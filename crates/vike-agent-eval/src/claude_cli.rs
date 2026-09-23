//! The third driver: the locally-installed **Claude Code CLI**, driving the same MCP surface as the
//! other two — so a run bills the operator's SUBSCRIPTION rather than API credits.
//!
//! # Why this exists
//!
//! [`crate::anthropic::Anthropic`] is the honest measurement and it costs money per model turn.
//! An operator who holds a Claude subscription already pays for a model; this driver spends that
//! instead. Nothing else about the evaluation changes: the same cases, the same real `vike-cli mcp`
//! server, the same real paper node, the same [`crate::grade`] checks over the same transcript.
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
//! for it — the one property `crates/vike-agent-eval/src/mcp.rs`'s `Actor` exists to protect.
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
//! ([`crate::anthropic::system_for`]) — the two drivers are told the same CONTENT by the mechanism
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
//! descriptions.** The API driver's whole claim is a clean room — [`crate::anthropic::SYSTEM`] and
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

use std::fmt;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::anthropic::SYSTEM;
use crate::driver::{DriveContext, ModelDriver, Outcome, ToolChannel};
use crate::mcp::ToolResult;

/// The CLI this driver spawns when `--claude` names nothing. Resolved through `PATH` — the CI box
/// installs it system-wide at `/usr/bin/claude`, and the lane ASSERTS that rather than installing
/// it, because a lane that installs a model client is a lane that can silently change which client
/// answered.
pub const DEFAULT_BINARY: &str = "claude";

/// The long-lived subscription token the CLI authenticates with, minted by `claude setup-token`.
///
/// ⚠ This constant is the CHILD-SIDE spelling. The READ of the same name lives in `main.rs`, which
/// declares its own constant beside it — deliberately, because
/// `crates/vike-ops/src/scan.rs` resolves a `Naming::Konst` against the constants of the FILE the
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

/// Where the bridge finds the harness, written by the harness into the case's own work directory.
///
/// ⚠ The nonce is a HANDSHAKE, not a secret. It exists so a stray local process that happens to
/// connect to the loopback port is refused rather than served the trading tools; it does not defend
/// against anything that can read the case's work directory, and it is not asked to — the tools on
/// the far side reach one throwaway PAPER node with `DUMMY-` keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bridge {
    /// `127.0.0.1:<port>` — always loopback, never a routable address.
    pub addr: String,
    pub nonce: String,
}

impl Bridge {
    pub fn to_json(&self) -> Value {
        json!({ "addr": self.addr, "nonce": self.nonce })
    }

    pub fn from_json(v: &Value) -> Result<Self, String> {
        let addr = v["addr"].as_str().ok_or("the bridge descriptor names no `addr`")?;
        let nonce = v["nonce"].as_str().ok_or("the bridge descriptor names no `nonce`")?;
        if !addr.starts_with("127.0.0.1:") {
            return Err(format!(
                "the bridge descriptor names {addr:?}, which is not a loopback address — this \
                 bridge connects to nothing else"
            ));
        }
        Ok(Self { addr: addr.to_string(), nonce: nonce.to_string() })
    }
}

/// The `--mcp-config` document that points the CLI at this harness.
///
/// One stdio server, whose command is THIS binary re-entered as `mcp-bridge`. No `env` map: the
/// bridge takes its one input as an argv PATH, so nothing about the harness's whereabouts becomes
/// an environment variable a settings-registry row would then have to exist for.
pub fn mcp_config(bridge_exe: &Path, descriptor: &Path) -> Value {
    let mut servers = serde_json::Map::new();
    servers.insert(
        MCP_SERVER_NAME.to_string(),
        json!({
            "type": "stdio",
            "command": bridge_exe.display().to_string(),
            "args": ["mcp-bridge", descriptor.display().to_string()],
        }),
    );
    json!({ "mcpServers": Value::Object(servers) })
}

/// The `--allowedTools` names for one roster, DERIVED from what `tools/list` answered.
///
/// ⚠ Every roster tool must be named. `--tools ""` removes the built-in tools but does NOT filter
/// the MCP roster (measured: a run naming one tool still exposed both), and `--allowedTools` is a
/// PERMISSION list — with `--permission-prompts none` an unnamed tool is denied automatically, so a
/// hand-written subset would silently evaluate the agent against a smaller surface than the server
/// advertises.
pub fn allowed_tool_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|t| t["name"].as_str())
        .map(|name| format!("mcp__{MCP_SERVER_NAME}__{name}"))
        .collect()
}

/// Everything the argv is built from.
pub struct Invocation<'a> {
    pub mcp_config: &'a Path,
    pub allowed_tools: &'a [String],
    /// `None` leaves the flag off entirely, so the subscription's own default model answers — which
    /// is the point of this driver, and which the `system`/`init` line then names.
    pub model: Option<&'a str>,
    pub system: &'a str,
}

/// The CLI arguments, verbatim.
///
/// Every flag here was read off the real `claude --help` and then RUN against the real CLI. Three
/// of them are not decoration:
///
///   * `--verbose` is MANDATORY with `--output-format stream-json` under `--print` — without it the
///     CLI refuses with `When using --print, --output-format=stream-json requires --verbose` and
///     produces no stream at all.
///   * `--allowedTools` is VARIADIC and is therefore LAST, with nothing after it. A variadic option
///     consumes every following token that does not start with `-`.
///   * there is NO positional prompt: the prompt is written to the child's stdin. That keeps it out
///     of a process listing and, more importantly, out of reach of the variadic above.
pub fn argv(inv: &Invocation<'_>) -> Vec<String> {
    let mut out: Vec<String> = vec![
        "-p".into(),
        "--verbose".into(),
        "--output-format".into(),
        "stream-json".into(),
        // The SAME instruction the API driver gives, so the two measurements differ in the model
        // and the billing rather than in what the agent was told.
        "--system-prompt".into(),
        inv.system.to_string(),
        "--mcp-config".into(),
        inv.mcp_config.display().to_string(),
        // Only ours: the box's own MCP servers must not join a trading evaluation.
        "--strict-mcp-config".into(),
        // No user, project or local settings file — the box's hooks and permissions are not part of
        // the surface under test.
        "--setting-sources".into(),
        String::new(),
        // Nothing of this run is written into the operator's session history.
        "--no-session-persistence".into(),
        "--disable-slash-commands".into(),
        // Anything that would prompt is DENIED rather than bypassed. `--dangerously-skip-permissions`
        // would be the other way to make a headless run not stall, and it is the wrong way here.
        "--permission-prompts".into(),
        "none".into(),
        // No built-in tools at all: the agent may reach the system ONLY through the MCP roster it is
        // being evaluated against.
        "--tools".into(),
        String::new(),
    ];
    if let Some(model) = inv.model {
        out.push("--model".into());
        out.push(model.to_string());
    }
    out.push("--allowedTools".into());
    out.extend(inv.allowed_tools.iter().cloned());
    out
}

/// What one answer to the CLI's MCP client is: a line to write back, or nothing at all.
///
/// ⚠ `PartialEq` only — `serde_json::Value` is not `Eq` (it carries `f64`), so a derived `Eq` here
/// would not compile.
#[derive(Debug, Clone, PartialEq)]
pub enum Served {
    Answer(Value),
    /// A notification (no `id`) takes no answer. Answering one is a protocol error.
    Nothing,
}

fn rpc_ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// One `tools/call` answer in the MCP envelope, from what the real server said.
///
/// ⚠ The server's own text is passed through VERBATIM, `isError` included — the same rule
/// `crates/vike-agent-eval/src/anthropic.rs`'s `tool_result_block` obeys, for the same reason: the
/// MCP refusals are sentences written for an agent to act on, and a harness that paraphrased one
/// would be evaluating its paraphrase instead of the shipped surface. `structuredContent` is
/// omitted rather than sent as `null` when the answer carried none, because the field is specified
/// as an object.
pub fn tool_envelope(result: &ToolResult) -> Value {
    let mut out = json!({
        "content": [{ "type": "text", "text": result.text }],
        "isError": result.is_error,
    });
    if result.structured.is_object() {
        out["structuredContent"] = result.structured.clone();
    }
    out
}

/// Answer one JSON-RPC message from the CLI's MCP client.
///
/// This is the whole server the CLI sees, and it is deliberately tiny: it advertises the tools
/// capability and nothing else, so the client asks for nothing else. `Err` is reserved for the ONE
/// thing that must abort the drive — the real `vike-cli mcp` server on the far side of the channel
/// being gone. A tool that answered `isError` is not that: it is information the agent must read.
pub fn serve_rpc(
    msg: &Value,
    tools: &[Value],
    instructions: &str,
    channel: &mut dyn ToolChannel,
    calls_left: &mut usize,
) -> Result<Served, String> {
    let Some(id) = msg.get("id").filter(|v| !v.is_null()).cloned() else {
        return Ok(Served::Nothing);
    };
    match msg["method"].as_str().unwrap_or_default() {
        // ⚠ The real server's `instructions` are FORWARDED here, and this is the one place the
        // bridge may not be a tiny server of its own invention. The field is part of the surface
        // under test (`crates/vike-cli/src/cmd/mcp.rs`'s `instructions`), and this handshake is the
        // only route by which a REAL MCP client — the Claude Code CLI — can be handed it. A bridge
        // that answered with its own empty handshake would silently withhold a shipped surface from
        // exactly the driver that exists to measure how a real client treats it. OMITTED rather
        // than sent empty when the server served none: the field is optional and an empty string is
        // not the same claim as an absent one.
        "initialize" => {
            let mut result = json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_INFO_NAME, "version": env!("CARGO_PKG_VERSION") },
            });
            if !instructions.is_empty() {
                result["instructions"] = json!(instructions);
            }
            Ok(Served::Answer(rpc_ok(&id, result)))
        }
        "ping" => Ok(Served::Answer(rpc_ok(&id, json!({})))),
        // ⚠ The roster VERBATIM, `annotations` and all. The tool descriptions ARE the surface under
        // test, and the write-refusal checks read `destructiveHint` off this same roster.
        "tools/list" => Ok(Served::Answer(rpc_ok(&id, json!({ "tools": tools })))),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or_default().to_string();
            let args = match msg["params"]["arguments"].clone() {
                Value::Null => json!({}),
                other => other,
            };
            if *calls_left == 0 {
                let refusal = ToolResult {
                    is_error: true,
                    text: BUDGET_EXHAUSTED.to_string(),
                    structured: Value::Null,
                };
                return Ok(Served::Answer(rpc_ok(&id, tool_envelope(&refusal))));
            }
            *calls_left -= 1;
            // A tool the server does not advertise still goes THROUGH the server, which answers
            // `unknown tool` — the same choice the API driver makes, because "the agent called a
            // tool that does not exist" is a finding the transcript must carry.
            let result = channel.call(&name, &args)?;
            Ok(Served::Answer(rpc_ok(&id, tool_envelope(&result))))
        }
        other => Ok(Served::Answer(rpc_err(
            &id,
            -32601,
            &format!(
                "this harness's MCP bridge serves initialize, ping, tools/list and tools/call; it \
                 was asked for {other:?}"
            ),
        ))),
    }
}

/// What the CLI's `--output-format stream-json` said, reduced to what this driver reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamSummary {
    /// The model id off the `system`/`init` line — what actually answered.
    pub model: Option<String>,
    /// `(name, status)` per MCP server the CLI connected.
    pub mcp_servers: Vec<(String, String)>,
    /// The tool names the CLI exposed to the model, `mcp__<server>__<tool>`-prefixed.
    pub exposed_tools: Vec<String>,
    /// The `result` line's own text — the answer as the operator would read it.
    pub final_text: String,
    /// The `result` line's `is_error`. ⚠ ABSENT reads as an error: a stream shape this driver does
    /// not understand must never be graded as a run that happened.
    pub is_error: bool,
    pub turns: usize,
    pub result_seen: bool,
}

/// Reduce the CLI's stream to a [`StreamSummary`].
///
/// Lines that are not JSON are IGNORED for parsing but reported when nothing usable was found — the
/// CLI writes its own startup refusals as plain text, and swallowing them would turn "the flags
/// were wrong" into "the model said nothing".
pub fn parse_stream(lines: &[String]) -> Result<StreamSummary, String> {
    let mut out = StreamSummary { is_error: true, ..StreamSummary::default() };
    let mut noise: Vec<&str> = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            noise.push(trimmed);
            continue;
        };
        match v["type"].as_str() {
            Some("system") if v["subtype"] == json!("init") => {
                out.model = v["model"].as_str().map(str::to_string);
                out.mcp_servers = v["mcp_servers"]
                    .as_array()
                    .map(|servers| {
                        servers
                            .iter()
                            .map(|s| {
                                (
                                    s["name"].as_str().unwrap_or_default().to_string(),
                                    s["status"].as_str().unwrap_or_default().to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.exposed_tools = v["tools"]
                    .as_array()
                    .map(|ts| ts.iter().filter_map(|t| t.as_str()).map(str::to_string).collect())
                    .unwrap_or_default();
            }
            Some("result") => {
                out.result_seen = true;
                out.final_text = v["result"].as_str().unwrap_or_default().to_string();
                out.is_error = v["is_error"].as_bool().unwrap_or(true);
                out.turns = usize::try_from(v["num_turns"].as_u64().unwrap_or(0)).unwrap_or(0);
            }
            _ => {}
        }
    }
    if !out.result_seen {
        let tail: Vec<&str> = noise.iter().rev().take(5).copied().collect();
        return Err(format!(
            "the Claude Code CLI produced no `result` line, so nothing was measured. {} JSON \
             line(s) and {} non-JSON line(s) were read{}",
            lines.len().saturating_sub(noise.len()),
            noise.len(),
            if tail.is_empty() {
                String::new()
            } else {
                format!("; its plain output was: {}", tail.join(" | "))
            }
        ));
    }
    Ok(out)
}

/// Turn a parsed stream into the case's outcome, or into the reason there is none.
///
/// ⚠ Three ways a run reads GREEN while having measured nothing, all refused here: a stream with no
/// `result` (refused in [`parse_stream`]), a run whose MCP server never connected (the agent had no
/// tools at all), and a `result` carrying `is_error` — which is what an expired subscription
/// session looks like, and whose `result` TEXT is an apology the grader would otherwise have read
/// as the agent's answer.
pub fn outcome_of(summary: &StreamSummary) -> Result<Outcome, String> {
    let connected = summary
        .mcp_servers
        .iter()
        .any(|(name, status)| name == MCP_SERVER_NAME && status == "connected");
    if !connected {
        return Err(format!(
            "the Claude Code CLI did not connect the harness's MCP server {MCP_SERVER_NAME:?}, so \
             the agent had no tools and NOTHING was measured; it reported {:?}",
            summary.mcp_servers
        ));
    }
    if summary.exposed_tools.is_empty() {
        return Err(
            "the Claude Code CLI exposed an EMPTY tool roster to the model, so nothing could be \
             evaluated"
                .to_string(),
        );
    }
    if summary.is_error {
        return Err(format!(
            "the Claude Code CLI reported a FAILED run rather than an answer: {}",
            summary.final_text
        ));
    }
    Ok(Outcome { final_text: summary.final_text.clone(), steps: summary.turns })
}

/// The Claude Code CLI as a driver.
pub struct ClaudeCli {
    binary: PathBuf,
    token: String,
    /// What `--model` was asked for, if anything.
    requested: Option<String>,
    /// What the CLI said actually answered, once a case has run.
    observed: Option<String>,
    max_tool_calls: usize,
    timeout: Duration,
    /// The executable the `--mcp-config` names as the bridge. `None` means
    /// `std::env::current_exe()`, which is the right answer for the shipped binary and the WRONG
    /// one for a test harness — a test binary has no `mcp-bridge` verb — so the live smoke names it
    /// outright. See [`ClaudeCli::with_bridge_exe`].
    bridge_exe: Option<PathBuf>,
}

impl fmt::Debug for ClaudeCli {
    /// Redacts the token. The manual impl is the point: a derived one would print a live
    /// subscription credential into any error, panic message or debug log that formats this struct.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeCli")
            .field("binary", &self.binary)
            .field("requested", &self.requested)
            .field("observed", &self.observed)
            .field("max_tool_calls", &self.max_tool_calls)
            .field("timeout", &self.timeout)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl ClaudeCli {
    /// `token` comes from `main.rs`'s one environment read and is never stored anywhere else.
    pub fn new(
        binary: PathBuf,
        token: String,
        requested: Option<String>,
        max_tool_calls: usize,
    ) -> Self {
        Self {
            binary,
            token,
            requested,
            observed: None,
            max_tool_calls,
            timeout: RUN_TIMEOUT,
            bridge_exe: None,
        }
    }

    /// Shorten the whole-run ceiling. Used by the bounded-wait test, which has to outlive nothing.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Name the executable the CLI will spawn as the MCP bridge, instead of this process.
    ///
    /// ⚠ It exists because `std::env::current_exe()` answers the RUNNING executable — the
    /// `vike-agent-eval` binary in production, and a `tests/` binary under `cargo test`. A test
    /// binary has no `mcp-bridge` verb, so without this override the live smoke could only ever
    /// have failed for a reason that had nothing to do with the bridge. It is an OVERRIDE, never a
    /// default: taking the shipped path from `current_exe` is what keeps the bridge and the harness
    /// the same build, which is the property that lets the bridge forward frames unread.
    #[must_use]
    pub fn with_bridge_exe(mut self, exe: PathBuf) -> Self {
        self.bridge_exe = Some(exe);
        self
    }

    /// The `Command` this driver spawns — BUILT and returned rather than spawned, so a test can
    /// read the argv and the environment the child would be given without a process existing.
    ///
    /// ⚠ The token is SET here, explicitly. It would also be inherited (this process holds it), and
    /// that is exactly why it is set: inheritance is invisible, and a reader could not tell the one
    /// child that must have the token from the two children `main.rs`'s `SCRUB_FROM_CHILDREN`
    /// removes it from.
    pub fn command(&self, inv: &Invocation<'_>, cwd: &Path) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.args(argv(inv));
        cmd.current_dir(cwd);
        cmd.env(OAUTH_TOKEN_ENV, &self.token);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        cmd
    }
}

/// Kills the CLI on every way out of [`ModelDriver::drive`], the `?` paths included.
///
/// Without it a driver error would return while a model was still working, holding this case's
/// paper node and this case's socket — and the next case is about to want both.
struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn get(&mut self) -> Result<&mut Child, String> {
        self.0.as_mut().ok_or_else(|| "the Claude Code CLI child is gone".to_string())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One live bridge connection, and the bytes of a frame that has not finished arriving.
struct Conn {
    stream: TcpStream,
    buf: Vec<u8>,
    /// The nonce line is the FIRST line on the socket; nothing is served before it matches.
    verified: bool,
}

/// A cheap per-run handshake token: the process id and the clock, folded. Uniqueness across a box
/// is not claimed and is not needed — see [`Bridge`].
fn mint_nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", std::process::id(), nanos)
}

/// The last of a child's stderr, so an error says more than that something died.
fn stderr_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let tail: String = text.lines().rev().take(10).collect::<Vec<_>>().join(" | ");
    if tail.is_empty() { "its stderr is empty".into() } else { format!("its stderr: {tail}") }
}

/// Split every COMPLETE newline-terminated frame out of an accumulating buffer.
fn take_lines(buf: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(at) = buf.iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = buf.drain(..=at).collect();
        if let Ok(text) = String::from_utf8(line) {
            let text = text.trim().to_string();
            if !text.is_empty() {
                out.push(text);
            }
        }
    }
    out
}

/// `true` when a read error means "nothing arrived", rather than "the peer is gone".
fn is_would_block(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted)
}

impl ModelDriver for ClaudeCli {
    /// The model id the CLI reported once a case has run; before that, what was asked for.
    fn name(&self) -> String {
        self.observed
            .clone()
            .or_else(|| self.requested.clone())
            .unwrap_or_else(|| "claude-code-cli (model not yet reported)".to_string())
    }

    fn drive(
        &mut self,
        ctx: &DriveContext<'_>,
        prompt: &str,
        tools: &[Value],
        channel: &mut dyn ToolChannel,
    ) -> Result<Outcome, String> {
        let allowed = allowed_tool_names(tools);
        if allowed.is_empty() {
            return Err(format!(
                "the MCP server advertised no named tool for case {}, so the CLI would have been \
                 given an empty permission list and NOTHING would have been measured",
                ctx.case
            ));
        }

        // Listening BEFORE the CLI is spawned, so the bridge's connect can never lose a race.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("bind a loopback port for the MCP bridge: {e}"))?;
        let addr =
            listener.local_addr().map_err(|e| format!("read the MCP bridge's own address: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("make the MCP bridge listener pollable: {e}"))?;

        let bridge = Bridge { addr: addr.to_string(), nonce: mint_nonce() };
        let descriptor_path = ctx.work_dir.join("claude-bridge.json");
        std::fs::write(&descriptor_path, bridge.to_json().to_string())
            .map_err(|e| format!("write {}: {e}", descriptor_path.display()))?;
        let exe = match &self.bridge_exe {
            Some(named) => named.clone(),
            None => std::env::current_exe()
                .map_err(|e| format!("locate this harness's own binary for the MCP bridge: {e}"))?,
        };
        let config_path = ctx.work_dir.join("claude-mcp.json");
        std::fs::write(&config_path, mcp_config(&exe, &descriptor_path).to_string())
            .map_err(|e| format!("write {}: {e}", config_path.display()))?;

        let inv = Invocation {
            mcp_config: &config_path,
            allowed_tools: &allowed,
            model: self.requested.as_deref(),
            system: SYSTEM,
        };
        let stderr_path = ctx.work_dir.join("claude.err");
        let stderr = std::fs::File::create(&stderr_path)
            .map_err(|e| format!("create {}: {e}", stderr_path.display()))?;
        let mut cmd = self.command(&inv, ctx.work_dir);
        cmd.stderr(Stdio::from(stderr));
        let mut child = ChildGuard(Some(cmd.spawn().map_err(|e| {
            format!(
                "spawn {} for case {}: {e}. The Claude Code CLI must be installed and on PATH (or \
                 named with --claude); `claude --version` is the check.",
                self.binary.display(),
                ctx.case
            )
        })?));

        // The prompt goes in on stdin and the pipe is then CLOSED — that is how `-p` learns the
        // prompt ended — which also keeps it out of a process listing and out of the way of the
        // variadic `--allowedTools`.
        {
            let mut stdin = child
                .get()?
                .stdin
                .take()
                .ok_or("the Claude Code CLI child has no stdin to write the prompt to")?;
            stdin
                .write_all(prompt.as_bytes())
                .and_then(|()| stdin.flush())
                .map_err(|e| format!("write the prompt for case {} to the CLI: {e}", ctx.case))?;
        }

        // A reader THREAD, so the serve loop below can carry a deadline. It ends when the child
        // closes its stdout, which disconnects the channel.
        let stdout = child.get()?.stdout.take().ok_or("the Claude Code CLI child has no stdout")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });

        let deadline = Instant::now() + self.timeout;
        let mut lines: Vec<String> = Vec::new();
        let mut conn: Option<Conn> = None;
        let mut calls_left = self.max_tool_calls;
        let mut budget_exhausted = false;

        let status = loop {
            while let Ok(line) = rx.try_recv() {
                lines.push(line);
            }
            match child.get()?.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => {
                    return Err(format!("waiting for the CLI on case {}: {e}", ctx.case));
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the Claude Code CLI did not finish case {} within {}s and was KILLED, so the \
                     case has no answer; {}",
                    ctx.case,
                    self.timeout.as_secs(),
                    stderr_tail(&stderr_path)
                ));
            }
            if conn.is_none() {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .and_then(|()| stream.set_read_timeout(Some(POLL)))
                            .map_err(|e| format!("configure the MCP bridge connection: {e}"))?;
                        conn = Some(Conn { stream, buf: Vec::new(), verified: false });
                    }
                    Err(e) if is_would_block(&e) => {}
                    Err(e) => return Err(format!("accept the MCP bridge connection: {e}")),
                }
            }
            // ⚠ The connection is dropped through a FLAG rather than by assigning `conn = None`
            // inside the borrow: `active` borrows `conn` for the whole block below, so the direct
            // spelling does not compile. `abort` is the same shape for the one error that must end
            // the drive — the real MCP server on the far side of the channel being gone.
            let mut drop_conn = false;
            let mut abort: Option<String> = None;
            if let Some(active) = conn.as_mut() {
                let mut chunk = [0u8; 8192];
                match active.stream.read(&mut chunk) {
                    // The bridge is gone. Keep listening: the CLI shuts its MCP servers down at the
                    // end of a run, and that is not a failure of the case.
                    Ok(0) => drop_conn = true,
                    Ok(n) => active.buf.extend_from_slice(&chunk[..n]),
                    Err(e) if is_would_block(&e) => {}
                    Err(_) => drop_conn = true,
                }
                if !drop_conn {
                    for frame in take_lines(&mut active.buf) {
                        let Ok(msg) = serde_json::from_str::<Value>(&frame) else {
                            // Not JSON: this is not our bridge, or the pipe is corrupt. Drop it
                            // rather than answering something that never spoke the protocol.
                            drop_conn = true;
                            break;
                        };
                        if !active.verified {
                            if msg["nonce"].as_str() == Some(bridge.nonce.as_str()) {
                                active.verified = true;
                                continue;
                            }
                            drop_conn = true;
                            break;
                        }
                        // ⚠ Read BEFORE the call, never after: `calls_left` reaches zero on the
                        // LAST call the budget allowed, and reading it afterwards would report a
                        // run that fitted its budget exactly as one that blew it.
                        let over_budget = msg["method"] == json!("tools/call") && calls_left == 0;
                        match serve_rpc(
                            &msg,
                            tools,
                            ctx.instructions,
                            &mut *channel,
                            &mut calls_left,
                        ) {
                            Ok(served) => {
                                if over_budget {
                                    budget_exhausted = true;
                                }
                                if let Served::Answer(answer) = served {
                                    let line = format!("{answer}\n");
                                    if active.stream.write_all(line.as_bytes()).is_err() {
                                        drop_conn = true;
                                        break;
                                    }
                                    let _ = active.stream.flush();
                                }
                            }
                            Err(e) => {
                                abort = Some(e);
                                break;
                            }
                        }
                    }
                }
            } else {
                // Nothing to read from and nothing accepted yet: pace the loop rather than spin.
                std::thread::sleep(POLL);
            }
            if let Some(e) = abort {
                return Err(e);
            }
            if drop_conn {
                conn = None;
            }
        };

        // The child has exited; its remaining stdout is still in flight. Bounded, like every other
        // wait here: the reader thread disconnects the channel when the pipe closes.
        let drain = Instant::now() + DRAIN_GRACE;
        loop {
            match rx.recv_timeout(POLL) {
                Ok(line) => lines.push(line),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= drain {
                        break;
                    }
                }
            }
        }

        let summary = parse_stream(&lines).map_err(|e| {
            format!(
                "{e} (case {}, the CLI exited {status}); {}",
                ctx.case,
                stderr_tail(&stderr_path)
            )
        })?;
        // What actually answered, for the report — read even when the run then fails, because a
        // failure by a named model is more useful than a failure by an unnamed one.
        if let Some(model) = &summary.model {
            self.observed = Some(model.clone());
        }
        if budget_exhausted {
            return Err(format!(
                "the agent was still calling tools after {} tool call(s) (--max-steps) on case {}; \
                 the case has no answer. Last text: {}",
                self.max_tool_calls, ctx.case, summary.final_text
            ));
        }
        outcome_of(&summary)
            .map_err(|e| format!("{e} (case {}, the CLI exited {status})", ctx.case))
    }
}

/// The `mcp-bridge` mode: a byte pump between the CLI's stdio and the harness's loopback socket.
///
/// It holds no state and makes no decision — every JSON-RPC frame is forwarded unread, so the
/// protocol lives in exactly one place ([`serve_rpc`], in the harness) and this half cannot answer
/// differently. It exists only because `--mcp-config` can name a command to spawn or a URL to
/// fetch, and this crate may not add an HTTP server to be the URL.
pub fn run_bridge(descriptor: &Path) -> Result<(), String> {
    let raw = std::fs::read_to_string(descriptor)
        .map_err(|e| format!("read the bridge descriptor {}: {e}", descriptor.display()))?;
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("the bridge descriptor {} is not JSON: {e}", descriptor.display()))?;
    let bridge = Bridge::from_json(&parsed)?;
    let addr: SocketAddr = bridge
        .addr
        .parse()
        .map_err(|e| format!("the bridge descriptor's addr {:?} is not one: {e}", bridge.addr))?;
    let stream = TcpStream::connect_timeout(&addr, BRIDGE_CONNECT_TIMEOUT)
        .map_err(|e| format!("connect the MCP bridge to {addr}: {e}"))?;
    let mut up = stream.try_clone().map_err(|e| format!("split the MCP bridge connection: {e}"))?;
    // The handshake line, first and once: the harness serves nothing until it matches.
    up.write_all(format!("{}\n", json!({ "nonce": bridge.nonce })).as_bytes())
        .and_then(|()| up.flush())
        .map_err(|e| format!("greet the harness over the MCP bridge: {e}"))?;

    // CLI stdin -> harness, on its own thread; harness -> CLI stdout, here. Each half ends when its
    // source reaches EOF, and the process ends with the second — which is what the CLI's own
    // shutdown of its MCP servers produces.
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let _ = std::io::copy(&mut stdin, &mut up);
        let _ = up.shutdown(Shutdown::Write);
    });
    let mut down = stream;
    let mut stdout = std::io::stdout();
    std::io::copy(&mut down, &mut stdout)
        .map_err(|e| format!("forward the harness's answers to the CLI: {e}"))?;
    stdout.flush().map_err(|e| format!("flush the MCP bridge's output: {e}"))?;
    Ok(())
}
